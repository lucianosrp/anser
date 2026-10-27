//! A small multi-series line chart painted on a canvas, for overlays the
//! kit's single-series charts can't do: a price with its average-cost line
//! and buy markers, value against money in. Linear or log scale.
//!
//! Hover needs no state: paint reads the mouse position, and the wrapper
//! asks for a repaint on every mouse move. The readout shows the hovered
//! point's label and each series' value.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use crate::zoom::px;

pub struct Series {
    pub name: SharedString,
    /// one value per x (None = a gap)
    pub ys: Vec<Option<f64>>,
    pub color: Hsla,
    pub width: f32,
    pub dashed: bool,
    /// area under the line, down to the bottom of the plot
    pub fill: Option<Hsla>,
}

impl Series {
    pub fn line(name: impl Into<SharedString>, ys: Vec<Option<f64>>, color: Hsla) -> Self {
        Series { name: name.into(), ys, color, width: 1.5, dashed: false, fill: None }
    }
    pub fn dashed(mut self) -> Self {
        self.dashed = true;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }
    pub fn fill(mut self, c: Hsla) -> Self {
        self.fill = Some(c);
        self
    }
}

/// A marker at x (fractional x allowed), value y, radius r.
pub struct Dot {
    pub x: f32,
    pub y: f64,
    pub r: f32,
    pub color: Hsla,
}

pub struct Chart {
    pub id: SharedString,
    /// one label per x: the hover readout's title
    pub labels: Vec<SharedString>,
    /// (x, text) along the bottom axis
    pub x_ticks: Vec<(usize, SharedString)>,
    pub series: Vec<Series>,
    pub dots: Vec<Dot>,
    pub log: bool,
    pub height: f32,
    pub y_fmt: Rc<dyn Fn(f64) -> String>,
}

const LEFT: f32 = 58.;
const RIGHT: f32 = 10.;
const TOP: f32 = 8.;
const BOTTOM: f32 = 22.;
const TICKS: usize = 5;
const FONT: f32 = 11.;

pub fn render(chart: Chart, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let (muted, grid, fg, pop, border) = (t.muted_foreground, t.border.opacity(0.5), t.foreground, t.popover, t.border);
    let height = chart.height;
    div()
        .id(ElementId::Name(chart.id.clone()))
        .w_full()
        .h(px(height))
        .on_mouse_move(|_, window, _| window.refresh())
        .on_hover(|_, window, _| window.refresh())
        .child(canvas(|bounds, _, _| bounds, move |bounds, _, window, cx| {
            paint(&chart, bounds, Palette { muted, grid, fg, pop, border }, window, cx)
        }).size_full())
}

struct Palette {
    muted: Hsla,
    grid: Hsla,
    fg: Hsla,
    pop: Hsla,
    border: Hsla,
}

fn text(s: &str, color: Hsla, origin: Point<Pixels>, window: &mut Window, cx: &mut App) -> Pixels {
    let font = window.text_style().font();
    let run = TextRun { len: s.len(), font, color, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(SharedString::from(s.to_string()), px(FONT), &[run], None);
    let w = line.width();
    let _ = line.paint(origin, px(FONT + 3.), TextAlign::Left, None, window, cx);
    w
}

fn text_width(s: &str, window: &Window) -> Pixels {
    let font = window.text_style().font();
    let run = TextRun { len: s.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(SharedString::from(s.to_string()), px(FONT), &[run], None).width()
}

fn paint(c: &Chart, b: Bounds<Pixels>, p: Palette, window: &mut Window, cx: &mut App) {
    let n = c.labels.len().max(c.series.iter().map(|s| s.ys.len()).max().unwrap_or(0));
    if n < 2 {
        return;
    }
    let (x0, y0) = (b.origin.x + px(LEFT), b.origin.y + px(TOP));
    let (w, h) = (b.size.width - px(LEFT + RIGHT), b.size.height - px(TOP + BOTTOM));
    let tf = |v: f64| if c.log { v.max(1e-9).ln() } else { v };
    let inv = |v: f64| if c.log { v.exp() } else { v };
    let vals = c.series.iter().flat_map(|s| s.ys.iter().flatten().copied()).chain(c.dots.iter().map(|d| d.y));
    let (mut lo, mut hi) = vals.filter(|v| v.is_finite() && (!c.log || *v > 0.)).map(tf)
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(v), b.max(v)));
    if !lo.is_finite() {
        return;
    }
    if !c.log && lo > 0. && c.series.iter().any(|s| s.fill.is_some()) {
        lo = 0.; // areas read wrong without their baseline
    }
    if hi - lo < 1e-9 {
        hi = lo + 1.;
    }
    let pad = (hi - lo) * 0.05;
    let (lo, hi) = (lo - if lo == 0. { 0. } else { pad }, hi + pad);
    let px_x = |x: f32| x0 + w * (x / (n - 1) as f32);
    let px_y = |v: f64| y0 + h * (1. - ((tf(v) - lo) / (hi - lo)) as f32);

    // grid + y labels
    for k in 0..TICKS {
        let tv = lo + (hi - lo) * k as f64 / (TICKS - 1) as f64;
        let y = y0 + h * (1. - k as f32 / (TICKS - 1) as f32);
        window.paint_quad(fill(Bounds::new(point(x0, y), size(w, px(1.))), p.grid));
        let label = (c.y_fmt)(inv(tv));
        let lw = text_width(&label, window);
        text(&label, p.muted, point(x0 - lw - px(8.), y - px(FONT * 0.7)), window, cx);
    }
    // x labels
    for (x, label) in &c.x_ticks {
        let lw = text_width(label, window);
        // kept inside the plot; a label wider than the plot starts at its left edge
        let cx_ = (px_x(*x as f32) - lw / 2.).clamp(x0, (x0 + w - lw).max(x0));
        text(label, p.muted, point(cx_, y0 + h + px(6.)), window, cx);
    }

    // areas first, then lines on top
    for s in c.series.iter().filter(|s| s.fill.is_some()) {
        let mut pb = PathBuilder::fill();
        let pts: Vec<(usize, f64)> = s.ys.iter().enumerate().filter_map(|(i, v)| v.map(|v| (i, v))).collect();
        if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
            pb.move_to(point(px_x(first.0 as f32), y0 + h));
            for (i, v) in &pts {
                pb.line_to(point(px_x(*i as f32), px_y(*v)));
            }
            pb.line_to(point(px_x(last.0 as f32), y0 + h));
            pb.close();
            if let Ok(path) = pb.build() {
                window.paint_path(path, s.fill.unwrap_or_default());
            }
        }
    }
    for s in &c.series {
        let mut pb = PathBuilder::stroke(px(s.width));
        if s.dashed {
            pb = pb.dash_array(&[px(5.), px(4.)]);
        }
        let mut pen = false;
        for (i, v) in s.ys.iter().enumerate() {
            match v {
                Some(v) if v.is_finite() && (!c.log || *v > 0.) => {
                    let pt = point(px_x(i as f32), px_y(*v));
                    if pen { pb.line_to(pt) } else { pb.move_to(pt) }
                    pen = true;
                }
                _ => pen = false,
            }
        }
        if let Ok(path) = pb.build() {
            window.paint_path(path, s.color);
        }
    }
    let circle = |center: Point<Pixels>, r: f32, color: Hsla, ring: Option<Hsla>| {
        let bounds = Bounds::centered_at(center, size(px(r * 2.), px(r * 2.)));
        quad(bounds, px(r), color, if ring.is_some() { px(1.5) } else { px(0.) }, ring.unwrap_or_default(), BorderStyle::default())
    };
    for d in &c.dots {
        window.paint_quad(circle(point(px_x(d.x), px_y(d.y)), d.r, d.color, None));
    }

    // hover: crosshair, the series' points, a readout box
    // (dev/screenshot aid: FINANCE_DESKTOP_HOVER=0.6, `?hover=0.6`, pins it at 60% of the width)
    let pinned = crate::app::dev_aid("FINANCE_DESKTOP_HOVER").and_then(|v| v.parse::<f32>().ok());
    let m = match pinned {
        Some(f) => point(x0 + w * f.clamp(0., 1.), y0 + h / 2.),
        None => window.mouse_position(),
    };
    if !(m.x >= x0 && m.x <= x0 + w && m.y >= b.origin.y && m.y <= b.origin.y + b.size.height) {
        return;
    }
    let i = (((m.x - x0) / w) * (n - 1) as f32).round().clamp(0., (n - 1) as f32) as usize;
    let x = px_x(i as f32);
    window.paint_quad(fill(Bounds::new(point(x, y0), size(px(1.), h)), p.muted.opacity(0.6)));
    let mut lines: Vec<(String, Hsla)> = vec![(c.labels.get(i).map(|s| s.to_string()).unwrap_or_default(), p.fg)];
    for s in &c.series {
        if let Some(Some(v)) = s.ys.get(i) {
            window.paint_quad(circle(point(x, px_y(*v)), 3.5, s.color, Some(p.pop)));
            lines.push((format!("{}  {}", s.name, (c.y_fmt)(*v)), s.color));
        }
    }
    let bw = lines.iter().map(|(l, _)| text_width(l, window)).fold(px(0.), |a, b| a.max(b)) + px(16.);
    let bh = px(lines.len() as f32 * (FONT + 5.) + 10.);
    let bx = if x + px(12.) + bw > x0 + w { x - px(12.) - bw } else { x + px(12.) };
    let by = y0 + px(4.);
    window.paint_quad(quad(Bounds::new(point(bx, by), size(bw, bh)), px(6.), p.pop, px(1.), p.border, BorderStyle::default()));
    for (k, (l, color)) in lines.iter().enumerate() {
        text(l, if k == 0 { p.fg } else { *color }, point(bx + px(8.), by + px(5. + k as f32 * (FONT + 5.))), window, cx);
    }
}
