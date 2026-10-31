//! The app icon: a goose quill writing a rising line, on a dark tile, drawn
//! from the accent -- so a matugen palette or a custom accent recolors the
//! launcher icon too. (Anser is Latin for goose; ledgers were kept with
//! goose quills.)
//!
//! The feather is engraved: every barb is a tapered ribbon along a curve off
//! the shaft. The trailing vane is wide and the leading one narrow, like a
//! real flight feather; downy barbs fan out at the base and a few splits open
//! from the edge. Everything is computed here, so the colors can follow the
//! theme; `web/icon.svg` and `web/icons/` are this drawing in the warm accent
//! (regenerate them with the ignored `regen_web_icons` test).
//!
//! `scripts/install.sh` writes it once (`anser --icon PATH`); after
//! that, every theme apply rewrites the installed copy when its colors change.

use std::fmt::Write as _;
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

/// The icon's three colors from one accent: the accent itself (kept vivid
/// and mid-light), a brighter feather, and a near-black ground of its hue.
fn tones(accent: Hsla) -> (Hsla, Hsla, Hsla) {
    let (h, s, l) = (accent.h, accent.s.max(0.35), accent.l.clamp(0.42, 0.66));
    let a = hsla(h, s, l, 1.);
    let bright = hsla((h - 0.02).rem_euclid(1.), s, (l + 0.12).min(1.), 1.);
    let night = hsla((h + 0.02).rem_euclid(1.), s.min(0.35), 0.09, 1.);
    (a, bright, night)
}

type Pt = (f32, f32);

fn bez(p: [Pt; 4], t: f32) -> Pt {
    let u = 1. - t;
    let (a, b, c, d) = (u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t);
    (a * p[0].0 + b * p[1].0 + c * p[2].0 + d * p[3].0, a * p[0].1 + b * p[1].1 + c * p[2].1 + d * p[3].1)
}

/// A polyline as a filled outline whose width varies point by point.
fn ribbon(d: &mut String, pts: &[Pt], ws: &[f32]) {
    let n = pts.len();
    let offset = |i: usize, sign: f32| {
        let (x, y) = pts[i];
        let (x0, y0) = pts[i.saturating_sub(1)];
        let (x1, y1) = pts[(i + 1).min(n - 1)];
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = dx.hypot(dy).max(1e-6);
        let w = ws[i] / 2. * sign;
        (x - dy / len * w, y + dx / len * w)
    };
    let mut first = true;
    for (i, sign) in (0..n).map(|i| (i, 1.)).chain((0..n).rev().map(|i| (i, -1.))) {
        let (x, y) = offset(i, sign);
        let _ = write!(d, "{}{x:.1} {y:.1}", if first { "M" } else { "L" });
        first = false;
    }
    d.push('Z');
}

/// The shaft, from the nib to the tip.
const SHAFT: [Pt; 4] = [(232., 392.), (282., 318.), (350., 186.), (426., 62.)];
/// Where the vane starts; below it is the bare barrel.
const VANE0: f32 = 0.24;
/// (where, half-width) of the splits in the trailing vane.
const SPLITS: [(f32, f32); 3] = [(0.50, 0.010), (0.69, 0.008), (0.83, 0.006)];

fn at(t: f32) -> Pt {
    bez(SHAFT, t)
}

fn tangent(t: f32) -> Pt {
    let (a, b) = (at((t - 0.004).max(0.)), at((t + 0.004).min(1.)));
    let len = (b.0 - a.0).hypot(b.1 - a.1).max(1e-6);
    ((b.0 - a.0) / len, (b.1 - a.1) / len)
}

/// The barbs: (vane, down) outlines.
fn barbs() -> (String, String) {
    let (mut vane, mut down) = (String::new(), String::new());
    const M: usize = 11;
    for (side, wmax, lean) in [(1f32, 104f32, 0.62f32), (-1., 44., 0.5)] {
        let n = 170;
        for i in 0..=n {
            let t = VANE0 + (1. - VANE0) * i as f32 / n as f32;
            let u = (t - VANE0) / (1. - VANE0);
            let mut w = wmax * (u * 5.).min(1.).sqrt() * (1. - u.powf(3.2)).max(0.).powf(0.55);
            if w < 2.5 {
                continue;
            }
            let (x0, y0) = at(t);
            let (tx, ty) = tangent(t);
            let (nx, ny) = (-ty * side, tx * side);
            let downy = u < 0.12;
            let lean = if downy {
                w *= 1.05;
                lean * 0.35
            } else {
                lean
            };
            // barbs bow toward the tip at their outer end
            let (mut ex, mut ey) = (x0 + nx * w + tx * w * lean, y0 + ny * w + ty * w * lean);
            let c1 = (x0 + nx * w * 0.45, y0 + ny * w * 0.45);
            let c2 = (x0 + nx * w * 0.85 + tx * w * lean * 0.45, y0 + ny * w * 0.85 + ty * w * lean * 0.45);
            if downy {
                let curl = (i as f32 * 1.7).sin() * 9.;
                ex += tx * curl;
                ey += ty * curl;
            }
            // at a split only the inner part of the barb is there
            let keep = if side > 0. && SPLITS.iter().any(|&(sp, half)| (t - sp).abs() < half) { 0.42 } else { 1. };
            let pts: Vec<Pt> = (0..M).map(|k| bez([(x0, y0), c1, c2, (ex, ey)], keep * k as f32 / (M - 1) as f32)).collect();
            let shade = 0.45 + 0.35 * (1. - u) + if side < 0. { 0.2 } else { 0. };
            if downy {
                let ws: Vec<f32> = (0..M).map(|k| 0.55 * (1. - k as f32 / M as f32) + 0.15).collect();
                ribbon(&mut down, &pts, &ws);
            } else {
                let ws: Vec<f32> = (0..M).map(|k| 0.35 + 1.45 * shade * (1. - k as f32 / (M + 1) as f32)).collect();
                ribbon(&mut vane, &pts, &ws);
            }
        }
    }
    (vane, down)
}

/// The icon in `accent`. `bleed`: fill the whole square and keep the mark in
/// the maskable safe zone (the web app's maskable icon).
fn render(accent: Hsla, bleed: bool) -> String {
    let (a, feather, ground) = tones(accent);
    let (a, feather, ground) = (css(a), css(feather), css(ground));
    let (vane, down) = barbs();
    let mut rachis = String::new();
    let rp: Vec<Pt> = (0..=80).map(|k| at(k as f32 / 80.)).collect();
    let rw: Vec<f32> = (0..=80).map(|k| 8.5 * (1. - k as f32 / 80.).powf(0.75) + 0.7).collect();
    ribbon(&mut rachis, &rp, &rw);
    let mut barrel = String::new();
    let bp: Vec<Pt> = (0..=40).map(|k| at(k as f32 / 40. * VANE0)).collect();
    ribbon(&mut barrel, &bp, &[9.5; 41]);
    let mut shine = String::new();
    let sp: Vec<Pt> = (0..=40).map(|k| at(0.03 + k as f32 / 40. * (VANE0 - 0.06))).collect();
    ribbon(&mut shine, &sp, &[1.6; 41]);
    // the cut nib below the barrel, its slit, and the ink on it
    let (tx, ty) = tangent(0.);
    let (nx, ny) = (-ty, tx);
    let (bx, by) = SHAFT[0];
    let (px, py) = (bx - tx * 26., by - ty * 26.);
    let nib = format!("M{:.1} {:.1}L{px:.1} {py:.1}L{:.1} {:.1}Z", bx + nx * 4.8, by + ny * 4.8, bx - nx * 4.8, by - ny * 4.8);
    let (sx, sy) = (bx - tx * 12., by - ty * 12.);
    let stain = format!("M{:.1} {:.1}L{px:.1} {py:.1}L{:.1} {:.1}Z", sx + nx * 2.6, sy + ny * 2.6, sx - nx * 2.6, sy - ny * 2.6);
    let slit = format!("M{px:.1} {py:.1}L{:.1} {:.1}", bx - tx * 9., by - ty * 9.);
    // the line it is writing, rising into the nib
    let line = format!("M74 446L110 424L138 436L182 410L{px:.1} {py:.1}");
    let (tile, open, close) = if bleed {
        (r#"x="0" y="0" width="512" height="512""#, r#"<g transform="translate(256 256) scale(.8) translate(-256 -256)">"#, "</g>")
    } else {
        (r#"x="16" y="16" width="480" height="480" rx="112""#, r#"<g clip-path="url(#sq)">"#, "</g>")
    };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">
<defs>
<clipPath id="sq"><rect {tile}/></clipPath>
<radialGradient id="gl" cx=".72" cy=".26" r=".75"><stop offset="0" stop-color="{a}" stop-opacity=".24"/><stop offset="1" stop-color="{a}" stop-opacity="0"/></radialGradient>
<linearGradient id="fv" x1="0" y1="1" x2="1" y2="0"><stop offset="0" stop-color="{a}"/><stop offset=".6" stop-color="{feather}"/></linearGradient>
</defs>
<rect {tile} fill="{ground}"/>
<rect {tile} fill="url(#gl)"/>
{open}
<path d="{line}" fill="none" stroke="{a}" stroke-width="9" stroke-linecap="round" stroke-linejoin="round"/>
<circle cx="74" cy="446" r="7" fill="{a}"/>
<path d="{down}" fill="{feather}" fill-opacity=".7"/>
<path d="{vane}" fill="url(#fv)"/>
<path d="{barrel}" fill="{feather}" fill-opacity=".35"/>
<path d="{rachis}" fill="{feather}"/>
<path d="{shine}" fill="#fff" fill-opacity=".55"/>
<path d="{nib}" fill="{feather}"/>
<path d="{stain}" fill="{ground}" fill-opacity=".85"/>
<path d="{slit}" stroke="{ground}" stroke-width="1.4"/>
{close}
</svg>
"##
    )
}

pub fn svg(accent: Hsla) -> String {
    render(accent, false)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn warm() -> Hsla {
        crate::theme::hex("#d9a35f").unwrap()
    }

    #[test]
    fn follows_the_accent() {
        let (w, b) = (svg(warm()), svg(crate::theme::hex("#7aa2f7").unwrap()));
        assert_ne!(w, b);
        assert_eq!(w, svg(warm()), "the same accent draws the same icon (sync_installed relies on it)");
        assert!(w.starts_with("<svg") && w.trim_end().ends_with("</svg>"));
        assert!(w.len() < 160_000, "icon is {} bytes", w.len());
        // a grey accent still gets a colored, readable icon
        let grey = svg(hsla(0., 0., 0.5, 1.));
        assert!(grey.contains("<path d=\"M"));
    }

    #[test]
    fn numbers_are_finite() {
        let s = svg(warm());
        assert!(!s.contains("NaN") && !s.contains("inf"));
    }

    /// Redraw the web app's committed icons in the warm accent:
    /// cargo test -p finance-desktop regen_web_icons -- --ignored
    /// then rasterize the PNGs (desktop/README.md, "Icons").
    #[test]
    #[ignore]
    fn regen_web_icons() {
        let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../web");
        std::fs::write(web.join("icon.svg"), render(warm(), false)).unwrap();
        std::fs::write(web.join("icons/icon-maskable.svg"), render(warm(), true)).unwrap();
    }
}
