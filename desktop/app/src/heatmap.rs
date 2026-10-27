//! A correlation heatmap painted on a canvas: one cell per pair, colored by
//! the correlation (the accent for moving together, red for moving apart,
//! faint near zero), the value printed in each cell, and a hover readout.
//!
//! Hover needs no state, like linechart.rs: paint reads the mouse, and the
//! wrapper repaints on every move. `FINANCE_DESKTOP_HEAT_HOVER=1,2` pins the
//! readout on a cell (dev and screenshots).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::zoom::px;

pub struct Heatmap {
    pub id: SharedString,
    pub labels: Vec<SharedString>,
    /// m[i][j], None = not enough shared days
    pub m: Vec<Vec<Option<f64>>>,
    /// returns behind each cell
    pub n: Vec<Vec<usize>>,
}

const LABEL_W: f32 = 52.;
const HEAD_H: f32 = 22.;
const CELL_MAX: f32 = 64.;
const FONT: f32 = 11.;

/// The height the grid wants for `k` symbols (cells up to 64 px).
pub fn height(k: usize) -> f32 {
    HEAD_H + CELL_MAX * k as f32 + 4.
}

pub fn render(h: Heatmap, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let pal = Palette { pos: t.primary, neg: crate::fmt::money_out(), fg: t.foreground, muted: t.muted_foreground, pop: t.popover, border: t.border, bg: t.secondary };
    let k = h.labels.len();
    div()
        .id(ElementId::Name(h.id.clone()))
        .w_full()
        .h(px(height(k)))
        .on_mouse_move(|_, window, _| window.refresh())
        .on_hover(|_, window, _| window.refresh())
        .child(canvas(|bounds, _, _| bounds, move |bounds, _, window, cx| paint(&h, bounds, &pal, window, cx)).size_full())
}

struct Palette {
    pos: Hsla,
    neg: Hsla,
    fg: Hsla,
    muted: Hsla,
    pop: Hsla,
    border: Hsla,
    bg: Hsla,
}

fn shape(s: &str, color: Hsla, window: &Window) -> ShapedLine {
    let run = TextRun { len: s.len(), font: window.text_style().font(), color, background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(SharedString::from(s.to_string()), px(FONT), &[run], None)
}

fn text_at(s: &str, color: Hsla, center: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let line = shape(s, color, window);
    let origin = point(center.x - line.width() / 2., center.y - px(FONT * 0.65));
    let _ = line.paint(origin, px(FONT + 3.), TextAlign::Left, None, window, cx);
}

/// The cell color: accent for +, red for -, its strength the |correlation|.
pub fn cell_color(c: Option<f64>, pos: Hsla, neg: Hsla, bg: Hsla) -> Hsla {
    match c {
        Some(c) if c >= 0. => pos.opacity(0.08 + 0.72 * c.clamp(0., 1.) as f32),
        Some(c) => neg.opacity(0.08 + 0.72 * (-c).clamp(0., 1.) as f32),
        None => bg,
    }
}

fn paint(h: &Heatmap, b: Bounds<Pixels>, p: &Palette, window: &mut Window, cx: &mut App) {
    let k = h.labels.len();
    if k == 0 {
        return;
    }
    let avail_w = b.size.width - px(LABEL_W);
    let avail_h = b.size.height - px(HEAD_H + 4.);
    let cell = (avail_w / k as f32).min(avail_h / k as f32).min(px(CELL_MAX));
    let (x0, y0) = (b.origin.x + px(LABEL_W), b.origin.y + px(HEAD_H));
    for (i, l) in h.labels.iter().enumerate() {
        // column heads and row labels
        text_at(l, p.muted, point(x0 + cell * (i as f32 + 0.5), b.origin.y + px(HEAD_H / 2.)), window, cx);
        let line = shape(l, p.muted, window);
        let _ = line.paint(point(x0 - line.width() - px(8.), y0 + cell * (i as f32 + 0.5) - px(FONT * 0.65)), px(FONT + 3.), TextAlign::Left, None, window, cx);
    }
    for i in 0..k {
        for j in 0..k {
            let c = h.m[i][j];
            let r = Bounds::new(point(x0 + cell * j as f32 + px(1.), y0 + cell * i as f32 + px(1.)), size(cell - px(2.), cell - px(2.)));
            window.paint_quad(quad(r, px(4.), cell_color(c, p.pos, p.neg, p.bg), px(0.), p.border, BorderStyle::default()));
            let txt = match c {
                Some(_) if i == j => "1".to_string(),
                Some(c) => format!("{c:+.2}"),
                None => "—".to_string(),
            };
            let strong = c.is_some_and(|c| c.abs() > 0.6);
            text_at(&txt, if strong || i == j { p.fg } else { p.muted }, r.center(), window, cx);
        }
    }

    // hover
    let pinned = std::env::var("FINANCE_DESKTOP_HEAT_HOVER").ok().and_then(|v| {
        let (a, c) = v.split_once(',')?;
        Some((a.trim().parse::<usize>().ok()?, c.trim().parse::<usize>().ok()?))
    });
    let hit = match pinned {
        Some((i, j)) if i < k && j < k => Some((i, j)),
        Some(_) => None,
        None => {
            let m = window.mouse_position();
            let (fx, fy) = ((m.x - x0) / cell, (m.y - y0) / cell);
            (fx >= 0. && fy >= 0. && fx < k as f32 && fy < k as f32).then_some((fy as usize, fx as usize))
        }
    };
    let Some((i, j)) = hit else { return };
    let r = Bounds::new(point(x0 + cell * j as f32, y0 + cell * i as f32), size(cell, cell));
    window.paint_quad(quad(r, px(5.), Hsla::transparent_black(), px(2.), p.fg, BorderStyle::default()));
    let lines: Vec<String> = if i == j {
        vec![h.labels[i].to_string()]
    } else {
        vec![
            format!("{} × {}", h.labels[i], h.labels[j]),
            match h.m[i][j] {
                Some(c) => format!("{c:+.2} over {} days both traded", h.n[i][j]),
                None => format!("not enough shared days ({})", h.n[i][j]),
            },
        ]
    };
    let bw = lines.iter().map(|l| shape(l, p.fg, window).width()).fold(px(0.), |a, w| a.max(w)) + px(16.);
    let bh = px(lines.len() as f32 * (FONT + 5.) + 10.);
    let right = b.origin.x + b.size.width;
    let bx = if r.right() + px(8.) + bw > right { r.left() - px(8.) - bw } else { r.right() + px(8.) };
    let by = r.top();
    window.paint_quad(quad(Bounds::new(point(bx, by), size(bw, bh)), px(6.), p.pop, px(1.), p.border, BorderStyle::default()));
    for (n, l) in lines.iter().enumerate() {
        let line = shape(l, if n == 0 { p.fg } else { p.muted }, window);
        let _ = line.paint(point(bx + px(8.), by + px(5. + n as f32 * (FONT + 5.))), px(FONT + 3.), TextAlign::Left, None, window, cx);
    }
}
