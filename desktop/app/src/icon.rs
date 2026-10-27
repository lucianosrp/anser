//! The app icon: a rounded tile in a gradient drawn from the accent (the
//! dashboard favicon's shape) with a rising line -- so a matugen palette or a
//! custom accent recolors the launcher icon too.
//!
//! `scripts/install.sh` writes it once (`anser --icon PATH`); after
//! that, every theme apply rewrites the installed copy when its colors change.

use std::path::PathBuf;

use gpui_kit::{Hsla, hsla};

/// Where install.sh puts the icon (the freedesktop user icon theme).
pub fn installed_path() -> PathBuf {
    dirs::data_dir().unwrap_or_default().join("icons/hicolor/scalable/apps/anser.svg")
}

fn css(c: Hsla) -> String {
    let rgb = c.to_rgb();
    let ch = |x: f32| (x.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", ch(rgb.r), ch(rgb.g), ch(rgb.b))
}

fn shift(c: Hsla, dh: f32, ds: f32, dl: f32) -> Hsla {
    hsla((c.h + dh).rem_euclid(1.), (c.s + ds).clamp(0., 1.), (c.l + dl).clamp(0., 1.), 1.)
}

pub fn svg(accent: Hsla) -> String {
    let a = hsla(accent.h, accent.s.max(0.35), accent.l.clamp(0.38, 0.62), 1.);
    let (top, mid, deep) = (shift(a, -0.03, 0.05, 0.16), a, shift(a, 0.07, 0.0, -0.2));
    format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{top}"/>
      <stop offset=".55" stop-color="{mid}"/>
      <stop offset="1" stop-color="{deep}"/>
    </linearGradient>
    <radialGradient id="h" cx=".3" cy=".22" r=".85">
      <stop offset="0" stop-color="#ffffff" stop-opacity=".38"/>
      <stop offset=".55" stop-color="#ffffff" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect x="4" y="4" width="56" height="56" rx="16" fill="url(#g)"/>
  <rect x="4" y="4" width="56" height="56" rx="16" fill="url(#h)"/>
  <polyline points="15,43 25,35 33,39 49,21" fill="none" stroke="#ffffff" stroke-opacity=".95"
            stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="49" cy="21" r="4.2" fill="#ffffff"/>
</svg>
"##, top = css(top), mid = css(mid), deep = css(deep))
}

/// Write the icon to `path` (creating its directory).
pub fn write(path: &std::path::Path, accent: Hsla) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, svg(accent))
}

/// Recolor the installed icon when the accent changed. Only touches an icon
/// that install.sh put there; touching the theme dir nudges launchers to reload.
pub fn sync_installed(accent: Hsla) {
    let path = installed_path();
    if !path.exists() {
        return;
    }
    let want = svg(accent);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(want.as_str()) {
        return;
    }
    if std::fs::write(&path, want).is_ok() {
        if let Some(theme_dir) = path.ancestors().nth(3) {
            let _ = std::process::Command::new("touch").arg(theme_dir).status();
        }
    }
}
