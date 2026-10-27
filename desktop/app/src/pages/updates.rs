//! Updates (Settings, General): whether a newer release of this app exists,
//! and a one-click update (finance_core::selfupdate does the work). Never
//! updates without the click; checks GitHub once on start (after the first
//! data load) and then at most daily, silently, unless turned off.
//! A development build is never offered anything and says so.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use finance_core::selfupdate::{self, Applied, Release, Stage};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::ui;

#[derive(Clone, PartialEq)]
enum State {
    /// nothing asked yet (or checks are off)
    Unknown,
    Checking,
    UpToDate,
    Available(Release),
    Working(Stage),
    /// installed: restart to run it
    Ready(String),
    /// the install directory isn't ours to write: the command instead
    Manual(Release, String),
}

pub struct Updates {
    state: State,
    /// when GitHub last answered (ms)
    checked_ms: Option<i64>,
    /// the last failure, plain words (shown here only)
    error: Option<String>,
    on: bool,
    started: bool,
}

fn stage_code(s: Stage) -> u8 {
    s as u8
}

fn stage_of(c: u8) -> Stage {
    match c {
        0 => Stage::Downloading,
        1 => Stage::Verifying,
        _ => Stage::Installing,
    }
}

/// Plain words for a failed check or download.
fn plain(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    s.chars().take(200).collect()
}

impl Updates {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        let mut u = Updates { state: State::Unknown, checked_ms: None, error: None, on: selfupdate::checks_enabled(), started: false };
        // what the last check found, without asking again
        if let Some(c) = selfupdate::cached() {
            u.take(c);
        }
        u
    }

    fn take(&mut self, c: selfupdate::Checked) {
        self.checked_ms = Some(c.at_ms);
        self.error = None;
        self.state = match selfupdate::offer(&selfupdate::running(), c.latest.as_ref()) {
            Some(r) => State::Available(r.clone()),
            None => State::UpToDate,
        };
    }

    /// After the first data load: look once now (the cache answers while
    /// it's under a day old), then hourly, which asks GitHub at most daily.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        self.check(false, cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3600)).await;
                if this.update(cx, |this, cx| this.check(false, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Ask (the cache when it's fresh and `force` is off). Silent about
    /// failures except in Settings' row.
    pub fn check(&mut self, force: bool, cx: &mut Context<Self>) {
        let busy = matches!(self.state, State::Checking | State::Working(_) | State::Ready(_));
        if busy || (!self.on && !force) || !selfupdate::supported() || !selfupdate::is_release_build(&selfupdate::running()) {
            return;
        }
        let before = std::mem::replace(&mut self.state, State::Checking);
        cx.notify();
        let task = cx.background_spawn(async move { selfupdate::check(force) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(Some(c)) => this.take(c),
                    Ok(None) => this.state = before,
                    Err(e) => {
                        this.state = if matches!(before, State::Checking) { State::Unknown } else { before };
                        this.error = Some(plain(&e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_now(&mut self, cx: &mut Context<Self>) {
        let State::Available(r) = self.state.clone() else { return };
        self.state = State::Working(Stage::Downloading);
        self.error = None;
        cx.notify();
        let stage = Arc::new(AtomicU8::new(0));
        let seen = stage.clone();
        let rel = r.clone();
        let task = cx.background_spawn(async move { selfupdate::apply(&rel, |s| seen.store(stage_code(s), Ordering::Relaxed)) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(Applied::Done) => this.state = State::Ready(r.tag.clone()),
                    Ok(Applied::Manual(cmd)) => this.state = State::Manual(r, cmd),
                    Err(e) => {
                        this.state = State::Available(r);
                        this.error = Some(plain(&e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        // the stage the worker is at, while it works
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let s = stage_of(stage.load(Ordering::Relaxed));
                let more = this.update(cx, |this, cx| {
                    if !matches!(this.state, State::Working(_)) {
                        return false;
                    }
                    if this.state != State::Working(s) {
                        this.state = State::Working(s);
                        cx.notify();
                    }
                    true
                });
                if !more.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    fn set_on(&mut self, on: bool, cx: &mut Context<Self>) {
        self.on = on;
        if let Err(e) = selfupdate::set_checks_enabled(on) {
            self.error = Some(format!("couldn't save the setting: {e}"));
        }
        if on {
            self.check(false, cx);
        }
        cx.notify();
    }
}

/// Settings, General: the Updates card. None where the app can't update
/// itself (no desktop build for this platform).
pub fn settings_card(entity: &Entity<Updates>, cx: &App) -> Option<AnyElement> {
    if !selfupdate::supported() {
        return None;
    }
    let u = entity.read(cx);
    let t = cx.theme();
    let dev = !selfupdate::is_release_build(&selfupdate::running());
    let muted = |s: String| div().text_xs().text_color(t.muted_foreground).child(s);
    let w = entity.downgrade();
    let notes = |url: String| {
        Button::new("update-notes").small().ghost().label("Release notes")
            .on_click(move |_, _, cx| cx.open_url(&url))
    };

    let status = match &u.state {
        _ if dev => muted("Development build: updates are offered to released builds only.".into()).into_any_element(),
        State::Unknown if !u.on => muted("Checks are off.".into()).into_any_element(),
        State::Unknown => muted("Not checked yet.".into()).into_any_element(),
        State::Checking => muted("Checking…".into()).into_any_element(),
        State::UpToDate => h_flex().gap_2().items_center().flex_wrap()
            .child(div().text_color(fmt::money_in()).child("Up to date"))
            .children(u.checked_ms.map(|ms| muted(format!("checked {}", fmt::ago(ms)))))
            .into_any_element(),
        State::Available(r) => {
            let w = w.clone();
            h_flex().gap_2().items_center().flex_wrap()
                .child(div().child(format!("{} available", r.tag)))
                .child(Button::new("update-now").small().primary().icon(IconName::ArrowDown).label("Update")
                    .on_click(move |_, _, cx| { let _ = w.update(cx, |u, cx| u.update_now(cx)); }))
                .child(notes(r.url.clone()))
                .into_any_element()
        }
        State::Working(s) => h_flex().gap_2().items_center()
            .child(Button::new("update-busy").small().outline().loading(true).disabled(true)
                .label(match s {
                    Stage::Downloading => "Downloading…",
                    Stage::Verifying => "Verifying…",
                    Stage::Installing => "Installing…",
                }))
            .into_any_element(),
        State::Ready(tag) => h_flex().gap_2().items_center().flex_wrap()
            .child(div().child(format!("{tag} installed. Restart to finish.")))
            .child(Button::new("update-restart").small().primary().label("Restart now")
                .on_click(|_, _, _| selfupdate::relaunch()))
            .into_any_element(),
        State::Manual(r, cmd) => v_flex().gap_1()
            .child(div().child(format!("{} is available, but this install can't be changed without admin rights. Run:", r.tag)))
            .child(div().text_xs().font_family("monospace").p_2().rounded(t.radius).bg(t.background).child(cmd.clone()))
            .child(h_flex().child(notes(r.url.clone())))
            .into_any_element(),
    };

    let busy = matches!(u.state, State::Checking | State::Working(_) | State::Ready(_));
    let w_switch = entity.downgrade();
    let w_check = entity.downgrade();
    Some(
        ui::card(cx)
            .gap_2()
            .child(ui::field_row("Version", div().text_xs().text_color(t.muted_foreground).child(selfupdate::running()), cx))
            .child(ui::field_row("Status", status, cx))
            .when_some(u.error.clone(), |c, e| c.child(div().text_xs().text_color(fmt::money_out()).child(e)))
            .child(Switch::new("updates-switch").with_size(ui::switch_size())
                .checked(u.on)
                .label("Look for updates (once a day)")
                .on_click(move |on: &bool, _, cx| {
                    let on = *on;
                    let _ = w_switch.update(cx, |u, cx| u.set_on(on, cx));
                }))
            .when(!dev, |c| c.child(h_flex().child(Button::new("update-check").small().outline().icon(IconName::RefreshCw).label("Check now")
                .disabled(busy)
                .on_click(move |_, _, cx| { let _ = w_check.update(cx, |u, cx| u.check(true, cx)); }))))
            .child(div().text_xs().text_color(t.muted_foreground)
                .child("Asks GitHub for the latest release, never more than once a day. Nothing is installed until you click Update; the download is checked against the release's SHA256SUMS."))
            .into_any_element(),
    )
}
