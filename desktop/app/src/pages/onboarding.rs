//! The first-run wizard (finance_core::onboarding has the steps and their
//! rules), on the desktop and in the browser. It opens by itself on an empty
//! server (no data, no configured source, onboarding not done), from
//! Settings ("Run setup again"), and on the sign-in step whenever the server
//! turns this device away (a 401, [`Onboarding::signing_in`]).
//!
//! It owns the whole window while it's open (the app draws nothing else: no
//! sidebar, header, tab bar or banners): the mark, one centred card per
//! step, and a row of dots under it (a tooltip names each, a caption counts
//! what's left). Each card is a title, one sentence and at most three
//! fields; what's optional sits behind "More options", and a longer
//! explanation behind "Why?". One primary button (Enter), Back as a text
//! button (Esc), Skip where the step can be left undone, and an X in the
//! corner when it was opened from the app rather than by itself. On the
//! phone the card takes the width and the buttons stack, full width,
//! anchored at the bottom.
//!
//! A welcome card comes first (the dotted watermark of the loading page
//! behind the name, "Get started" or "Just try it"; it then stays, dimmer,
//! behind every step). Then
//! where (desktop: this machine's server, or one elsewhere) → sign in ("Do
//! you have a pairing code?" Yes: the code; No: a new account, the first on
//! the server, by invite, or open signup) → your name → currency and home →
//! your picture → your money (logo chips to toggle) → one card per API
//! source (its key, then Save) → bank exports (how to get them, then
//! Upload) → protect this computer's copy (desktop: encrypt it, the key in
//! the OS keyring or a passphrase; on by default, and the admin's nightly
//! backup key with it) → first sync → you're all set. Each step is saved in the
//! profile's `onboarding_step`, so the wizard resumes where it was left.
//!
//! The welcome card (and the sign-in question of a turned-away device) also
//! offers "Just try it": the app opens on a made-up account
//! instead (crate::app::demo), and the wizard is there again from its
//! "Set up for real".

use std::sync::OnceLock;
// std's Instant panics in the browser; web_time is std's on native
use web_time::Instant;

use finance_core::client::{Request, WhoAmI};
use finance_core::onboarding::{self, Choice, Group, Place, Protect, Step, Wizard};
use finance_core::signin::{self, Signup};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    switch::Switch,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use super::avatar_edit::{AvatarChanged, AvatarEdit};
use super::sources::{call, json_of, SourcesChanged, SourcesView};
use finance_core::supported::Source;
use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

pub enum OnboardingEvent {
    /// sync now (first sync)
    Sync,
    /// signed in (a code or a new account that sees the data): load the data
    SignedIn,
    OpenUpload,
    /// finished or dismissed
    Close,
}

// the wizard's own glyphs (only these are embedded, not the whole catalog)
gpui_kit::assets::icon_assets!(pub WizardIcons, [Monitor, Server, KeyRound, UserPlus, Smartphone, Compass]);

/// The app's name, for the welcome card (to be decided: one place to change).
const APP_NAME: &str = "Anser";

/// The card's width, at most, and its least height (the cards don't jump).
const CARD: f32 = 520.;
const CARD_MIN_H: f32 = 400.;

/// The browser's stand-in for FINANCE_DESKTOP_ONBOARDING (`?onboarding=money`).
static DEV_AID: OnceLock<String> = OnceLock::new();

#[cfg(target_family = "wasm")]
pub fn set_dev_aid(v: String) {
    let _ = DEV_AID.set(v);
}

/// Dev/screenshot aid: FINANCE_DESKTOP_ONBOARDING (or the browser's
/// `?onboarding=`) opens the wizard at start; a step key starts it there.
pub fn dev_aid() -> Option<String> {
    std::env::var("FINANCE_DESKTOP_ONBOARDING").ok().or_else(|| DEV_AID.get().cloned()).filter(|s| !s.is_empty())
}

pub struct Onboarding {
    w: Wizard,
    /// the catalog's sources, with their how-to text
    files: Vec<Source>,
    /// bank-exports step: the sources whose steps are toggled (open when
    /// there is one source, else closed, until toggled)
    how_toggled: std::collections::BTreeSet<String>,
    url: Entity<InputState>,
    name: Entity<InputState>,
    ccy: Entity<InputState>,
    country: Entity<InputState>,
    city: Entity<InputState>,
    // the sign-in step
    /// None: asking "Do you have a pairing code?"; Some(true) the code; Some(false) a new account
    have_code: Option<bool>,
    /// one request per code (core's CodeSubmit), and what the slots held before the last change
    submit: signin::CodeSubmit,
    slots: String,
    code: Entity<InputState>,
    acct_name: Entity<InputState>,
    acct_email: Entity<InputState>,
    invite: Entity<InputState>,
    who: Option<WhoAmI>,
    /// signed in to an account that sees no data yet (an invite)
    no_data: Option<String>,
    /// opened because the server turned us away: once signed in to a server
    /// that is already set up, close instead of walking the setup
    close_when_set_up: bool,
    /// opened from the app (Settings, the no-data page): the X leaves
    closable: bool,
    /// an added account that hasn't signed in, with another to go back to:
    /// the sign-in step has an X that cancels it (profiles::cancel_unfinished)
    cancel: bool,
    avatar: Entity<AvatarEdit>,
    sources: Entity<SourcesView>,
    /// the card's Continue is saving its API key: go on once it's saved
    saving: bool,
    profile: Value,
    /// the profile arrived: fill the fields on the next render
    fill: bool,
    /// a new account's name: the name field's too, on the next render
    fill_name: bool,
    /// why the money list couldn't load (a 401, the server is down)
    catalog_note: Option<String>,
    /// the keys' card has its form (it opened on it: dev aid, a resume)
    api_shown: bool,
    /// "More options" is open on this card
    more: bool,
    /// "Why?" is open on this card
    why: bool,
    /// when the welcome card opened (its watermark animates from here)
    started: Instant,
    // the Protect card
    protect_on: bool,
    /// "Use a passphrase instead"
    passphrase: bool,
    /// the OS keyring works here (None: probing)
    keyring: Option<bool>,
    pass1: Entity<InputState>,
    pass2: Entity<InputState>,
    /// the admin's nightly-backup key, made here (on by default)
    seal_backups: bool,
    /// the card did its work (a second Continue just moves on)
    protected: bool,
    /// this computer's copy is already encrypted (setup run again)
    sealed: bool,
    /// no backup key on this computer yet (one already made is kept)
    key_missing: bool,
    /// the server already has a backup recipient (None: not answered yet)
    server_recipient: Option<bool>,
    /// the Done card's phone QR is showing
    qr: bool,
    note: Option<(bool, String)>,
    busy: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<OnboardingEvent> for Onboarding {}

impl Focusable for Onboarding {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn web() -> bool {
    cfg!(target_family = "wasm")
}

impl Onboarding {
    pub fn new(closable: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with(Wizard::new(web()), false, closable, window, cx)
    }

    /// The server turned this device away: open on the sign-in step.
    pub fn signing_in(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with(Wizard::signing_in(web()), true, false, window, cx)
    }

    fn with(mut w: Wizard, close_when_set_up: bool, closable: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |ph: &str, v: String, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v))
        };
        // dev/screenshot aid: FINANCE_DESKTOP_ONBOARDING=money starts on that step
        if let Some(s) = dev_aid().as_deref().and_then(Step::from_key) {
            w.step = s;
            // the first cards run for real, so the server's answer decides
            // whether there is a sign-in (a later step pretends it's done)
            if !matches!(s, Step::Welcome | Step::Where) {
                w.paired = s != Step::SignIn;
                w.connected = true;
                w.sign_in = s == Step::SignIn;
                w.place = Some(Place::Remote);
            }
        }
        let sources = cx.new(|cx| SourcesView::new(Some(vec![]), cx));
        let avatar = cx.new(|cx| AvatarEdit::new(window, cx).simple());
        let subs = vec![
            cx.subscribe(&avatar, |this, _, ev: &AvatarChanged, cx| {
                // keep the step and the rest: only the avatar fields moved
                if let (Some(p), Some(n)) = (this.profile.as_object_mut(), ev.0.as_object()) {
                    for (k, v) in n.iter().filter(|(k, _)| k.starts_with("avatar_")) {
                        p.insert(k.clone(), v.clone());
                    }
                } else {
                    this.profile = ev.0.clone();
                }
                cx.notify();
            }),
            // the API card's Continue saved its key: on to the next card
            cx.subscribe_in(&sources, window, |this, _, _: &SourcesChanged, window, cx| {
                if std::mem::take(&mut this.saving) {
                    if let Some(c) = this.w.current_api() {
                        let k = c.key.clone();
                        this.w.saved.insert(k);
                    }
                    this.advance(window, cx);
                }
            }),
        ];
        // the device's locale pre-fills the currency and country (USD, none when it has no region)
        let locale = finance_core::locale::detect();
        let mut o = Onboarding {
            w,
            files: vec![],
            how_toggled: Default::default(),
            url: input("https://your-server.tailXXXX.ts.net", finance_core::cache::server_url(), window, cx),
            name: input("Your name", String::new(), window, cx),
            ccy: input("USD", locale.0, window, cx),
            country: input("US", locale.1, window, cx),
            city: input("Your city", String::new(), window, cx),
            have_code: None,
            submit: signin::CodeSubmit::default(),
            slots: String::new(),
            code: input("ABCD-EFGH", String::new(), window, cx),
            acct_name: input("Your name", String::new(), window, cx),
            acct_email: input("you@example.com", String::new(), window, cx),
            invite: input("ABCD-EFGH", String::new(), window, cx),
            who: None,
            no_data: None,
            close_when_set_up,
            closable,
            #[cfg(not(target_family = "wasm"))]
            cancel: finance_core::profiles::cancellable(),
            #[cfg(target_family = "wasm")]
            cancel: false,
            avatar,
            sources,
            saving: false,
            profile: Value::Null,
            fill: false,
            fill_name: false,
            api_shown: false,
            catalog_note: None,
            more: false,
            why: false,
            started: Instant::now(),
            protect_on: true,
            passphrase: false,
            keyring: None,
            pass1: cx.new(|cx| InputState::new(window, cx).placeholder("Passphrase").masked(true)),
            pass2: cx.new(|cx| InputState::new(window, cx).placeholder("Once more").masked(true)),
            seal_backups: true,
            protected: false,
            sealed: finance_core::replica::status().encrypted,
            key_missing: false,
            server_recipient: None,
            qr: false,
            note: None,
            busy: false,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        // the code slots: tidy what's typed, sign in on the eighth character
        let sub = cx.subscribe_in(&o.code, window, |this, state, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::Change) {
                let code = crate::codeslots::clean(state, Some(&this.slots), window, cx);
                this.slots = code.clone();
                // once per code: the tidy-up and the paste each say Change, and a
                // refused code must not go round again (the server's tries are few)
                if this.submit.auto(&code) {
                    this.send_code(code, cx);
                }
                cx.notify();
            }
        });
        o._subs.push(sub);
        o.load(cx);
        // the keyring probe may wait on D-Bus: off the UI thread
        #[cfg(not(target_family = "wasm"))]
        {
            let probe = cx.background_spawn(async move {
                (finance_core::replica::keyring_available(), finance_core::backups::identity().is_none())
            });
            cx.spawn(async move |this, cx| {
                let (ok, key_missing) = probe.await;
                let _ = this.update(cx, |this, cx| {
                    this.keyring = Some(ok);
                    this.key_missing = key_missing;
                    cx.notify();
                });
            })
            .detach();
        }
        o
    }

    /// The checklist from a catalog answer (the server's, or the demo's compiled-in one).
    fn set_catalog(&mut self, v: &Value) {
        {
            self.files = finance_core::supported::parse(v);
            let list: Vec<Choice> = self.files.iter().filter(|e| e.api).chain(self.files.iter().filter(|e| !e.api))
                .map(|e| Choice { key: e.key.clone(), label: e.label.clone(), api: e.api, group: e.group })
                .collect();
            // jumping straight to the keys (dev aid): a card for each
            if self.w.step == Step::ApiSources && self.w.chosen.is_empty() {
                self.w.chosen = list.iter().filter(|c| c.api).map(|c| c.key.clone()).collect();
            }
            // jumping straight to the bank exports (dev aid): N26's, or the keys in
            // FINANCE_DESKTOP_SOURCES_OPEN (?sources_open=n26,mox)
            if self.w.step == Step::FileSources && self.w.chosen.is_empty() {
                let want = crate::app::dev_aid("FINANCE_DESKTOP_SOURCES_OPEN").unwrap_or_else(|| "n26".into());
                self.w.chosen = want.split(',').map(String::from).filter(|k| list.iter().any(|c| &c.key == k)).collect();
            }
            self.w.catalog = list;
        }
    }

    /// Who this device is to the server; once signed in, the catalog (the
    /// checklist) and the profile (where it was left).
    pub fn load(&mut self, cx: &mut Context<Self>) {
        call(Request::new("GET", "/api/whoami"), cx, |this, r, _| {
            if let Ok(v) = json_of(r) {
                let who: WhoAmI = serde_json::from_value(v).unwrap_or_default();
                // a server that wants nothing for writes needs no sign-in either
                if !this.w.paired {
                    this.w.server_answered(who.known() && !who.no_data_yet());
                }
                // already talking to a server that knows this device: "where" is answered
                if this.w.paired && this.w.place.is_none() {
                    this.w.place = Some(Place::Remote);
                }
                this.who = Some(who);
            }
        });
        if signin::needed() {
            return;
        }
        // the demo answers the catalog itself (finance_core::demo)
        call(Request::new("GET", "/api/sources/catalog"), cx, |this, r, cx| {
            let v = match json_of(r) {
                Ok(v) => v,
                // the list is waiting on this: say why it isn't coming
                Err(e) => {
                    this.catalog_note = Some(e);
                    cx.notify();
                    return;
                }
            };
            this.catalog_note = None;
            this.set_catalog(&v);
        });
        // the backup key is made only where the server has none yet
        #[cfg(not(target_family = "wasm"))]
        call(Request::new("GET", "/api/admin/backup-recipient"), cx, |this, r, cx| {
            if let Ok(v) = json_of(r) {
                this.server_recipient = Some(v["recipient"].as_str().is_some_and(|s| !s.is_empty()));
                cx.notify();
            }
        });
        call(Request::new("GET", "/api/profile"), cx, |this, r, cx| {
            if let Ok(v) = json_of(r) {
                let step = v["onboarding_step"].as_str().and_then(Step::from_key);
                if this.close_when_set_up && step == Some(Step::Done) {
                    // signed in to a server that's set up: straight to the data
                    cx.emit(OnboardingEvent::Close);
                    return;
                }
                let pinned = dev_aid().as_deref().and_then(Step::from_key).is_some();
                if let Some(s) = step {
                    if s != Step::Done && this.w.applies(s) && !pinned && this.w.step != Step::SignIn {
                        this.w.step = s;
                    }
                }
                this.avatar.update(cx, |a, cx| a.set_profile(v.clone(), cx));
                this.profile = v;
                this.fill = true;
            }
        });
    }

    fn val(i: &Entity<InputState>, cx: &App) -> String {
        i.read(cx).value().trim().to_string()
    }

    /// Save the profile (with the step) on the server.
    fn save_profile(&mut self, step: Step, cx: &mut Context<Self>) {
        if signin::needed() {
            return;
        }
        let mut p = self.profile.as_object().cloned().unwrap_or_default();
        let pr = &self.w.profile;
        if !pr.base_currency.is_empty() {
            p.insert("base_currency".into(), json!(pr.base_currency.to_uppercase()));
        }
        p.entry("base_currency").or_insert(json!(finance_core::locale::detect().0));
        for (k, v) in [("name", &pr.name), ("home_country", &pr.home_country), ("home_city", &pr.home_city)] {
            if !v.is_empty() {
                p.insert(k.into(), json!(v));
            }
        }
        p.insert("onboarding_step".into(), json!(step.key()));
        call(Request::new("PUT", "/api/profile").json(&Value::Object(p)), cx, |this, r, cx| match json_of(r) {
            Ok(v) => {
                this.profile = v["profile"].clone();
                crate::avatar::set_spec(&this.profile, cx);
            }
            Err(e) => this.note = Some((false, format!("Couldn't save the profile: {e}"))),
        });
    }

    /// The next card (if the step allows), saving where the wizard is.
    fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.w.profile.name = Self::val(&self.name, cx);
        self.w.profile.base_currency = Self::val(&self.ccy, cx).to_uppercase();
        self.w.profile.home_country = Self::val(&self.country, cx).to_uppercase();
        self.w.profile.home_city = Self::val(&self.city, cx);
        match self.w.next() {
            Ok(step) => {
                self.note = None;
                self.more = false;
                self.why = false;
                self.save_profile(step, cx);
                self.show_api_card(window, cx);
            }
            Err(why) => self.note = Some((false, why.into())),
        }
        cx.notify();
    }

    /// Back one card (Esc): the sign-in's question first, then the wizard.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.w.step == Step::SignIn && self.have_code.is_some() && !self.w.paired {
            self.have_code = None;
        } else if self.w.step != Step::Done && self.w.back_possible() {
            self.w.back();
            self.show_api_card(window, cx);
        } else {
            return;
        }
        self.note = None;
        self.more = false;
        self.why = false;
        cx.notify();
    }

    /// On an API source's card: its form.
    fn show_api_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.w.step != Step::ApiSources {
            return;
        }
        if let Some(key) = self.w.current_api().map(|c| c.key.clone()) {
            self.sources.update(cx, |s, cx| s.focus_one(&key, window, cx));
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.save_profile(Step::Done, cx);
        if self.w.synced {
            cx.emit(OnboardingEvent::Sync);
        }
        cx.emit(OnboardingEvent::Close);
    }

    /// The card's primary button (and Enter).
    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.saving {
            return;
        }
        match self.w.step {
            #[cfg(not(target_family = "wasm"))]
            Step::Where if self.w.place.is_some() && !self.w.connected => self.connect(window, cx),
            Step::SignIn if !self.w.paired => match self.have_code {
                Some(true) => self.sign_in_with_code(cx),
                Some(false) if self.no_data.is_none() => self.create_account(cx),
                _ => {}
            },
            Step::ApiSources => {
                let key = self.w.current_api().map(|c| c.key.clone()).unwrap_or_default();
                let s = self.sources.read(cx);
                let (typed, configured) = (s.typed(cx), s.configured(&key));
                if typed {
                    self.saving = true;
                    self.note = None;
                    self.sources.update(cx, |s, cx| s.save(cx));
                    // a refused key stays on the card (SourcesChanged never comes)
                    cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                            let Ok(done) = this.update(cx, |this, cx| {
                                let busy = this.sources.read(cx).busy();
                                if !busy && this.saving {
                                    this.saving = false;
                                    cx.notify();
                                }
                                !busy
                            }) else { return };
                            if done {
                                return;
                            }
                        }
                    })
                    .detach();
                } else if configured {
                    self.advance(window, cx);
                } else {
                    self.note = Some((false, "Paste the key, or skip this one.".into()));
                }
            }
            #[cfg(not(target_family = "wasm"))]
            Step::Protect if !self.protected => self.protect(window, cx),
            Step::FirstSync if !self.w.synced => self.first_sync(window, cx),
            Step::Done => self.close(cx),
            _ => self.advance(window, cx),
        }
        cx.notify();
    }

    /// Desktop "where": does the server answer, and does it know this
    /// machine? Answered: on to the next card.
    #[cfg(not(target_family = "wasm"))]
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = Self::val(&self.url, cx).trim_end_matches('/').to_string();
        if url.is_empty() {
            self.note = Some((false, "Enter the server's address.".into()));
            return;
        }
        self.busy = true;
        // this machine's server: reused when it answers, else started (the app owns it)
        let local = finance_core::local_server::is_loopback(&url);
        self.note = local.then(|| (true, "Starting the local server…".to_string()));
        let task = cx.background_spawn(async move {
            if local {
                finance_core::local_server::ensure(&url, &finance_core::cache::dir().join("anser-server.log"))?;
            }
            finance_core::sync::healthz(&url).map_err(|e| format!("{url} doesn't answer: {e:#}"))?;
            finance_core::cache::save_server_url(&url).map_err(|e| e.to_string())?;
            finance_core::auth::whoami(&url).map_err(|e| format!("{e:#}"))
        });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    match r {
                        Ok(who) => {
                            let known = who.known() && !who.no_data_yet();
                            this.w.server_answered(known);
                            this.who = Some(who);
                            if known {
                                signin::signed_in();
                                this.load(cx);
                            }
                            this.advance(window, cx);
                        }
                        Err(e) => this.note = Some((false, e)),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }

    /// "Yes, I have a code": the browser trades it for a session cookie, the
    /// desktop for a device token.
    /// The Paste button by the code slots: the code goes in (and signs in
    /// once whole, through the slots' subscription), or a note says why not.
    fn paste_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paste = crate::codeslots::paste(&self.code, window, cx);
        cx.spawn(async move |this, cx| {
            if let Err(why) = paste.await {
                let _ = this.update(cx, |this, cx| {
                    this.note = Some((false, why.into()));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Enter or Sign in: the code in the slots goes, unless a request for a
    /// code is out (a refused code goes again: they asked).
    fn sign_in_with_code(&mut self, cx: &mut Context<Self>) {
        let code = Self::val(&self.code, cx);
        if let Some(why) = signin::code_problem(&code) {
            self.note = Some((false, why.into()));
            cx.notify();
            return;
        }
        if self.submit.asked(&code, false) {
            self.send_code(code, cx);
        }
    }

    /// The request itself; the callers decided it may go ([`signin::CodeSubmit`]).
    fn send_code(&mut self, code: String, cx: &mut Context<Self>) {
        self.busy = true;
        self.note = None;
        #[cfg(target_family = "wasm")]
        call(Request::new("POST", "/api/login").json(&json!({"code": code, "resume_key": signin::resume_key()})), cx, |this, r, cx| {
            this.busy = false;
            this.submit.answered();
            match r {
                Ok(resp) if resp.ok() => this.signed_in(cx),
                Ok(resp) => this.note = Some((false, signin::refusal(resp.status, &resp.json()))),
                Err(e) => this.note = Some((false, format!("The server didn't answer: {e}"))),
            }
        });
        #[cfg(not(target_family = "wasm"))]
        {
            let server = finance_core::cache::server_url();
            let task = cx.background_spawn(async move {
                finance_core::auth::pair(&server, &code, &finance_core::auth::default_device_name()).map_err(|e| format!("{e:#}"))
            });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    this.submit.answered();
                    match r {
                        Ok(_) => this.signed_in(cx),
                        Err(e) => this.note = Some((false, e)),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// "No: create a new account".
    fn create_account(&mut self, cx: &mut Context<Self>) {
        let name = Self::val(&self.acct_name, cx);
        let email = Self::val(&self.acct_email, cx);
        let invite = Self::val(&self.invite, cx);
        let mode = self.signup_mode();
        if name.is_empty() {
            self.note = Some((false, "Enter your name.".into()));
            cx.notify();
            return;
        }
        if mode == Signup::Invite {
            if let Some(why) = signin::code_problem(&invite) {
                self.note = Some((false, format!("The invite: {why}")));
                cx.notify();
                return;
            }
        }
        self.busy = true;
        self.note = None;
        #[cfg(target_family = "wasm")]
        call(Request::new("POST", "/api/signup/session").json(&json!({"name": name, "email": email, "invite": invite})), cx, |this, r, cx| {
            this.busy = false;
            match r {
                Ok(resp) if resp.ok() => {
                    let v = resp.json();
                    this.account_made(v["data"].as_bool().unwrap_or(true), v["note"].as_str().map(String::from), cx);
                }
                Ok(resp) => this.note = Some((false, signin::refusal(resp.status, &resp.json()))),
                Err(e) => this.note = Some((false, format!("The server didn't answer: {e}"))),
            }
        });
        #[cfg(not(target_family = "wasm"))]
        {
            let server = finance_core::cache::server_url();
            let task = cx.background_spawn(async move {
                finance_core::auth::signup(&server, &name, &email, &invite, &finance_core::auth::default_device_name()).map_err(|e| format!("{e:#}"))
            });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    match r {
                        Ok(a) => this.account_made(a.data, a.note, cx),
                        Err(e) => this.note = Some((false, e)),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn account_made(&mut self, data: bool, note: Option<String>, cx: &mut Context<Self>) {
        if data {
            // an account with its data (the first one, the admin; or a
            // household member with a tree of their own): set it up
            self.close_when_set_up = false;
            // the name it was made with is the profile's too
            let name = Self::val(&self.acct_name, cx);
            self.w.profile.name = name;
            self.fill_name = true;
            self.signed_in(cx);
        } else {
            self.no_data = Some(note.unwrap_or_else(|| {
                "Your account is made, but it has no data on this server yet: the admin restarts the server to make it.".into()
            }));
            cx.notify();
        }
    }

    fn signed_in(&mut self, cx: &mut Context<Self>) {
        signin::signed_in();
        self.w.server_answered(true);
        self.note = None;
        cx.emit(OnboardingEvent::SignedIn);
        // the profile decides: a set-up server closes the wizard, else on to the name
        let _ = self.w.next();
        self.load(cx);
        cx.notify();
    }

    /// Ask the server to fetch everything, then on to the last card (the
    /// server goes on fetching; the app pulls when the wizard closes).
    fn first_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = true;
        let window_handle = window.window_handle();
        call(Request::new("POST", "/sync"), cx, move |this, r, cx| {
            this.busy = false;
            match r {
                Ok(resp) if resp.status < 400 => {
                    // the app pulls when the wizard closes (the browser's pull
                    // reloads the page: it would take the last card with it)
                    this.w.synced = true;
                    let view = cx.entity().downgrade();
                    cx.defer(move |cx| {
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            let _ = view.update(cx, |this, cx| this.advance(window, cx));
                        });
                    });
                }
                other => this.note = Some((false, json_of(other).err().unwrap_or_default())),
            }
        });
        cx.notify();
    }

    /// The backup-key switch is offered (see onboarding::offer_backup_key).
    fn offer_key(&self) -> bool {
        onboarding::offer_backup_key(self.who.as_ref().is_some_and(|w| w.admin()), self.key_missing, self.server_recipient)
    }

    /// The Protect card's Continue: encrypt this computer's copy (while it
    /// is still empty, so it's instant; a passphrase takes ~0.5 s of Argon2),
    /// and for the admin make the nightly-backup key and hand it to the
    /// server. A key already there is kept: a new one would orphan the
    /// backups sealed to the old.
    #[cfg(not(target_family = "wasm"))]
    fn protect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use finance_core::{backups, replica};
        let want_enc = self.protect_on && !self.sealed;
        let want_key = self.seal_backups && self.offer_key();
        let plan = onboarding::protect_plan(self.protect_on, self.passphrase, self.keyring);
        let pass = (plan == Protect::Passphrase).then(|| (Self::val(&self.pass1, cx), Self::val(&self.pass2, cx)));
        if want_enc {
            match (&plan, &pass) {
                (Protect::Checking, _) => {
                    self.note = Some((true, "Checking the OS keyring…".into()));
                    cx.notify();
                    return;
                }
                (_, Some((a, b))) if a != b => {
                    self.note = Some((false, "The two passphrases differ.".into()));
                    cx.notify();
                    return;
                }
                (_, Some((a, _))) if a.chars().count() < 8 => {
                    self.note = Some((false, "8 characters at least (four random words is better).".into()));
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        if !want_enc && !want_key {
            self.protected = true;
            return self.advance(window, cx);
        }
        self.busy = true;
        self.note = None;
        let pass = pass.map(|(a, _)| a);
        let task = cx.background_spawn(async move {
            if want_enc {
                replica::enable(pass.as_deref(), &super::vault::extra_files()).map_err(|e| format!("{e:#}"))?;
            }
            let mut public = None;
            if want_key {
                let (text, p) = finance_engine::archive::generate_identity();
                backups::store_identity(&text).map_err(|e| format!("couldn't keep the backup key: {e:#}"))?;
                public = Some(p);
            }
            Ok::<_, String>(public)
        });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    match r {
                        Ok(None) => {
                            this.protected = true;
                            this.sealed = this.sealed || this.protect_on;
                            this.advance(window, cx);
                        }
                        Ok(Some(public)) => {
                            this.protected = true;
                            this.sealed = this.sealed || this.protect_on;
                            this.busy = true;
                            call(Request::new("PUT", "/api/admin/backup-recipient").json(&json!({"recipient": public})), cx, move |this, r, cx| {
                                this.busy = false;
                                // the key is kept here either way; a refusal can be sent again from Settings
                                let sent = json_of(r);
                                let view = cx.entity().downgrade();
                                cx.defer(move |cx| {
                                    let _ = cx.update_window(window_handle, |_, window, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            this.advance(window, cx);
                                            if sent.is_err() {
                                                this.note = Some((false, "The server didn't take the backup key: Settings, Sync & storage, Backups sends it again.".into()));
                                            }
                                            cx.notify();
                                        });
                                    });
                                });
                            });
                        }
                        Err(e) => {
                            // the copy may be encrypted already: a retry does only what's left
                            this.sealed = finance_core::replica::status().encrypted;
                            this.note = Some((false, e));
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }

    /// "Just try it": the demo account (the app restarts on it, or the
    /// browser loads it); a failure stays on the card.
    fn try_demo(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.note = Some((true, "Making up a household…".into()));
        let view = cx.entity().downgrade();
        crate::app::demo::start(cx, move |e, cx| {
            let _ = view.update(cx, |this, cx| {
                this.busy = false;
                this.note = Some((false, e));
                cx.notify();
            });
        });
        cx.notify();
    }

    /// The first card's way to look around before setting anything up.
    fn demo_choice(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        v_flex().gap_2()
            .child(h_flex().gap_2().items_center()
                .child(div().flex_1().h(px(1.)).bg(ui::hairline(cx)))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("or"))
                .child(div().flex_1().h(px(1.)).bg(ui::hairline(cx))))
            .child(Self::choice("ob-demo", Lucide::Compass, "Just try it", "Explore with made-up data. Nothing is sent anywhere.", false, cx)
                .on_click(cx.listener(|this, _, _, cx| this.try_demo(cx))))
    }

    fn signup_mode(&self) -> Signup {
        Signup::parse(self.who.as_ref().map(|w| w.signup.as_str()).unwrap_or(""))
    }

    // ---------- the card ----------

    /// The card's title and its one sentence.
    fn heading(&self) -> (String, String) {
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        match self.w.step {
            Step::Welcome => s(APP_NAME, "Your money, all in one place."),
            Step::Where => s("Where should your data live?", "A finance server keeps it: on this computer, or on another one you own."),
            Step::SignIn if self.no_data.is_some() => s("Signed in", "Your account is made."),
            Step::SignIn if self.w.paired => s("Signed in", "This device can see the data on this server."),
            Step::SignIn => match self.have_code {
                None => s("Sign in", "Do you have a pairing code from a device that's already signed in?"),
                Some(true) => s("Your code", "Eight letters and digits. It signs in by itself once complete."),
                Some(false) => match self.signup_mode() {
                    Signup::First => s("Create your account", "You'll be the admin, and the data on this server is yours."),
                    Signup::Open => s("Create your account", "Anyone who reaches this server can make one."),
                    Signup::Invite => s("Create your account", "You need an invite from an admin (Settings, Admin, Invite someone)."),
                },
            },
            Step::You => s("What's your name?", "As your bank writes it, so transfers to yourself are recognised."),
            Step::Home => s("Where are you based?", "Amounts show in your base currency; trips start from home."),
            Step::Picture => s("Add a picture", "Optional: a photo, your Gravatar, or just your initials."),
            Step::Money => s("Where is your money?", "Pick everything that applies. You can add more later."),
            Step::ApiSources => {
                let label = self.w.current_api().map(|c| c.label.clone()).unwrap_or_default();
                (format!("Connect {label}"), "Paste a read-only key. One that can trade or withdraw is refused.".into())
            }
            Step::FileSources => s("Bring in your exports", "Download one from each, then add the files on the Upload page."),
            Step::Protect => s("Protect this computer's copy", "This computer keeps a copy of your data. Encrypt it, in case the disk isn't."),
            Step::FirstSync => s("Fetch your data", "Anser gets each account's history. It can take a few minutes."),
            Step::Done => s("You're all set", "Pages appear as their data arrives; the Doctor shows how each source is doing."),
        }
    }

    /// The longer explanation behind the card's "Why?", where it has one.
    fn why_text(&self) -> Option<&'static str> {
        Some(match self.w.step {
            Step::Where => "A small server fetches and keeps your data; this app is a window on it. It can run right here (this app includes it), or on a home server or VPS you own.",
            Step::SignIn if self.have_code == Some(true) && !self.w.paired =>
                "A pairing code comes from Settings, Account, Pair another device on a device that's signed in (once, for 10 minutes), or it's the code this device got when it signed out.",
            Step::Protect if !self.w.web => "This computer's disk may not be encrypted, so the copy is sealed. The key stays in the OS keyring, never next to the data. \
                Lost it? The copy is only a cache of the server: start over and the next sync fetches everything again.",
            _ => return None,
        })
    }

    /// The primary button's label; None: the card has its own buttons.
    fn primary_label(&self) -> Option<&'static str> {
        match self.w.step {
            Step::Welcome => Some("Get started"),
            Step::Where if !self.w.connected => Some("Connect"),
            Step::SignIn if self.no_data.is_some() => None,
            Step::SignIn if !self.w.paired => match self.have_code {
                None => None,
                Some(true) if self.submit.pending() => Some("Signing in"),
                Some(true) => Some("Sign in"),
                Some(false) => Some("Create account"),
            },
            Step::ApiSources => Some("Save and continue"),
            Step::Protect if !self.protected && self.protect_on && !self.sealed => Some("Encrypt and continue"),
            Step::FirstSync if !self.w.synced => Some("Sync now"),
            Step::Done => Some("Open the dashboard"),
            _ => Some("Continue"),
        }
    }

    fn can_go_back(&self) -> bool {
        (self.w.step == Step::SignIn && self.have_code.is_some() && !self.w.paired && self.no_data.is_none())
            || (self.w.step != Step::Done && self.w.back_possible())
    }

    /// A field: its label, then the input across the card.
    fn field(label: &str, i: &Entity<InputState>, cx: &App) -> Div {
        v_flex().gap(px(4.))
            .child(div().text_size(px(ts::CAPTION)).text_color(cx.theme().muted_foreground).child(label.to_string()))
            .child(Input::new(i))
    }

    /// "More options": a quiet toggle, then what it hides.
    fn more(&self, inner: Div, cx: &mut Context<Self>) -> Div {
        let open = self.more;
        v_flex().gap_3()
            .child(h_flex().child(Button::new("ob-more").small().ghost()
                .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                .label("More options")
                .on_click(cx.listener(|this, _, _, cx| { this.more = !this.more; cx.notify(); }))))
            .when(open, |d| d.child(inner))
    }

    /// "Why?": a quiet toggle, then the explanation.
    fn why_row(&self, text: &'static str, cx: &mut Context<Self>) -> Div {
        let open = self.why;
        v_flex().gap_2()
            .child(h_flex().child(Button::new("ob-why").small().ghost()
                .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                .label("Why?")
                .on_click(cx.listener(|this, _, _, cx| { this.why = !this.why; cx.notify(); }))))
            .when(open, |d| d.child(ui::caption_wrap(text, cx)))
    }

    /// A big choice: icon, label and a caption, outlined when picked.
    fn choice(id: &'static str, icon: impl Into<Icon>, label: &str, sub: &str, on: bool, cx: &App) -> Stateful<Div> {
        let t = cx.theme();
        h_flex().id(id).gap_3().items_center().w_full().min_h(px(crate::layout::TOUCH + 8.)).px(px(14.)).py(px(10.))
            .rounded(t.radius_lg).border_1().cursor_pointer()
            .border_color(if on { t.primary } else { ui::hairline(cx) })
            .bg(if on { t.primary.opacity(0.08) } else { t.background })
            .hover(|s| s.bg(t.accent))
            .child(Icon::new(icon).size(px(20.)).text_color(if on { t.primary } else { t.muted_foreground }))
            .child(v_flex().flex_1().min_w_0()
                .child(div().text_sm().font_medium().child(label.to_string()))
                .when(!sub.is_empty(), |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(sub.to_string()))))
    }

    fn sign_in_body(&mut self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let muted = |s: &str| div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(s.to_string());
        if let Some(n) = self.no_data.clone() {
            return v_flex().gap_3()
                .child(h_flex().gap_2().items_center().child(ui::dot(t.warning, 8.)).child(div().text_sm().child("Signed in to your new account.")))
                .child(muted(&n))
                .child(muted("The data on this server stays its owner's. You can sign out from Settings, Account."));
        }
        if self.w.paired {
            let who = self.who.as_ref().and_then(|w| w.user.as_ref()).map(|u| u.name.clone()).unwrap_or_default();
            return v_flex().child(h_flex().gap_2().items_center().child(ui::dot(fmt::money_in(), 8.))
                .child(div().text_sm().child(if who.is_empty() { "Signed in.".to_string() } else { format!("Signed in as {who}.") })));
        }
        match self.have_code {
            None => v_flex().gap_2()
                .child(Self::choice("ob-yes", Lucide::KeyRound, "Yes, I have a code", "From Settings, Account, Pair another device on a signed-in device", false, cx)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.have_code = Some(true);
                        this.note = None;
                        this.code.update(cx, |i, cx| i.focus(window, cx));
                        cx.notify();
                    })))
                .child(Self::choice("ob-no", Lucide::UserPlus, "No, create a new account", "", false, cx)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.have_code = Some(false);
                        this.note = None;
                        this.acct_name.update(cx, |i, cx| i.focus(window, cx));
                        cx.notify();
                    })))
                // a turned-away device has no welcome card before this one
                .when(!self.w.welcome, |d| d.child(self.demo_choice(cx))),
            Some(true) => v_flex().gap_2().items_center()
                .child(crate::codeslots::render("ob-code", &self.code, cx.listener(|this, _, window, cx| this.paste_code(window, cx)), cx))
                .when_some(self.why_text(), |d, w| d.child(self.why_row(w, cx))),
            Some(false) => {
                let mode = self.signup_mode();
                let more = self.more(v_flex().child(Self::field("Email (optional, for your Gravatar)", &self.acct_email, cx)), cx);
                v_flex().gap_3()
                    .child(Self::field("Your name", &self.acct_name, cx))
                    .when(mode == Signup::Invite, |d| d.child(Self::field("Invite", &self.invite, cx)))
                    .when(mode != Signup::First, |d| d.child(muted(
                        "The new account gets data of its own on this server: nobody else sees it.")))
                    .child(more)
            }
        }
    }

    fn body(&mut self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let muted = |s: String| div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(s);
        match self.w.step {
            Step::Where => {
                let place = self.w.place;
                let pick = |id: &'static str, icon: Lucide, label: &str, sub: &str, p: Place, cx: &mut Context<Self>| {
                    Self::choice(id, icon, label, sub, place == Some(p), cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.w.place = Some(p);
                            this.w.connected = false;
                            this.note = None;
                            let url = if p == Place::Local { finance_core::cache::DEFAULT_SERVER.to_string() } else { String::new() };
                            this.url.update(cx, |i, cx| { i.set_value(url, window, cx); i.focus(window, cx) });
                            cx.notify();
                        }))
                };
                let url = v_flex().gap_2()
                    .child(Self::field("Server address", &self.url, cx))
                    .when(place == Some(Place::Local), |d| d.child(muted("The app starts it, and stops it when it quits.".into())));
                let url = match place {
                    Some(Place::Remote) => Some(url),
                    // this machine's address is the usual one: tucked away
                    Some(Place::Local) => Some(self.more(url, cx)),
                    None => None,
                };
                v_flex().gap_3()
                    .child(v_flex().gap_2()
                        .child(pick("ob-local", Lucide::Monitor, "This machine", "This app runs the server too", Place::Local, cx))
                        .child(pick("ob-remote", Lucide::Server, "Another machine", "A home server or VPS on your tailnet", Place::Remote, cx)))
                    .children(url)
                    .when_some(self.why_text(), |d, w| d.child(self.why_row(w, cx)))
            }
            // the welcome card draws itself (render)
            Step::Welcome => v_flex(),
            Step::SignIn => self.sign_in_body(cx),
            Step::Protect => self.protect_body(cx),
            Step::You => v_flex().child(Self::field("Name", &self.name, cx)),
            Step::Home => v_flex().gap_3()
                .child(h_flex().gap_3()
                    .child(div().flex_1().child(Self::field("Base currency", &self.ccy, cx)))
                    .child(div().flex_1().child(Self::field("Home country", &self.country, cx))))
                .child(self.more(v_flex().child(Self::field("Home city", &self.city, cx)), cx)),
            Step::Picture => v_flex().child(self.avatar.clone()),
            Step::Money => {
                let chosen = self.w.chosen.clone();
                let grid = |group: Group, cx: &mut Context<Self>| {
                    let chips: Vec<_> = self.w.catalog.iter().filter(|c| c.group == group).map(|c| {
                        let key = c.key.clone();
                        let on = chosen.contains(&c.key);
                        h_flex().id(SharedString::from(format!("ob-pick-{key}"))).min_w_0().gap_2().items_center()
                            .h(px(if crate::layout::compact() { crate::layout::TOUCH } else { 38. })).px(px(10.))
                            .rounded(t.radius).border_1().cursor_pointer()
                            .border_color(if on { t.primary } else { ui::hairline(cx) })
                            .bg(if on { t.primary.opacity(0.08) } else { t.background })
                            .hover(|s| s.bg(t.accent))
                            .when_some(crate::logo::tile(crate::logo::for_source(&c.key), 22., cx), |d, l| d.child(l))
                            .child(v_flex().flex_1().min_w_0()
                                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(c.label.clone()))
                                .when_some(self.files.iter().find(|f| f.key == c.key).map(|f| f.format.clone()).filter(|f| !f.is_empty()), |d, f| d.child(
                                    div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap().child(f))))
                            .when(on, |d| d.child(Icon::new(IconName::Check).small().text_color(t.primary)))
                            .on_click(cx.listener(move |this, _, _, cx| { this.w.toggle(&key); cx.notify(); }))
                            .into_any_element()
                    }).collect();
                    // two to a row, in a grid: the columns line up whatever the labels
                    (!chips.is_empty()).then(|| v_flex().gap_2()
                        .child(ui::eyebrow(group.title(), cx))
                        .child(div().grid().grid_cols(2).gap_2().w_full().children(chips)))
                };
                if self.w.catalog.is_empty() {
                    return match self.catalog_note.clone() {
                        Some(e) => v_flex().gap_2()
                            .child(div().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(format!("Couldn't load the list: {e}")))
                            .child(h_flex().child(Button::new("ob-retry").small().ghost().label("Try again")
                                .on_click(cx.listener(|this, _, _, cx| { this.catalog_note = None; this.load(cx); cx.notify(); })))),
                        // the list is never asked for before signing in: say so
                        None if signin::needed() => v_flex().child(muted("Sign in first to see the list: go Back to the sign-in step.".into())),
                        None => v_flex().child(muted("Loading the list…".into())),
                    };
                }
                // a group with nothing in it isn't shown
                v_flex().gap_3().children(Group::ALL.into_iter().filter_map(|g| grid(g, cx)))
            }
            Step::ApiSources => v_flex().child(self.sources.clone()),
            Step::FileSources => {
                let picked: Vec<String> = self.w.files_to_export().iter().map(|c| c.key.clone()).collect();
                let one = picked.len() == 1;
                let shown: Vec<Source> = self.files.iter().filter(|f| picked.contains(&f.key)).cloned().collect();
                v_flex().gap_3()
                    .children(shown.into_iter().map(|f| {
                        // one source: its steps are open; several: closed until asked for
                        let open = one != self.how_toggled.contains(&f.key);
                        let key = f.key.clone();
                        let first = f.how.first().cloned().unwrap_or_default();
                        h_flex().gap_3().items_start()
                            .when_some(crate::logo::tile(crate::logo::for_source(&f.key), 22., cx), |d, l| d.child(l))
                            .child(v_flex().flex_1().min_w_0().gap(px(4.))
                                .child(h_flex().gap_2().items_center().flex_wrap()
                                    .child(div().text_sm().font_medium().child(f.label.clone()))
                                    .when(!f.format.is_empty(), |d| d.child(ui::tag(f.format.clone(), cx))))
                                .when(!open, |d| d.child(muted(first)))
                                .when(open, |d| d.child(super::supported::steps(&f.how, cx))
                                    .when(!f.notes.is_empty(), |d| d.child(muted(f.notes.clone()))))
                                .when(f.how.len() > 1 || !f.notes.is_empty(), |d| d.child(h_flex().child(
                                    Button::new(SharedString::from(format!("ob-how-{key}"))).xsmall().ghost()
                                        .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                        .label(if open { "Fewer details" } else { "All the steps" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.how_toggled.remove(&key) {
                                                this.how_toggled.insert(key.clone());
                                            }
                                            cx.notify();
                                        }))))))
                    }))
                    .child(h_flex().child(Button::new("ob-upload").small().ghost().icon(IconName::ArrowUp).label("Open Upload now")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(OnboardingEvent::OpenUpload)))))
            }
            Step::FirstSync if self.w.synced => v_flex().child(muted("The server is fetching your accounts; the app syncs when it's done.".into())),
            Step::FirstSync => v_flex(),
            Step::Done => self.done_body(cx),
        }
    }

    /// The Protect card (desktop): the switch, on; "use a passphrase
    /// instead"; the passphrase form when the keyring can't keep the key;
    /// the admin's backup key.
    fn protect_body(&mut self, cx: &mut Context<Self>) -> Div {
        #[cfg(not(target_family = "wasm"))]
        {
            let t = cx.theme().clone();
            let plan = onboarding::protect_plan(self.protect_on, self.passphrase, self.keyring);
            let (on, busy) = (self.protect_on, self.busy);
            let row = |id: &'static str, label: &'static str, sub: &'static str, checked: bool, cx: &mut Context<Self>,
                       f: fn(&mut Self, bool)| {
                v_flex().gap(px(4.))
                    .child(Switch::new(id).with_size(ui::switch_size()).checked(checked).label(label).disabled(busy)
                        .on_click(cx.listener(move |this, v: &bool, _, cx| { f(this, *v); cx.notify(); })))
                    .child(ui::caption_wrap(sub, cx))
            };
            let mut d = v_flex().gap_4();
            if self.sealed {
                d = d.child(h_flex().gap_2().items_center().child(ui::dot(fmt::money_in(), 8.))
                    .child(div().text_sm().child(format!("Already encrypted: {}.", finance_core::replica::status().label()))));
            } else {
                d = d.child(row("ob-seal", "Encrypt this computer's copy",
                    "The key stays in your OS keyring, never next to the data. Nothing to type at start.", on, cx, |this, v| this.protect_on = v));
                if on && plan == Protect::Keyring {
                    d = d.child(h_flex().child(Button::new("ob-pass").small().ghost().label("Use a passphrase instead")
                        .on_click(cx.listener(|this, _, _, cx| { this.passphrase = true; cx.notify(); }))));
                }
                if plan == Protect::Passphrase {
                    let (a, b) = (self.pass1.read(cx).value().to_string(), self.pass2.read(cx).value().to_string());
                    let (_, hint) = finance_core::backups::strength(&a);
                    d = d.child(v_flex().gap_2()
                        .child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child(if self.keyring == Some(false) {
                            "There's no OS keyring here, so a passphrase keeps it on. You type it at every start; forget it and the copy is unreadable (it can always be synced again)."
                        } else {
                            "You type this passphrase at every start; forget it and the copy is unreadable (it can always be synced again)."
                        }))
                        .child(h_flex().gap_2().flex_wrap()
                            .child(div().flex_1().min_w(px(180.)).child(Input::new(&self.pass1).mask_toggle()))
                            .child(div().flex_1().min_w(px(180.)).child(Input::new(&self.pass2).mask_toggle())))
                        .child(ui::caption(if b.is_empty() || a == b { hint.to_string() } else { "the two differ".into() }, cx)));
                }
                if !on {
                    d = d.child(ui::caption_wrap("Left as plain files on this disk. You can turn it on later in Settings, Sync & storage.", cx));
                }
            }
            if self.offer_key() {
                d = d.child(row("ob-backups", "Seal the server's nightly backups to this computer",
                    "A key made here: only this computer opens them. Save a copy later in Settings, Sync & storage.", self.seal_backups, cx, |this, v| this.seal_backups = v));
            }
            return d.when_some(self.why_text(), |d, w| d.child(self.why_row(w, cx)));
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = cx;
            v_flex()
        }
    }

    /// The last card: the owner's note (the admin only), then the phone's QR.
    fn done_body(&mut self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let admin = self.who.as_ref().is_some_and(|w| w.admin());
        let owner = h_flex().gap_3().items_start().px(px(14.)).py(px(10.)).rounded(t.radius_lg).border_1().border_color(ui::hairline(cx))
            .child(Icon::new(Lucide::ShieldCheck).size(px(20.)).text_color(t.primary))
            .child(v_flex().flex_1().min_w_0()
                .child(div().text_sm().font_medium().child("You're the admin of this server"))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
                    .child("You own this server: invite others, pair devices and manage accounts in Settings, Admin.")));
        v_flex().gap_3().when(admin, |d| d.child(owner)).child(self.phone_body(cx))
    }

    /// The phone's QR behind a small link (the desktop, a server a phone can
    /// reach).
    fn phone_body(&mut self, cx: &mut Context<Self>) -> Div {
        #[cfg(not(target_family = "wasm"))]
        if let Some(url) = finance_core::doctor::phone_url(&finance_core::cache::server_url()) {
            let t = cx.theme().clone();
            let open = self.qr;
            let qr = finance_core::qr::Qr::encode(&url).map(|q| super::doctor::qr_view(std::rc::Rc::new(q), 132.));
            return v_flex().gap_2()
                .child(h_flex().child(Button::new("ob-qr").small().ghost().icon(Icon::new(Lucide::Smartphone)).label("Open it on your phone")
                    .on_click(cx.listener(|this, _, _, cx| { this.qr = !this.qr; cx.notify(); }))))
                .when(open, |d| d.child(h_flex().gap_3().items_center()
                    .children(qr)
                    .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(url))));
        }
        let _ = cx;
        v_flex()
    }

    /// The card's buttons; None when it has none of its own (the sign-in question).
    fn actions(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let compact = crate::layout::compact();
        let big = |b: Button| if compact { b.large().w_full() } else { b };
        let primary = self.primary_label().map(|label| big(Button::new("ob-next").primary().label(label)
            .loading(self.busy || self.saving || (self.w.step == Step::ApiSources && self.sources.read(cx).busy()))
            .disabled(self.submit.pending())
            .on_click(cx.listener(|this, _, window, cx| this.primary(window, cx)))));
        if self.w.step == Step::Welcome {
            let demo = big(Button::new("ob-try").ghost().label("Just try it (made-up data)")
                .on_click(cx.listener(|this, _, _, cx| this.try_demo(cx))));
            return Some(if compact {
                v_flex().gap_2().pt_2().children(primary).child(demo)
            } else {
                h_flex().gap_3().pt_2().justify_center().children(primary).child(demo)
            });
        }
        let skip = (self.w.step.skippable() && !(self.w.step == Step::FirstSync && self.w.synced))
            .then(|| big(Button::new("ob-skip").ghost().label(if self.w.step == Step::ApiSources { "Skip this one" } else { "Skip" })
                .on_click(cx.listener(|this, _, window, cx| { this.saving = false; this.advance(window, cx) }))));
        let back = self.can_go_back().then(|| big(Button::new("ob-back").ghost().label("Back")
            .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx)))));
        if primary.is_none() && skip.is_none() && back.is_none() {
            return None;
        }
        Some(if compact {
            v_flex().gap_2().pt_2().children(primary).children(skip).children(back)
        } else {
            h_flex().gap_2().pt_2().items_center().children(back).child(div().flex_1()).children(skip).children(primary)
        })
    }

    /// The welcome card's text: the name, the accent line, the promise.
    fn welcome(&self, cx: &App) -> Div {
        let t = cx.theme();
        let compact = crate::layout::compact();
        let (title, accent) = self.heading();
        v_flex().w_full().max_w(px(CARD)).items_center().gap(px(14.)).text_center()
            .child(h_flex().gap_2().items_center()
                .child(div().w(px(22.)).h(px(1.)).bg(t.primary))
                .child(div().text_size(px(11.5)).font_family(t.mono_font_family.clone()).text_color(t.primary).child("LOCAL-FIRST PERSONAL FINANCE")))
            .child(div().text_size(px(if compact { 40. } else { 52. })).font_bold().line_height(relative(1.)).child(title))
            // the editorial contrast of the brand: an italic serif line in the accent
            .child(div().text_size(px(if compact { 24. } else { 30. })).italic().font_family(crate::theme::ACCENT_FONT).text_color(t.primary).child(accent))
            .child(div().text_sm().text_color(t.muted_foreground)
                .child("Every bank, broker and wallet, on a server you own. Private by default."))
    }
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        if std::mem::take(&mut self.fill) {
            let s = |k: &str| self.profile[k].as_str().unwrap_or("").to_string();
            let (n, c, h, city) = (s("name"), s("base_currency"), s("home_country"), s("home_city"));
            for (i, v) in [(&self.name, n), (&self.country, h), (&self.city, city)] {
                if !v.is_empty() {
                    i.update(cx, |i, cx| i.set_value(v, window, cx));
                }
            }
            if !c.is_empty() {
                self.ccy.update(cx, |i, cx| i.set_value(c, window, cx));
            }
        }
        if std::mem::take(&mut self.fill_name) && Self::val(&self.name, cx).is_empty() {
            let n = self.w.profile.name.clone();
            self.name.update(cx, |i, cx| i.set_value(n, window, cx));
        }
        // the keys' card opened by the dev aid: its form, once the catalog is in
        if self.w.step == Step::ApiSources && self.w.current_api().is_some() && !self.api_shown {
            self.api_shown = true;
            self.show_api_card(window, cx);
        }
        // the keys stay with the wizard: a card change drops the focused
        // button with it, and Enter / Esc would then reach nothing. Checked
        // once the frame is drawn, so a field just focused on the new card
        // (not in the last frame yet) keeps the caret.
        if !self.focus.contains_focused(window, cx) {
            let f = self.focus.clone();
            window.on_next_frame(move |window, cx| {
                if !f.contains_focused(window, cx) {
                    window.focus(&f, cx);
                }
            });
        }
        let compact = crate::layout::compact();
        let (n, of) = self.w.progress();
        let step = self.w.step;
        let welcome = step == Step::Welcome;
        let (title, lede) = self.heading();
        let body = self.body(cx);
        let actions = self.actions(cx);
        let note = self.note.clone();
        // signing in is the way in: no leaving it
        let gate = step == Step::SignIn && !self.w.paired;
        // turned away from a set-up server: one card, no count
        let dots = !welcome && !(gate && self.close_when_set_up) && of > 1;
        // the buttons sit under the card on the phone (anchored at the bottom)
        let below = compact;
        let card = v_flex().w_full().max_w(px(CARD)).gap(px(20.))
            .p(px(if compact { 20. } else { 40. }))
            .rounded(t.radius_lg).border_1().border_color(ui::hairline(cx)).bg(t.secondary)
            .when(!compact, |d| d.min_h(px(CARD_MIN_H)))
            .when(!t.is_dark(), |d| d.shadow_sm())
            .child(v_flex().gap(px(8.))
                .child(div().text_size(px(if compact { 22. } else { 26. })).font_semibold().child(title))
                .child(div().text_sm().text_color(t.muted_foreground).child(lede)))
            .child(div().flex_1().child(body))
            .when_some(note, |d, (ok, s)| d.child(div().text_size(px(ts::CAPTION))
                .text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(s)));
        let mut actions = actions;
        let card = if welcome {
            self.welcome(cx).when(!below, |d| d.child(div().pt_4().children(actions.take())))
        } else {
            card.when(!below, |d| d.children(actions.take()))
        };
        let names = self.w.card_names();
        let left = of.saturating_sub(n);
        let progress = v_flex().gap(px(6.)).items_center()
            .child(h_flex().justify_center().children((1..=of).map(|i| {
                let name = SharedString::from(names.get(i - 1).cloned().unwrap_or_default());
                // a taller box than the dot: easy to hover
                h_flex().id(("ob-dot", i)).h(px(14.)).px(px(3.)).items_center()
                    .child(div().h(px(4.)).w(px(if i == n { 18. } else { 6. })).rounded_full()
                        .bg(if i == n { t.primary } else { t.muted_foreground.opacity(0.3) }))
                    .tooltip(move |w, cx| Tooltip::new(name.clone()).build(w, cx))
            })))
            .child(ui::caption(match left { 0 => "Last step".to_string(), 1 => "1 step left".into(), k => format!("{k} steps left") }, cx));
        div().id("onboarding").key_context("Onboarding").track_focus(&self.focus)
            .size_full().relative().bg(t.background).text_color(t.foreground)
            // Enter = the primary button, Esc = Back (before the field under the caret sees them)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                // Ctrl+V (Cmd+V) anywhere on the sign-in card pastes the code, no
                // field to click first; from "Do you have a code?" it goes
                // straight to the code (a copied code signs in in one key)
                if k.key == "v" && (k.modifiers.control || k.modifiers.platform) && !k.modifiers.alt
                    && this.w.step == Step::SignIn && !this.w.paired && this.have_code != Some(false)
                {
                    cx.stop_propagation();
                    this.have_code = Some(true);
                    this.note = None;
                    this.paste_code(window, cx);
                    cx.notify();
                    return;
                }
                if k.modifiers.modified() {
                    return;
                }
                match k.key.as_str() {
                    "enter" => {
                        cx.stop_propagation();
                        // a held key repeats: one press, one request
                        if !ev.is_held {
                            this.primary(window, cx);
                        }
                    }
                    "escape" => {
                        cx.stop_propagation();
                        this.go_back(window, cx);
                    }
                    _ => {}
                }
            }))
            // the dotted cloth runs behind every step, one wave from the welcome on;
            // the welcome's text floats on it (a clear ellipse), the other steps'
            // cards cover it, so it stays dimmer there
            .child(div().absolute().size_full().child(ui::dot_field_with(self.started, if welcome {
                ui::Field { delay_ms: 0, strength: 0.5, hole: (if compact { 150. } else { 290. }, 180.) }
            } else {
                ui::Field { delay_ms: 0, strength: 0.3, hole: (1., 1.) }
            }, cx)))
            .child(div().id("ob-scroll").size_full().overflow_y_scroll()
                .child(v_flex().w_full().min_h_full().items_center()
                    .when(!compact, |d| d.justify_center())
                    .gap(px(if compact { 16. } else { 24. }))
                    .px(px(if compact { 16. } else { 24. }))
                    .pt(px(if compact { 24. } else { 40. }) + crate::layout::safe_top())
                    .pb(px(if compact { 24. } else { 40. }) + crate::layout::safe_bottom())
                    // the card (and the mark over it), centred in what the buttons leave
                    .child(v_flex().w_full().items_center().gap(px(if compact { 16. } else { 24. }))
                        .when(compact, |d| d.flex_1().justify_center())
                        .when(!welcome, |d| d.child(crate::avatar::owner("ob-mark", 40., 11., cx)))
                        .child(div().w_full().max_w(px(CARD)).flex().justify_center().child(ui::page_frame(card, false))))
                    .when(below, |d| d.children(actions.map(|a| div().w_full().max_w(px(CARD)).child(a))))
                    .when(dots, |d| d.child(progress))))
            .when((self.closable && !gate) || (self.cancel && gate), |d| d.child(
                div().absolute().top(px(if compact { 10. } else { 16. }) + crate::layout::safe_top()).right(px(if compact { 10. } else { 16. }))
                    .child(Button::new("ob-close").ghost().icon(IconName::Close)
                        .tooltip(if gate { "Cancel adding this account" } else { "Leave setup" })
                        .when(compact, |b| b.large())
                        // before signing in there's nothing to save: the app drops the account
                        .on_click(cx.listener(move |this, _, _, cx| if gate { cx.emit(OnboardingEvent::Close) } else { this.close(cx) })))))
    }
}
