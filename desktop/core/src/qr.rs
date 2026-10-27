//! A QR code as a square of modules, for the Doctor's phone-access check.
//! The encoding is Nayuki's `qrcodegen` (MIT, no dependencies); the app
//! paints the modules itself, so no image is ever written.

use qrcodegen::{QrCode, QrCodeEcc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qr {
    /// modules per side (21 for version 1, +4 per version)
    pub size: usize,
    /// row-major, true = dark
    dark: Vec<bool>,
}

impl Qr {
    /// Medium error correction: a phone reads it off a screen easily.
    pub fn encode(text: &str) -> Option<Qr> {
        let code = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
        let n = code.size();
        let dark = (0..n).flat_map(|y| (0..n).map(move |x| (x, y))).map(|(x, y)| code.get_module(x, y)).collect();
        Some(Qr { size: n as usize, dark })
    }

    pub fn dark(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.dark[y * self.size + x]
    }
}

#[cfg(test)]
mod tests {
    use super::Qr;

    #[test]
    fn a_tailnet_url_fits_a_small_code() {
        let q = Qr::encode("https://server.example.ts.net:8787/app").unwrap();
        assert!(q.size >= 21 && q.size <= 41, "size {}", q.size);
        assert_eq!((q.size - 17) % 4, 0, "sizes are 17 + 4 × version");
        // the three finder patterns: dark corners, a light ring inside
        for (x, y) in [(0, 0), (q.size - 1, 0), (0, q.size - 1)] {
            assert!(q.dark(x, y));
        }
        assert!(!q.dark(1, 1) && q.dark(3, 3));
        assert!(!q.dark(q.size, 0), "out of range is light");
    }
}
