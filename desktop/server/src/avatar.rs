//! The profile picture: files in the user's data dir (`data/avatar/`),
//! never in finance.db, so it stays out of the replica and the backups'
//! db copies.
//!
//! * upload: a PNG, JPEG or WebP of at most [`MAX_UPLOAD`] bytes, decoded
//!   under size limits, cropped to the centre square and resized to
//!   [`SIDE`]×[`SIDE`], stored as `upload.png`. Anything else is refused.
//! * gravatar: the server fetches
//!   `https://gravatar.com/avatar/<sha256 of the trimmed, lowercased email>?s=256&d=404`
//!   (clients never contact Gravatar), stores it as `gravatar.png` with
//!   `gravatar.json` (whose address, when), and fetches it again in the
//!   background once it is a day old, or on request. A 404 is "no Gravatar
//!   for this email".
//! * initials: nothing stored here; the clients draw them.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use image::{imageops::FilterType, ImageFormat, ImageReader, Limits};
use serde_json::{json, Value};

use finance_engine::{config, db, secrets};

pub const MAX_UPLOAD: usize = 5 << 20;
pub const SIDE: u32 = 256;
/// a Gravatar reply larger than this is not a picture we want
const MAX_FETCH: u64 = 2 << 20;
const REFRESH_MS: i64 = 24 * 3600 * 1000;

pub fn dir() -> PathBuf {
    finance_engine::tenant::active().avatar.clone()
}

fn upload_path() -> PathBuf {
    dir().join("upload.png")
}

fn gravatar_path() -> PathBuf {
    dir().join("gravatar.png")
}

fn gravatar_meta_path() -> PathBuf {
    dir().join("gravatar.json")
}

/// Why a picture was refused (the message goes to the client).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureError {
    TooLarge,
    Unsupported,
    Unreadable,
}

impl std::fmt::Display for PictureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PictureError::TooLarge => "The picture is larger than 5 MB.",
            PictureError::Unsupported => "Use a PNG, JPEG or WebP picture.",
            PictureError::Unreadable => "That file isn't a picture this server can read.",
        })
    }
}

impl std::error::Error for PictureError {}

/// The format from the first bytes; only the three we take.
pub fn sniff(b: &[u8]) -> Option<ImageFormat> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(ImageFormat::WebP)
    } else {
        None
    }
}

/// Any accepted picture -> the centre square at SIDE×SIDE, as PNG. The
/// decoder runs under limits (8192 px a side, 256 MB), so a small file
/// that unpacks huge is refused rather than run.
pub fn to_square_png(bytes: &[u8]) -> Result<Vec<u8>, PictureError> {
    if bytes.len() > MAX_UPLOAD {
        return Err(PictureError::TooLarge);
    }
    let fmt = sniff(bytes).ok_or(PictureError::Unsupported)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), fmt);
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let img = reader.decode().map_err(|_| PictureError::Unreadable)?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(PictureError::Unreadable);
    }
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let out = square.resize_exact(SIDE, SIDE, FilterType::Lanczos3).to_rgba8();
    let mut png = Vec::new();
    out.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).map_err(|_| PictureError::Unreadable)?;
    Ok(png)
}

/// Gravatar's hash: SHA-256 of the trimmed, lowercased address.
pub fn gravatar_hash(email: &str) -> String {
    crate::auth::sha256_hex(&email.trim().to_lowercase())
}

pub fn gravatar_url(email: &str) -> String {
    format!("https://gravatar.com/avatar/{}?s={SIDE}&d=404", gravatar_hash(email))
}

/// GET a URL: (status, body). Tests swap in a stand-in.
pub type Fetcher = fn(&str) -> Result<(u16, Vec<u8>)>;
static FETCHER: RwLock<Fetcher> = RwLock::new(http_get);

pub fn set_fetcher(f: Fetcher) {
    if let Ok(mut g) = FETCHER.write() {
        *g = f;
    }
}

fn http_get(url: &str) -> Result<(u16, Vec<u8>)> {
    if config::offline() {
        bail!("the server is offline (FINANCE_OFFLINE): it can't reach Gravatar");
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut r = agent.get(url).call().with_context(|| "reaching gravatar.com".to_string())?;
    let status = r.status().as_u16();
    let body = r.body_mut().with_config().limit(MAX_FETCH).read_to_vec().context("reading the Gravatar reply")?;
    Ok((status, body))
}

pub enum Gravatar {
    Found,
    /// Gravatar has no picture for this address (its 404)
    Missing,
}

/// Fetch the address's Gravatar now and keep it. Missing leaves any
/// earlier picture alone.
pub fn fetch_gravatar(email: &str) -> Result<Gravatar> {
    let f = FETCHER.read().map(|g| *g).unwrap_or(http_get);
    let (status, body) = f(&gravatar_url(email))?;
    match status {
        404 => Ok(Gravatar::Missing),
        200 => {
            let png = to_square_png(&body).map_err(|e| anyhow::anyhow!("Gravatar sent something odd: {e}"))?;
            secrets::write_private(&gravatar_path(), &png)?;
            let meta = json!({"hash": gravatar_hash(email), "fetched": db::now_ms()});
            secrets::write_private(&gravatar_meta_path(), meta.to_string().as_bytes())?;
            Ok(Gravatar::Found)
        }
        s => bail!("Gravatar answered HTTP {s}"),
    }
}

fn gravatar_meta() -> Value {
    std::fs::read(gravatar_meta_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

/// A day-old Gravatar is fetched again, off the request: one at a time
/// per user (their avatar dir), on a thread that carries their tenant.
fn refresh_in_background(email: String) {
    static RUNNING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    if config::offline() {
        return;
    }
    let d = dir();
    {
        let Ok(mut r) = RUNNING.lock() else { return };
        if r.contains(&d) {
            return;
        }
        r.push(d.clone());
    }
    let done = {
        let d = d.clone();
        move || {
            if let Ok(mut r) = RUNNING.lock() {
                r.retain(|x| *x != d);
            }
        }
    };
    let done2 = done.clone();
    let spawned = finance_engine::tenant::spawn("gravatar", move || {
        if let Err(e) = fetch_gravatar(&email) {
            crate::log::log(&format!("[avatar] Gravatar refresh failed: {e:#}"));
        }
        done();
    });
    if spawned.is_err() {
        done2();
    }
}

/// The picture to serve for this profile: the upload, or the cached
/// Gravatar of its address (a day old: served, and fetched again behind
/// it; `refresh`: fetched first). None for initials or nothing yet.
pub fn picture(kind: Option<&str>, email: Option<&str>, refresh: bool) -> Option<Vec<u8>> {
    match kind {
        Some("upload") => std::fs::read(upload_path()).ok(),
        Some("gravatar") => {
            let email = email?;
            if refresh {
                let _ = fetch_gravatar(email);
            }
            let meta = gravatar_meta();
            if meta["hash"].as_str() != Some(gravatar_hash(email).as_str()) {
                return None; // cached for another address
            }
            if db::now_ms() - meta["fetched"].as_i64().unwrap_or(0) > REFRESH_MS {
                refresh_in_background(email.to_string());
            }
            std::fs::read(gravatar_path()).ok()
        }
        _ => None,
    }
}

pub fn save_upload(png: &[u8]) -> Result<()> {
    secrets::write_private(&upload_path(), png)
}

pub fn remove_upload() {
    let _ = std::fs::remove_file(upload_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture_of(w: u32, h: u32, fmt: ImageFormat) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, _| if x < w / 2 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img).to_rgb8().write_to(&mut Cursor::new(&mut out), fmt).unwrap();
        out
    }

    #[test]
    fn uploads_are_checked_cropped_and_resized() {
        for fmt in [ImageFormat::Png, ImageFormat::Jpeg] {
            let src = picture_of(640, 400, fmt);
            assert_eq!(sniff(&src), Some(fmt));
            let png = to_square_png(&src).unwrap();
            assert_eq!(sniff(&png), Some(ImageFormat::Png));
            let back = image::load_from_memory(&png).unwrap();
            assert_eq!((back.width(), back.height()), (SIDE, SIDE), "{fmt:?}");
        }
        // a small picture is scaled up to the same square
        let tiny = to_square_png(&picture_of(10, 30, ImageFormat::Png)).unwrap();
        assert_eq!(image::load_from_memory(&tiny).unwrap().width(), SIDE);
        // not a picture we take
        assert_eq!(to_square_png(b"GIF89a....").unwrap_err(), PictureError::Unsupported);
        assert_eq!(to_square_png(b"<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap_err(), PictureError::Unsupported);
        // the right magic, then garbage
        assert_eq!(to_square_png(b"\x89PNG\r\n\x1a\nnot really").unwrap_err(), PictureError::Unreadable);
        // over the cap, whatever it is
        let mut big = picture_of(8, 8, ImageFormat::Png);
        big.resize(MAX_UPLOAD + 1, 0);
        assert_eq!(to_square_png(&big).unwrap_err(), PictureError::TooLarge);
    }

    #[test]
    fn gravatar_hash_is_sha256_of_the_normalized_address() {
        // Gravatar's documented example: sha256("myemailaddress@example.com")
        assert_eq!(gravatar_hash("  MyEmailAddress@example.com "), "84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee");
        assert_eq!(
            gravatar_url("MyEmailAddress@example.com"),
            "https://gravatar.com/avatar/84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee?s=256&d=404"
        );
    }
}
