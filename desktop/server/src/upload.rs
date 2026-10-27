//! POST /upload: multipart/form-data -> sanitized files in transactions/
//! (the validation and naming rules below).
//!
//! Only .csv / .xml / .pdf / .xlsx / .xls, nothing empty, 20MB per request. Same name +
//! same content is skipped; same name + new content gets a numbered suffix,
//! so an existing export is never silently overwritten.

use std::path::Path;

use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};

/// A bank CSV is ~100KB; 20MB is generous.
pub const MAX_BYTES: usize = finance_core::upload::MAX_BYTES;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Outcome {
    pub saved: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

impl Outcome {
    /// The redirect query: saved / skipped / err.
    pub fn query(&self) -> String {
        let mut p: Vec<(&str, String)> = Vec::new();
        if !self.saved.is_empty() {
            p.push(("saved", self.saved.join(", ")));
        }
        if !self.skipped.is_empty() {
            p.push(("skipped", self.skipped.join(", ")));
        }
        if !self.errors.is_empty() {
            p.push(("err", self.errors.join("; ")));
        }
        if p.is_empty() {
            p.push(("err", "no files received".into()));
        }
        finance_engine::fetch::http::urlencode(&p)
    }
}

pub fn err_query(msg: &str) -> String {
    finance_engine::fetch::http::urlencode(&[("err", msg.to_string())])
}

/// One file part of a multipart body.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub filename: String,
    pub data: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// `boundary=` from a Content-Type header (quoted or bare).
pub fn boundary(content_type: &str) -> Option<String> {
    content_type.split(';').map(str::trim).find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        k.trim().eq_ignore_ascii_case("boundary").then(|| v.trim().trim_matches('"').to_string())
    })
}

/// filename="..." from a Content-Disposition value (`filename*=` UTF-8
/// form too). Parts without one aren't files and are ignored.
fn filename(disposition: &str) -> Option<String> {
    let mut plain = None;
    for kv in disposition.split(';').map(str::trim) {
        let Some((k, v)) = kv.split_once('=') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "filename*" => {
                let enc = v.rsplit("''").next().unwrap_or(v);
                return Some(percent_decode(enc));
            }
            "filename" => plain = Some(v.trim_matches('"').replace("\\\"", "\"")),
            _ => {}
        }
    }
    plain.filter(|n| !n.is_empty())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split a multipart body into its file parts.
pub fn parse_multipart(content_type: &str, body: &[u8]) -> Result<Vec<Part>> {
    let b = boundary(content_type).context("multipart without a boundary")?;
    let delim = format!("--{b}").into_bytes();
    let mut parts = Vec::new();
    let Some(mut pos) = find(body, &delim, 0) else { bail!("multipart boundary not found") };
    loop {
        pos += delim.len();
        if body[pos..].starts_with(b"--") {
            break; // closing delimiter
        }
        let head_end = find(body, b"\r\n\r\n", pos).context("truncated multipart headers")?;
        let headers = String::from_utf8_lossy(&body[pos..head_end]).into_owned();
        let data_start = head_end + 4;
        let next = find(body, &[b"\r\n".as_slice(), &delim].concat(), data_start).context("unterminated multipart part")?;
        let name = headers
            .lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-disposition")).map(|(_, v)| v.to_string()))
            .and_then(|d| filename(&d));
        if let Some(filename) = name {
            parts.push(Part { filename, data: body[data_start..next].to_vec() });
        }
        pos = next + 2;
    }
    Ok(parts)
}

/// One part of a multipart form: its field name, its file name when it is
/// a file, and the bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct FormPart {
    pub name: String,
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

impl FormPart {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// Every part of a multipart form, fields and files alike.
pub fn parse_form(content_type: &str, body: &[u8]) -> Result<Vec<FormPart>> {
    let b = boundary(content_type).context("multipart without a boundary")?;
    let delim = format!("--{b}").into_bytes();
    let mut parts = Vec::new();
    let Some(mut pos) = find(body, &delim, 0) else { bail!("multipart boundary not found") };
    loop {
        pos += delim.len();
        if body[pos..].starts_with(b"--") {
            break;
        }
        let head_end = find(body, b"\r\n\r\n", pos).context("truncated multipart headers")?;
        let headers = String::from_utf8_lossy(&body[pos..head_end]).into_owned();
        let data_start = head_end + 4;
        let next = find(body, &[b"\r\n".as_slice(), &delim].concat(), data_start).context("unterminated multipart part")?;
        let disp = headers
            .lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-disposition")).map(|(_, v)| v.to_string()))
            .unwrap_or_default();
        let name = disp
            .split(';')
            .map(str::trim)
            .find_map(|kv| kv.strip_prefix("name=").map(|v| v.trim_matches('"').to_string()))
            .unwrap_or_default();
        parts.push(FormPart { name, filename: filename(&disp), data: body[data_start..next].to_vec() });
        pos = next + 2;
    }
    Ok(parts)
}

/// The server's file-name rule, shared with the upload client.
pub use finance_core::upload::sanitize;

fn split_ext(name: &str) -> (&str, &str) {
    // Path.stem / Path.suffix: the last dot, unless it leads the name
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Write the parts into `dir`, file by file.
pub fn save(dir: &Path, parts: &[Part]) -> Result<Outcome> {
    std::fs::create_dir_all(dir)?;
    let mut out = Outcome::default();
    for p in parts {
        let name = sanitize(&p.filename);
        let lower = name.to_lowercase();
        if !finance_core::upload::EXTS.iter().any(|e| lower.ends_with(e)) {
            out.errors.push(format!("{name}: only .csv / .xml / .pdf / .xlsx / .xls accepted"));
            continue;
        }
        if p.data.iter().all(u8::is_ascii_whitespace) {
            out.errors.push(format!("{name}: empty file"));
            continue;
        }
        let digest = Md5::digest(&p.data);
        let (stem, ext) = split_ext(&name);
        let mut target = dir.join(&name);
        let mut n = 2;
        let mut duplicate = false;
        while target.exists() {
            if Md5::digest(std::fs::read(&target)?) == digest {
                duplicate = true;
                break;
            }
            target = dir.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        if duplicate {
            out.skipped.push(name);
            continue;
        }
        std::fs::write(&target, &p.data)?;
        out.saved.push(target.file_name().unwrap().to_string_lossy().into_owned());
    }
    Ok(out)
}

/// The client side (push, the desktop app) builds bodies with this.
pub use finance_core::upload::encode_multipart;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("finance-upload-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn roundtrip_and_rules() {
        let files = vec![
            ("n26 export.csv".to_string(), b"a,b\n1,2\n".to_vec()),
            ("../../etc/passwd".to_string(), b"root".to_vec()),
            ("blank.csv".to_string(), b"  \n".to_vec()),
            ("Statement \u{e9}t\u{e9}.PDF".to_string(), b"%PDF-1.4".to_vec()),
        ];
        let body = encode_multipart("XyZ", &files);
        let parts = parse_multipart("multipart/form-data; boundary=XyZ", &body).unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].data, b"a,b\n1,2\n");

        let dir = tmp("rules");
        let out = save(&dir, &parts).unwrap();
        assert_eq!(out.saved, vec!["n26 export.csv", "Statement _t_.PDF"]);
        assert_eq!(out.errors, vec!["passwd: only .csv / .xml / .pdf / .xlsx / .xls accepted", "blank.csv: empty file"]);
        // spreadsheets go through the same rules
        let xl = save(&dir, &[Part { filename: "Activity.XLSX".into(), data: b"PK\x03\x04".to_vec() }, Part { filename: "e.xls".into(), data: b" ".to_vec() }]).unwrap();
        assert_eq!((xl.saved, xl.errors), (vec!["Activity.XLSX".to_string()], vec!["e.xls: empty file".to_string()]));

        // same content again -> skipped; new content, same name -> suffixed
        let again = save(&dir, &parts[..1]).unwrap();
        assert_eq!(again.skipped, vec!["n26 export.csv"]);
        let changed = save(&dir, &[Part { filename: "n26 export.csv".into(), data: b"x".to_vec() }]).unwrap();
        assert_eq!(changed.saved, vec!["n26 export (2).csv"]);
        assert_eq!(changed.query(), "saved=n26+export+%282%29.csv");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn browser_style_body() {
        let body = b"------WebKitFormBoundaryAbc\r\nContent-Disposition: form-data; name=\"files\"; filename=\"a.csv\"\r\nContent-Type: text/csv\r\n\r\nx,y\r\n1,2\r\n------WebKitFormBoundaryAbc\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhi\r\n------WebKitFormBoundaryAbc--\r\n";
        let parts = parse_multipart("multipart/form-data; boundary=----WebKitFormBoundaryAbc", body).unwrap();
        assert_eq!(parts, vec![Part { filename: "a.csv".into(), data: b"x,y\r\n1,2".to_vec() }]);
        assert_eq!(Outcome::default().query(), "err=no+files+received");
    }

    #[test]
    fn sanitize_keeps_the_base_name() {
        assert_eq!(sanitize("C:\\x\\y.csv"), "C__x_y.csv");
        assert_eq!(sanitize(" .hidden.csv. "), "hidden.csv");
        assert_eq!(sanitize("a/b/c (1).csv"), "c (1).csv");
    }
}
