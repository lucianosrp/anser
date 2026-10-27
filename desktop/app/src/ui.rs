//! The design system: spacing and type scales, surfaces, and the small
//! building blocks every page uses (cards, stat tiles, chips, sparklines,
//! keycaps, the page-enter transition).
//!
//! Principles: quiet surfaces, one accent, color only where it carries
//! meaning (money in/out, state). Figures use tabular numerals so columns
//! and changing values never jitter.

#![allow(dead_code)] // a shared kit: not every part is used by every build of the pages

use std::sync::Arc;
use std::time::Duration;

// std's Instant panics in the browser; web_time is std's on native
use web_time::Instant;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, chart::AreaChart, h_flex,
    tooltip::Tooltip, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::zoom::px;

// ---------- scales ----------

/// Spacing scale (px). Use these instead of ad-hoc numbers.
pub mod sp {
    pub const XS: f32 = 4.;
    pub const S: f32 = 8.;
    pub const M: f32 = 12.;
    pub const L: f32 = 16.;
    pub const XL: f32 = 24.;
    /// gap between cards in a grid / between page sections
    pub const GRID: f32 = 12.;
    pub const SECTION: f32 = 20.;
}

/// Type scale (px).
pub mod ts {
    pub const EYEBROW: f32 = 11.;
    pub const CAPTION: f32 = 12.;
    pub const BODY: f32 = 13.;
    pub const TITLE: f32 = 14.;
    pub const FIGURE: f32 = 22.;
    pub const HERO: f32 = 34.;
}

/// OpenType `tnum` (+ `lnum`): every digit the same width.
pub fn tnum() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1), ("lnum".into(), 1)]))
}

/// `.figures()` on any styled element: tabular, lining numerals.
pub trait Figures: Styled + Sized {
    fn figures(self) -> Self {
        self.font_features(tnum())
    }
}
impl<T: Styled + Sized> Figures for T {}

/// The palette's structure beyond colors (theme.rs; the Terminal palette's).
pub fn look(cx: &App) -> crate::theme::Look {
    cx.try_global::<crate::theme::Look>().copied().unwrap_or_default()
}

/// The color of a page's one hero figure: the ink, or the accent under the
/// terminal look (a key figure is where the terminal's amber points).
pub fn hero_color(cx: &App) -> Hsla {
    let t = cx.theme();
    if look(cx).terminal { t.primary } else { t.foreground }
}

/// The quiet border used by cards and dividers.
pub fn hairline(cx: &App) -> Hsla {
    let t = cx.theme();
    t.border.opacity(if t.is_dark() { 0.85 } else { 1. })
}

// ---------- one chart point ----------

/// One chart point: a band/x label and a value.
#[derive(Clone)]
pub struct Pt {
    pub label: SharedString,
    pub v: f64,
}

// ---------- surfaces ----------

pub fn card(cx: &App) -> Div {
    let t = cx.theme();
    v_flex()
        .gap_1()
        .p(px(sp::L))
        .rounded(t.radius_lg)
        .border_1()
        .border_color(hairline(cx))
        .bg(t.secondary)
        .when(!t.is_dark() && !look(cx).terminal, |d| d.shadow_xs())
}

/// One slice of a card cut into rows, so a long card can go into a
/// virtualized list a row at a time: the first slice has the top edge, the
/// last the bottom one, and together they read as one [`card`].
pub fn card_slice(first: bool, last: bool, cx: &App) -> Div {
    let t = cx.theme();
    v_flex()
        .px(px(sp::L))
        .border_l_1()
        .border_r_1()
        .border_color(hairline(cx))
        .bg(t.secondary)
        .when(first, |d| d.pt(px(sp::L)).border_t_1().rounded_t(t.radius_lg))
        .when(last, |d| d.pb(px(sp::L)).border_b_1().rounded_b(t.radius_lg))
}

/// A card that reacts to the pointer (border + surface lift on hover).
pub fn card_interactive(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let (hover_border, hover_bg) = (t.muted_foreground.opacity(0.35), t.accent);
    card(cx)
        .id(id)
        .cursor_pointer()
        .hover(move |s| s.border_color(hover_border).bg(hover_bg))
}

/// Small uppercase label above a figure.
pub fn eyebrow(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = cx.theme();
    div()
        .text_size(px(ts::EYEBROW))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
        .whitespace_nowrap()
        .child(text.into().to_uppercase())
}

/// Muted one-line caption.
pub fn caption(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(px(ts::CAPTION))
        .text_color(cx.theme().muted_foreground)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(text.into())
}

/// Muted caption that wraps (a note, an explanation). `caption` is one line
/// with an ellipsis, and `.whitespace_normal()` on it still truncates.
pub fn caption_wrap(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(px(ts::CAPTION))
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

// ---------- stat tiles ----------

/// Label / big value / caption stat tile. `tone` colors the value.
pub fn stat(label: &str, value: impl Into<SharedString>, sub: impl Into<SharedString>, tone: Option<Hsla>, cx: &App) -> Div {
    let mut tile = Tile::new(label, value).sub(sub);
    if let Some(c) = tone {
        tile = tile.tone(c);
    }
    tile.render(cx)
}

/// A stat tile with optional delta chip and sparkline. Builder:
///
/// ```ignore
/// ui::Tile::new("Cash", fmt::hkd(cash)).sub("banks").delta_over(1.8, "1m").spark(vals, t.chart_2).render(cx)
/// ```
pub struct Tile {
    label: SharedString,
    value: SharedString,
    sub: Option<SharedString>,
    tone: Option<Hsla>,
    delta: Option<(f64, SharedString)>,
    spark: Option<(Vec<f64>, Option<Hsla>)>,
    id: Option<ElementId>,
    info: Option<SharedString>,
}

impl Tile {
    pub fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Tile { label: label.into(), value: value.into(), sub: None, tone: None, delta: None, spark: None, id: None, info: None }
    }
    pub fn sub(mut self, s: impl Into<SharedString>) -> Self {
        let s = s.into();
        self.sub = (!s.is_empty()).then_some(s);
        self
    }
    /// Color of the value (use the money colors only when it means in/out).
    /// `new` with an ⓘ explainer.
    pub fn explained(label: impl Into<SharedString>, explain: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self::new(label, value).info(explain)
    }

    /// An ⓘ next to the label that explains the number on hover.
    pub fn info(mut self, text: impl Into<SharedString>) -> Self {
        self.info = Some(text.into());
        self
    }

    pub fn tone(mut self, c: Hsla) -> Self {
        self.tone = Some(c);
        self
    }
    /// Delta chip with a period suffix, e.g. `delta_over(3.2, "1m")` -> "▲ 3.2% 1m".
    pub fn delta_over(mut self, pct: f64, period: impl Into<SharedString>) -> Self {
        self.delta = pct.is_finite().then(|| (pct, period.into()));
        self
    }
    /// `delta_over` when there is a change to show.
    pub fn delta_opt(self, pct: Option<f64>, period: impl Into<SharedString>) -> Self {
        match pct {
            Some(p) => self.delta_over(p, period),
            None => self,
        }
    }
    /// Sparkline under the value. `None` color = the accent.
    pub fn spark(mut self, values: Vec<f64>, color: impl Into<Option<Hsla>>) -> Self {
        if values.len() >= 2 {
            self.spark = Some((values, color.into()));
        }
        self
    }
    /// Needed when several tiles with sparklines come from one call site.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn render(self, cx: &App) -> Div {
        let t = cx.theme();
        let spark_id: ElementId = self.id.clone().unwrap_or_else(|| ElementId::Name(format!("spark-{}", self.label).into()));
        card(cx)
            .flex_1()
            .min_w(px(170.))
            .gap(px(6.))
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .h(px(18.))
                    .child(match self.info.clone() {
                        Some(text) => eyebrow_info(self.label.clone(), text, cx),
                        None => eyebrow(self.label.clone(), cx),
                    })
                    .when_some(self.delta, |d, (pct, period)| d.child(delta_over(pct, period, cx))),
            )
            .child(
                div()
                    .text_size(px(ts::FIGURE))
                    .line_height(px(28.))
                    .font_semibold()
                    .figures()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .when_some(self.tone, |d, c| d.text_color(c))
                    .child(self.value),
            )
            .when_some(self.sub, |d, s| d.child(caption(s, cx)))
            .when_some(self.spark, |d, (vals, color)| {
                let c = color.unwrap_or(t.primary);
                d.child(div().mt_1().h(px(30.)).child(sparkline(spark_id, &vals, c)))
            })
    }
}

// ---------- explainers ----------

/// The ⓘ's box (unzoomed px): a hair over a capital's height at the eyebrow
/// size, so it reads as part of the label, not as a button.
pub const INFO: f32 = 13.;
/// How far above its line box's middle a label's capitals sit, in em (Noto
/// Sans' ascent and descent aren't symmetric). The ⓘ is lifted by this much
/// of the text size next to it, so it centers on the capitals rather than a
/// pixel below them. Measured from screenshots at zooms 0.8–1.5: what's
/// left is gpui snapping the glyphs and the icon to device pixels on their
/// own, at most one device pixel either way.
const CAP_LIFT: f32 = 0.03;

/// A small ⓘ that explains a number on hover: Lucide's info glyph, one
/// shape in a fixed box (a text "i" in a bordered circle snapped to the
/// pixel grid on its own and drifted inside the ring), following the theme.
/// Hover shows a wrapped card. Next to text use [`with_info`] (or
/// [`info_for`]), which centers it on the text.
pub fn info(id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let t = cx.theme();
    let text: SharedString = text.into();
    let hover = t.foreground;
    div()
        .id(id)
        .size(px(INFO))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(t.muted_foreground.opacity(0.85))
        .hover(move |s| s.text_color(hover))
        .child(Icon::new(IconName::Info).size(px(INFO)))
        .tooltip(move |window, cx| {
            let text = text.clone();
            Tooltip::element(move |_, cx| {
                div().max_w(px(300.)).text_size(px(ts::CAPTION)).line_height(px(17.))
                    .text_color(cx.theme().popover_foreground).child(text.clone())
            })
            .build(window, cx)
        })
}

/// `stat` with an ⓘ explainer next to the label.
pub fn stat_info(label: &str, value: impl Into<SharedString>, sub: impl Into<SharedString>, tone: Option<Hsla>,
                 explain: impl Into<SharedString>, cx: &App) -> Div {
    let mut tile = Tile::new(label.to_string(), value).sub(sub).info(explain);
    if let Some(c) = tone {
        tile = tile.tone(c);
    }
    tile.render(cx)
}

/// [`info`] for a center-aligned row next to text of `text_size` px: lifted
/// onto the text's optical center.
pub fn info_for(text_size: f32, id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    info(id, text, cx).relative().top(px(-text_size * CAP_LIFT))
}

/// A label with its ⓘ: `label` is one line of text at `text_size` px (an
/// eyebrow, a title, a caption). Every explainer next to a label goes
/// through here, so they all sit the same.
pub fn with_info(label: impl IntoElement, text_size: f32, id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .gap(px(5.))
        .items_center()
        .flex_shrink_0()
        .child(label)
        .child(info_for(text_size, id, text, cx))
}

/// An [`eyebrow`] with its ⓘ (stat tiles and every card header like them).
pub fn eyebrow_info(label: impl Into<SharedString>, text: impl Into<SharedString>, cx: &App) -> Div {
    let label: SharedString = label.into();
    let id = ElementId::Name(format!("info-{label}").into());
    with_info(eyebrow(label, cx), ts::EYEBROW, id, text, cx)
}

/// A card or section title (14px semibold, one line).
pub fn title(text: impl Into<SharedString>) -> Div {
    div().text_size(px(ts::TITLE)).font_semibold().whitespace_nowrap().child(text.into())
}

/// A section or chart card's title: [`title`], or under the terminal look
/// capitals in the accent at the caption's size. With its text size.
fn heading(text: &str, cx: &App) -> (Div, f32) {
    if !look(cx).terminal {
        return (title(text.to_string()), ts::TITLE);
    }
    let d = div()
        .text_size(px(ts::CAPTION))
        .font_semibold()
        .whitespace_nowrap()
        .text_color(cx.theme().primary)
        .child(text.to_uppercase());
    (d, ts::CAPTION)
}

/// Box-drawing glyphs for the terminal look's rules: in the heading's
/// monospace at its size, so they sit on the text's line and join up.
fn box_rule(cx: &App) -> Div {
    let t = cx.theme();
    div().text_size(px(ts::CAPTION)).line_height(px(RULE_LINE)).whitespace_nowrap().text_color(t.muted_foreground.opacity(0.45))
}

/// The rules' line box (px): the lead-in and the fill share it, so their
/// glyphs sit at the same height.
const RULE_LINE: f32 = 16.;

/// A run of `─` long enough for any width, clipped where its box ends.
static RULE_FILL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| "─".repeat(360));

/// Under the terminal look, a heading drawn as `── TITLE  caption ─────`:
/// the lead-in before the title, the fill after the caption. The fill's
/// text is out of the flow (absolute), so it takes the room that's left and
/// never widens the row. Rows holding them center their items.
fn heading_rules(cx: &App) -> Option<(Div, Div)> {
    look(cx).terminal.then(|| {
        let fill = div().flex_1().min_w(px(sp::L)).h(px(RULE_LINE)).relative().overflow_hidden()
            .child(box_rule(cx).absolute().top_0().left_0().child(RULE_FILL.as_str()));
        (box_rule(cx).flex_shrink_0().child("──"), fill)
    })
}

/// Section title with an ⓘ explainer (next to the title, then the caption).
pub fn section_info(title_txt: &str, caption_txt: &str, explain: impl Into<SharedString>, cx: &App) -> Div {
    let id = ElementId::Name(format!("info-sec-{title_txt}").into());
    let (head, size) = heading(title_txt, cx);
    let (lead, fill) = heading_rules(cx).unzip();
    h_flex()
        .gap_2()
        .items_center()
        .mt(px(sp::S))
        .children(lead)
        .child(with_info(head, size, id, explain, cx))
        .child(caption(caption_txt.to_string(), cx))
        .children(fill)
}

// ---------- chips ----------

/// A small pill. `fg` colors text; the fill is a faint wash of it.
/// Under the terminal look it is the text in brackets, `[+4.4% 1m]`, unfilled.
pub fn chip(text: impl Into<SharedString>, fg: Hsla, cx: &App) -> Div {
    let d = h_flex()
        .flex_shrink_0()
        .gap_1()
        .h(px(18.))
        .text_color(fg)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .figures()
        .whitespace_nowrap();
    if look(cx).terminal {
        return d.child(format!("[{}]", text.into()));
    }
    d.px(px(6.)).rounded(px(5.)).bg(fg.opacity(0.12)).child(text.into())
}

/// A neutral pill (muted text on the muted surface).
pub fn tag(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = cx.theme();
    let tag = chip(text, t.muted_foreground, cx);
    if look(cx).terminal { tag } else { tag.bg(t.muted) }
}

/// Percent-change chip with a period suffix: "▲ 3.2% 1m".
pub fn delta_over(pct: f64, period: impl Into<SharedString>, cx: &App) -> Div {
    let period = period.into();
    // the terminal look signs it, [+4.4% 1m], where the others draw an arrow
    let term = look(cx).terminal;
    let (arrow, color) = if pct >= 0.05 {
        (if term { "+" } else { "▲ " }, fmt::money_in())
    } else if pct <= -0.05 {
        (if term { "−" } else { "▼ " }, fmt::money_out())
    } else {
        ("", cx.theme().muted_foreground)
    };
    let txt = format!("{arrow}{:.1}%{}{period}", pct.abs(), if period.is_empty() { "" } else { " " });
    chip(txt, color, cx)
}

/// A small filled circle (status, legends).
pub fn dot(color: Hsla, size: f32) -> Div {
    div().flex_shrink_0().size(px(size)).rounded_full().bg(color)
}

/// Legend entry: dot + label (+ optional muted value).
pub fn legend(color: Hsla, label: impl Into<SharedString>, value: impl Into<SharedString>, cx: &App) -> Div {
    let value = value.into();
    h_flex()
        .gap(px(6.))
        .text_size(px(ts::CAPTION))
        .whitespace_nowrap()
        .child(dot(color, 7.))
        .child(div().text_color(cx.theme().foreground).child(label.into()))
        .when(!value.is_empty(), |d| d.child(div().text_color(cx.theme().muted_foreground).figures().child(value)))
}

// ---------- keycaps ----------

/// Do key hints mean anything here? Not on a phone layout or a touch-only
/// device: there every kbd cap, sidebar key and "(Ctrl+X)" goes away.
pub fn show_shortcuts() -> bool {
    crate::layout::has_keyboard()
}

/// A tooltip or label with its shortcut: "Sync now (Ctrl+R)", or just
/// "Sync now" where there's no keyboard ([`show_shortcuts`]).
pub fn keyed(text: &str, keys: &str) -> SharedString {
    if show_shortcuts() { format!("{text} ({keys})").into() } else { SharedString::from(text.to_string()) }
}

/// Keyboard hint: `kbd("ctrl-k")` -> [Ctrl][K]. Accepts "-" or "+" separators.
/// Callers that show a hint row check [`show_shortcuts`] first (a cap on its
/// own here collapses to nothing on a phone).
pub fn kbd(keys: &str, cx: &App) -> Div {
    if !show_shortcuts() {
        return div();
    }
    let t = cx.theme();
    let cap = |k: &str| {
        let label = match k.to_lowercase().as_str() {
            "ctrl" => "Ctrl".to_string(),
            "shift" => "Shift".into(),
            "alt" => "Alt".into(),
            "enter" => "↵".into(),
            "esc" | "escape" => "Esc".into(),
            other if other.len() == 1 => other.to_uppercase(),
            other => {
                let mut c = other.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        };
        div()
            .flex_shrink_0()
            .min_w(px(16.))
            .h(px(16.))
            .px(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(t.border)
            .bg(t.background.opacity(0.6))
            .text_size(px(10.))
            .line_height(px(14.))
            .text_color(t.muted_foreground)
            .child(label)
    };
    h_flex().flex_shrink_0().gap(px(2.)).children(keys.split(['-', '+']).filter(|k| !k.is_empty()).map(cap))
}

// ---------- charts ----------

/// Tiny axis-less trend line with a soft fill. Fills its parent, which needs
/// a height. Pinned to the data's own range so small moves stay visible.
pub fn sparkline(id: impl Into<ElementId>, values: &[f64], color: Hsla) -> impl IntoElement {
    let pts: Vec<Pt> = values.iter().enumerate().map(|(i, v)| Pt { label: i.to_string().into(), v: *v }).collect();
    let (lo, hi) = values.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    let pad = ((hi - lo) * 0.08).max(hi.abs() * 0.001).max(1e-9);
    AreaChart::new(pts).appear(false)
        .id(id)
        .x(|p: &Pt| p.label.clone())
        .y(|p: &Pt| p.v)
        .stroke(color)
        .fill(soft_fill(color, 0.22))
        .natural()
        .x_axis(false)
        .grid(false)
        .y_domain(lo - pad, hi + pad)
        .y_padding(2., 0.)
        .interactive(false)
}

/// Vertical wash under an area series: `alpha` at the line, clear at the floor.
pub fn soft_fill(color: Hsla, alpha: f32) -> Background {
    linear_gradient(180., linear_color_stop(color.opacity(alpha), 0.), linear_color_stop(color.opacity(0.), 1.))
}

/// A chart in a fixed-height card (charts fill their parent, which needs a height).
pub fn chart_card(title: &str, caption: &str, height: f32, chart: impl IntoElement, cx: &App) -> Div {
    chart_card_with(title, caption, div(), height, chart, cx)
}

/// `chart_card` with something on the right of the title row (legend, chips).
pub fn chart_card_with(title: &str, caption_txt: &str, right: impl IntoElement, height: f32, chart: impl IntoElement, cx: &App) -> Div {
    card(cx)
        .h(px(height))
        .gap(px(sp::M))
        .child(
            h_flex()
                .gap_3()
                .justify_between()
                .flex_wrap()
                .child(
                    h_flex()
                        .gap_2()
                        .items_baseline()
                        .min_w_0()
                        .child(heading(title, cx).0)
                        .child(caption(caption_txt.to_string(), cx)),
                )
                .child(right),
        )
        .child(div().flex_1().min_h_0().child(chart))
}

// ---------- sections, rows ----------

pub fn section(title: &str, caption_txt: &str, cx: &App) -> Div {
    let (lead, fill) = heading_rules(cx).unzip();
    let ruled = lead.is_some();
    h_flex()
        .gap_2()
        .when(ruled, |d| d.items_center())
        .when(!ruled, |d| d.items_baseline())
        .mt(px(sp::S))
        .children(lead)
        .child(heading(title, cx).0)
        .child(caption(caption_txt.to_string(), cx))
        .children(fill)
}

/// The page's title in the header: the page's name, or under the terminal
/// look a prompt, `> overview`, and a block cursor (steady: nothing moves on
/// a page that is ready).
pub fn page_title(label: &str, cx: &App) -> Div {
    let t = cx.theme();
    let base = || div().text_size(px(20.)).line_height(px(26.)).font_semibold().whitespace_nowrap().overflow_hidden().text_ellipsis();
    if !look(cx).terminal {
        return base().child(label.to_string());
    }
    let cursor = div().flex_shrink_0().w(px(10.)).h(px(20.)).bg(t.primary).opacity(0.85);
    // the phone's header is narrow: the name keeps the room, no cursor
    let compact = crate::layout::compact();
    h_flex()
        .items_center()
        .gap(px(if compact { 8. } else { 10. }))
        .min_w_0()
        .child(base().flex_shrink_0().text_color(t.primary).child(">"))
        .child(base().min_w_0().child(label.to_lowercase()))
        .when(!compact, |d| d.child(cursor))
}

/// Horizontal bar row: label, proportional bar, value.
pub fn bar_row(label: &str, value: f64, max: f64, value_txt: String, color: Hsla, cx: &App) -> Div {
    let t = cx.theme();
    let frac = if max > 0. { (value / max).clamp(0., 1.) as f32 } else { 0. };
    h_flex()
        .gap_3()
        .py(px(5.))
        .text_size(px(ts::BODY))
        .child(div().w(px(140.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label.to_string()))
        .child(
            div().flex_1().min_w(px(40.)).h(px(6.)).rounded_full().bg(t.muted).child(
                div().h_full().rounded_full().bg(color).w(relative(frac)),
            ),
        )
        .child(div().min_w(px(110.)).flex_shrink_0().text_right().whitespace_nowrap().figures().child(value_txt))
}

/// A settings-style "label: value" row: a 110px muted label, then the value.
/// Both sides share one line height, so a smaller value (a path in text_xs)
/// sits on the label's line instead of riding a few px above it. (Top-
/// aligned, not baseline: a switch or a button has no text baseline.)
pub fn field_row(label: &str, value: impl IntoElement, cx: &App) -> Div {
    const LINE: f32 = 20.;
    h_flex()
        .gap_3()
        .py_1()
        .items_start()
        .child(div().w(px(110.)).flex_shrink_0().text_sm().line_height(px(LINE)).text_color(cx.theme().muted_foreground).child(label.to_string()))
        .child(div().flex_1().min_w_0().text_sm().line_height(px(LINE)).child(value))
}

/// A labelled switch in a settings card: small like the buttons next to it
/// (the default one's text_base label was bigger than everything around
/// it), full size on a phone where it's tapped.
pub fn switch_size() -> gpui_kit::component::Size {
    use gpui_kit::component::Size;
    if crate::layout::compact() { Size::Medium } else { Size::Small }
}

// ---------- empty / loading ----------

pub fn empty(msg: &str, cx: &App) -> Div {
    empty_with(IconName::Inbox, msg, "", cx)
}

/// Centered icon + message + optional detail line.
pub fn empty_with(icon: IconName, msg: &str, detail: &str, cx: &App) -> Div {
    let t = cx.theme();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .py(px(48.))
        .text_color(t.muted_foreground)
        .child(
            div().size(px(40.)).rounded_full().bg(t.muted).flex().items_center().justify_center()
                .child(Icon::new(icon).small().text_color(t.muted_foreground)),
        )
        .child(div().mt_1().text_size(px(ts::TITLE)).text_color(t.foreground).child(msg.to_string()))
        .when(!detail.is_empty(), |d| d.child(div().text_size(px(ts::CAPTION)).max_w(px(460.)).text_center().child(detail.to_string())))
}

// ---------- motion ----------

/// The page's frame: a column, so the page stretches to the content width
/// (gpui's default flex-row would size it to its natural width and push wide
/// rows, four stat cards, past the edge instead of letting them wrap). No
/// enter transition: data that is ready is drawn at once (DESIGN.md, Motion).
/// `fill` makes it take the full height (for pages that manage their own scroll).
pub fn page_frame(child: impl IntoElement, fill: bool) -> Div {
    v_flex().w_full().min_w_0().when(fill, |d| d.h_full()).relative().child(child)
}

/// A dot that breathes while something is in progress.
pub fn pulse_dot(id: impl Into<ElementId>, color: Hsla, size: f32) -> impl IntoElement {
    dot(color, size).with_animation(
        id,
        Animation::new(Duration::from_millis(1200)).repeat().with_easing(pulsating_between(0.35, 1.)),
        |d, v| d.opacity(v),
    )
}

/// One-glyph braille spinner in `color`: ~10 frames a second, one text element.
pub fn braille_spinner(id: impl Into<ElementId>, color: Hsla, size: f32) -> impl IntoElement {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    div().text_color(color).text_size(px(size)).line_height(px(size)).font_family(crate::theme::BRAILLE_FONT).with_animation(
        id,
        Animation::new(Duration::from_millis(90 * FRAMES.len() as u64)).repeat(),
        |d, v| d.child(FRAMES[((v * FRAMES.len() as f32) as usize).min(FRAMES.len() - 1)]),
    )
}

/// The loading watermark: how long a load runs before it shows at all, and
/// the dot grid (logical px).
const FIELD_DELAY_MS: u128 = 400;
const FIELD_FADE_MS: u128 = 500;
const FIELD_STEP: f32 = 10.;
/// Half-axes (logical px) of the clear ellipse around the note at the centre.
const FIELD_HOLE: (f32, f32) = (190., 44.);

/// How a [`dot_field`] looks: the loading note's by default; the wizard's
/// welcome card asks for no delay, dimmer dots and a bigger clear ellipse.
#[derive(Clone, Copy)]
pub struct Field {
    /// logical ms before it shows
    pub delay_ms: u128,
    /// scales the dots' opacity
    pub strength: f32,
    /// half-axes (logical px) of the clear ellipse at the centre
    pub hole: (f32, f32),
}

impl Default for Field {
    fn default() -> Self {
        Field { delay_ms: FIELD_DELAY_MS, strength: 1., hole: FIELD_HOLE }
    }
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

/// How lit the dot at grid cell (`i`, `j`, 10 px apart) is at `t` seconds,
/// 0..=1: one travelling wave at ~25 degrees (260 px wavelength, a cycle in
/// 1.8 s) whose phase is warped by a slow, long cross wave (600 px), so the
/// crest lines run across the whole field as gently bent parallel bands, like
/// a cloth's folds. A faint second harmonic adds texture.
fn wave_intensity(i: f32, j: f32, t: f32) -> f32 {
    const A: (f32, f32) = (0.906, 0.423); // cos, sin of 25 degrees
    let warp = 1.5 * (0.1047 * (j * A.0 - i * A.1) + 1.3 * t).sin();
    let phase = 0.2417 * (i * A.0 + j * A.1) - 3.49 * t + warp;
    let z = (phase.sin() + 0.25 * (2. * phase + 0.8).sin()) / 1.25;
    (z * 0.5 + 0.5).powf(1.3)
}

fn paint_field(bounds: Bounds<Pixels>, started: Instant, look: Field, primary: Hsla, dark: bool, window: &mut Window) {
    // screenshots (?still=1): fully faded in, frozen at one instant, so the frame settles
    let still = crate::app::dev_aid("FINANCE_DESKTOP_STILL").is_some_and(|v| v == "1");
    let ms = if still { 2000 + look.delay_ms } else { started.elapsed().as_millis() };
    let fade = (ms.saturating_sub(look.delay_ms) as f32 / FIELD_FADE_MS as f32).min(1.);
    if !still {
        window.request_animation_frame();
    }
    if fade <= 0. {
        return;
    }
    let t = ms as f32 / 1000.;
    let raw = gpui_kit::px; // the step below is already zoomed
    let step = f32::from(px(FIELD_STEP));
    let unit = step / FIELD_STEP; // zoom
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (cols, rows) = ((w / step) as usize, (h / step) as usize);
    let (x0, y0) = (f32::from(bounds.origin.x) + (w - cols as f32 * step + step) / 2., f32::from(bounds.origin.y) + (h - rows as f32 * step + step) / 2.);
    for j in 0..rows {
        let ny = (j as f32 + 0.5) / rows as f32;
        for i in 0..cols {
            let nx = (i as f32 + 0.5) / cols as f32;
            let (dx, dy) = (nx * 2. - 1., ny * 2. - 1.);
            let vignette = (1. - (dx * dx + dy * dy) / 2.).max(0.);
            // clear around the note at the centre, so its backdrop blends in
            let (hx, hy) = (dx * w / 2. / (look.hole.0 * unit), dy * h / 2. / (look.hole.1 * unit));
            let hole = smooth(((hx * hx + hy * hy).sqrt() - 0.7) / 0.6);
            let lit = wave_intensity(i as f32, j as f32, t);
            let a = (0.12 + 0.73 * lit) * vignette * hole * fade * look.strength;
            if a < 0.01 {
                continue;
            }
            let r = (1.0 + 0.8 * lit) * unit; // lit dots grow a little, up to 1.8px; they never move
            // always the accent's own hue; across the diagonal and on the crests
            // only the lightness moves a little (lighter on dark, deeper on light)
            let g = ((nx + ny) / 2. + 0.5 * lit).min(1.);
            let l = (primary.l + if dark { 0.10 } else { -0.10 } * g).clamp(0., 1.);
            let color = hsla(primary.h, primary.s, l, a);
            let c = point(raw(x0 + i as f32 * step - r), raw(y0 + j as f32 * step - r));
            window.paint_quad(quad(Bounds::new(c, size(raw(2. * r), raw(2. * r))), raw(r), color, raw(0.), Hsla::transparent_black(), BorderStyle::default()));
        }
    }
}

/// A dotted-matrix watermark over the empty first-load page: a grid of faint
/// dots with a rolling cloth of them lighting up (the accent, a little lighter or
/// deeper across the diagonal, fading out toward the edges and around the centre
/// note). Blank for the first moments after `started` (a fast load never
/// shows it), then it fades in. Painted on a canvas: one quad per dot.
pub fn dot_field(started: Instant, cx: &App) -> impl IntoElement {
    dot_field_with(started, Field::default(), cx)
}

/// [`dot_field`] with its [`Field`] options.
pub fn dot_field_with(started: Instant, look: Field, cx: &App) -> impl IntoElement {
    let primary = cx.theme().primary;
    let dark = cx.theme().is_dark();
    canvas(|bounds, _, _| bounds, move |bounds, _, window, _| paint_field(bounds, started, look, primary, dark, window)).size_full()
}

/// A page as stacked sections. It draws as one column like `v_flex().gap(..)`
/// (`IntoElement`), but the app takes the parts instead and draws them as a
/// virtualized list: a scroll frame then lays out only the sections on
/// screen, not the whole page (taffy layout was most of a phone's frame).
pub struct Sections {
    gap: f32,
    items: Vec<AnyElement>,
}

impl Default for Sections {
    fn default() -> Self {
        Sections { gap: sp::GRID, items: Vec::new() }
    }
}

impl Sections {
    pub fn new() -> Self {
        Self::default()
    }

    /// Vertical space between sections (unzoomed px).
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    pub fn child(mut self, e: impl IntoElement) -> Self {
        self.items.push(e.into_any_element());
        self
    }

    pub fn children(mut self, it: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.items.extend(it.into_iter().map(IntoElement::into_any_element));
        self
    }

    /// (gap, sections)
    pub fn into_parts(self) -> (f32, Vec<AnyElement>) {
        (self.gap, self.items)
    }
}

impl IntoElement for Sections {
    type Element = Div;

    fn into_element(self) -> Div {
        v_flex().gap(px(self.gap)).children(self.items)
    }
}

#[cfg(test)]
mod braille_tests {
    use super::wave_intensity;

    #[test]
    fn wave_is_bounded_deterministic_and_moves() {
        let mut lit = 0;
        for t in [0., 0.5, 3.3, 120.] {
            for (i, j) in (0..60).flat_map(|i| (0..40).map(move |j| (i, j))) {
                let v = wave_intensity(i as f32, j as f32, t);
                assert!((0. ..=1.).contains(&v));
                assert_eq!(v, wave_intensity(i as f32, j as f32, t));
                lit += (v > 0.5) as usize;
            }
        }
        assert!(lit > 0, "some dots light up");
        let at = |t: f32| (0..60).map(|i| wave_intensity(i as f32, 7., t)).collect::<Vec<_>>();
        assert_ne!(at(0.), at(0.4));
    }
}
