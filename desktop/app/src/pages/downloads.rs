//! The Downloads watcher (opt-in, Settings): on start and whenever the
//! window gains focus, look in the downloads folder for bank exports the
//! server doesn't have yet (`crate::downloads`), and say so in a small
//! banner at the bottom right: *Import all* (the Upload page's normal flow),
//! *Review* (the Upload page with the files parsed and waiting) or
//! *Dismiss* (remembered: those files aren't suggested again).
//!
//! Native only: the web build has no filesystem (`downloads_web.rs`).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use finance_core::cache;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::downloads::{self, Config, Found, Scanner};
use crate::fmt;
use crate::ui::{self, sp, ts};
use crate::zoom::px;

pub enum DownloadsEvent {
    /// send these through the Upload page now
    ImportAll(Vec<PathBuf>),
    /// show them on the Upload page, parsed, waiting for the user
    Review(Vec<PathBuf>),
}

pub struct Downloads {
    cfg: Config,
    found: Vec<Found>,
    scanning: bool,
    last_scan: Option<Instant>,
    scanner: Arc<Mutex<Scanner>>,
    /// imported or reviewed this session: not suggested again while the
    /// server catches up
    handled: BTreeSet<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DownloadsEvent> for Downloads {}

/// A focus within this long of the last scan doesn't scan again.
const RESCAN: Duration = Duration::from_secs(30);
const LISTED: usize = 3;

impl Downloads {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.scan(false, cx);
            }
        })];
        let mut this = Downloads {
            cfg: Config::load(),
            found: vec![],
            scanning: false,
            last_scan: None,
            scanner: Arc::new(Mutex::new(Scanner::default())),
            handled: BTreeSet::new(),
            _subs: subs,
        };
        this.scan(true, cx);
        this
    }

    fn scan(&mut self, force: bool, cx: &mut Context<Self>) {
        self.cfg = Config::load();
        if !self.cfg.enabled() {
            self.found.clear();
            cx.notify();
            return;
        }
        if self.scanning || (!force && self.last_scan.is_some_and(|t| t.elapsed() < RESCAN)) {
            return;
        }
        let Some(dir) = self.cfg.dir() else { return };
        self.scanning = true;
        self.last_scan = Some(Instant::now());
        let (scanner, server, replica) = (self.scanner.clone(), cache::server_url(), cache::db_path());
        let (days, dismissed) = (self.cfg.days(), self.cfg.dismissed.clone());
        let task = cx.background_spawn(async move {
            let known = crate::upload::known_hashes(&server, &replica);
            let mut s = scanner.lock().unwrap_or_else(|p| p.into_inner());
            s.scan(&dir, days, SystemTime::now(), &known, &dismissed)
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |this, cx| {
                this.scanning = false;
                this.found = found.into_iter().filter(|f| !this.handled.contains(&f.md5)).collect();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn take(&mut self) -> Vec<PathBuf> {
        let found = std::mem::take(&mut self.found);
        self.handled.extend(found.iter().map(|f| f.md5.clone()));
        found.into_iter().map(|f| f.path).collect()
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        let mut cfg = Config::load();
        for f in std::mem::take(&mut self.found) {
            if !cfg.dismissed.contains(&f.md5) {
                cfg.dismissed.push(f.md5);
            }
        }
        let _ = cfg.save();
        self.cfg = cfg;
        cx.notify();
    }

    fn update_cfg(&mut self, f: impl FnOnce(&mut Config), cx: &mut Context<Self>) {
        let mut cfg = Config::load();
        f(&mut cfg);
        let _ = cfg.save();
        self.cfg = cfg;
        self.last_scan = None;
        self.scan(true, cx);
    }
}

impl Render for Downloads {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.found.is_empty() || !self.cfg.enabled() {
            return div().into_any_element();
        }
        let t = cx.theme().clone();
        let dir = self.cfg.dir().unwrap_or_default();
        let n = self.found.len();
        let names = self.found.iter().take(LISTED).map(|f| {
            h_flex()
                .gap(px(6.))
                .min_w_0()
                .child(Icon::new(IconName::FileText).xsmall().text_color(t.muted_foreground))
                .child(div().flex_1().min_w_0().text_size(px(ts::CAPTION)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(f.name.clone()))
                .child(div().flex_shrink_0().text_size(px(11.)).text_color(t.muted_foreground).child(f.label.clone()))
        });
        let more = (n > LISTED).then(|| ui::caption(format!("and {} more", fmt::count((n - LISTED) as f64)), cx));
        v_flex()
            .id("downloads-banner")
            .absolute()
            .bottom(px(16.))
            .right(px(16.))
            .w(px(360.))
            .max_w(relative(0.9))
            .gap(px(sp::S))
            .p(px(sp::M))
            .rounded(t.radius_lg)
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .occlude()
            .child(
                h_flex()
                    .gap(px(sp::S))
                    .child(
                        div()
                            .size(px(26.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(t.primary.opacity(0.14))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::ArrowDown).small().text_color(t.primary)),
                    )
                    .child(div().flex_1().text_size(px(ts::BODY)).font_semibold().child(downloads::headline(n, &dir))),
            )
            .child(v_flex().gap(px(3.)).children(names).children(more))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("dl-dismiss").small().ghost().label("Dismiss").tooltip("Don't suggest these files again")
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))))
                    .child(Button::new("dl-review").small().outline().label("Review")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let paths = this.take();
                            cx.emit(DownloadsEvent::Review(paths));
                            cx.notify();
                        })))
                    .child(Button::new("dl-import").small().primary().icon(IconName::ArrowUp).label("Import all")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let paths = this.take();
                            cx.emit(DownloadsEvent::ImportAll(paths));
                            cx.notify();
                        }))),
            )
            .into_any_element()
    }
}

/// Settings → Downloads: the opt-in, the folder, the window, what it found.
pub fn settings_card(entity: &Entity<Downloads>, cx: &App) -> Option<AnyElement> {
    let d = entity.read(cx);
    let t = cx.theme();
    let on = d.cfg.enabled();
    let days = d.cfg.days();
    let dir = d.cfg.dir().map(|p| p.display().to_string()).unwrap_or_else(|| "no downloads folder on this system".into());
    let dismissed = d.cfg.dismissed.len();
    let status = if !on {
        "Off. Nothing is scanned.".to_string()
    } else if d.scanning {
        "Scanning…".to_string()
    } else if d.found.is_empty() {
        "Nothing new there.".to_string()
    } else {
        format!("{} new: see the banner.", fmt::count(d.found.len() as f64))
    };
    let w = entity.downgrade();
    let toggle = Switch::new("downloads-switch").with_size(ui::switch_size())
        .checked(on)
        .label("Suggest new bank exports from Downloads")
        .on_click(move |on: &bool, _, cx| {
            let on = *on;
            let _ = w.update(cx, |d, cx| d.update_cfg(|c| c.enabled = on, cx));
        });
    let day_btn = |n: u32| {
        let w = entity.downgrade();
        Button::new(("dl-days", n as usize))
            .xsmall()
            .label(format!("{n} days"))
            .map(|b| if days == n { b.primary() } else { b.outline() })
            .on_click(move |_, _, cx| {
                let _ = w.update(cx, |d, cx| d.update_cfg(|c| c.days = Some(n), cx));
            })
    };
    let w_scan = entity.downgrade();
    let w_forget = entity.downgrade();
    let label = |s: &str| div().w(px(110.)).flex_shrink_0().text_sm().text_color(t.muted_foreground).child(s.to_string());
    Some(
        ui::card(cx)
            .gap_2()
            .child(toggle)
            .child(h_flex().gap_3().items_start().child(label("Folder"))
                .child(div().flex_1().min_w_0().text_xs().text_color(t.muted_foreground).overflow_hidden().text_ellipsis().child(dir)))
            .child(h_flex().gap_3().child(label("Modified in")).child(h_flex().gap_1().child(day_btn(7)).child(day_btn(30)).child(day_btn(90))))
            .child(h_flex().gap_3().child(label("Status")).child(div().text_sm().child(status)))
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("dl-scan").small().outline().icon(IconName::RefreshCw).label("Scan now").disabled(!on)
                    .on_click(move |_, _, cx| {
                        let _ = w_scan.update(cx, |d, cx| d.scan(true, cx));
                    }))
                .when(dismissed > 0, |r| r.child(Button::new("dl-forget").small().ghost()
                    .label(format!("Forget {} dismissed", fmt::count(dismissed as f64)))
                    .on_click(move |_, _, cx| {
                        let _ = w_forget.update(cx, |d, cx| d.update_cfg(|c| c.dismissed.clear(), cx));
                    }))))
            .child(div().text_xs().text_color(t.muted_foreground).child(
                "On start and when the window gains focus: files a known bank or format reads, that the server doesn't have yet. \
                 Read here only; nothing is sent until you import. Saved in downloads.toml.",
            ))
            .into_any_element(),
    )
}
