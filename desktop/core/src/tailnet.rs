//! Tailnet name resolution that survives a VPN owning the system DNS.
//!
//! With NordVPN up, every DNS query goes to its resolvers, which answer a
//! `*.ts.net` name with the public IPv6 addresses tailscale publishes, and
//! the client then times out against them. Tailscale's MagicDNS still
//! answers correctly on 100.100.100.100 (reachable through the tailscale
//! interface whatever DNS the VPN imposes), so for `.ts.net` names whose
//! system answer is not a tailnet address (100.64.0.0/10, fd7a:115c:a1e0::/48)
//! or fails, we ask MagicDNS directly with a hand-rolled A query.
//!
//! * [`resolve`] -- host + port -> socket addresses, with that fallback
//!   (`$FINANCE_MAGIC_DNS` stands a fake MagicDNS in for tests)
//! * [`Resolver`] -- the same, as a ureq resolver; [`agent`] builds a client
//! * [`encode_query`] / [`decode_a`] -- the ~60 lines of DNS wire format

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// Tailscale's in-tunnel resolver.
pub const MAGIC_DNS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 100, 100, 100)), 53);

/// MagicDNS, or `$FINANCE_MAGIC_DNS` (`host:port`): a stand-in resolver for
/// testing the fallback without a tailnet.
pub fn magic_dns() -> SocketAddr {
    std::env::var("FINANCE_MAGIC_DNS").ok().and_then(|v| v.parse().ok()).unwrap_or(MAGIC_DNS)
}
const SYSTEM_TIMEOUT: Duration = Duration::from_secs(2);
const MAGIC_TIMEOUT: Duration = Duration::from_millis(1500);

/// Is this an address tailscale hands out (CGNAT range or its ULA prefix)?
pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xC0) == 64
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

pub fn is_tailnet_name(host: &str) -> bool {
    host.trim_end_matches('.').to_ascii_lowercase().ends_with(".ts.net")
}

/// A recursive A-record query for `name` (RFC 1035 section 4).
pub fn encode_query(id: u16, name: &str) -> Result<Vec<u8>> {
    let mut q = Vec::with_capacity(18 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00]); // flags: RD
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // QD=1, AN=NS=AR=0
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            bail!("bad DNS name {name:?}");
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1, 0, 1]); // QTYPE=A, QCLASS=IN
    Ok(q)
}

/// Offset just past a (possibly compressed) name starting at `i`.
fn skip_name(msg: &[u8], mut i: usize) -> Result<usize> {
    loop {
        let len = *msg.get(i).context("truncated name")? as usize;
        match len {
            0 => return Ok(i + 1),
            l if l & 0xC0 == 0xC0 => return Ok(i + 2), // pointer ends the name
            l => i += 1 + l,
        }
    }
}

fn u16_at(msg: &[u8], i: usize) -> Result<u16> {
    Ok(u16::from_be_bytes([*msg.get(i).context("truncated")?, *msg.get(i + 1).context("truncated")?]))
}

/// The A records of a response to query `id`.
pub fn decode_a(msg: &[u8], id: u16) -> Result<Vec<Ipv4Addr>> {
    if msg.len() < 12 || u16_at(msg, 0)? != id {
        bail!("DNS reply does not match the query");
    }
    let flags = u16_at(msg, 2)?;
    if flags & 0x8000 == 0 {
        bail!("not a DNS response");
    }
    if flags & 0x000F != 0 {
        bail!("DNS rcode {}", flags & 0x000F);
    }
    let (qd, an) = (u16_at(msg, 4)?, u16_at(msg, 6)?);
    let mut i = 12;
    for _ in 0..qd {
        i = skip_name(msg, i)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        i = skip_name(msg, i)?;
        let (rtype, class, rdlen) = (u16_at(msg, i)?, u16_at(msg, i + 2)?, u16_at(msg, i + 8)? as usize);
        i += 10;
        let rdata = msg.get(i..i + rdlen).context("truncated rdata")?;
        if rtype == 1 && class == 1 && rdlen == 4 {
            out.push(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]));
        }
        i += rdlen; // CNAMEs etc. are skipped; MagicDNS answers with A directly
    }
    Ok(out)
}

/// Ask a DNS server (MagicDNS by default) for `name`'s A records over UDP.
pub fn query_a(server: SocketAddr, name: &str, timeout: Duration) -> Result<Vec<Ipv4Addr>> {
    let id = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(7)
        & 0xFFFF) as u16;
    let sock = UdpSocket::bind("0.0.0.0:0").context("udp bind")?;
    sock.set_read_timeout(Some(timeout))?;
    sock.connect(server).with_context(|| format!("udp connect {server}"))?;
    sock.send(&encode_query(id, name)?)?;
    let mut buf = [0u8; 1500];
    loop {
        let n = sock.recv(&mut buf).with_context(|| format!("no answer from {server} for {name}"))?;
        // a stray datagram with another id: keep waiting for ours
        match decode_a(&buf[..n], id) {
            Err(e) if e.to_string().contains("does not match") => continue,
            other => return other,
        }
    }
}

/// The system resolver, bounded (a VPN's DNS can hang rather than fail).
pub fn system_lookup(host: &str, port: u16, timeout: Duration) -> Result<Vec<SocketAddr>> {
    let (tx, rx) = mpsc::sync_channel(1);
    let target = format!("{host}:{port}");
    std::thread::spawn(move || {
        let _ = tx.send(target.to_socket_addrs().map(|a| a.collect::<Vec<_>>()));
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => Ok(r?),
        Err(_) => bail!("system DNS timed out for {host}"),
    }
}

/// host + port -> addresses. IP literals pass through; `.ts.net` names whose
/// system answer holds no tailnet address fall back to MagicDNS (and, if
/// that fails too, to whatever the system said).
pub fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    resolve_via(host, port, magic_dns())
}

/// [`resolve`] asking `magic` instead of MagicDNS.
pub fn resolve_via(host: &str, port: u16, magic: SocketAddr) -> Result<Vec<SocketAddr>> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let system = system_lookup(bare, port, SYSTEM_TIMEOUT);
    if !is_tailnet_name(bare) {
        return system;
    }
    if let Ok(addrs) = &system {
        let tail: Vec<SocketAddr> = addrs.iter().copied().filter(|a| is_tailnet_ip(a.ip())).collect();
        if !tail.is_empty() {
            return Ok(tail);
        }
    }
    match query_a(magic, bare, MAGIC_TIMEOUT) {
        Ok(ips) if !ips.is_empty() => Ok(ips.into_iter().map(|ip| SocketAddr::new(IpAddr::V4(ip), port)).collect()),
        magic => match system {
            Ok(a) if !a.is_empty() => Ok(a),
            _ => bail!("cannot resolve {bare}: {}", magic.err().map(|e| e.to_string()).unwrap_or("no A record".into())),
        },
    }
}

/// [`resolve`] as a ureq resolver.
#[derive(Debug, Default)]
pub struct Resolver;

impl ureq::unversioned::resolver::Resolver for Resolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let host = uri.host().ok_or(ureq::Error::HostNotFound)?;
        let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("http") { 80 } else { 443 });
        let addrs = resolve(host, port).map_err(|_| ureq::Error::HostNotFound)?;
        let mut out = self.empty();
        for a in addrs.into_iter().take(16) {
            out.push(a);
        }
        if out.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(out)
    }
}

/// A ureq agent that resolves through [`Resolver`]: use it for anything that
/// talks to the server by its tailnet name (sync, push, health checks). Status
/// codes come back as responses, not errors.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build();
    ureq::Agent::with_parts(config, ureq::unversioned::transport::DefaultConnector::new(), Resolver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_wire_format() {
        let q = encode_query(0xBEEF, "server.tail1234.ts.net.").unwrap();
        assert_eq!(&q[..12], &[0xBE, 0xEF, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&q[12..19], &[6, b's', b'e', b'r', b'v', b'e', b'r']);
        assert_eq!(&q[q.len() - 5..], &[0, 0, 1, 0, 1]);
        assert!(encode_query(1, "a..b").is_err());
    }

    /// A reply as MagicDNS sends it: the question echoed, then a compressed
    /// answer name (pointer 0xC00C), a CNAME to skip, and the A record.
    #[test]
    fn decodes_reply_with_compression() {
        let mut r = encode_query(7, "server.x.ts.net").unwrap();
        r[2] = 0x81;
        r[3] = 0x80; // QR + RD + RA, rcode 0
        r[7] = 2; // ANCOUNT
        r.extend_from_slice(&[0xC0, 0x0C, 0, 5, 0, 1, 0, 0, 0, 60, 0, 2, 0xC0, 0x0C]); // CNAME
        r.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 100, 101, 102, 103]);
        assert_eq!(decode_a(&r, 7).unwrap(), vec![Ipv4Addr::new(100, 101, 102, 103)]);
        assert!(decode_a(&r, 8).is_err(), "id mismatch");
        r[3] = 0x83; // NXDOMAIN
        assert!(decode_a(&r, 7).unwrap_err().to_string().contains("rcode 3"));
    }

    #[test]
    fn truncated_reply_is_an_error() {
        let mut r = encode_query(9, "a.ts.net").unwrap();
        r[2] = 0x81;
        r[7] = 1;
        r.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 100]);
        assert!(decode_a(&r, 9).is_err());
    }

    #[test]
    fn tailnet_ranges() {
        assert!(is_tailnet_ip("100.64.0.1".parse().unwrap()));
        assert!(is_tailnet_ip("100.127.255.254".parse().unwrap()));
        assert!(!is_tailnet_ip("100.128.0.1".parse().unwrap()));
        assert!(!is_tailnet_ip("2a05:d014::1".parse().unwrap()));
        assert!(is_tailnet_ip("fd7a:115c:a1e0::1".parse().unwrap()));
        assert!(is_tailnet_name("Server.Tail1234.TS.NET."));
        assert!(!is_tailnet_name("example.com"));
    }

    /// Manual: `FINANCE_TAILNET_HOST=your-server.tailXXXX.ts.net cargo test -p
    /// finance-core tailnet_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn tailnet_live() {
        let host = std::env::var("FINANCE_TAILNET_HOST").expect("set FINANCE_TAILNET_HOST");
        println!("system: {:?}", system_lookup(&host, 443, SYSTEM_TIMEOUT));
        println!("magic:  {:?}", query_a(MAGIC_DNS, &host, MAGIC_TIMEOUT));
        let r = resolve(&host, 443).unwrap();
        println!("resolve: {r:?}");
        assert!(r.iter().all(|a| is_tailnet_ip(a.ip())));
    }

    /// A stand-in MagicDNS on loopback answering every A query with
    /// 127.0.0.1: a `.ts.net` name the system can't place (or places on a
    /// public address, as under NordVPN) resolves through it.
    #[test]
    fn falls_back_to_magic_dns() {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            while let Ok((n, from)) = sock.recv_from(&mut buf) {
                let mut r = buf[..n].to_vec();
                r[2] = 0x81;
                r[3] = 0x80;
                r[7] = 1; // ANCOUNT
                r.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 1]);
                let _ = sock.send_to(&r, from);
            }
        });
        let got = resolve_via("finance-test-nowhere.tail0000.ts.net", 8787, addr).unwrap();
        assert_eq!(got, vec!["127.0.0.1:8787".parse::<SocketAddr>().unwrap()]);
        // not a tailnet name: never asks it
        assert!(resolve_via("finance-test-nowhere.invalid", 80, addr).is_err());
    }

    #[test]
    fn ip_literals_skip_dns() {
        assert_eq!(resolve("127.0.0.1", 8787).unwrap(), vec!["127.0.0.1:8787".parse().unwrap()]);
        assert_eq!(resolve("[::1]", 1).unwrap(), vec!["[::1]:1".parse().unwrap()]);
    }
}
