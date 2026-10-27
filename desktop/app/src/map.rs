//! The travel map: country outlines from the bundled Natural Earth data,
//! painted on a canvas in theme colors (land a muted fill with hairline
//! borders, the sea is the card). No tiles, no network.
//!
//! Miller projection, fitted to whatever the map has to show (a trip's
//! stops and home, or every trip). Visited countries are tinted with the
//! accent by days spent there; routes are curved arcs, dashed when a leg is
//! inferred; stops are dots sized by spend. Hover works like linechart.rs:
//! paint reads the mouse, the wrapper repaints on every move. Labels are
//! placed once per map and size (memo.rs) clear of every dot and of each
//! other; the readout sits beside the hovered stop and its label.

use finance_core::geo::{self, LonLat};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::zoom::px;

pub struct Arc {
    pub from: LonLat,
    pub to: LonLat,
    pub dashed: bool,
    /// 0..1: how much it stands out (the overview draws every trip faintly)
    pub weight: f32,
}

pub struct Dot {
    pub at: LonLat,
    /// 0..1 of the biggest spend on the map
    pub size: f32,
    /// drawn next to the dot (None: only on hover)
    pub label: Option<String>,
    /// hover readout: title, then lines
    pub tip: Vec<String>,
    /// placed by neighbouring rows only: a hollow dot
    pub inferred: bool,
}

pub struct Map {
    pub id: SharedString,
    pub height: f32,
    pub home: Option<(LonLat, String)>,
    /// ISO country -> 0..1 tint
    pub tint: Vec<(String, f32)>,
    pub arcs: Vec<Arc>,
    pub dots: Vec<Dot>,
}

const FONT: f32 = 11.;

pub fn render(map: Map, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let pal = Palette {
        land: blend(t.secondary, t.foreground, if t.is_dark() { 0.09 } else { 0.08 }),
        border: blend(t.secondary, t.foreground, if t.is_dark() { 0.22 } else { 0.25 }),
        accent: t.primary,
        fg: t.foreground,
        muted: t.muted_foreground,
        pop: t.popover,
        pop_border: t.border,
        bg: t.secondary,
    };
    div()
        .id(ElementId::Name(map.id.clone()))
        .w_full()
        .size_full()
        .min_h(px(map.height * 0.5))
        .on_mouse_move(|_, window, _| window.refresh())
        .on_hover(|_, window, _| window.refresh())
        .child(canvas(|bounds, _, _| bounds, move |bounds, _, window, cx| {
            window.with_content_mask(Some(ContentMask { bounds }), |window| paint(&map, bounds, &pal, window, cx))
        }).size_full())
}

struct Palette {
    land: Hsla,
    border: Hsla,
    accent: Hsla,
    fg: Hsla,
    muted: Hsla,
    pop: Hsla,
    pop_border: Hsla,
    bg: Hsla,
}

/// Miller cylindrical: (x, y) in radians-ish units, y up.
fn project(p: LonLat) -> (f32, f32) {
    let lat = p.lat.clamp(-80., 84.).to_radians();
    (p.lon.to_radians(), 1.25 * (std::f32::consts::FRAC_PI_4 + 0.4 * lat).tan().ln())
}

/// The visible window in projected units: the points' box, padded, grown to
/// the canvas' aspect ratio.
fn fit(points: &[LonLat], w: f32, h: f32) -> (f32, f32, f32) {
    let pts: Vec<(f32, f32)> = points.iter().map(|p| project(*p)).collect();
    let (mut x0, mut y0, mut x1, mut y1) = pts.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(a, b, c, d), (x, y)| {
        (a.min(*x), b.min(*y), c.max(*x), d.max(*y))
    });
    if pts.is_empty() {
        (x0, y0, x1, y1) = (-3.1, -1.0, 3.1, 1.9);
    }
    // never closer than about 3° across: a day in Macau still shows the coast
    let min_span = 0.035;
    let (cx_, cy_) = ((x0 + x1) / 2., (y0 + y1) / 2.);
    let (mut sw, mut sh) = ((x1 - x0).max(min_span) * 1.35 + 0.03, (y1 - y0).max(min_span * 0.6) * 1.35 + 0.05);
    let aspect = w / h.max(1.);
    if sw / sh > aspect { sh = sw / aspect } else { sw = sh * aspect }
    let scale = w / sw;
    // (left, top, pixels per unit)
    (cx_ - sw / 2., cy_ + sh / 2., scale)
}

fn text(s: &str, color: Hsla, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let font = window.text_style().font();
    let run = TextRun { len: s.len(), font, color, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(SharedString::from(s.to_string()), px(FONT), &[run], None);
    let _ = line.paint(origin, px(FONT + 3.), TextAlign::Left, None, window, cx);
}

fn text_width(s: &str, window: &Window) -> Pixels {
    let font = window.text_style().font();
    let run = TextRun { len: s.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(SharedString::from(s.to_string()), px(FONT), &[run], None).width()
}

fn circle(center: Point<Pixels>, r: Pixels, color: Hsla, ring: Option<(Pixels, Hsla)>) -> PaintQuad {
    let (bw, bc) = ring.unwrap_or((Pixels::ZERO, Hsla::transparent_black()));
    quad(Bounds::centered_at(center, size(r * 2., r * 2.)), r, color, bw, bc, BorderStyle::default())
}

fn paint(m: &Map, b: Bounds<Pixels>, p: &Palette, window: &mut Window, cx: &mut App) {
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    if w < 10. || h < 10. {
        return;
    }
    let mut pts: Vec<LonLat> = m.dots.iter().map(|d| d.at).collect();
    pts.extend(m.home.as_ref().map(|(at, _)| *at));
    let (left, top, scale) = fit(&pts, w, h);
    let to_px = |ll: LonLat| {
        let (x, y) = project(ll);
        point(b.origin.x + gpui_kit::px((x - left) * scale), b.origin.y + gpui_kit::px((top - y) * scale))
    };
    let (right, bottom) = (left + w / scale, top - h / scale);

    // land, then the visited tint, then borders over both
    let mut borders = PathBuilder::stroke(px(0.6));
    for c in geo::countries() {
        let (bx0, by0) = project(LonLat { lon: c.bbox.0, lat: c.bbox.1 });
        let (bx1, by1) = project(LonLat { lon: c.bbox.2, lat: c.bbox.3 });
        if bx1 < left || bx0 > right || by1 < bottom || by0 > top {
            continue;
        }
        let mut fill = PathBuilder::fill();
        for ring in &c.rings {
            for (i, ll) in ring.iter().enumerate() {
                let pt = to_px(*ll);
                if i == 0 {
                    fill.move_to(pt);
                    borders.move_to(pt);
                } else {
                    fill.line_to(pt);
                    borders.line_to(pt);
                }
            }
            fill.close();
            borders.close();
        }
        if let Ok(path) = fill.build() {
            let tint = m.tint.iter().find(|(cc, _)| *cc == c.iso).map(|(_, v)| *v);
            let color = match tint {
                Some(v) => blend(p.land, p.accent, 0.25 + 0.6 * v.clamp(0., 1.)),
                None => p.land,
            };
            window.paint_path(path, color);
        }
    }
    if let Ok(path) = borders.build() {
        window.paint_path(path, p.border);
    }

    // routes: arcs bowed toward the pole, like great circles on a flat map
    for a in &m.arcs {
        let (s, e) = (to_px(a.from), to_px(a.to));
        let (dx, dy) = (f32::from(e.x - s.x), f32::from(e.y - s.y));
        let len = (dx * dx + dy * dy).sqrt();
        if len < 2. {
            continue;
        }
        let (mut nx, mut ny) = (-dy / len, dx / len);
        if ny > 0. {
            (nx, ny) = (-nx, -ny);
        }
        let bow = len * 0.2;
        let ctrl = point(s.x + gpui_kit::px(dx / 2. + nx * bow), s.y + gpui_kit::px(dy / 2. + ny * bow));
        let width = 1.0 + 0.8 * a.weight;
        let mut pb = PathBuilder::stroke(px(width));
        if a.dashed {
            pb = pb.dash_array(&[px(4.), px(3.)]);
        }
        pb.move_to(s);
        pb.curve_to(e, ctrl);
        if let Ok(path) = pb.build() {
            window.paint_path(path, p.accent.opacity(0.35 + 0.6 * a.weight));
        }
    }

    // stops, biggest under smallest
    let radius = |d: &Dot| px(3. + 8. * d.size.clamp(0., 1.).sqrt());
    let mut order: Vec<usize> = (0..m.dots.len()).collect();
    order.sort_by(|a, b| m.dots[*b].size.total_cmp(&m.dots[*a].size));
    for &i in &order {
        let d = &m.dots[i];
        let c = to_px(d.at);
        if d.inferred {
            window.paint_quad(circle(c, radius(d), p.bg, Some((px(1.5), p.accent))));
        } else {
            window.paint_quad(circle(c, radius(d), p.accent.opacity(0.9), Some((px(1.), p.bg))));
        }
    }
    // home: a ring with a dot, in the text color
    if let Some((at, _)) = &m.home {
        let c = to_px(*at);
        window.paint_quad(circle(c, px(6.), p.bg, Some((px(2.), p.fg))));
        window.paint_quad(circle(c, px(2.), p.fg, None));
    }

    // labels: placed once per map and size, not on every mouse move
    let local = |ll: LonLat| {
        let (x, y) = project(ll);
        ((x - left) * scale, (top - y) * scale)
    };
    let key = (
        w.to_bits(), h.to_bits(), crate::zoom::get().to_bits(),
        m.home.as_ref().map(|(at, s)| (at.lon.to_bits(), at.lat.to_bits(), s.as_str())),
        m.dots.iter().map(|d| (d.at.lon.to_bits(), d.at.lat.to_bits(), d.size.to_bits(), d.label.as_deref())).collect::<Vec<_>>(),
    );
    let placed = crate::memo::get("map.labels", &key, || {
        let lh = f32::from(px(FONT + 3.));
        // every dot is in every label's way, home's ring too, with a little air
        let pad = f32::from(px(2.));
        let mut dots: Vec<Rect> = m.dots.iter().map(|d| {
            let (x, y) = local(d.at);
            Rect::around(x, y, f32::from(radius(d)) + pad)
        }).collect();
        // (the dot, None for home; the text): home first, then the biggest stops
        let mut texts: Vec<(Option<usize>, &str)> = Vec::new();
        let mut wants: Vec<Want> = Vec::new();
        if let Some((at, name)) = &m.home {
            let (x, y) = local(*at);
            dots.push(Rect::around(x, y, f32::from(px(6.)) + pad));
            texts.push((None, name.as_str()));
            wants.push(Want { x, y, r: f32::from(px(7.)), w: f32::from(text_width(name, window)), h: lh });
        }
        for &i in &order {
            let Some(l) = &m.dots[i].label else { continue };
            // a place visited twice is labelled once
            if texts.iter().any(|(_, s)| *s == l.as_str()) {
                continue;
            }
            let (x, y) = local(m.dots[i].at);
            texts.push((Some(i), l.as_str()));
            wants.push(Want { x, y, r: f32::from(radius(&m.dots[i])), w: f32::from(text_width(l, window)), h: lh });
        }
        let spots = place_labels(&wants, &dots, (w, h), f32::from(px(4.)));
        let mut of_dot = vec![None; m.dots.len()];
        let mut shown = Vec::new();
        for ((dot, s), spot) in texts.into_iter().zip(spots) {
            // no room: the stop's name is still in its hover readout
            let Some(spot) = spot else { continue };
            if let Some(i) = dot {
                of_dot[i] = Some(spot);
            }
            shown.push((spot, s.to_string(), dot.is_none()));
        }
        Labels { shown, of_dot }
    });
    let at = |r: Rect| point(b.origin.x + gpui_kit::px(r.x), b.origin.y + gpui_kit::px(r.y));
    for (spot, s, home) in &placed.shown {
        text(s, if *home { p.fg } else { p.muted }, at(*spot), window, cx);
    }

    // hover: the nearest stop under the pointer
    // (screenshot aid: FINANCE_DESKTOP_MAP_HOVER=2, `?map_hover=2`, pins the readout on dot 2)
    let pinned = crate::app::dev_aid("FINANCE_DESKTOP_MAP_HOVER").and_then(|v| v.parse::<usize>().ok());
    let hit = match pinned {
        Some(i) if i < m.dots.len() => Some(i),
        Some(_) => None,
        None => {
            let mouse = window.mouse_position();
            if !b.contains(&mouse) {
                return;
            }
            m.dots.iter().enumerate()
                .map(|(i, d)| {
                    let c = to_px(d.at);
                    let dist = f32::from(c.x - mouse.x).hypot(f32::from(c.y - mouse.y));
                    (i, dist - f32::from(radius(d)))
                })
                .filter(|(_, dist)| *dist <= f32::from(px(5.)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        }
    };
    let Some(i) = hit else { return };
    let d = &m.dots[i];
    let c = to_px(d.at);
    let ring = radius(d) + px(2.);
    window.paint_quad(circle(c, ring, Hsla::transparent_black(), Some((px(1.5), p.fg))));
    let bw = d.tip.iter().map(|l| text_width(l, window)).fold(px(0.), |a, b| a.max(b)) + px(16.);
    let bh = px(d.tip.len() as f32 * (FONT + 5.) + 10.);
    // beside the stop and its label, never over them
    let (x, y) = local(d.at);
    let mut clear = Rect::around(x, y, f32::from(ring + px(1.5)));
    if let Some(l) = placed.of_dot.get(i).copied().flatten() {
        clear = clear.union(&l);
    }
    let tip = at(place_tip(clear, (f32::from(bw), f32::from(bh)), (w, h), f32::from(px(8.)), f32::from(px(2.))));
    let (bx, by) = (tip.x, tip.y);
    window.paint_quad(quad(Bounds::new(point(bx, by), size(bw, bh)), px(6.), p.pop, px(1.), p.pop_border, BorderStyle::default()));
    for (k, l) in d.tip.iter().enumerate() {
        text(l, if k == 0 { p.fg } else { p.muted }, point(bx + px(8.), by + px(5. + k as f32 * (FONT + 5.))), window, cx);
    }
}

/// A box in pixels from the map's top left corner (plain numbers, so the
/// placement below is tested without gpui).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    /// A dot's box.
    fn around(x: f32, y: f32, r: f32) -> Rect {
        Rect { x: x - r, y: y - r, w: 2. * r, h: 2. * r }
    }

    /// Touching edges don't count.
    fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }

    fn inside(&self, (w, h): (f32, f32)) -> bool {
        self.x >= 0. && self.y >= 0. && self.x + self.w <= w && self.y + self.h <= h
    }

    fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect { x, y, w: (self.x + self.w).max(o.x + o.w) - x, h: (self.y + self.h).max(o.y + o.h) - y }
    }
}

/// A label to place: its dot (centre, radius) and the text's size.
struct Want {
    x: f32,
    y: f32,
    r: f32,
    w: f32,
    h: f32,
}

/// The labels a map shows at one size.
struct Labels {
    /// (box, text, is home)
    shown: Vec<(Rect, String, bool)>,
    /// each dot's label box, when it shows one
    of_dot: Vec<Option<Rect>>,
}

/// Each label's box, first come first served: right of its dot, else left,
/// above, below, the first inside the map that touches no dot and no label
/// placed before it. None: nowhere fits, the label is left out.
fn place_labels(wants: &[Want], dots: &[Rect], area: (f32, f32), gap: f32) -> Vec<Option<Rect>> {
    let mut taken: Vec<Rect> = Vec::new();
    wants.iter().map(|l| {
        let mid_y = l.y - l.h / 2.;
        let spots = [
            Rect { x: l.x + l.r + gap, y: mid_y, w: l.w, h: l.h },
            Rect { x: l.x - l.r - gap - l.w, y: mid_y, w: l.w, h: l.h },
            Rect { x: l.x - l.w / 2., y: l.y - l.r - gap / 2. - l.h, w: l.w, h: l.h },
            Rect { x: l.x - l.w / 2., y: l.y + l.r + gap / 2., w: l.w, h: l.h },
        ];
        let spot = spots.into_iter().find(|s| {
            s.inside(area) && !dots.iter().any(|d| d.overlaps(s)) && !taken.iter().any(|t| t.overlaps(s))
        })?;
        taken.push(spot);
        Some(spot)
    }).collect()
}

/// Where the hover readout (width, height) goes: right of `clear` (the
/// hovered stop and its label), else left, below, above, the first that
/// fits inside the map once slid along that side, `margin` from the edges.
/// Nothing fits (a tiny map): the right side, clamped in.
fn place_tip(clear: Rect, (tw, th): (f32, f32), (w, h): (f32, f32), gap: f32, margin: f32) -> Rect {
    let slide = |v: f32, size: f32, room: f32| v.clamp(margin, (room - size - margin).max(margin));
    let (mid_x, mid_y) = (clear.x + clear.w / 2., clear.y + clear.h / 2.);
    let beside = slide(mid_y - th / 2., th, h);
    let over = slide(mid_x - tw / 2., tw, w);
    let spots = [
        Rect { x: clear.x + clear.w + gap, y: beside, w: tw, h: th },
        Rect { x: clear.x - gap - tw, y: beside, w: tw, h: th },
        Rect { x: over, y: clear.y + clear.h + gap, w: tw, h: th },
        Rect { x: over, y: clear.y - gap - th, w: tw, h: th },
    ];
    let inner = (w - margin, h - margin);
    spots.into_iter()
        .find(|s| s.x >= margin && s.y >= margin && s.inside(inner))
        .unwrap_or(Rect { x: slide(spots[0].x, tw, w), ..spots[0] })
}

// The Lucide icons the travel page uses beyond gpui-kit's default set,
// served by logo::AppAssets.
gpui_kit::assets::icon_assets!(pub TravelIcons, [Plane, TrainFront, BusFront, Ship, CarTaxiFront, TramFront, BedDouble, Ticket]);

/// `a` mixed toward `b` by `t` (in RGB, so hues don't swing).
fn blend(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let mix = |x: f32, y: f32| x + (y - x) * t;
    Rgba { r: mix(a.r, b.r), g: mix(a.g, b.g), b: mix(a.b, b.b), a: mix(a.a, b.a).max(a.a) }.into()
}

#[cfg(test)]
mod tests {
    // not super::*: that brings in gpui's own `test` attribute
    use super::{place_labels, place_tip, Rect, Want};

    const AREA: (f32, f32) = (400., 300.);

    fn want(x: f32, y: f32, w: f32) -> Want {
        Want { x, y, r: 5., w, h: 14. }
    }

    fn dots(ws: &[Want]) -> Vec<Rect> {
        ws.iter().map(|l| Rect::around(l.x, l.y, l.r)).collect()
    }

    #[test]
    fn a_lone_label_sits_right_of_its_dot() {
        let ws = [want(100., 100., 40.)];
        let got = place_labels(&ws, &dots(&ws), AREA, 4.);
        assert_eq!(got, vec![Some(Rect { x: 109., y: 93., w: 40., h: 14. })]);
    }

    #[test]
    fn near_the_right_edge_it_goes_left() {
        let ws = [want(390., 100., 40.)];
        let got = place_labels(&ws, &dots(&ws), AREA, 4.)[0].unwrap();
        assert_eq!(got.x + got.w, 381.);
    }

    #[test]
    fn close_stops_dont_collide() {
        // Tokyo and Osaka: the bigger placed first, the other moves aside
        let ws = [want(220., 140., 40.), want(190., 150., 40.)];
        let ds = dots(&ws);
        let got = place_labels(&ws, &ds, AREA, 4.);
        assert_eq!(got[0], Some(Rect { x: 229., y: 133., w: 40., h: 14. }));
        assert_eq!(got[1], Some(Rect { x: 141., y: 143., w: 40., h: 14. }));
        let shown: Vec<Rect> = got.iter().flatten().copied().collect();
        for (k, a) in shown.iter().enumerate() {
            assert!(a.inside(AREA));
            assert!(!ds.iter().any(|d| d.overlaps(a)), "{a:?} covers a dot");
            assert!(!shown[k + 1..].iter().any(|b| b.overlaps(a)), "{a:?} overlaps a label");
        }
        // the same input places the same way
        assert_eq!(got, place_labels(&ws, &ds, AREA, 4.));
    }

    #[test]
    fn a_label_with_no_room_is_left_out() {
        // boxed in by dots on every side
        let ws = [want(100., 100., 40.)];
        let mut ds = dots(&ws);
        ds.extend([(130., 100.), (70., 100.), (100., 80.), (100., 120.)].map(|(x, y)| Rect::around(x, y, 6.)));
        assert_eq!(place_labels(&ws, &ds, AREA, 4.), vec![None]);
        // and in a map too small for it
        let wide = [want(30., 30., 80.)];
        assert_eq!(place_labels(&wide, &dots(&wide), (60., 60.), 4.), vec![None]);
    }

    #[test]
    fn the_readout_never_covers_the_stop_or_its_label() {
        let tip = (120., 60.);
        let clear = |x: f32, y: f32| Rect::around(x, y, 8.).union(&Rect { x: x + 12., y: y - 7., w: 50., h: 14. });
        for (x, y) in [(100., 100.), (380., 150.), (340., 290.), (10., 10.), (200., 5.), (390., 295.)] {
            let c = clear(x, y);
            let t = place_tip(c, tip, AREA, 8., 2.);
            assert!(!t.overlaps(&c), "at ({x}, {y}): {t:?} covers {c:?}");
            assert!(t.inside(AREA), "at ({x}, {y}): {t:?} outside");
        }
        // room on the right: there, level with the stop
        let t = place_tip(clear(100., 100.), tip, AREA, 8., 2.);
        assert_eq!((t.x, t.y), (100. - 8. + 70. + 8., 100. - 30.));
        // at the right edge: flipped left
        let c = clear(380., 150.);
        assert_eq!(place_tip(c, tip, AREA, 8., 2.).x + tip.0, c.x - 8.);
    }

    #[test]
    fn a_map_too_small_still_clamps_the_readout_in() {
        let t = place_tip(Rect::around(50., 50., 8.), (120., 60.), (100., 80.), 8., 2.);
        assert_eq!((t.x, t.y), (2., 18.));
    }
}
