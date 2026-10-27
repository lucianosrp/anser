//! Settings → Data sources (and the onboarding wizard's "Connect accounts" step):
//! every API source with its state (configured, from where, the last 4
//! characters of each key, the last error), an Add/Edit form with the
//! catalog's fields and instructions, Test, then Save (the server tests
//! again and refuses a key that can trade or withdraw), and Remove.
//!
//! The wizard shows one source per card ([`SourcesView::focus_one`]): just
//! its fields, how to make the key behind a disclosure, and the wizard's own
//! Continue saves it ([`SourcesView::save`]).
//!
//! Below the API sources, Account countries (Phase 4): where each bank or
//! card account is, so a trip is told from home (finance_core::base). The
//! catalog's default (an HSBC Hong Kong account is in Hong Kong), another
//! country, or none (its rows are placed by their currency). Kept in
//! `user_settings.accounts`.
//!
//! The same view on the desktop and in the browser: it talks to the server
//! through `finance_core::client` (the device token, or the browser's
//! session cookie). Keys typed here go to the server and are never shown
//! again; an empty field on Edit keeps the stored value.

use finance_core::client::{self, Request};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

/// A source was saved or removed (the app re-syncs: pages may appear).
pub struct SourcesChanged;

pub struct SourcesView {
    /// show only these API keys (the wizard); None = all
    only: Option<Vec<String>>,
    catalog: Option<Value>,
    status: Option<Value>,
    load_error: Option<String>,
    editing: Option<String>,
    inputs: Vec<(String, Entity<InputState>)>,
    busy: bool,
    /// (ok, text) of the last test / save / remove
    note: Option<(bool, String)>,
    /// dev/screenshot aid: FINANCE_DESKTOP_SOURCE_EDIT=binance opens its form
    edit_on_load: Option<String>,
    /// the wizard's card: one source, its form always open, no buttons
    single: bool,
    /// the card's "How to make a read-only key" is open
    how: bool,
}

impl EventEmitter<SourcesChanged> for SourcesView {}

/// Send `req`; `done` runs on the view with the reply.
pub fn call<T: 'static>(req: Request, cx: &mut Context<T>, done: impl FnOnce(&mut T, client::Reply, &mut Context<T>) + 'static) {
    let fut = client::send(req);
    cx.spawn(async move |this, cx| {
        let r = fut.await;
        let _ = this.update(cx, |this, cx| {
            done(this, r, cx);
            cx.notify();
        });
    })
    .detach();
}

/// The JSON of a 2xx reply, else its error in words.
pub fn json_of(r: client::Reply) -> Result<Value, String> {
    match r {
        Ok(resp) if resp.ok() => Ok(resp.json()),
        Ok(resp) if resp.status == 401 => Err("this device isn't signed in to the server: sign in under Settings, Account".into()),
        Ok(resp) => Err(resp.error()),
        Err(e) => Err(e),
    }
}

impl SourcesView {
    pub fn new(only: Option<Vec<String>>, cx: &mut Context<Self>) -> Self {
        let mut v = SourcesView { only, catalog: None, status: None, load_error: None, editing: None, inputs: vec![], busy: false, note: None,
            edit_on_load: std::env::var("FINANCE_DESKTOP_SOURCE_EDIT").ok(), single: false, how: false };
        v.refresh(cx);
        v
    }

    /// The wizard's card for `key`: its form, open (once the catalog is in).
    pub fn focus_one(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.only = Some(vec![key.to_string()]);
        self.single = true;
        self.how = false;
        self.note = None;
        self.editing = None;
        self.inputs.clear();
        if self.catalog.is_some() {
            self.edit(key, window, cx);
        } else {
            // made before signing in, the first fetch was skipped: again now
            self.edit_on_load = Some(key.to_string());
            self.load_error = None;
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Something was typed into the open form.
    pub fn typed(&self, cx: &App) -> bool {
        self.inputs.iter().any(|(_, i)| !i.read(cx).value().trim().is_empty())
    }

    pub fn configured(&self, key: &str) -> bool {
        self.state(key)["configured"] == json!(true)
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // turned away: these would only hear 401 (Settings refreshes after signing in)
        if finance_core::signin::needed() {
            return;
        }
        call(Request::new("GET", "/api/sources/catalog"), cx, |this, r, _| match json_of(r) {
            Ok(v) => this.catalog = Some(v),
            Err(e) => this.load_error = Some(e),
        });
        call(Request::new("GET", "/api/sources"), cx, |this, r, _| {
            if let Ok(v) = json_of(r) {
                this.status = Some(v);
            }
        });
    }

    /// Catalog entries of the API sources shown.
    fn entries(&self) -> Vec<Value> {
        let all = self.catalog.as_ref().and_then(|c| c.get("api")).and_then(Value::as_array).cloned().unwrap_or_default();
        all.into_iter()
            .filter(|e| self.only.as_ref().is_none_or(|o| o.iter().any(|k| e["key"] == json!(k))))
            .collect()
    }

    fn state(&self, key: &str) -> Value {
        self.status.as_ref().and_then(|s| s.get("api")).and_then(Value::as_array)
            .and_then(|a| a.iter().find(|x| x["key"] == json!(key)).cloned())
            .unwrap_or(Value::Null)
    }

    /// Supported sources' *Set up*: this key's form, open (once the catalog is in).
    pub fn open_form(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.catalog.is_some() {
            self.edit(key, window, cx);
        } else {
            self.edit_on_load = Some(key.to_string());
            self.refresh(cx);
        }
    }

    fn edit(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entries().into_iter().find(|e| e["key"] == json!(key)) else { return };
        let st = self.state(key);
        self.inputs = entry["fields"].as_array().cloned().unwrap_or_default().iter().map(|f| {
            let fk = f["key"].as_str().unwrap_or("").to_string();
            let set = st["fields"].as_array().is_some_and(|a| a.iter().any(|x| x["key"] == json!(fk) && x["set"] == json!(true)));
            let ph = if set { "set: leave empty to keep".to_string() } else { f["label"].as_str().unwrap_or("").to_string() };
            let secret = f["secret"].as_bool().unwrap_or(false);
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(ph).masked(secret));
            (fk, input)
        }).collect();
        self.editing = Some(key.to_string());
        self.note = None;
        cx.notify();
    }

    fn fields(&self, cx: &App) -> Value {
        let mut m = serde_json::Map::new();
        for (k, i) in &self.inputs {
            let v = i.read(cx).value().trim().to_string();
            if !v.is_empty() {
                m.insert(k.clone(), json!(v));
            }
        }
        json!({"fields": m})
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.editing.clone() else { return };
        self.busy = true;
        self.note = Some((true, if key == "ibkr" { "Testing… (IBKR can take up to a minute)" } else { "Testing…" }.into()));
        let req = Request::new("POST", &format!("/api/sources/{key}/test")).json(&self.fields(cx));
        call(req, cx, |this, r, _| {
            this.busy = false;
            this.note = Some(match json_of(r) {
                Ok(v) => (v["ok"] == json!(true), v["test"]["detail"].as_str().unwrap_or("").to_string()),
                Err(e) => (false, e),
            });
        });
        cx.notify();
    }

    /// Test, then save the open form (the server tests again); emits
    /// [`SourcesChanged`] once it's saved.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.editing.clone() else { return };
        self.busy = true;
        self.note = Some((true, "Testing, then saving…".into()));
        let req = Request::new("PUT", &format!("/api/sources/{key}/secret")).json(&self.fields(cx));
        call(req, cx, |this, r, cx| {
            this.busy = false;
            match r {
                Ok(resp) if resp.ok() => {
                    let v = resp.json();
                    this.note = Some((true, format!("Saved. {}", v["test"]["detail"].as_str().unwrap_or(""))));
                    this.editing = None;
                    this.inputs.clear();
                    this.refresh(cx);
                    cx.emit(SourcesChanged);
                }
                Ok(resp) if resp.status == 422 => {
                    let v = resp.json();
                    this.note = Some((false, format!("Not saved: {}", v["test"]["detail"].as_str().unwrap_or("the test failed"))));
                }
                other => this.note = Some((false, json_of(other).err().unwrap_or_default())),
            }
        });
        cx.notify();
    }

    fn remove(&mut self, key: &str, cx: &mut Context<Self>) {
        self.busy = true;
        call(Request::new("DELETE", &format!("/api/sources/{key}/secret")), cx, |this, r, cx| {
            this.busy = false;
            this.note = Some(match json_of(r) {
                Ok(v) => {
                    let left = v["still_configured_elsewhere"].as_array().map(|a| a.len()).unwrap_or(0);
                    this.refresh(cx);
                    cx.emit(SourcesChanged);
                    (true, if left > 0 { "Removed from the store; some keys are still set in the server's .env or environment.".into() } else { "Removed.".into() })
                }
                Err(e) => (false, e),
            });
        });
        cx.notify();
    }

    fn status_line(&self, key: &str, cx: &App) -> Div {
        let t = cx.theme();
        let st = self.state(key);
        if st.is_null() {
            return div();
        }
        let configured = st["configured"] == json!(true);
        let via = st["configured_via"].as_str().map(|v| match v {
            "store" => "the encrypted store",
            ".env" => "the server's .env",
            _ => "the server's environment",
        });
        let hints: Vec<String> = st["fields"].as_array().cloned().unwrap_or_default().iter()
            .filter_map(|f| Some(format!("{} {}", f["key"].as_str()?.to_lowercase().replace('_', " "), f["hint"].as_str()?)))
            .collect();
        let mut line = if configured { format!("Configured, from {}", via.unwrap_or("?")) } else { "Not configured".into() };
        if !hints.is_empty() {
            line.push_str(&format!(" · {}", hints.join(", ")));
        }
        v_flex().gap(px(2.))
            .child(h_flex().gap_2().items_center()
                .child(ui::dot(if configured { fmt::money_in() } else { t.muted_foreground }, 7.))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(line)))
            .when_some(st["last_error"].as_str().map(String::from), |d, e| {
                d.child(div().text_size(px(ts::CAPTION)).text_color(fmt::money_out()).child(format!("Last fetch failed: {e}")))
            })
    }

    fn form(&self, entry: &Value, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let steps = entry["instructions"].as_array().cloned().unwrap_or_default();
        let fields = entry["fields"].as_array().cloned().unwrap_or_default();
        v_flex().gap_2().mt_2().p(px(12.)).rounded(t.radius).border_1().border_color(ui::hairline(cx))
            .child(v_flex().gap_1().children(steps.iter().enumerate().map(|(i, s)| {
                div().text_size(px(ts::CAPTION)).child(format!("{}. {}", i + 1, s.as_str().unwrap_or("")))
            })))
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
                .child(format!("Refused: {}.", entry["refuses"].as_str().unwrap_or(""))))
            .children(self.inputs.iter().map(|(k, input)| {
                let f = fields.iter().find(|f| f["key"] == json!(k)).cloned().unwrap_or_default();
                v_flex().gap(px(3.))
                    .child(div().text_sm().font_medium().child(f["label"].as_str().unwrap_or(k).to_string()))
                    .child(Input::new(input).small())
                    .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(f["help"].as_str().unwrap_or("").to_string()))
            }))
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("src-test").small().outline().label("Test").loading(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.test(cx))))
                .child(Button::new("src-save").small().primary().label("Save").loading(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))))
                .child(Button::new("src-cancel").small().ghost().label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| { this.editing = None; this.inputs.clear(); this.note = None; cx.notify(); }))))
    }
}

impl SourcesView {
    /// Account countries: one row per bank or card source with rows.
    fn accounts_card(&self, cx: &mut Context<Self>) -> Option<Div> {
        let t = cx.theme().clone();
        let files = self.catalog.as_ref()?.get("files")?.as_array()?.clone();
        let with_rows: Vec<String> = self.status.as_ref()?.get("files")?.as_array()?.iter()
            .filter(|f| f["rows"].as_i64().unwrap_or(0) > 0)
            .filter_map(|f| f["source"].as_str().map(String::from))
            .collect();
        let shown: Vec<Value> = files.into_iter()
            .filter(|f| matches!(f["kind"].as_str(), Some("bank" | "card")))
            .filter(|f| f["key"].as_str().is_some_and(|k| with_rows.iter().any(|s| s == k)))
            .collect();
        if shown.is_empty() {
            return None;
        }
        let home = finance_core::base::get();
        let label = |cc: Option<&str>| match cc {
            Some(c) => format!("{} ({c})", finance_core::travel::place_name(c)),
            None => "By currency".to_string(),
        };
        let mut card = ui::card(cx).gap_2();
        for f in shown {
            let key = f["key"].as_str().unwrap_or("").to_string();
            let default = f["country"].as_str().map(String::from);
            let now = home.account_country(&key);
            let view = cx.entity().downgrade();
            let mut choices: Vec<Option<String>> = vec![None];
            for c in default.iter().cloned().chain(home.country.clone()).chain(finance_core::base::COUNTRY_CHOICES.iter().map(|c| c.to_string())) {
                if !choices.contains(&Some(c.clone())) {
                    choices.push(Some(c));
                }
            }
            let (k, d) = (key.clone(), default.clone());
            let btn = Button::new(SharedString::from(format!("src-cc-{key}"))).xsmall().outline().label(label(now.as_deref()))
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.min_w(px(200.)).max_h(px(360.)).scrollable(true);
                    for c in &choices {
                        let (view, k, c2, d) = (view.clone(), k.clone(), c.clone(), d.clone());
                        let suffix = if *c == d && d.is_some() { " · default" } else { "" };
                        menu = menu.item(PopupMenuItem::new(format!("{}{suffix}", label(c.as_deref()))).on_click(move |_, _, cx| {
                            // the catalog's default goes back to "unset"
                            let set = if c2 == d { None } else { Some(c2.clone()) };
                            let write = finance_core::base::set_account_country(&k, set);
                            // re-sync once the server has it (the trips follow at once meanwhile)
                            let _ = view.update(cx, |_, cx| {
                                cx.notify();
                                if let Some(f) = write {
                                    cx.spawn(async move |this, cx| {
                                        let _ = f.await;
                                        let _ = this.update(cx, |_, cx| cx.emit(SourcesChanged));
                                    })
                                    .detach();
                                }
                            });
                        }));
                    }
                    menu
                });
            card = card.child(h_flex().gap_2().items_center()
                .child(div().text_sm().flex_1().child(f["label"].as_str().unwrap_or(&key).to_string()))
                .child(btn));
        }
        Some(card.child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
            .child("Where each account is: its rows place you there, so trips are told from home. By currency: a JPY account was spent in Japan.")))
    }

    /// The wizard's card body: the open form's fields, then how to make the
    /// key (and Test) behind a disclosure.
    fn one(&mut self, note: Option<(bool, String)>, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let Some(entry) = self.entries().into_iter().next() else { return div() };
        let key = entry["key"].as_str().unwrap_or("").to_string();
        let fields = entry["fields"].as_array().cloned().unwrap_or_default();
        let caption = |s: String| div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(s);
        let how = self.how;
        v_flex().gap_3()
            .when(self.configured(&key), |d| d.child(self.status_line(&key, cx)))
            .children(self.inputs.iter().map(|(k, input)| {
                let f = fields.iter().find(|f| f["key"] == json!(k)).cloned().unwrap_or_default();
                v_flex().gap(px(4.))
                    .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(f["label"].as_str().unwrap_or(k).to_string()))
                    .child(Input::new(input))
            }))
            .child(h_flex().child(Button::new("src-how").small().ghost()
                .icon(if how { IconName::ChevronDown } else { IconName::ChevronRight })
                .label("How to make a read-only key")
                .on_click(cx.listener(|this, _, _, cx| { this.how = !this.how; cx.notify(); }))))
            .when(how, |d| d.child(v_flex().gap_1().pl(px(8.))
                .children(entry["instructions"].as_array().cloned().unwrap_or_default().iter().enumerate()
                    .map(|(i, s)| div().text_size(px(ts::CAPTION)).child(format!("{}. {}", i + 1, s.as_str().unwrap_or("")))))
                .child(caption(format!("Refused: {}.", entry["refuses"].as_str().unwrap_or(""))))
                .child(caption("Kept encrypted on the server and never shown again.".into()))
                .child(h_flex().pt_1().child(Button::new("src-test").small().outline().label("Test the key").loading(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.test(cx)))))))
            .when_some(note, |d, (ok, n)| d.child(div().text_size(px(ts::CAPTION))
                .text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(n)))
    }
}

impl Render for SourcesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        if self.catalog.is_some() && self.status.is_some() {
            if let Some(k) = self.edit_on_load.take() {
                self.edit(&k, window, cx);
            }
        }
        if self.catalog.is_none() {
            return if self.single { div() } else { ui::card(cx) }.child(div().text_sm().text_color(t.muted_foreground).child(match &self.load_error {
                Some(e) => format!("Couldn't load the sources: {e}"),
                None => "Loading the sources…".into(),
            }));
        }
        let note = self.note.clone();
        if self.single {
            return self.one(note, cx);
        }
        let mut list = ui::card(cx).gap_3();
        for entry in self.entries() {
            let key = entry["key"].as_str().unwrap_or("").to_string();
            let configured = self.state(&key)["configured"] == json!(true);
            let in_store = self.state(&key)["configured_via"] == json!("store");
            let editing = self.editing.as_deref() == Some(key.as_str());
            let (k1, k2) = (key.clone(), key.clone());
            list = list.child(v_flex().gap_1()
                .child(h_flex().gap_2().items_center().flex_wrap()
                    .child(div().text_sm().font_semibold().flex_1().child(entry["label"].as_str().unwrap_or("").to_string()))
                    .when(!editing, |d| d.child(Button::new(SharedString::from(format!("src-edit-{key}"))).small().outline()
                        .icon(if configured { IconName::Settings } else { IconName::Plus })
                        .label(if configured { "Edit" } else { "Add" })
                        .on_click(cx.listener(move |this, _, window, cx| this.edit(&k1, window, cx)))))
                    .when(in_store && !editing, |d| d.child(Button::new(SharedString::from(format!("src-rm-{key}"))).small().ghost()
                        .label("Remove").on_click(cx.listener(move |this, _, _, cx| this.remove(&k2, cx))))))
                .child(self.status_line(&key, cx))
                .when(editing, |d| d.child(self.form(&entry, cx))));
        }
        let accounts = self.accounts_card(cx);
        let list = list.when_some(note, |d, (ok, n)| d.child(div().text_size(px(ts::CAPTION))
            .text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(n)))
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
                .child("Read-only keys only. They are tested before they are saved, kept encrypted on the server, and never shown again."));
        match accounts {
            Some(a) => v_flex().gap_2().child(list).child(ui::section("Account countries", "where each account is, for trips", cx)).child(a),
            None => list,
        }
    }
}
