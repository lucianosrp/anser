//! Settings → Supported sources: how to get each source's data in, from the
//! server's catalog (`GET /api/sources/catalog`, read by
//! `finance_core::supported`). Banks, Investments, Crypto, one card per
//! source: its logo, a format chip, a tick when the user already has it, and
//! the steps behind a disclosure. An API source's *Set up* opens its key form
//! (Settings → Data sources); a file source's *Upload* opens Upload.
//!
//! Loaded once and kept for the session (the catalog is static); a server
//! without the route (an older one) is told in words, never an empty page.
//! The same view on the desktop and in the browser.

use std::collections::BTreeSet;
use std::sync::Mutex;

use finance_core::capabilities::Capabilities;
use finance_core::client::Request;
use finance_core::supported::{self, Source};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::sources::{call, json_of};
use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

pub enum SupportedEvent {
    /// an API source's *Set up*: its key form
    SetUp(String),
    /// a file source's *Upload*
    Upload,
}

impl EventEmitter<SupportedEvent> for Supported {}

/// The catalog for the session; `demo` says whose it is (the demo's is
/// compiled in, a server's is its own).
static CACHE: Mutex<Option<(bool, Vec<Source>)>> = Mutex::new(None);

pub struct Supported {
    sources: Option<Vec<Source>>,
    error: Option<String>,
    loading: bool,
    caps: Capabilities,
    open: BTreeSet<String>,
    /// dev/screenshot aid: FINANCE_DESKTOP_SOURCES_OPEN=all, or keys (n26,ibkr)
    open_all: bool,
}

/// The numbered steps, as captions.
pub fn steps(how: &[String], cx: &App) -> Div {
    v_flex().gap(px(3.)).children(how.iter().enumerate().map(|(i, s)| ui::caption_wrap(format!("{}. {s}", i + 1), cx).text_color(cx.theme().foreground)))
}

/// Open a docs page in the browser.
pub fn docs_button(id: SharedString, url: String) -> Button {
    Button::new(id).xsmall().ghost().icon(IconName::ExternalLink).label("Official guide")
        .on_click(move |_, _, cx| cx.open_url(&url))
}

impl Supported {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let aid = crate::app::dev_aid("FINANCE_DESKTOP_SOURCES_OPEN").unwrap_or_default();
        let open_all = aid == "all";
        let open = if open_all { BTreeSet::new() } else { aid.split(',').filter(|k| !k.is_empty()).map(String::from).collect() };
        let mut v = Supported { sources: None, error: None, loading: false, caps: Capabilities::default(), open, open_all };
        v.load(cx);
        v
    }

    /// What the user has (a new load): the ticks follow.
    pub fn set_capabilities(&mut self, caps: Capabilities, cx: &mut Context<Self>) {
        if caps != self.caps {
            self.caps = caps;
            cx.notify();
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let demo = finance_core::demo::active();
        if let Some((d, list)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            if d == demo {
                self.sources = Some(list);
                return;
            }
        }
        // turned away: this would only hear 401 (the first render after signing in asks again)
        if self.loading || finance_core::signin::needed() {
            return;
        }
        self.loading = true;
        self.error = None;
        call(Request::new("GET", "/api/sources/catalog"), cx, move |this, r, _| {
            this.loading = false;
            match json_of(r) {
                Ok(v) => {
                    let list = supported::parse(&v);
                    if list.is_empty() {
                        this.error = Some("the server sent an empty list".into());
                    } else {
                        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((demo, list.clone()));
                        this.sources = Some(list);
                    }
                }
                Err(e) => this.error = Some(e),
            }
        });
    }

    fn card(&self, s: &Source, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let compact = crate::layout::compact();
        let open = self.open_all || self.open.contains(&s.key);
        let have = supported::have(&self.caps, &s.key);
        let key = s.key.clone();
        let kind = match s.kind.as_str() {
            "bank" => "Bank account",
            "card" => "Credit card",
            "fund_units" => "Fund orders",
            "crypto_wallet" => "Crypto wallet",
            "exchange" => "Exchange",
            "broker" => "Broker",
            _ => "",
        };
        let head = h_flex().id(SharedString::from(format!("sup-{key}"))).w_full().gap_3().items_center().cursor_pointer()
            .min_h(px(if compact { crate::layout::TOUCH } else { 36. }))
            .when_some(crate::logo::tile(crate::logo::for_source(&s.key), 28., cx), |d, l| d.child(l))
            .child(v_flex().flex_1().min_w_0().gap(px(2.))
                .child(div().text_sm().font_semibold().overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.label.clone()))
                .child(h_flex().gap_2().items_center().flex_wrap()
                    .when(!s.format.is_empty(), |d| d.child(ui::tag(s.format.clone(), cx)))
                    .when(!kind.is_empty(), |d| d.child(ui::caption(kind, cx)))))
            .when(have, |d| d.child(h_flex().gap_1().items_center().flex_shrink_0()
                .child(Icon::new(IconName::Check).small().text_color(fmt::money_in()))
                .child(div().text_size(px(ts::CAPTION)).text_color(fmt::money_in()).child("You have this"))))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small().text_color(t.muted_foreground))
            .on_click(cx.listener(move |this, _, _, cx| {
                // "all" (a dev aid) is every card open: the first click closes it
                if this.open_all {
                    this.open_all = false;
                    this.open = this.sources.iter().flatten().map(|s| s.key.clone()).collect();
                }
                if !this.open.remove(&key) {
                    this.open.insert(key.clone());
                }
                cx.notify();
            }));
        let mut card = ui::card(cx).gap_2().child(head);
        if open {
            let key = s.key.clone();
            card = card
                .child(steps(&s.how, cx).pl(px(2.)))
                .when(!s.notes.is_empty(), |d| d.child(ui::caption_wrap(s.notes.clone(), cx)))
                .when_some(s.refuses.clone(), |d, r| d.child(ui::caption_wrap(format!("Refused: {r}."), cx)))
                .child(h_flex().gap_2().items_center().flex_wrap().pt_1()
                    .child(if s.api {
                        let k = key.clone();
                        Button::new(SharedString::from(format!("sup-set-{key}"))).small().primary().label("Set up")
                            .when(compact, |b| b.large())
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SupportedEvent::SetUp(k.clone()))))
                    } else {
                        Button::new(SharedString::from(format!("sup-up-{key}"))).small().outline().icon(IconName::ArrowUp).label("Upload")
                            .when(compact, |b| b.large())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SupportedEvent::Upload)))
                    })
                    .when_some(s.docs_url.clone(), |d, u| d.child(docs_button(SharedString::from(format!("sup-doc-{key}")), u))));
        }
        card
    }

    fn missing(&self, cx: &mut Context<Self>) -> Div {
        let card = ui::card(cx).gap_2();
        match self.error.clone() {
            Some(e) => card
                .child(ui::caption_wrap(format!("Couldn't load the list of sources: {e}. An older server may not have it; update the server and try again."), cx).text_color(fmt::money_out()))
                .child(h_flex().child(Button::new("sup-retry").small().ghost().label("Try again")
                    .on_click(cx.listener(|this, _, _, cx| { this.error = None; this.load(cx); cx.notify(); })))),
            None if finance_core::signin::needed() => card.child(ui::caption_wrap("Sign in first (Settings, Account) to see the list.", cx)),
            None => card.child(ui::caption("Loading the list…", cx)),
        }
    }
}

impl Render for Supported {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sources.is_none() && self.error.is_none() && !self.loading {
            self.load(cx);
        }
        let Some(list) = self.sources.clone() else { return self.missing(cx) };
        let compact = crate::layout::compact();
        let mut page = v_flex().w_full().gap(px(ui::sp::L))
            .child(ui::caption_wrap("How to get each source's data in. A tick marks the ones you already have.", cx));
        for (group, sources) in supported::grouped(&list) {
            let n = sources.len();
            let cards: Vec<Div> = sources.into_iter().map(|s| self.card(s, cx)).collect();
            page = page.child(v_flex().gap_2()
                .child(ui::section(group.title(), &format!("{n} source{}", if n == 1 { "" } else { "s" }), cx))
                .child(div().grid().grid_cols(if compact { 1 } else { 2 }).gap_2().w_full().items_start().children(cards)));
        }
        let map_where = if cfg!(target_family = "wasm") { "the desktop app's Upload page" } else { "the Upload page" };
        page.child(v_flex().gap_2()
            .child(ui::section("Your bank isn't here?", "any table-shaped export works", cx))
            .child(ui::card(cx).gap_2()
                .child(ui::caption_wrap(format!("Any bank's CSV or Excel export is read once its columns are mapped: drop it on {map_where} and choose Map columns. \
                    A format file (TOML) teaches the app a bank for good: see desktop/docs/formats.md in the repository."), cx))
                .child(h_flex().child(Button::new("sup-upload").small().outline().icon(IconName::ArrowUp).label("Open Upload")
                    .when(compact, |b| b.large())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SupportedEvent::Upload)))))))
    }
}
