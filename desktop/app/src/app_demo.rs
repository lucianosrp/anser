//! "Just try it": the demo account (finance_core::demo), from the window's
//! side. The wizard's first card, the no-data page and the avatar menu
//! start it; a pill in the header says it is on and leads back out ("Set
//! up for real"), as does the avatar menu ("Leave the demo").
//!
//! * Desktop: the household is built on this machine
//!   (finance_engine::demo, a fraction of a second, off the UI thread) into
//!   the demo profile's own folder; then the app restarts on that profile,
//!   like any switch of account. A copy older than [`STALE`] is rebuilt
//!   at start, so its months stay current. Leaving deletes the profile and
//!   its copy and restarts on the account in use before, or on the wizard
//!   when that one has nothing to show yet.
//! * Browser: a navigation to the page with `?demo=1` (the entry point
//!   then loads `/api/demo/*`), and back without it.

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::FinanceApp;
use crate::ui;
use crate::zoom::px;

/// A demo copy older than this is built again at start (seconds).
#[cfg(not(target_family = "wasm"))]
const STALE: u64 = 6 * 3600;

pub use finance_core::demo::active;

/// Build the demo's copy into its profile folder, as of today.
#[cfg(not(target_family = "wasm"))]
pub fn rebuild() -> anyhow::Result<()> {
    finance_engine::demo::write(&finance_core::profiles::dir_of(finance_core::demo::ID), finance_engine::config::utc_today())
}

/// At start, before the copy is read: a missing or stale demo copy is built again.
#[cfg(not(target_family = "wasm"))]
pub fn refresh_if_stale() {
    if !active() || finance_core::cache::age_secs().is_some_and(|a| a <= STALE) {
        return;
    }
    if let Err(e) = rebuild() {
        eprintln!("anser: the demo couldn't be rebuilt: {e:#}");
    }
}

/// Open the demo. Desktop: build it, switch to it, restart (`failed` gets
/// the error if any of that fails). Browser: a navigation.
pub fn start(cx: &mut App, failed: impl FnOnce(String, &mut App) + 'static) {
    #[cfg(not(target_family = "wasm"))]
    {
        let task = cx.background_spawn(async move {
            rebuild()?;
            finance_core::profiles::enter_demo()
        });
        cx.spawn(async move |cx| match task.await {
            Ok(()) => super::accounts_menu::relaunch_with(false),
            Err(e) => {
                cx.update(|cx| failed(format!("The demo couldn't be made: {e:#}"), cx));
            }
        })
        .detach();
    }
    #[cfg(target_family = "wasm")]
    {
        if !finance_core::demo::enter_web() {
            failed("This build can't open the demo.".into(), cx);
        }
    }
}

/// Leave the demo. `setup`: straight to the wizard (Set up for real);
/// otherwise back to the account in use before (the wizard if it has no data).
pub fn leave(setup: bool) {
    #[cfg(not(target_family = "wasm"))]
    {
        let has_copy = finance_core::profiles::leave_demo().unwrap_or(false);
        super::accounts_menu::relaunch_with(setup || !has_copy);
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = setup;
        finance_core::demo::leave_web();
    }
}

impl FinanceApp {
    /// The header's demo pill: what this is, and the way out.
    pub(super) fn demo_pill(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let out = Button::new("demo-real")
            .small()
            .when(compact, |b| b.ghost().label("Demo"))
            .when(!compact, |b| b.outline().label("Set up for real"))
            .tooltip("Made-up data. Set up your own: a server, then your accounts.")
            .on_click(|_, _, _| leave(true));
        h_flex()
            .id("demo-pill")
            .flex_shrink_0()
            .gap_2()
            .items_center()
            .when(!compact, |d| {
                d.child(
                    h_flex()
                        .gap(px(6.))
                        .items_center()
                        .h(px(26.))
                        .px(px(10.))
                        .rounded_full()
                        .border_1()
                        .border_color(ui::hairline(cx))
                        .text_size(px(12.))
                        .text_color(t.muted_foreground)
                        .child(ui::dot(t.warning, 7.))
                        .child(div().whitespace_nowrap().child(finance_core::demo::LABEL)),
                )
            })
            .child(out)
            .into_any_element()
    }

    /// A page that only means something with a real setup (the Doctor),
    /// while the demo is on.
    pub(super) fn render_demo_only(&self, what: &str, cx: &mut Context<Self>) -> AnyElement {
        ui::empty_with(gpui_kit::component::IconName::Info, &format!("{what} checks a real setup"), finance_core::demo::NOTE, cx)
            .child(div().mt_2().child(Button::new("demo-only-real").small().primary().label("Set up for real").on_click(|_, _, _| leave(true))))
            .into_any_element()
    }
}
