//! The responsive breakpoint, and the compact (phone) chrome.
//!
//! Below [`COMPACT_BELOW`] logical px of window width (a phone in the web
//! build, a very narrow desktop window) the app is "compact":
//! * the sidebar gives way to a bottom tab bar ([`tab_bar`]): the four most
//!   used pages plus "More", a bottom sheet with every page
//! * the page gutter shrinks, the header keeps the title and icon buttons
//! * pages stack their tiles in one column and show lists instead of wide
//!   tables (they ask [`compact`])
//! * touch targets are at least [`TOUCH`] px tall
//!
//! The window decides it once per frame ([`update`], first thing in the
//! app's render), so every page render sees the same answer without a
//! `&Window` of its own. The desktop at any width >= the breakpoint renders
//! exactly as before.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use gpui_kit::component::{ActiveTheme as _, Placement, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::pages::Page;
use crate::zoom::px;

/// Narrower than this (logical px at zoom 1) is compact.
pub const COMPACT_BELOW: f32 = 600.;
/// Minimum height of a tappable row or tab in compact mode.
pub const TOUCH: f32 = 44.;
/// Height of the bottom tab bar.
pub const TAB_BAR: f32 = 58.;
/// The page gutter: compact, regular.
pub const GUTTER: (f32, f32) = (12., 24.);

static COMPACT: AtomicBool = AtomicBool::new(false);
/// a touch screen without a mouse (the web build on a phone or tablet)
static TOUCH_ONLY: AtomicBool = AtomicBool::new(false);
/// the window's width this frame (f32 bits, unzoomed window px)
static WIDTH: AtomicU32 = AtomicU32::new(0);
/// the system's safe area this frame, top and bottom (f32 bits, window px)
static SAFE_TOP: AtomicU32 = AtomicU32::new(0);
static SAFE_BOTTOM: AtomicU32 = AtomicU32::new(0);

/// Decide compact for this frame from the window's width. The app's px
/// are zoomed, so the breakpoint is too.
pub fn update(window: &Window) -> bool {
    let w = f32::from(window.viewport_size().width);
    let c = w < COMPACT_BELOW * crate::zoom::get();
    COMPACT.store(c, Ordering::Relaxed);
    WIDTH.store(w.to_bits(), Ordering::Relaxed);
    // a phone's notch / status bar and home indicator (the web page asks
    // for viewport-fit=cover, so the canvas runs under them); zero on the
    // desktop. An open keyboard shrinks the visible area much more than a
    // home indicator: that's not a safe area, the bar may go under it.
    let full = window.viewport_size();
    let vis = window.fully_visible_bounds();
    let top = f32::from(vis.origin.y).clamp(0., 80.);
    let bottom = f32::from(full.height - vis.origin.y - vis.size.height).max(0.);
    SAFE_TOP.store(top.to_bits(), Ordering::Relaxed);
    SAFE_BOTTOM.store(if bottom > 60. { 0f32 } else { bottom }.to_bits(), Ordering::Relaxed);
    c
}

/// The safe area above the content this frame (unzoomed window px).
pub fn safe_top() -> Pixels {
    gpui_kit::px(f32::from_bits(SAFE_TOP.load(Ordering::Relaxed)))
}

/// The safe area below the content (the home indicator) this frame.
pub fn safe_bottom() -> Pixels {
    gpui_kit::px(f32::from_bits(SAFE_BOTTOM.load(Ordering::Relaxed)))
}

/// Is this frame compact? (see [`update`])
pub fn compact() -> bool {
    COMPACT.load(Ordering::Relaxed)
}

/// The device has no keyboard-and-mouse pointer to speak of: the web build
/// sets it at start from `(pointer: coarse) and (hover: none)`.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))] // the web build sets it
pub fn set_touch_only(on: bool) {
    TOUCH_ONLY.store(on, Ordering::Relaxed);
}

/// Keyboard shortcuts mean something here: not on a phone layout or a
/// touch-only device. Key hints, kbd caps and "(Ctrl+X)" in tooltips ask
/// this (through `ui::show_shortcuts`).
pub fn has_keyboard() -> bool {
    !compact() && !TOUCH_ONLY.load(Ordering::Relaxed)
}

/// The page gutter for this frame.
pub fn gutter() -> f32 {
    if compact() { GUTTER.0 } else { GUTTER.1 }
}

/// Minimum width of a side-by-side page column (`flex_1().min_w(col(420.))`).
/// Compact caps it under half a phone's width, so the columns still wrap
/// one per row but none is wider than the screen.
pub fn col(regular: f32) -> Pixels {
    px(if compact() { regular.min(280.) } else { regular })
}

/// Pages on the tab bar; everything else is under "More".
pub const TABS: [Page; 4] = [Page::Overview, Page::Spending, Page::Transactions, Page::Investments];

pub type OnGo = Rc<dyn Fn(Page, &mut Window, &mut App)>;

fn short_label(p: Page) -> &'static str {
    match p {
        Page::Transactions => "Activity",
        Page::Investments => "Invest",
        Page::Review => "Year",
        Page::Subscriptions => "Subs",
        p => p.label(),
    }
}

/// The bottom tab bar: [`TABS`] and "More" (every page, in a bottom sheet).
pub fn tab_bar(active: Page, on_go: OnGo, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let more_active = !TABS.contains(&active);
    let tab = |id: SharedString, icon: gpui_kit::component::Icon, label: &'static str, on: bool| {
        v_flex()
            .id(id)
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .gap(px(3.))
            .cursor_pointer()
            .text_size(px(10.5))
            .text_color(if on { t.primary } else { t.muted_foreground })
            .when(on, |d| d.font_weight(FontWeight::MEDIUM))
            .child(icon.size(px(20.)).text_color(if on { t.primary } else { t.muted_foreground }))
            .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(label))
    };
    let mut bar = h_flex()
        .id("tab-bar")
        .flex_shrink_0()
        .w_full()
        .h(px(TAB_BAR) + safe_bottom())
        .px(px(4.))
        .pb(safe_bottom())
        .bg(t.sidebar)
        .border_t_1()
        .border_color(t.sidebar_border);
    for page in TABS.into_iter().filter(|p| p.visible()) {
        let go = on_go.clone();
        bar = bar.child(
            tab(format!("tab-{}", page.label()).into(), page.icon(), short_label(page), page == active)
                .on_click(move |_, window, cx| go(page, window, cx)),
        );
    }
    let go = on_go.clone();
    bar.child(
        tab("tab-more".into(), gpui_kit::component::Icon::new(gpui_kit::component::IconName::Menu),
            if more_active { short_label(active) } else { "More" }, more_active)
            .on_click(move |_, window, cx| open_more(active, go.clone(), window, cx)),
    )
}

/// "More": every page as a big tappable row, in a sheet from the bottom.
pub fn open_more(active: Page, on_go: OnGo, window: &mut Window, cx: &mut App) {
    let rows = (Page::ALL.iter().filter(|p| p.visible()).count() as f32 * (TOUCH + 2.) + 90.).min(f32::from(window.viewport_size().height) / crate::zoom::get() * 0.85);
    window.open_sheet_at(Placement::Bottom, cx, move |sheet, _, cx| {
        let t = cx.theme();
        let mut list = v_flex().gap(px(2.)).pb(px(12.));
        for page in Page::ALL.into_iter().filter(|p| p.visible()) {
            let go = on_go.clone();
            let on = page == active;
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("more-{}", page.label())))
                    .h(px(TOUCH))
                    .px(px(12.))
                    .gap(px(14.))
                    .rounded(t.radius)
                    .cursor_pointer()
                    .text_size(px(15.))
                    .when(on, |d| d.bg(t.sidebar_accent).font_weight(FontWeight::MEDIUM))
                    .child(page.icon().size(px(20.)).text_color(if on { t.primary } else { t.muted_foreground }))
                    .child(page.label())
                    .on_click(move |_, window, cx| {
                        window.close_sheet(cx);
                        go(page, window, cx);
                    }),
            );
        }
        sheet.title("Go to").size(px(rows)).resizable(false).child(list)
    });
}

/// A side sheet's width: the whole window when compact, else `regular`.
pub fn sheet_width(regular: f32) -> Pixels {
    if compact() { gpui_kit::px(f32::from_bits(WIDTH.load(Ordering::Relaxed))) } else { px(regular) }
}
