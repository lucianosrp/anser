//! What this machine has around the app: VPNs and overlay networks, the
//! package manager, Hyprland, the OS. The Doctor uses it to show only the
//! checks that apply and to word their fixes for this machine.
//!
//! Detection is cheap and read-only: executables on `$PATH`, interface names
//! in `/sys/class/net`, app bundles in `/Applications` (macOS),
//! `/etc/os-release`, one environment variable. Nothing is run.
//!
//! [`detect`] fills an [`Env`]; everything after that (the Doctor's verdicts,
//! [`CheckId::applies`](crate::doctor::CheckId::applies)) is a pure function
//! of the `Env`, so tests build synthetic ones. The lookups themselves go
//! through [`Probe`], so [`detect_with`] is tested against a fake machine.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Os {
    Linux,
    MacOs,
    Other,
}

impl Os {
    pub fn current() -> Os {
        match std::env::consts::OS {
            "linux" => Os::Linux,
            "macos" => Os::MacOs,
            _ => Os::Other,
        }
    }
}

// ---------- VPNs and overlay networks ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Vpn {
    Tailscale,
    Twingate,
    NordVpn,
    Mullvad,
    ProtonVpn,
    Warp,
    ZeroTier,
    NetBird,
    WireGuard,
    OpenVpn,
}

impl Vpn {
    /// Most specific first: an interface goes to the first VPN that claims it
    /// (wg-mullvad is Mullvad's, not plain WireGuard's).
    pub const ALL: [Vpn; 10] = [
        Vpn::Tailscale, Vpn::Twingate, Vpn::NordVpn, Vpn::Mullvad, Vpn::ProtonVpn,
        Vpn::Warp, Vpn::ZeroTier, Vpn::NetBird, Vpn::WireGuard, Vpn::OpenVpn,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Vpn::Tailscale => "Tailscale",
            Vpn::Twingate => "Twingate",
            Vpn::NordVpn => "NordVPN",
            Vpn::Mullvad => "Mullvad",
            Vpn::ProtonVpn => "ProtonVPN",
            Vpn::Warp => "Cloudflare WARP",
            Vpn::ZeroTier => "ZeroTier",
            Vpn::NetBird => "NetBird",
            Vpn::WireGuard => "WireGuard",
            Vpn::OpenVpn => "OpenVPN",
        }
    }

    pub fn from_label(s: &str) -> Option<Vpn> {
        Vpn::ALL.into_iter().find(|v| v.label().eq_ignore_ascii_case(s))
    }

    /// Its command-line tools.
    pub fn executables(self) -> &'static [&'static str] {
        match self {
            Vpn::Tailscale => &["tailscale"],
            Vpn::Twingate => &["twingate"],
            Vpn::NordVpn => &["nordvpn"],
            Vpn::Mullvad => &["mullvad"],
            Vpn::ProtonVpn => &["protonvpn", "protonvpn-app", "protonvpn-cli"],
            Vpn::Warp => &["warp-cli"],
            Vpn::ZeroTier => &["zerotier-cli"],
            Vpn::NetBird => &["netbird"],
            Vpn::WireGuard => &["wg", "wg-quick"],
            Vpn::OpenVpn => &["openvpn"],
        }
    }

    /// Its macOS app bundle in /Applications.
    pub fn app_bundle(self) -> Option<&'static str> {
        Some(match self {
            Vpn::Tailscale => "Tailscale.app",
            Vpn::Twingate => "Twingate.app",
            Vpn::NordVpn => "NordVPN.app",
            Vpn::Mullvad => "Mullvad VPN.app",
            Vpn::ProtonVpn => "ProtonVPN.app",
            Vpn::Warp => "Cloudflare WARP.app",
            Vpn::ZeroTier => "ZeroTier One.app",
            Vpn::WireGuard => "WireGuard.app",
            Vpn::NetBird | Vpn::OpenVpn => return None,
        })
    }

    /// Does a network interface of this name belong to it?
    pub fn owns_interface(self, name: &str) -> bool {
        let n = name;
        match self {
            Vpn::Tailscale => n == "tailscale0",
            Vpn::Twingate => n == "sdwan0" || n.starts_with("tg"),
            Vpn::NordVpn => n == "nordlynx" || n.starts_with("nordtun"),
            Vpn::Mullvad => n.starts_with("wg-mullvad") || n.starts_with("wg0-mullvad"),
            Vpn::ProtonVpn => n.starts_with("proton") || n.starts_with("pvpn"),
            Vpn::Warp => n == "CloudflareWARP",
            Vpn::ZeroTier => n.starts_with("zt"),
            Vpn::NetBird => n == "wt0",
            Vpn::WireGuard => n.starts_with("wg"),
            Vpn::OpenVpn => n.starts_with("tun"),
        }
    }
}

/// One VPN as found on this machine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VpnFound {
    pub vpn: Vpn,
    /// its tool, app or interface is there
    pub installed: bool,
    /// its interface is present
    pub active: bool,
    /// what matched: "nordvpn on PATH", "interface nordlynx", "/Applications/NordVPN.app"
    pub signals: Vec<String>,
}

impl VpnFound {
    /// "NordVPN (up: interface nordlynx)", "Twingate (installed, not up)"
    pub fn describe(&self) -> String {
        let iface: Vec<&str> = self.signals.iter().filter_map(|s| s.strip_prefix("interface ")).collect();
        if self.active {
            format!("{} (up: {})", self.vpn.label(), iface.join(", "))
        } else {
            format!("{} (installed, not up)", self.vpn.label())
        }
    }
}

// ---------- package managers ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum PkgManager {
    Pacman,
    Apt,
    Dnf,
    Zypper,
    Apk,
    Brew,
}

impl PkgManager {
    /// Linux order; brew last (Linuxbrew sits beside the distro's own).
    pub const ALL: [PkgManager; 6] =
        [PkgManager::Pacman, PkgManager::Apt, PkgManager::Dnf, PkgManager::Zypper, PkgManager::Apk, PkgManager::Brew];

    pub fn executable(self) -> &'static str {
        match self {
            PkgManager::Pacman => "pacman",
            PkgManager::Apt => "apt",
            PkgManager::Dnf => "dnf",
            PkgManager::Zypper => "zypper",
            PkgManager::Apk => "apk",
            PkgManager::Brew => "brew",
        }
    }

    /// The command that installs `pkg`. Shown for the owner to run, never run.
    pub fn install(self, pkg: &str) -> String {
        match self {
            PkgManager::Pacman => format!("sudo pacman -S {pkg}"),
            PkgManager::Apt => format!("sudo apt install {pkg}"),
            PkgManager::Dnf => format!("sudo dnf install {pkg}"),
            PkgManager::Zypper => format!("sudo zypper install {pkg}"),
            PkgManager::Apk => format!("sudo apk add {pkg}"),
            PkgManager::Brew => format!("brew install {pkg}"),
        }
    }

    /// The distro's manager from an /etc/os-release ID (or ID_LIKE word).
    pub fn for_distro(id: &str) -> Option<PkgManager> {
        Some(match id {
            "arch" | "archarm" | "manjaro" | "endeavouros" | "garuda" | "cachyos" | "artix" => PkgManager::Pacman,
            "debian" | "ubuntu" | "linuxmint" | "pop" | "raspbian" | "elementary" | "zorin" | "kali" | "neon" => PkgManager::Apt,
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "nobara" => PkgManager::Dnf,
            "opensuse" | "suse" | "sles" => PkgManager::Zypper,
            id if id.starts_with("opensuse") => PkgManager::Zypper,
            "alpine" | "postmarketos" => PkgManager::Apk,
            _ => return None,
        })
    }
}

// ---------- the snapshot ----------

/// What [`detect`] found. Plain data: build one by hand in tests.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Env {
    pub os: Os,
    /// /etc/os-release ID (Linux)
    pub os_id: Option<String>,
    pub pkg: Option<PkgManager>,
    /// HYPRLAND_INSTANCE_SIGNATURE is set
    pub hyprland: bool,
    /// only the VPNs that were found, in [`Vpn::ALL`] order
    pub vpns: Vec<VpnFound>,
}

impl Env {
    /// An empty machine: Linux, nothing installed.
    pub fn bare(os: Os) -> Env {
        Env { os, os_id: None, pkg: None, hyprland: false, vpns: Vec::new() }
    }

    pub fn vpn(&self, v: Vpn) -> Option<&VpnFound> {
        self.vpns.iter().find(|f| f.vpn == v)
    }

    pub fn has(&self, v: Vpn) -> bool {
        self.vpn(v).is_some_and(|f| f.installed)
    }

    /// Every VPN but Tailscale.
    pub fn other_vpns(&self) -> impl Iterator<Item = &VpnFound> {
        self.vpns.iter().filter(|f| f.vpn != Vpn::Tailscale)
    }

    /// The VPN that most likely owns the system DNS: the first one up that
    /// isn't Tailscale.
    pub fn dns_vpn(&self) -> Option<&VpnFound> {
        self.other_vpns().find(|f| f.active)
    }
}

// ---------- detection ----------

/// The lookups [`detect_with`] makes; [`System`] is the real machine.
pub trait Probe {
    fn on_path(&self, exe: &str) -> bool;
    /// names in /sys/class/net
    fn interfaces(&self) -> Vec<String>;
    fn exists(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> Option<String>;
    fn env(&self, key: &str) -> Option<String>;
}

pub struct System;

impl Probe for System {
    fn on_path(&self, exe: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
    }

    fn interfaces(&self) -> Vec<String> {
        std::fs::read_dir("/sys/class/net").map(|rd| rd.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect())
            .unwrap_or_default()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
}

/// This machine, now. Blocking but quick (a few stat calls).
pub fn detect() -> Env {
    detect_with(&System, Os::current())
}

/// `KEY=value` or `KEY="value"` from os-release.
fn os_release_field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
}

pub fn detect_with(p: &dyn Probe, os: Os) -> Env {
    let ifaces = if os == Os::Linux { p.interfaces() } else { Vec::new() };
    let mut found: Vec<VpnFound> = Vpn::ALL.iter().map(|&vpn| VpnFound { vpn, installed: false, active: false, signals: Vec::new() }).collect();
    for name in &ifaces {
        if let Some(f) = found.iter_mut().find(|f| f.vpn.owns_interface(name)) {
            f.active = true;
            f.installed = true;
            f.signals.push(format!("interface {name}"));
        }
    }
    for f in &mut found {
        for exe in f.vpn.executables() {
            if p.on_path(exe) {
                f.installed = true;
                f.signals.push(format!("{exe} on PATH"));
            }
        }
        if os == Os::MacOs {
            if let Some(app) = f.vpn.app_bundle() {
                let path = PathBuf::from("/Applications").join(app);
                if p.exists(&path) {
                    f.installed = true;
                    f.signals.push(path.display().to_string());
                }
            }
        }
    }
    found.retain(|f| f.installed);

    let release = (os == Os::Linux).then(|| p.read(Path::new("/etc/os-release"))).flatten();
    let os_id = release.as_deref().and_then(|r| os_release_field(r, "ID"));
    let like = release.as_deref().and_then(|r| os_release_field(r, "ID_LIKE")).unwrap_or_default();
    let pkg = match os {
        Os::MacOs => p.on_path("brew").then_some(PkgManager::Brew),
        Os::Other => None,
        Os::Linux => {
            // the distro's own manager when its tool is there, else the first tool found
            let distro = os_id.iter().map(String::as_str).chain(like.split_whitespace())
                .find_map(PkgManager::for_distro).filter(|m| p.on_path(m.executable()));
            distro.or_else(|| PkgManager::ALL.into_iter().find(|m| p.on_path(m.executable())))
        }
    };
    Env { os, os_id, pkg, hyprland: p.env("HYPRLAND_INSTANCE_SIGNATURE").is_some(), vpns: found }
}

// ---------- verdict helpers ----------

/// The integrated GPU's Mesa Vulkan driver for this package manager.
/// `intel`: the CPU is Intel's (else AMD's Radeon driver).
pub fn mesa_vulkan_package(pkg: PkgManager, intel: bool) -> Option<&'static str> {
    Some(match (pkg, intel) {
        (PkgManager::Pacman, false) => "vulkan-radeon",
        (PkgManager::Pacman, true) => "vulkan-intel",
        (PkgManager::Apt | PkgManager::Dnf, _) => "mesa-vulkan-drivers",
        (PkgManager::Zypper, false) => "libvulkan_radeon",
        (PkgManager::Zypper, true) => "libvulkan_intel",
        (PkgManager::Apk, false) => "mesa-vulkan-ati",
        (PkgManager::Apk, true) => "mesa-vulkan-intel",
        (PkgManager::Brew, _) => return None,
    })
}

/// The command that routes `*.ts.net` lookups to MagicDNS while the VPN
/// keeps every other domain (systemd-resolved). Shown, never run.
pub const DNS_FIX: &str = "sudo resolvectl domain tailscale0 '~ts.net'";

/// What to say, and offer, when a VPN owns the system DNS and the tailnet
/// name doesn't resolve through it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DnsHint {
    /// the opening of the summary, ending in a space
    pub why: String,
    /// commands to copy
    pub commands: Vec<String>,
    /// one more sentence for the facts, if any
    pub advice: Option<String>,
}

/// `vpn`: the VPN up besides Tailscale (None: nothing detected; `label`
/// names an unknown one).
pub fn dns_hint(vpn: Option<Vpn>, label: Option<&str>, os: Os) -> DnsHint {
    let name = vpn.map(Vpn::label).or(label);
    let Some(name) = name else {
        return DnsHint {
            why: "The system DNS doesn't give a tailnet address. ".into(),
            commands: if os == Os::Linux { vec![DNS_FIX.into()] } else { Vec::new() },
            advice: None,
        };
    };
    if os != Os::Linux {
        return DnsHint {
            why: format!("{name} owns the system DNS, which doesn't know the tailnet. "),
            commands: Vec::new(),
            advice: Some(format!("Exclude Tailscale (or *.ts.net) in {name}'s split-tunnel or DNS settings, \
                or turn {name} off while you use the tailnet.")),
        };
    }
    let (why, advice) = match vpn {
        Some(Vpn::NordVpn) => (
            "NordVPN owns the system DNS and answers with tailscale's public addresses. ".to_string(),
            None,
        ),
        Some(Vpn::Twingate) => (
            "Twingate resolves every name through its own DNS (sdwan0), which doesn't know the tailnet. ".to_string(),
            Some("Twingate puts its DNS back when it reconnects: run the command again then.".to_string()),
        ),
        Some(Vpn::Mullvad) => (
            "Mullvad forces its own DNS server, which doesn't know the tailnet. ".to_string(),
            Some("Mullvad also firewalls traffic outside its tunnel: if MagicDNS stops answering, \
                allow Tailscale through it (split tunneling) in the Mullvad app.".to_string()),
        ),
        Some(Vpn::ProtonVpn) => (
            "ProtonVPN sends every lookup to its own DNS, which doesn't know the tailnet. ".to_string(),
            Some("ProtonVPN puts its DNS back when it reconnects: run the command again then.".to_string()),
        ),
        Some(Vpn::Warp) => (
            "Cloudflare WARP answers every lookup through its own resolver, which doesn't know the tailnet. ".to_string(),
            Some("Under Cloudflare Zero Trust, adding ts.net to Local Domain Fallback (server 100.100.100.100) \
                fixes it for good.".to_string()),
        ),
        _ => (
            format!("{name} is up and the system DNS doesn't give a tailnet address. "),
            Some(format!("If {name}'s config sets a DNS server, it takes every domain; the command gives *.ts.net \
                back to MagicDNS.")),
        ),
    };
    DnsHint { why, commands: vec![DNS_FIX.into()], advice }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct Fake {
        path: HashSet<&'static str>,
        ifaces: Vec<&'static str>,
        files: HashMap<PathBuf, String>,
        env: HashMap<&'static str, &'static str>,
    }

    impl Probe for Fake {
        fn on_path(&self, exe: &str) -> bool {
            self.path.contains(exe)
        }
        fn interfaces(&self) -> Vec<String> {
            self.ifaces.iter().map(|s| s.to_string()).collect()
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }
        fn read(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).map(|s| s.to_string())
        }
    }

    #[test]
    fn detects_vpns_by_tool_and_interface() {
        let f = Fake {
            path: ["tailscale", "nordvpn", "twingate", "wg", "pacman"].into(),
            ifaces: vec!["lo", "wlan0", "tailscale0", "nordlynx", "wg-mullvad", "tun0"],
            ..Default::default()
        };
        let env = detect_with(&f, Os::Linux);
        let ts = env.vpn(Vpn::Tailscale).unwrap();
        assert!(ts.installed && ts.active);
        assert_eq!(ts.signals, vec!["interface tailscale0", "tailscale on PATH"]);
        let nord = env.vpn(Vpn::NordVpn).unwrap();
        assert!(nord.active && nord.describe().contains("nordlynx"));
        let tg = env.vpn(Vpn::Twingate).unwrap();
        assert!(tg.installed && !tg.active);
        assert_eq!(tg.describe(), "Twingate (installed, not up)");
        // wg-mullvad is Mullvad's, not plain WireGuard's; wg on PATH still counts
        assert!(env.vpn(Vpn::Mullvad).unwrap().active);
        let wg = env.vpn(Vpn::WireGuard).unwrap();
        assert!(wg.installed && !wg.active);
        assert!(env.vpn(Vpn::OpenVpn).unwrap().active);
        assert!(env.vpn(Vpn::ZeroTier).is_none());
        // the DNS owner: the first VPN up that isn't Tailscale
        assert_eq!(env.dns_vpn().map(|f| f.vpn), Some(Vpn::NordVpn));
        assert!(!env.hyprland);
    }

    #[test]
    fn nothing_found() {
        let env = detect_with(&Fake { ifaces: vec!["lo", "eth0"], ..Default::default() }, Os::Linux);
        assert_eq!(env, Env::bare(Os::Linux));
        assert!(env.dns_vpn().is_none() && !env.has(Vpn::Tailscale));
    }

    #[test]
    fn package_manager_and_os_release() {
        let release = |text: &str| HashMap::from([(PathBuf::from("/etc/os-release"), text.to_string())]);
        // Debian with brew installed too: the distro's own wins
        let f = Fake { path: ["apt", "brew"].into(), files: release("NAME=\"Debian\"\nID=debian\n"), ..Default::default() };
        let env = detect_with(&f, Os::Linux);
        assert_eq!((env.os_id.as_deref(), env.pkg), (Some("debian"), Some(PkgManager::Apt)));
        // a derivative found through ID_LIKE
        let f = Fake { path: ["pacman"].into(), files: release("ID=\"somearch\"\nID_LIKE=\"arch\"\n"), ..Default::default() };
        assert_eq!(detect_with(&f, Os::Linux).pkg, Some(PkgManager::Pacman));
        // no os-release: the first tool on PATH
        let f = Fake { path: ["zypper", "brew"].into(), ..Default::default() };
        assert_eq!(detect_with(&f, Os::Linux).pkg, Some(PkgManager::Zypper));
        assert_eq!(PkgManager::for_distro("opensuse-tumbleweed"), Some(PkgManager::Zypper));
        assert_eq!(PkgManager::Pacman.install("x"), "sudo pacman -S x");
        assert_eq!(PkgManager::Apt.install("x"), "sudo apt install x");
    }

    #[test]
    fn macos_bundles_and_brew() {
        let f = Fake {
            path: ["brew"].into(),
            ifaces: vec!["tailscale0"], // never read off Linux
            files: HashMap::from([(PathBuf::from("/Applications/Twingate.app"), String::new())]),
            ..Default::default()
        };
        let env = detect_with(&f, Os::MacOs);
        assert_eq!(env.pkg, Some(PkgManager::Brew));
        assert!(env.has(Vpn::Twingate) && !env.has(Vpn::Tailscale));
        assert_eq!(env.vpn(Vpn::Twingate).unwrap().signals, vec!["/Applications/Twingate.app"]);
    }

    #[test]
    fn hyprland_from_env() {
        let f = Fake { env: HashMap::from([("HYPRLAND_INSTANCE_SIGNATURE", "abc")]), ..Default::default() };
        assert!(detect_with(&f, Os::Linux).hyprland);
    }

    #[test]
    fn mesa_packages() {
        assert_eq!(mesa_vulkan_package(PkgManager::Pacman, false), Some("vulkan-radeon"));
        assert_eq!(mesa_vulkan_package(PkgManager::Pacman, true), Some("vulkan-intel"));
        assert_eq!(mesa_vulkan_package(PkgManager::Apt, true), Some("mesa-vulkan-drivers"));
        assert_eq!(mesa_vulkan_package(PkgManager::Brew, true), None);
    }

    #[test]
    fn dns_hints() {
        let nord = dns_hint(Some(Vpn::NordVpn), None, Os::Linux);
        assert_eq!(nord.commands, vec![DNS_FIX.to_string()]);
        assert!(nord.why.starts_with("NordVPN owns the system DNS") && nord.advice.is_none());
        let tg = dns_hint(Some(Vpn::Twingate), None, Os::Linux);
        assert!(tg.why.starts_with("Twingate") && tg.advice.is_some());
        assert!(dns_hint(Some(Vpn::Warp), None, Os::Linux).advice.unwrap().contains("Local Domain Fallback"));
        assert!(dns_hint(None, Some("SomeVPN"), Os::Linux).why.starts_with("SomeVPN is up"));
        assert!(dns_hint(None, None, Os::Linux).why.starts_with("The system DNS"));
        // no resolvectl on macOS
        assert!(dns_hint(Some(Vpn::NordVpn), None, Os::MacOs).commands.is_empty());
        assert_eq!(Vpn::from_label("nordvpn"), Some(Vpn::NordVpn));
    }
}
