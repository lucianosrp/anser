//! The framing: a raw capture becomes a window (rounded, a hairline border,
//! a soft two-layer shadow) or a phone (a rounded device body) on a
//! gradient backdrop. Pure pixels, no Chrome: unit-tested headless.
//!
//! Every length is in CSS pixels times `s`, the capture's device scale, so
//! a 2x desktop and a 3x phone come out in the same proportions.

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, Path, PathBuilder, Pattern, Pixmap,
    Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, Transform,
};

/// A backdrop: a diagonal gradient (`from` top-left, `to` bottom-right)
/// with a soft glow of `glow` from above.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Backdrop {
    pub from: [u8; 3],
    pub to: [u8; 3],
    pub glow: [u8; 3],
    pub glow_alpha: f32,
    /// a dark backdrop takes a deeper shadow
    pub dark: bool,
}

const fn bd(from: u32, to: u32, glow: u32, glow_alpha: f32, dark: bool) -> Backdrop {
    const fn rgb(v: u32) -> [u8; 3] {
        [(v >> 16) as u8, (v >> 8) as u8, v as u8]
    }
    Backdrop { from: rgb(from), to: rgb(to), glow: rgb(glow), glow_alpha, dark }
}

/// The presets, named after the app palette each goes with.
pub const BACKDROPS: [(&str, Backdrop); 11] = [
    ("warm", bd(0x2c2116, 0x0c0a08, 0xd9a35f, 0.20, true)),
    ("paper", bd(0xf6ecdc, 0xe2d2b6, 0xffffff, 0.55, false)),
    ("nord", bd(0x3a4860, 0x1a202c, 0x88c0d0, 0.18, true)),
    ("frost", bd(0xe8eef5, 0xc6d2e2, 0xffffff, 0.55, false)),
    ("mocha", bd(0x2f2848, 0x13111d, 0xcba6f7, 0.20, true)),
    ("latte", bd(0xf3eefb, 0xd8cfec, 0xffffff, 0.55, false)),
    ("tokyo", bd(0x262b45, 0x0e1019, 0x7aa2f7, 0.20, true)),
    ("rose", bd(0x2c2336, 0x110e16, 0xebbcba, 0.16, true)),
    ("dawn", bd(0xfaf2ea, 0xeedcd0, 0xffffff, 0.55, false)),
    ("gruvbox", bd(0x3a3024, 0x161311, 0xfabd2f, 0.13, true)),
    ("terminal", bd(0x1c1a17, 0x050506, 0xf0a43a, 0.10, true)),
];

pub fn backdrop(name: &str) -> Option<Backdrop> {
    BACKDROPS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// The backdrop that goes with an app palette in a mode.
pub fn backdrop_for(palette: &str, dark: bool) -> &'static str {
    match (palette, dark) {
        ("nord", true) => "nord",
        ("nord", false) => "frost",
        ("catppuccin", true) => "mocha",
        ("catppuccin", false) => "latte",
        ("tokyo-night", true) => "tokyo",
        ("tokyo-night", false) => "frost",
        ("rose-pine", true) => "rose",
        ("rose-pine", false) => "dawn",
        ("gruvbox", true) => "gruvbox",
        ("terminal", true) => "terminal",
        (_, true) => "warm",
        (_, false) => "paper",
    }
}

fn color([r, g, b]: [u8; 3], a: f32) -> Color {
    Color::from_rgba8(r, g, b, (a.clamp(0., 1.) * 255.).round() as u8)
}

fn paint(shader: Shader<'_>) -> Paint<'_> {
    Paint { shader, anti_alias: true, ..Default::default() }
}

/// A rounded rectangle.
pub fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w / 2.).min(h / 2.).max(0.);
    let c = r * (1. - 0.552_284_8); // control-point inset of a quarter circle
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - c, y, x + w, y + c, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - c, x + w - c, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + c, y + h, x, y + h - c, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + c, x + c, y, x + r, y);
    pb.close();
    pb.finish().expect("a rounded rect")
}

fn polygon(pts: &[(f32, f32)]) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    for p in &pts[1..] {
        pb.line_to(p.0, p.1);
    }
    pb.close();
    pb.finish().expect("a polygon")
}

/// Fill the whole pixmap with the backdrop.
pub fn paint_backdrop(pm: &mut Pixmap, b: &Backdrop) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let full = Rect::from_xywh(0., 0., w, h).expect("a size");
    let lin = LinearGradient::new(
        Point::from_xy(0., 0.),
        Point::from_xy(w * 0.45, h),
        vec![GradientStop::new(0., color(b.from, 1.)), GradientStop::new(1., color(b.to, 1.))],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("a gradient");
    pm.fill_rect(full, &paint(lin), Transform::identity(), None);
    let c = Point::from_xy(w * 0.5, -h * 0.15);
    let glow = RadialGradient::new(
        c,
        c,
        w.max(h) * 0.8,
        vec![GradientStop::new(0., color(b.glow, b.glow_alpha)), GradientStop::new(1., color(b.glow, 0.))],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("a glow");
    pm.fill_rect(full, &paint(glow), Transform::identity(), None);
}

/// One box-blur pass along rows (`stride` 1) or columns, radius `r`,
/// zero outside.
fn box_pass(a: &mut [f32], w: usize, h: usize, r: usize, rows: bool) {
    let (n, lines) = if rows { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| if rows { line * w + i } else { i * w + line };
    let norm = 1. / (2 * r + 1) as f32;
    let mut buf = vec![0f32; n];
    for line in 0..lines {
        for (i, v) in buf.iter_mut().enumerate() {
            *v = a[idx(line, i)];
        }
        let mut sum: f32 = buf.iter().take(r + 1).sum();
        for i in 0..n {
            a[idx(line, i)] = sum * norm;
            if i + r + 1 < n {
                sum += buf[i + r + 1];
            }
            if i >= r {
                sum -= buf[i - r];
            }
        }
    }
}

/// A soft shadow of `path`, moved down `dy` and blurred to about `blur`
/// (three box passes approximate a gaussian), laid over the pixmap.
pub fn shadow(pm: &mut Pixmap, path: &Path, dy: f32, blur: f32, alpha: f32) {
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let Some(mut mask) = Mask::new(w as u32, h as u32) else { return };
    mask.fill_path(path, FillRule::Winding, true, Transform::from_translate(0., dy));
    let mut a: Vec<f32> = mask.data().iter().map(|&v| v as f32 / 255.).collect();
    // three passes of width b: sigma^2 = 3 (b^2 - 1) / 12
    let sigma = blur / 2.;
    let b = ((4. * sigma * sigma + 1.).sqrt()).round().max(1.);
    let r = ((b - 1.) / 2.).round() as usize;
    if r > 0 {
        for _ in 0..3 {
            box_pass(&mut a, w, h, r, true);
            box_pass(&mut a, w, h, r, false);
        }
    }
    // the canvas is opaque, so premultiplied = straight: darken toward black
    for (px, m) in pm.data_mut().chunks_exact_mut(4).zip(a) {
        let t = (m * alpha).clamp(0., 1.);
        for c in &mut px[..3] {
            *c = (*c as f32 * (1. - t)).round() as u8;
        }
    }
}

/// `img` drawn at (x, y), clipped to `clip` (anti-aliased edges).
fn draw_clipped(pm: &mut Pixmap, img: &Pixmap, x: f32, y: f32, clip: &Path) {
    let pat = Pattern::new(img.as_ref(), SpreadMode::Pad, FilterQuality::Nearest, 1., Transform::from_translate(x, y));
    pm.fill_path(clip, &paint(pat), FillRule::Winding, Transform::identity(), None);
}

fn stroke(pm: &mut Pixmap, path: &Path, c: Color, width: f32) {
    let p = Paint { shader: Shader::SolidColor(c), anti_alias: true, ..Default::default() };
    pm.stroke_path(path, &p, &Stroke { width, ..Default::default() }, Transform::identity(), None);
}

fn fill(pm: &mut Pixmap, path: &Path, c: Color) {
    let p = Paint { shader: Shader::SolidColor(c), anti_alias: true, ..Default::default() };
    pm.fill_path(path, &p, FillRule::Winding, Transform::identity(), None);
}

/// Window padding around the capture, CSS px.
pub const WINDOW_PAD: f32 = 80.;

/// A desktop capture as a window on the backdrop. `app_dark`: the app's
/// own mode, for the border's ink.
pub fn window(shot: &Pixmap, s: f32, b: &Backdrop, app_dark: bool) -> Pixmap {
    let pad = (WINDOW_PAD * s).round();
    let (sw, sh) = (shot.width() as f32, shot.height() as f32);
    let mut pm = Pixmap::new((sw + 2. * pad) as u32, (sh + 2. * pad) as u32).expect("a size");
    paint_backdrop(&mut pm, b);
    let r = 12. * s;
    let win = rrect(pad, pad, sw, sh, r);
    let deep = if b.dark { 1. } else { 0.5 };
    shadow(&mut pm, &win, 22. * s, 44. * s, 0.55 * deep);
    shadow(&mut pm, &win, 2. * s, 4. * s, 0.30 * deep);
    draw_clipped(&mut pm, shot, pad, pad, &win);
    let ink = if app_dark { color([255, 255, 255], 0.12) } else { color([0, 0, 0], 0.12) };
    let half = 0.5 * s;
    stroke(&mut pm, &rrect(pad + half, pad + half, sw - 2. * half, sh - 2. * half, r - half), ink, s);
    pm
}

/// Phone padding around the device body, CSS px.
pub const PHONE_PAD: f32 = 72.;
const BEZEL: f32 = 11.;
const SCREEN_R: f32 = 46.;

/// A phone capture in a plain device body on the backdrop.
pub fn phone(shot: &Pixmap, s: f32, b: &Backdrop) -> Pixmap {
    let pad = (PHONE_PAD * s).round();
    let bezel = (BEZEL * s).round();
    let (sw, sh) = (shot.width() as f32, shot.height() as f32);
    let (bw, bh) = (sw + 2. * bezel, sh + 2. * bezel);
    let mut pm = Pixmap::new((bw + 2. * pad) as u32, (bh + 2. * pad) as u32).expect("a size");
    paint_backdrop(&mut pm, b);
    let body_r = (SCREEN_R + BEZEL) * s;
    let body = rrect(pad, pad, bw, bh, body_r);
    let deep = if b.dark { 1. } else { 0.55 };
    shadow(&mut pm, &body, 26. * s, 50. * s, 0.6 * deep);
    shadow(&mut pm, &body, 3. * s, 6. * s, 0.3 * deep);
    // the side buttons, just proud of the body
    let key = color([0x2c, 0x2c, 0x31], 1.);
    for (left, top, len) in [(true, 118., 30.), (true, 172., 58.), (true, 240., 58.), (false, 196., 92.)] {
        let x = if left { pad - 2.5 * s } else { pad + bw - 1.5 * s };
        fill(&mut pm, &rrect(x, pad + top * s, 4. * s, len * s, 1.5 * s), key);
    }
    // the body: a satin rim, then the black glass around the screen
    let rim = LinearGradient::new(
        Point::from_xy(pad, pad),
        Point::from_xy(pad + bw, pad + bh),
        vec![GradientStop::new(0., color([0x4a, 0x4a, 0x52], 1.)), GradientStop::new(0.5, color([0x24, 0x24, 0x29], 1.)), GradientStop::new(1., color([0x3c, 0x3c, 0x43], 1.))],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("a gradient");
    pm.fill_path(&body, &paint(rim), FillRule::Winding, Transform::identity(), None);
    let inset = 2.5 * s;
    fill(&mut pm, &rrect(pad + inset, pad + inset, bw - 2. * inset, bh - 2. * inset, body_r - inset), color([0x06, 0x06, 0x07], 1.));
    let screen = rrect(pad + bezel, pad + bezel, sw, sh, SCREEN_R * s);
    draw_clipped(&mut pm, shot, pad + bezel, pad + bezel, &screen);
    pm
}

/// Two captures of the same view (dark, light) in one: dark on the left,
/// light on the right, along a slanted line through the middle.
pub fn split(dark: &Pixmap, light: &Pixmap) -> Pixmap {
    let mut pm = dark.clone();
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let cut = polygon(&[(w * 0.58, 0.), (w + 1., 0.), (w + 1., h + 1.), (w * 0.42, h + 1.)]);
    draw_clipped(&mut pm, light, 0., 0., &cut);
    pm
}

/// A decoded PNG as a pixmap (premultiplied, as tiny-skia wants).
pub fn from_png(bytes: &[u8]) -> anyhow::Result<Pixmap> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u16 * a + 127) / 255) as u8;
            }
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h).ok_or_else(|| anyhow::anyhow!("empty image"))?)
        .ok_or_else(|| anyhow::anyhow!("bad image size"))
}

/// Straight RGBA of a pixmap (the frames are opaque, so this is a copy).
pub fn rgba(pm: &Pixmap) -> image::RgbaImage {
    let mut data = pm.data().to_vec();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    image::RgbaImage::from_raw(pm.width(), pm.height(), data).expect("a size")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat capture with a marker pixel in its middle.
    fn shot(w: u32, h: u32, rgb: [u8; 3]) -> Pixmap {
        let mut pm = Pixmap::new(w, h).unwrap();
        pm.fill(Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255));
        pm
    }

    fn at(pm: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let p = pm.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    #[test]
    fn every_palette_has_a_backdrop_and_presets_are_unique() {
        for (id, _) in [("warm", 0), ("nord", 0), ("catppuccin", 0), ("gruvbox", 0), ("tokyo-night", 0), ("rose-pine", 0), ("glass", 0), ("terminal", 0)] {
            for dark in [true, false] {
                let name = backdrop_for(id, dark);
                let b = backdrop(name).unwrap_or_else(|| panic!("{name}"));
                assert_eq!(b.dark, dark, "{id} {dark}: {name}");
            }
        }
        let mut names: Vec<_> = BACKDROPS.iter().map(|(n, _)| *n).collect();
        names.dedup();
        assert_eq!(names.len(), BACKDROPS.len());
    }

    #[test]
    fn the_backdrop_runs_from_its_first_colour_to_its_second() {
        let b = backdrop("warm").unwrap();
        let mut pm = Pixmap::new(200, 100).unwrap();
        paint_backdrop(&mut pm, &b);
        let (tl, br) = (at(&pm, 0, 0), at(&pm, 199, 99));
        assert_eq!(tl[3], 255);
        assert!(tl[0] > br[0] && tl[1] > br[1], "{tl:?} {br:?}");
        // the far corner is the gradient's end (the glow has faded there)
        assert!((br[0] as i32 - b.to[0] as i32).abs() <= 6, "{br:?}");
    }

    #[test]
    fn a_window_is_the_capture_inset_rounded_and_shadowed() {
        let s = 2.;
        let cap = shot(400, 300, [10, 200, 30]);
        let b = backdrop("warm").unwrap();
        let pm = window(&cap, s, &b, true);
        let pad = (WINDOW_PAD * s) as u32;
        assert_eq!((pm.width(), pm.height()), (400 + 2 * pad, 300 + 2 * pad));
        // the middle is the capture, untouched
        assert_eq!(at(&pm, pad + 200, pad + 150), [10, 200, 30, 255]);
        // opaque everywhere, the corners are backdrop
        assert!(pm.pixels().iter().all(|p| p.alpha() == 255));
        assert_ne!(at(&pm, 0, 0)[1], 200);
        // the rounded corner: its very corner pixel isn't the capture
        let c = at(&pm, pad, pad);
        assert!(c[1] < 150, "{c:?}");
        // the shadow falls below: darker under the window than above it
        let mut flat = Pixmap::new(pm.width(), pm.height()).unwrap();
        paint_backdrop(&mut flat, &b);
        let x = pad + 200;
        let lum = |p: [u8; 4]| p[0] as i32 + p[1] as i32 + p[2] as i32;
        let below = lum(at(&flat, x, pad + 300 + 20)) - lum(at(&pm, x, pad + 300 + 20));
        let above = lum(at(&flat, x, pad - 20)) - lum(at(&pm, x, pad - 20));
        assert!(below > above && below > 0, "below {below} above {above}");
    }

    #[test]
    fn a_phone_frames_the_screen_in_a_body() {
        let s = 3.;
        let cap = shot(390, 844, [240, 240, 240]);
        let pm = phone(&cap, s, &backdrop("paper").unwrap());
        let pad = (PHONE_PAD * s).round() as u32;
        let bezel = (BEZEL * s).round() as u32;
        assert_eq!((pm.width(), pm.height()), (390 + 2 * (bezel + pad), 844 + 2 * (bezel + pad)));
        assert_eq!(at(&pm, pad + bezel + 195, pad + bezel + 422), [240, 240, 240, 255]);
        // the bezel beside the screen's middle is near black glass
        let g = at(&pm, pad + bezel / 2, pad + bezel + 422);
        assert!(g[0] < 40, "{g:?}");
    }

    #[test]
    fn a_split_is_dark_on_the_left_and_light_on_the_right() {
        let (d, l) = (shot(100, 60, [0, 0, 0]), shot(100, 60, [255, 255, 255]));
        let pm = split(&d, &l);
        assert_eq!(at(&pm, 2, 2), [0, 0, 0, 255]);
        assert_eq!(at(&pm, 97, 57), [255, 255, 255, 255]);
        assert_eq!(at(&pm, 2, 57), [0, 0, 0, 255]);
        assert_eq!(at(&pm, 97, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn png_round_trip() {
        let cap = shot(8, 4, [1, 2, 3]);
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(rgba(&cap)).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let back = from_png(&png).unwrap();
        assert_eq!(back.data(), cap.data());
    }
}
