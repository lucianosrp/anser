//! The avatar editor, in Settings' account card and the wizard's "You"
//! step: a preview, then one of three ways to set it.
//!
//! * Picture: the desktop's file dialog or the browser's file input; the
//!   server checks it (PNG, JPEG or WebP, 5 MB) and keeps a 256×256 copy.
//! * Gravatar: an email; the server fetches the picture (the app never
//!   contacts Gravatar) and says so when there is none.
//! * Initials: one or two letters (empty = from the name) on a chart colour
//!   or a custom hex.
//!
//! Each change lands on the server at once and in [`crate::avatar`] (the
//! sidebar, the header), and is told to the page that holds the editor
//! ([`AvatarChanged`], with the profile the server answered).

use finance_core::avatar::{Color, Kind, Spec, PRESETS};
use finance_core::client::{self, Request};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use super::sources::{call, json_of};
use crate::fmt;
use crate::ui::ts;
use crate::zoom::px;

/// The server saved a change; the profile it answered.
pub struct AvatarChanged(pub Value);

pub struct AvatarEdit {
    profile: Value,
    /// the way being edited (starts at the profile's)
    kind: Kind,
    email: Entity<InputState>,
    initials: Entity<InputState>,
    hex: Entity<InputState>,
    color: Option<Color>,
    busy: bool,
    note: Option<(bool, String)>,
    /// the profile arrived: fill the fields on the next render
    fill: bool,
    /// the wizard's card: the swatches only, no custom hex field
    simple: bool,
}

impl EventEmitter<AvatarChanged> for AvatarEdit {}

impl AvatarEdit {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        AvatarEdit {
            profile: Value::Null,
            kind: Kind::Initials,
            email: input("you@example.com", window, cx),
            initials: input("JD", window, cx),
            hex: input("#7aa2f7", window, cx),
            color: None,
            busy: false,
            note: None,
            fill: false,
            simple: false,
        }
    }

    /// For the wizard's small card: fewer fields.
    pub fn simple(mut self) -> Self {
        self.simple = true;
        self
    }

    /// The profile as the page holding the editor knows it.
    pub fn set_profile(&mut self, p: Value, cx: &mut Context<Self>) {
        let spec = Spec::from_profile(&p);
        self.kind = spec.kind;
        self.color = spec.color;
        self.profile = p;
        self.fill = true;
        cx.notify();
    }

    fn val(i: &Entity<InputState>, cx: &App) -> String {
        i.read(cx).value().trim().to_string()
    }

    /// The server answered a change with the profile: tell everyone, then
    /// fetch the picture (or drop it).
    fn saved(&mut self, v: &Value, cx: &mut Context<Self>) {
        let p = v["profile"].clone();
        crate::avatar::set_spec(&p, cx);
        self.profile = p.clone();
        let spec = Spec::from_profile(&p);
        if spec.has_picture() {
            self.fetch_picture(false, cx);
        } else {
            crate::avatar::set_picture(None, cx);
        }
        cx.emit(AvatarChanged(p));
    }

    fn fetch_picture(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let path = if refresh { "/api/profile/avatar?refresh=1" } else { "/api/profile/avatar" };
        call(Request::new("GET", path), cx, |this, r, cx| {
            this.busy = false;
            match r {
                Ok(resp) if resp.status == 200 && resp.body.starts_with(b"\x89PNG") => crate::avatar::set_picture(Some(resp.body), cx),
                Ok(resp) if resp.status == 404 => crate::avatar::set_picture(None, cx),
                other => this.note = Some((false, json_of(other).err().unwrap_or_default())),
            }
        });
    }

    fn upload(&mut self, cx: &mut Context<Self>) {
        self.note = None;
        if cfg!(target_family = "wasm") {
            let pick = client::pick_image();
            cx.spawn(async move |this, cx| {
                let names = pick.await;
                if !names.as_ref().is_ok_and(|n| !n.is_empty()) {
                    return;
                }
                let _ = this.update(cx, |this, cx| { this.busy = true; cx.notify(); });
                let r = client::send_picked("/api/profile/avatar").await;
                let _ = this.update(cx, |this, cx| this.upload_done(r, cx));
            })
            .detach();
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use as picture".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| { this.note = Some((false, format!("Couldn't read the file: {e}"))); cx.notify(); });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| { this.busy = true; cx.notify(); });
            let mut req = Request::new("PUT", "/api/profile/avatar");
            req.body = Some(bytes);
            req.content_type = Some("application/octet-stream".into());
            let r = client::send(req).await;
            let _ = this.update(cx, |this, cx| this.upload_done(r, cx));
        })
        .detach();
    }

    fn upload_done(&mut self, r: client::Reply, cx: &mut Context<Self>) {
        self.busy = false;
        match json_of(r) {
            Ok(v) => {
                self.note = Some((true, "Saved: the server keeps a 256×256 copy.".into()));
                self.saved(&v, cx);
            }
            Err(e) => self.note = Some((false, e)),
        }
        cx.notify();
    }

    fn use_gravatar(&mut self, cx: &mut Context<Self>) {
        let email = Self::val(&self.email, cx);
        if !email.contains('@') {
            self.note = Some((false, "Enter the email your Gravatar is under.".into()));
            cx.notify();
            return;
        }
        self.busy = true;
        self.note = None;
        call(Request::new("POST", "/api/profile/avatar/gravatar").json(&json!({"email": email})), cx, |this, r, cx| {
            this.busy = false;
            match r {
                Ok(resp) if resp.status == 404 => this.note = Some((false, "No Gravatar for this email.".into())),
                other => match json_of(other) {
                    Ok(v) => {
                        this.note = Some((true, "Using your Gravatar. The server fetches it again daily.".into()));
                        this.saved(&v, cx);
                    }
                    Err(e) => this.note = Some((false, e)),
                },
            }
        });
        cx.notify();
    }

    fn save_initials(&mut self, cx: &mut Context<Self>) {
        let letters = Self::val(&self.initials, cx);
        let n = letters.chars().count();
        if n > 2 || !letters.chars().all(char::is_alphanumeric) {
            self.note = Some((false, "Initials are one or two letters (empty: from your name).".into()));
            cx.notify();
            return;
        }
        let typed = Self::val(&self.hex, cx);
        let color = if typed.is_empty() {
            self.color
        } else {
            let hex = if typed.starts_with('#') { typed } else { format!("#{typed}") };
            match Color::parse(&hex) {
                Some(c) => Some(c),
                None => {
                    self.note = Some((false, "A colour is #rrggbb, like #7aa2f7.".into()));
                    cx.notify();
                    return;
                }
            }
        };
        let mut p = self.profile.as_object().cloned().unwrap_or_default();
        p.entry("base_currency").or_insert(json!(finance_core::locale::detect().0));
        p.insert("avatar_kind".into(), json!("initials"));
        p.insert("avatar_initials".into(), json!(letters));
        p.insert("avatar_color".into(), json!(color.map(Color::key).unwrap_or_default()));
        self.busy = true;
        self.note = None;
        call(Request::new("PUT", "/api/profile").json(&Value::Object(p)), cx, |this, r, cx| {
            this.busy = false;
            match json_of(r) {
                Ok(v) => {
                    this.note = Some((true, "Saved.".into()));
                    this.saved(&v, cx);
                }
                Err(e) => this.note = Some((false, e)),
            }
        });
        cx.notify();
    }

    /// The spec as it would look with what is typed now (the preview).
    fn pending(&self, cx: &App) -> Spec {
        let mut s = Spec::from_profile(&self.profile);
        if self.kind == Kind::Initials {
            s.kind = Kind::Initials;
            s.initials = Self::val(&self.initials, cx).to_uppercase();
            let typed = Self::val(&self.hex, cx);
            let hex = if typed.starts_with('#') { typed } else { format!("#{typed}") };
            s.color = Color::parse(&hex).or(self.color);
        }
        s
    }
}

impl Render for AvatarEdit {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        if std::mem::take(&mut self.fill) {
            let s = Spec::from_profile(&self.profile);
            let (email, initials) = (s.email.clone(), s.initials.clone());
            let hex = match s.color {
                Some(Color::Hex(h)) => format!("#{h:06x}"),
                _ => String::new(),
            };
            self.email.update(cx, |i, cx| i.set_value(email, window, cx));
            self.initials.update(cx, |i, cx| i.set_value(initials, window, cx));
            self.hex.update(cx, |i, cx| i.set_value(hex, window, cx));
        }
        let spec = self.pending(cx);
        // the preview: the picture only while the profile uses this way
        let preview = {
            let current = Spec::from_profile(&self.profile);
            let pic = (self.kind != Kind::Initials && current.kind == self.kind).then(|| crate::avatar::picture(cx)).flatten();
            crate::avatar::draw(&spec, pic, 56., 12., cx)
        };
        let kind = self.kind;
        let tab = |id: &'static str, label: &'static str, k: Kind, cx: &mut Context<Self>| {
            Button::new(id).small().when(kind == k, |b| b.primary()).when(kind != k, |b| b.ghost()).label(label)
                .on_click(cx.listener(move |this, _, _, cx| { this.kind = k; this.note = None; cx.notify(); }))
        };
        let caption = |s: &str| div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(s.to_string());
        let body = match kind {
            Kind::Upload => v_flex().gap_2()
                .child(caption("A PNG, JPEG or WebP picture, 5 MB at most. The server crops it to a square."))
                .child(h_flex().child(Button::new("av-upload").small().outline().icon(IconName::ArrowUp).label("Choose a picture…")
                    .loading(self.busy).on_click(cx.listener(|this, _, _, cx| this.upload(cx))))),
            Kind::Gravatar => v_flex().gap_2()
                .child(caption("The server fetches your Gravatar; this device never contacts Gravatar."))
                .child(h_flex().gap_2().flex_wrap().items_center()
                    .child(div().w(px(240.)).child(Input::new(&self.email).small()))
                    .child(Button::new("av-grav").small().outline().label("Use Gravatar").loading(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.use_gravatar(cx))))
                    .when(Spec::from_profile(&self.profile).kind == Kind::Gravatar, |d| d.child(
                        Button::new("av-grav-refresh").small().ghost().icon(IconName::RefreshCw).tooltip("Fetch it again now")
                            .on_click(cx.listener(|this, _, _, cx| { this.busy = true; this.fetch_picture(true, cx); cx.notify(); }))))),
            Kind::Initials => {
                let chosen = self.color;
                v_flex().gap_2()
                    .child(h_flex().gap_2().flex_wrap().items_center()
                        .child(div().w(px(64.)).child(Input::new(&self.initials).small()))
                        .children(PRESETS.iter().enumerate().map(|(i, c)| {
                            let c = *c;
                            let bg = crate::avatar::background(c, cx);
                            div().id(("av-swatch", i)).size(px(22.)).rounded_full().bg(bg).cursor_pointer()
                                .border_2().border_color(if chosen == Some(c) { t.foreground } else { transparent_black() })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.color = Some(c);
                                    this.hex.update(cx, |i, cx| i.set_value(String::new(), window, cx));
                                    cx.notify();
                                }))
                        }))
                        .when(!self.simple, |d| d.child(div().w(px(96.)).child(Input::new(&self.hex).small())))
                        .child(Button::new("av-initials").small().outline().label("Save").loading(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.save_initials(cx)))))
                    .child(caption(if self.simple { "One or two letters (empty: from your name) on a colour." }
                        else { "One or two letters (empty: from your name) on a theme colour, or your own #rrggbb." }))
            }
        };
        h_flex().gap_4().items_start()
            .child(preview)
            .child(v_flex().flex_1().min_w_0().gap_2()
                .child(h_flex().gap_1()
                    .child(tab("av-k-up", "Picture", Kind::Upload, cx))
                    .child(tab("av-k-grav", "Gravatar", Kind::Gravatar, cx))
                    .child(tab("av-k-init", "Initials", Kind::Initials, cx)))
                .child(body)
                .when_some(self.note.clone(), |d, (ok, s)| d.child(div().text_size(px(ts::CAPTION))
                    .text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(s))))
    }
}
