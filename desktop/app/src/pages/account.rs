//! The server account part of Settings, on the desktop and in the browser:
//!
//! * Device: paired as X or not; pair with a one-time code (`anser-server
//!   pair` on the server, or "Pair another device" in Settings), or the code
//!   this device got when it signed out. The desktop keeps the token
//!   (keyring, else a 0600 file); the browser gets a session cookie. Sign
//!   out and Sign out for good go through the avatar menu's flow
//!   (accounts_menu.rs).
//! * Admin: a new pairing code for another device, an invite for another
//!   person (one account, 24 hours), locking reads (FINANCE_AUTH=all) once
//!   every device is paired, importing .env keys into the encrypted store.
//! * You: the avatar (pages::avatar_edit) and the profile (name, base
//!   currency, home).
//! * Sources: [`super::sources::SourcesView`].
//! * Investments: [`super::invest_setup::InvestSetup`].
//! * Set up again: the onboarding wizard.

use finance_core::client::{Request, WhoAmI};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use super::invest_setup::{InvestChanged, InvestSetup};
use super::sources::{call, json_of, SourcesChanged, SourcesView};
use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

pub enum AccountEvent {
    /// paired, or a source changed: sync again
    Resync,
    /// open the onboarding wizard
    RunSetup,
    /// signed in now (a code): fetch the data
    SignedIn,
    /// Sign out… (false) or Sign out for good… (true): the avatar menu's flow
    SignOut(bool),
    /// Supported sources, an API source's *Set up*: Data sources, its form
    SetUpSource(String),
    /// Supported sources, *Upload*: the Upload page
    OpenUpload,
}

pub struct Account {
    /// Settings' open tab: which of the cards below it shows
    pub tab: super::settings::Tab,
    /// Delete all: what goes (this computer, the server) and what stays
    dz: DeleteAll,
    dz_note: Option<(bool, String)>,
    who: Option<Result<WhoAmI, String>>,
    code: Entity<InputState>,
    device_name: Entity<InputState>,
    pairing: bool,
    /// one request per code (core's CodeSubmit), and what the slots held before the last change
    submit: finance_core::signin::CodeSubmit,
    slots: String,
    pair_note: Option<(bool, String)>,
    new_code: Option<(String, i64)>,
    invite: Option<(String, i64)>,
    pub avatar: Entity<super::avatar_edit::AvatarEdit>,
    admin_note: Option<(bool, String)>,
    /// Admin, Users: the accounts and their devices, and the one being removed
    users: Vec<Value>,
    devices: Vec<Value>,
    /// devices signed out with a code to jump back in (an admin can end it)
    signed_out: Vec<Value>,
    users_note: Option<(bool, String)>,
    confirm_remove: Option<i64>,
    p_name: Entity<InputState>,
    p_ccy: Entity<InputState>,
    p_country: Entity<InputState>,
    p_city: Entity<InputState>,
    profile: Value,
    profile_note: Option<(bool, String)>,
    /// the profile arrived: put it in the fields on the next render
    fill_profile: bool,
    pub sources: Entity<SourcesView>,
    /// Settings, Supported sources
    pub supported: Entity<super::supported::Supported>,
    pub invest: Entity<InvestSetup>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<AccountEvent> for Account {}

fn web() -> bool {
    cfg!(target_family = "wasm")
}

fn note_el(n: &Option<(bool, String)>, cx: &App) -> Option<Div> {
    n.as_ref().map(|(ok, s)| div().text_size(px(ts::CAPTION))
        .text_color(if *ok { cx.theme().muted_foreground } else { fmt::money_out() }).child(s.clone()))
}

fn row(label: &str, value: impl IntoElement, cx: &App) -> Div {
    ui::field_row(label, value, cx)
}

impl Account {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // the device's locale, as the placeholders of currency and country (as in onboarding)
        let locale = finance_core::locale::detect();
        let input = |ph: &str, v: String, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v))
        };
        #[cfg(not(target_family = "wasm"))]
        let dn = finance_core::auth::default_device_name();
        #[cfg(target_family = "wasm")]
        let dn = String::new();
        let sources = cx.new(|cx| SourcesView::new(None, cx));
        let supported = cx.new(super::supported::Supported::new);
        let invest = cx.new(|cx| InvestSetup::new(window, cx));
        let avatar = cx.new(|cx| super::avatar_edit::AvatarEdit::new(window, cx));
        let subs = vec![
            cx.subscribe(&sources, |_, _, _: &SourcesChanged, cx| cx.emit(AccountEvent::Resync)),
            cx.subscribe(&invest, |_, _, _: &InvestChanged, cx| cx.emit(AccountEvent::Resync)),
            cx.subscribe(&supported, |_, _, ev: &super::supported::SupportedEvent, cx| cx.emit(match ev {
                super::supported::SupportedEvent::SetUp(k) => AccountEvent::SetUpSource(k.clone()),
                super::supported::SupportedEvent::Upload => AccountEvent::OpenUpload,
            })),
            cx.subscribe(&avatar, |this, _, ev: &super::avatar_edit::AvatarChanged, cx| {
                this.profile = ev.0.clone();
                cx.notify();
            }),
        ];
        let mut a = Account {
            tab: Default::default(),
            who: None,
            code: input("ABCD-EFGH", String::new(), window, cx),
            device_name: input("This device's name", dn, window, cx),
            pairing: false,
            submit: finance_core::signin::CodeSubmit::default(),
            slots: String::new(),
            pair_note: None,
            new_code: None,
            invite: None,
            avatar,
            admin_note: None,
            users: vec![],
            devices: vec![],
            signed_out: vec![],
            users_note: None,
            confirm_remove: None,
            dz: DeleteAll { here: true, server: false, keep_backups: true, keep_keys: false, keep_backup_key: true },
            dz_note: None,
            p_name: input("Your name", String::new(), window, cx),
            p_ccy: input(&locale.0, String::new(), window, cx),
            p_country: input(&locale.1, String::new(), window, cx),
            p_city: input("City (optional)", String::new(), window, cx),
            profile: Value::Null,
            profile_note: None,
            fill_profile: false,
            sources,
            supported,
            invest,
            _subs: subs,
        };
        // the code slots: tidy what's typed, pair on the eighth character
        let sub = cx.subscribe_in(&a.code, window, |this, state, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.pair_asked(cx);
            }
            if matches!(ev, InputEvent::Change) {
                let code = crate::codeslots::clean(state, Some(&this.slots), window, cx);
                this.slots = code.clone();
                // once per code (see onboarding.rs): a refused code isn't resent by itself
                if this.submit.auto(&code) {
                    this.pair(cx);
                }
                cx.notify();
            }
        });
        a._subs.push(sub);
        a.refresh(window, cx);
        a
    }

    /// Ask the server again: who this device is, the profile, the sources.
    pub fn refresh(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_cx(cx);
    }

    /// What the server said about this device and its user, if it answered.
    pub fn who(&self) -> Option<WhoAmI> {
        self.who.as_ref().and_then(|w| w.as_ref().ok()).cloned()
    }

    /// The answer to whoami: kept, and on the desktop the account's name on
    /// its profile (the avatar menu lists it).
    fn set_who(&mut self, r: finance_core::client::Reply) {
        let w: Result<WhoAmI, String> = json_of(r).map(|v| serde_json::from_value(v).unwrap_or_default());
        #[cfg(not(target_family = "wasm"))]
        if let Ok(WhoAmI { user: Some(u), .. }) = &w {
            let _ = finance_core::profiles::set_identity(&u.name, Some(u.id));
        }
        self.who = Some(w);
    }

    /// What the user has (a new load): Supported sources' ticks.
    pub fn set_capabilities(&mut self, caps: finance_core::capabilities::Capabilities, cx: &mut Context<Self>) {
        self.supported.update(cx, |v, cx| v.set_capabilities(caps, cx));
    }

    /// Supported sources' *Set up*: the form of this API source.
    pub fn open_source_form(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.sources.update(cx, |s, cx| s.open_form(key, window, cx));
    }

    /// whoami always; the profile and the sources only once signed in (a
    /// turned-away device would only hear 401 again).
    pub fn refresh_cx(&mut self, cx: &mut Context<Self>) {
        call(Request::new("GET", "/api/whoami"), cx, |this, r, cx| {
            this.set_who(r);
            if this.admin() {
                this.refresh_users(cx);
            }
        });
        if finance_core::signin::needed() {
            return;
        }
        call(Request::new("GET", "/api/profile"), cx, |this, r, cx| {
            if let Ok(v) = json_of(r) {
                this.avatar.update(cx, |a, cx| a.set_profile(v.clone(), cx));
                this.profile = v;
                this.fill_profile = true;
            }
        });
        self.sources.update(cx, |s, cx| s.refresh(cx));
        self.invest.update(cx, |s, cx| s.refresh(cx));
    }

    /// The Paste button by the code slots: the code goes in (and pairs once
    /// whole, through the slots' subscription), or a note says why not.
    fn paste_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paste = crate::codeslots::paste(&self.code, window, cx);
        cx.spawn(async move |this, cx| {
            if let Err(why) = paste.await {
                let _ = this.update(cx, |this, cx| {
                    this.pair_note = Some((false, why.into()));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Ctrl+V (Cmd+V) with no field focused, on the Device card while this
    /// device is signed out: pastes the code, like the wizard's sign-in step.
    /// True when the key was taken.
    pub fn paste_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let k = &ev.keystroke;
        let signed_out = self.who.as_ref().and_then(|w| w.as_ref().ok()).is_some_and(|w| !w.paired);
        if k.key != "v" || !(k.modifiers.control || k.modifiers.platform) || k.modifiers.alt
            || self.tab != super::settings::Tab::Account || !signed_out
        {
            return false;
        }
        self.pair_note = None;
        self.paste_code(window, cx);
        cx.notify();
        true
    }

    /// Enter or Sign in: the code in the slots goes, even one already sent and
    /// refused (they ask again knowingly); not while a request is out.
    fn pair_asked(&mut self, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_string();
        if let Some(why) = finance_core::signin::code_problem(&code) {
            self.pair_note = Some((false, why.into()));
            cx.notify();
        } else if self.submit.asked(&code, false) {
            self.pair(cx);
        }
    }

    fn pair(&mut self, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_string();
        if let Some(why) = finance_core::signin::code_problem(&code) {
            self.pair_note = Some((false, why.into()));
            cx.notify();
            return;
        }
        self.pairing = true;
        self.pair_note = None;
        #[cfg(target_family = "wasm")]
        {
            // the browser: a session cookie, then the data, in the page
            let body = json!({"code": code, "resume_key": finance_core::signin::resume_key()});
            call(Request::new("POST", "/api/login").json(&body), cx, |this, r, cx| {
                this.pairing = false;
                this.submit.answered();
                match r {
                    Ok(resp) if resp.ok() => {
                        this.pair_note = Some((true, "Signed in.".into()));
                        finance_core::signin::signed_in();
                        cx.emit(AccountEvent::SignedIn);
                        this.refresh_cx(cx);
                    }
                    Ok(resp) => this.pair_note = Some((false, finance_core::signin::refusal(resp.status, &resp.json()))),
                    Err(e) => this.pair_note = Some((false, e)),
                }
            });
        }
        #[cfg(not(target_family = "wasm"))]
        {
            let name = self.device_name.read(cx).value().trim().to_string();
            let server = finance_core::cache::server_url();
            let task = cx.background_spawn(async move { finance_core::auth::pair(&server, &code, &name).map_err(|e| format!("{e:#}")) });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.pairing = false;
                    this.submit.answered();
                    match r {
                        Ok(d) => {
                            this.pair_note = Some((true, format!("Paired as {}.", d.name)));
                            finance_core::signin::signed_in();
                            cx.emit(AccountEvent::SignedIn);
                            call(Request::new("GET", "/api/whoami"), cx, |this, r, _| this.set_who(r));
                            this.sources.update(cx, |s, cx| s.refresh(cx));
                            this.invest.update(cx, |s, cx| s.refresh(cx));
                        }
                        Err(e) => this.pair_note = Some((false, e)),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// The browser's session ends here, with no code (the account or the
    /// server just went): the page again, on the sign-in screen.
    #[cfg(target_family = "wasm")]
    fn unpair(&mut self, cx: &mut Context<Self>) {
        call(Request::new("POST", "/api/logout"), cx, |this, _, _| {
            this.who = None;
            // the page reloads without a session: the sign-in screen
            finance_core::signin::user_action();
            let _ = finance_core::sync::pull();
        });
    }

    /// Danger zone: the caller's own account goes (a member's; the owner
    /// has Delete all), with its data or not. Then this device is signed
    /// out: another account on this computer, else the sign-in screen.
    fn delete_me(&mut self, purge: bool, cx: &mut Context<Self>) {
        call(Request::new("POST", "/api/me/delete").json(&json!({"purge": purge})), cx, |this, r, cx| match json_of(r) {
            Ok(_) => {
                #[cfg(not(target_family = "wasm"))]
                {
                    let me = finance_core::profiles::active();
                    if finance_core::profiles::remove(&me.id).is_err() {
                        let _ = finance_core::auth::forget(&finance_core::cache::server_url());
                    }
                    crate::app::accounts_menu::relaunch_with(true);
                }
                #[cfg(target_family = "wasm")]
                this.unpair(cx);
            }
            Err(e) => {
                this.dz_note = Some((false, e));
                cx.notify();
            }
        });
    }

    /// Danger zone, Delete all: the server (it restarts empty), this
    /// computer (profiles::wipe_local), or both; then a first run.
    fn delete_all(&mut self, cx: &mut Context<Self>) {
        let dz = self.dz;
        let here = move || {
            #[cfg(not(target_family = "wasm"))]
            {
                if dz.here {
                    let _ = finance_core::profiles::wipe_local(dz.keep_backup_key);
                }
                crate::app::accounts_menu::relaunch_with(true);
            }
        };
        if !dz.server {
            here();
            #[cfg(target_family = "wasm")]
            self.unpair(cx);
            return;
        }
        let body = json!({"confirm": "DELETE ALL", "keep_backups": dz.keep_backups, "keep_keys": dz.keep_keys});
        call(Request::new("POST", "/api/admin/wipe").json(&body), cx, move |this, r, cx| match json_of(r) {
            Ok(_) => {
                here();
                #[cfg(target_family = "wasm")]
                this.unpair(cx);
            }
            Err(e) => {
                this.dz_note = Some((false, e));
                cx.notify();
            }
        });
    }

    pub fn admin(&self) -> bool {
        self.who().is_some_and(|w| w.admin())
    }

    /// Admin, Users: the accounts and the devices (no one's data).
    fn refresh_users(&mut self, cx: &mut Context<Self>) {
        call(Request::new("GET", "/api/admin/users"), cx, |this, r, _| {
            if let Ok(v) = json_of(r) {
                this.users = v["users"].as_array().cloned().unwrap_or_default();
            }
        });
        call(Request::new("GET", "/api/admin/devices"), cx, |this, r, _| {
            if let Ok(v) = json_of(r) {
                this.devices = v["devices"].as_array().cloned().unwrap_or_default();
                this.signed_out = v["signed_out"].as_array().cloned().unwrap_or_default();
            }
        });
    }

    fn remove_user(&mut self, id: i64, cx: &mut Context<Self>) {
        call(Request::new("DELETE", &format!("/api/admin/users/{id}")), cx, |this, r, cx| {
            match json_of(r) {
                Ok(v) => this.users_note = Some((true, format!(
                    "Removed, with {} device(s). Their data stays on the server's disk until someone deletes it there \
                    (anser-server user remove ID --purge on the server).", v["devices"].as_i64().unwrap_or(0)))),
                Err(e) => this.users_note = Some((false, e)),
            }
            this.confirm_remove = None;
            this.refresh_users(cx);
        });
    }

    fn revoke_device(&mut self, id: i64, cx: &mut Context<Self>) {
        call(Request::new("DELETE", &format!("/api/admin/devices/{id}")), cx, |this, r, cx| {
            match json_of(r) {
                Ok(_) => this.users_note = Some((true, "Device signed out.".into())),
                Err(e) => this.users_note = Some((false, e)),
            }
            this.refresh_users(cx);
        });
    }

    /// A pairing code for another of this account's own devices.
    fn my_code(&mut self, cx: &mut Context<Self>) {
        call(Request::new("POST", "/api/me/pair-code"), cx, |this, r, _| match json_of(r) {
            Ok(v) => this.new_code = Some((v["code"].as_str().unwrap_or("").to_string(), v["expires"].as_i64().unwrap_or(0))),
            Err(e) => this.pair_note = Some((false, e)),
        });
    }

    fn invite(&mut self, cx: &mut Context<Self>) {
        call(Request::new("POST", "/api/admin/invite"), cx, |this, r, _| match json_of(r) {
            Ok(v) => this.invite = Some((v["code"].as_str().unwrap_or("").to_string(), v["expires"].as_i64().unwrap_or(0))),
            Err(e) => this.admin_note = Some((false, e)),
        });
    }

    fn lock_reads(&mut self, cx: &mut Context<Self>) {
        call(Request::new("POST", "/api/admin/auth-mode").json(&json!({"mode": "all"})), cx, |this, r, _| match json_of(r) {
            Ok(_) => {
                this.admin_note = Some((true, "Reads now need a paired device too (FINANCE_AUTH=all).".into()));
                if let Some(Ok(w)) = this.who.as_mut() {
                    w.auth_mode = "all".into();
                }
            }
            Err(e) => this.admin_note = Some((false, e)),
        });
    }

    fn import_env(&mut self, cx: &mut Context<Self>) {
        call(Request::new("POST", "/api/secrets/import-env"), cx, |this, r, cx| match json_of(r) {
            Ok(v) => {
                let keys: Vec<String> = v["imported"].as_array().cloned().unwrap_or_default().iter().filter_map(|k| k.as_str().map(String::from)).collect();
                this.admin_note = Some((true, if keys.is_empty() { "Nothing to import: .env has no source keys.".into() } else {
                    format!("Imported {} into the encrypted store. The server kept .env.bak and commented the lines out of .env.", keys.join(", "))
                }));
                this.sources.update(cx, |s, cx| s.refresh(cx));
                this.invest.update(cx, |s, cx| s.refresh(cx));
            }
            Err(e) => this.admin_note = Some((false, e)),
        });
    }

    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let mut p = self.profile.as_object().cloned().unwrap_or_default();
        let val = |i: &Entity<InputState>, cx: &App| i.read(cx).value().trim().to_string();
        p.insert("name".into(), json!(val(&self.p_name, cx)));
        p.insert("base_currency".into(), json!(val(&self.p_ccy, cx).to_uppercase()));
        p.insert("home_country".into(), json!(val(&self.p_country, cx).to_uppercase()));
        p.insert("home_city".into(), json!(val(&self.p_city, cx)));
        call(Request::new("PUT", "/api/profile").json(&Value::Object(p)), cx, |this, r, cx| match json_of(r) {
            Ok(v) => {
                this.profile = v["profile"].clone();
                crate::avatar::set_spec(&this.profile, cx);
                this.profile_note = Some((true, "Saved.".into()));
            }
            Err(e) => this.profile_note = Some((false, e)),
        });
    }
}

impl Render for Account {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        if std::mem::take(&mut self.fill_profile) {
            let s = |k: &str| self.profile[k].as_str().unwrap_or("").to_string();
            let (n, c, h, city) = (s("name"), s("base_currency"), s("home_country"), s("home_city"));
            self.p_name.update(cx, |i, cx| i.set_value(n, window, cx));
            self.p_ccy.update(cx, |i, cx| i.set_value(c, window, cx));
            self.p_country.update(cx, |i, cx| i.set_value(h, window, cx));
            self.p_city.update(cx, |i, cx| i.set_value(city, window, cx));
        }
        let who = self.who.clone();
        let paired = who.as_ref().and_then(|w| w.as_ref().ok()).is_some_and(|w| w.paired);
        let admin = who.as_ref().and_then(|w| w.as_ref().ok()).is_some_and(|w| w.admin());

        // ---- device ----
        let status = match &who {
            None => div().text_color(t.muted_foreground).child("asking the server…"),
            Some(Err(e)) => div().text_color(fmt::money_out()).child(format!("the server didn't answer: {}", e.chars().take(120).collect::<String>())),
            Some(Ok(w)) => h_flex().gap_2().items_center()
                .child(ui::dot(if w.paired { fmt::money_in() } else { t.warning }, 7.))
                .child(w.label())
                .when(!w.auth_mode.is_empty(), |d| d.child(ui::tag(format!("auth: {}", w.auth_mode), cx))),
        };
        let no_data = who.as_ref().and_then(|w| w.as_ref().ok()).is_some_and(|w| w.no_data_yet());
        let mut device = ui::card(cx).gap_2().child(row("This device", status, cx))
            .when(no_data, |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child(
                "Your account has no data on this server yet: the admin restarts the server to make it.")));
        if !paired {
            device = device
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(
                    "Pair once with a one-time code: run anser-server pair on the server, or Pair another device on one that is signed in. \
                    The code you got when this device signed out works here too."))
                .when(!web(), |d| d.child(div().w(px(200.)).child(Input::new(&self.device_name).small())))
                .child(crate::codeslots::render("acct-code-slots", &self.code, cx.listener(|this, _, window, cx| this.paste_code(window, cx)), cx))
                .child(Button::new("acct-sign-in").small().primary().label("Sign in").loading(self.pairing).disabled(self.pairing)
                    .on_click(cx.listener(|this, _, _, cx| this.pair_asked(cx))))
                .when(self.pairing, |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("Signing in…")));
        } else {
            device = device.child(h_flex().gap_2()
                .child(Button::new("acct-my-code").small().outline().icon(IconName::Plus).label("Pair another device")
                    .tooltip("A one-time code for another phone, laptop or browser of yours")
                    .on_click(cx.listener(|this, _, _, cx| this.my_code(cx))))
                .child(Button::new("acct-unpair").small().ghost().label("Sign out…")
                    .tooltip("Signs this device out, with a code to jump back in here")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountEvent::SignOut(false)))))
                .child(Button::new("acct-unpair-good").small().ghost().label("Sign out for good…")
                    .tooltip("A shared or lost device: no code to come back with")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountEvent::SignOut(true))))));
            if let Some((code, exp)) = self.new_code.clone() {
                let until = chrono::DateTime::from_timestamp_millis(exp)
                    .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default();
                device = device.child(code_row("acct-copy-code", &code, format!("one device, until {until}: enter it on that device's sign-in screen"), cx));
            }
        }
        #[cfg(not(target_family = "wasm"))]
        {
            device = device.child(row("Token kept in", div().text_xs().text_color(t.muted_foreground).child(finance_core::auth::store_label()), cx));
        }
        device = device.children(note_el(&self.pair_note, cx));

        // ---- admin ----
        let admin_card = admin.then(|| {
            let reads_open = who.as_ref().and_then(|w| w.as_ref().ok()).is_some_and(|w| w.reads_open());
            let from_env = who.as_ref().and_then(|w| w.as_ref().ok()).is_some_and(|w| w.auth_mode_from_env);
            ui::card(cx).gap_2()
                .child(h_flex().gap_2().flex_wrap()
                    .child(Button::new("acct-invite").small().outline().icon(IconName::Plus).label("Invite someone")
                        .tooltip("A one-time code, valid 24 hours, for one new account")
                        .on_click(cx.listener(|this, _, _, cx| this.invite(cx))))
                    .when(reads_open && !from_env, |d| d.child(Button::new("acct-lock").small().outline().label("Lock reads (FINANCE_AUTH=all)")
                        .tooltip("Every request needs a paired device. Pair the laptop and the phone first.")
                        .on_click(cx.listener(|this, _, _, cx| this.lock_reads(cx)))))
                    .child(Button::new("acct-import").small().ghost().label("Import keys from .env")
                        .on_click(cx.listener(|this, _, _, cx| this.import_env(cx)))))
                .when_some(self.invite.clone(), |d, (code, exp)| {
                    let until = chrono::DateTime::from_timestamp_millis(exp)
                        .map(|d| d.with_timezone(&chrono::Local).format("%a %H:%M").to_string()).unwrap_or_default();
                    d.child(code_row("acct-copy-invite", &code, format!("an invite for one new account, until {until}: on the sign-in screen, No, Create a new account. \
                        The account gets data of its own: it never sees yours, nor you its."), cx))
                })
                .when(reads_open, |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.warning)
                    .child("Reads are open to every tailnet device: lock them once your devices are paired.")))
                .children(note_el(&self.admin_note, cx))
        });

        // ---- admin: users ----
        let me = self.who().and_then(|w| w.user.map(|u| u.id)).unwrap_or(0);
        let this_device = self.who().and_then(|w| w.device.map(|d| d.id)).unwrap_or(0);
        let users_card = admin.then(|| {
            let rows = self.users.iter().enumerate().map(|(i, u)| {
                let id = u["id"].as_i64().unwrap_or(0);
                let name = u["name"].as_str().unwrap_or("").to_string();
                let owner = u["owner"].as_bool().unwrap_or(false);
                let devs: Vec<&Value> = self.devices.iter().filter(|d| d["user_id"].as_i64() == Some(id)).collect();
                let confirming = self.confirm_remove == Some(id);
                let n2 = name.clone();
                let head = h_flex().gap_2().items_center().flex_wrap()
                    .child(div().text_sm().font_semibold().child(name.clone()))
                    .child(ui::tag(if u["role"] == "admin" { "admin" } else { "user" }, cx))
                    .when(owner, |d| d.child(ui::tag("owner of this server", cx)))
                    .when(id == me, |d| d.child(ui::tag("you", cx)))
                    .when_some(u["email"].as_str().map(String::from), |d, e| d.child(div().text_xs().text_color(t.muted_foreground).child(e)))
                    .child(div().flex_1())
                    .when(!owner && id != me && !confirming, |d| d.child(Button::new(("user-rm", i)).xsmall().ghost().label("Remove…")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_remove = Some(id);
                            cx.notify();
                        }))));
                let mut devices = vec![];
                for (j, d) in devs.into_iter().enumerate() {
                    let did = d["id"].as_i64().unwrap_or(0);
                    let seen = d["last_seen"].as_i64().and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|d| d.with_timezone(&chrono::Local).format("last seen %d %b %H:%M").to_string()).unwrap_or_else(|| "never seen".into());
                    devices.push(h_flex().gap_2().items_center().pl(px(12.)).text_xs().text_color(t.muted_foreground)
                        .child(div().child(d["name"].as_str().unwrap_or("").to_string()))
                        .child(div().child(seen))
                        .when(did == this_device, |r| r.child(ui::tag("this device", cx)))
                        .when(did != this_device, |r| r.child(Button::new(("dev-rm", i * 1000 + j)).xsmall().ghost().label("Sign out")
                            .tooltip("Revoke this device's token: it has to pair again")
                            .on_click(cx.listener(move |this, _, _, cx| this.revoke_device(did, cx))))));
                }
                for (j, d) in self.signed_out.iter().filter(|d| d["user_id"].as_i64() == Some(id)).enumerate() {
                    let did = d["id"].as_i64().unwrap_or(0);
                    let when = d["signed_out"].as_i64().and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|d| d.with_timezone(&chrono::Local).format("signed out %d %b %H:%M").to_string()).unwrap_or_default();
                    devices.push(h_flex().gap_2().items_center().pl(px(12.)).text_xs().text_color(t.muted_foreground)
                        .child(div().child(d["name"].as_str().unwrap_or("").to_string()))
                        .child(div().child(when))
                        .child(ui::tag("can jump back in", cx))
                        .child(Button::new(("dev-out-rm", i * 1000 + j)).xsmall().ghost().label("Sign out")
                            .tooltip("End its code to jump back in: it has to pair again")
                            .on_click(cx.listener(move |this, _, _, cx| this.revoke_device(did, cx)))));
                }
                v_flex().gap_1().py_1()
                    .child(head)
                    .children(devices)
                    .when(confirming, |d| d.child(h_flex().gap_2().items_center().flex_wrap()
                        .child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child(format!(
                            "Remove {n2}? Their devices are signed out and the account goes. Their data stays on the server's disk.")))
                        .child(Button::new(("user-rm-go", i)).xsmall().danger().label("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_user(id, cx))))
                        .child(Button::new(("user-rm-no", i)).xsmall().ghost().label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_remove = None;
                                cx.notify();
                            })))))
            }).collect::<Vec<_>>();
            ui::card(cx).gap_1()
                .children(rows)
                .child(div().h(px(1.)).bg(ui::hairline(cx)))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(
                    "Each account has its own data on the server, its own sources and keys. An admin manages accounts and \
                    devices here, but can't open anyone else's data through the app. (On the server's disk, whoever runs \
                    it can read every account's files.)"))
                .children(note_el(&self.users_note, cx))
        });

        // ---- you ----
        let field = |label: &str, i: &Entity<InputState>, w: f32, cx: &App| {
            v_flex().gap(px(3.)).child(div().text_size(px(ts::CAPTION)).text_color(cx.theme().muted_foreground).child(label.to_string()))
                .child(div().w(px(w)).child(Input::new(i).small()))
        };
        let you = ui::card(cx).gap_2()
            .child(self.avatar.clone())
            .child(div().h(px(1.)).bg(ui::hairline(cx)))
            .child(h_flex().gap_3().flex_wrap().items_end()
                .child(crate::avatar::owner("acct-avatar", 32., 8., cx))
                .child(field("Name", &self.p_name, 200., cx))
                .child(field("Base currency", &self.p_ccy, 80., cx))
                .child(field("Home country", &self.p_country, 80., cx))
                .child(field("Home city", &self.p_city, 160., cx)))
            .child(h_flex().gap_2()
                .child(Button::new("acct-profile").small().outline().label("Save").on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))))
                .child(Button::new("acct-setup").small().ghost().label("Run setup again")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountEvent::RunSetup)))))
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
                .child("The name helps recognise transfers between your own accounts. Amounts are shown in the base currency."))
            .children(note_el(&self.profile_note, cx));

        // ---- danger zone ----
        let owner = self.who().and_then(|w| w.user).is_some_and(|u| u.id == 1);
        let dz_title = |cx: &App| ui::section("Danger zone", "can't be undone", cx);
        let mine = ui::card(cx).gap_2().border_color(fmt::money_out().opacity(0.5))
            .when(owner, |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
                .child("You own this server, so your account can't be deleted on its own: Delete all (Settings, Admin) removes the server's data instead.")))
            .when(!owner && paired, |d| d
                .child(h_flex().gap_3().items_center().flex_wrap()
                    .child(Button::new("dz-me").small().outline().label("Delete my account…").on_click(cx.listener(|_, _, window, cx| {
                        let view = cx.entity().downgrade();
                        crate::danger::confirm("Delete your account?", "Your account and every device signed in with it are removed. \
                            Your data stays on the server, so an admin can still restore it for you.".into(), "Delete my account", None,
                            std::rc::Rc::new(move |_, cx| { let _ = view.update(cx, |this, cx| this.delete_me(false, cx)); }), window, cx);
                    })))
                    .child(ui::caption("keeps your data on the server", cx)))
                .child(h_flex().gap_3().items_center().flex_wrap()
                    .child(Button::new("dz-me-all").small().danger().label("Delete my account and data…").on_click(cx.listener(|_, _, window, cx| {
                        let view = cx.entity().downgrade();
                        crate::danger::confirm("Delete your account and all your data?", "Your account, your devices, your transactions, \
                            uploads, keys and settings on the server are deleted for good.".into(), "Delete everything of mine", Some("DELETE"),
                            std::rc::Rc::new(move |_, cx| { let _ = view.update(cx, |this, cx| this.delete_me(true, cx)); }), window, cx);
                    })))
                    .child(ui::caption("nothing of yours is kept", cx))))
            .children(note_el(&self.dz_note, cx));
        let dz = self.dz;
        let check = |id: &'static str, label: &'static str, on: bool, f: fn(&mut DeleteAll, bool), cx: &mut Context<Self>| {
            Checkbox::new(id).label(label).checked(on).on_click(cx.listener(move |this, v: &bool, _, cx| { f(&mut this.dz, *v); cx.notify(); }))
        };
        let here_label = if web() { "This browser (signs it out)" } else { "This computer: every account, local copy, token and setting" };
        let everything = ui::card(cx).gap_2().border_color(fmt::money_out().opacity(0.5))
            .child(div().text_sm().font_semibold().child("Delete all"))
            .child(check("dz-here", here_label, dz.here, |d, v| d.here = v, cx))
            .when(dz.here && !web(), |d| d.child(div().pl_6().child(check("dz-keep-bk", "Keep this computer's backup key (it alone opens the server's backups)", dz.keep_backup_key, |d, v| d.keep_backup_key = v, cx))))
            .child(check("dz-server", "The server: every account, device, upload, database and key", dz.server, |d, v| d.server = v, cx))
            .when(dz.server, |d| d.child(v_flex().pl_6().gap_1()
                .child(check("dz-keep-backups", "Keep the nightly backups (restore later with Import)", dz.keep_backups, |d, v| d.keep_backups = v, cx))
                .child(check("dz-keep-keys", "Keep the API keys", dz.keep_keys, |d, v| d.keep_keys = v, cx))))
            .child(h_flex().pt_1().child(Button::new("dz-all").small().danger().label("Delete all…").disabled(!dz.here && !dz.server)
                .on_click(cx.listener(|this, _, window, cx| {
                    let dz = this.dz;
                    let what = match (dz.here, dz.server) {
                        (true, true) => "this computer and the server",
                        (false, true) => "the server",
                        _ => if web() { "this browser" } else { "this computer" },
                    };
                    let mut body = format!("Everything on {what} is deleted and the app starts over as if new.");
                    if dz.server {
                        body += if dz.keep_backups { " The server keeps its nightly backups." } else { " The server's backups go too." };
                        body += if dz.keep_keys { " The API keys stay." } else { " The API keys go." };
                    }
                    let view = cx.entity().downgrade();
                    crate::danger::confirm("Delete all?", body, "Delete all", Some("DELETE ALL"),
                        std::rc::Rc::new(move |_, cx| { let _ = view.update(cx, |this, cx| this.delete_all(cx)); }), window, cx);
                }))))
            .children(note_el(&self.dz_note, cx));

        use super::settings::Tab;
        let column = || v_flex().flex_1().min_w(crate::layout::col(360.)).gap_2();
        let two = |a: Div, b: Div| h_flex().w_full().gap_4().items_start().flex_wrap().child(a).child(b);
        match self.tab {
            Tab::Account => v_flex().w_full().gap_4().child(two(
                column().child(ui::section("You", "profile, on the server", cx)).child(you),
                column().child(ui::section("Device", "pairing with the server", cx)).child(device)))
                .child(v_flex().gap_2().child(dz_title(cx)).child(mine)),
            Tab::Data => two(
                column().child(ui::section("Sources", "API keys, kept encrypted on the server", cx)).child(self.sources.clone()),
                column().child(ui::section("Investments", "the accounts money goes to, and their fees", cx)).child(self.invest.clone())),
            Tab::Admin => v_flex().w_full().gap_4().child(two(
                column().children(admin_card.map(|c| v_flex().gap_2().child(ui::section("Admin", "devices, access, keys", cx)).child(c))),
                column().children(users_card.map(|c| v_flex().gap_2().child(ui::section("Users", "the accounts on this server", cx)).child(c)))))
                .when(admin, |d| d.child(v_flex().gap_2().child(dz_title(cx)).child(everything))),
            Tab::Supported => v_flex().w_full().child(self.supported.clone()),
            Tab::General | Tab::Home | Tab::Sync | Tab::Assistants => h_flex(),
        }
    }
}

/// A one-time code, big, with a Copy button and what it is for.
fn code_row(id: &'static str, code: &str, what: String, cx: &App) -> Div {
    let t = cx.theme();
    let shown = finance_core::signin::format_code(code);
    let copied = shown.clone();
    h_flex().gap_3().items_center().flex_wrap()
        .child(div().text_xl().font_semibold().font_family(t.mono_font_family.clone()).child(shown))
        .child(Button::new(id).xsmall().outline().icon(IconName::Copy).label("Copy")
            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))))
        .child(div().flex_1().min_w(px(200.)).text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(what))
}

/// Delete all's choices (Settings, Admin, Danger zone).
#[derive(Clone, Copy)]
struct DeleteAll {
    here: bool,
    server: bool,
    keep_backups: bool,
    keep_keys: bool,
    keep_backup_key: bool,
}
