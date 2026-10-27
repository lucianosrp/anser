//! Upload, web build: the browser's file dialog, a dry run on the server
//! (POST /api/preview: which export each file is, how many rows are new,
//! nothing written), then the upload (POST /upload, multipart, with the
//! session cookie). Same public surface as `upload.rs` (the app wires both
//! identically); writes need a paired browser (Settings, Account, Device). *Paste a
//! message* shows only to a user with an HSBC source
//! (`Capabilities::paste_sms`).

use std::path::PathBuf;

use finance_core::capabilities::Capabilities;
use finance_core::client;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

/// Emitted when an upload saved at least one file on the server.
pub struct Uploaded;

pub struct Uploader {
    names: Vec<String>,
    preview: Option<Result<Vec<Value>, String>>,
    outcome: Option<Result<Value, String>>,
    busy: bool,
    /// *Paste a message*: the pasted text, its date, and what to tell about it
    paste: Entity<TextareaState>,
    paste_date: Entity<InputState>,
    paste_note: Option<Result<String, String>>,
    /// what the server has (the last load's): whether the paste card applies
    caps: Capabilities,
}

impl EventEmitter<Uploaded> for Uploader {}

fn json_of(r: client::Reply) -> Result<Value, String> {
    match r {
        Ok(resp) if resp.ok() => Ok(resp.json()),
        Ok(resp) if resp.status == 401 => Err("this browser isn't paired: Settings, Account, Device".into()),
        Ok(resp) => Err(resp.error()),
        Err(e) => Err(e),
    }
}

impl Uploader {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paste = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder("Paste the HSBC FlexInvest SMS here, or several"));
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let paste_date = cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD").default_value(today));
        Uploader { names: vec![], preview: None, outcome: None, busy: false, paste, paste_date, paste_note: None, caps: Capabilities::default() }
    }

    /// The capabilities of a new load (see `upload.rs`).
    pub fn set_capabilities(&mut self, caps: Capabilities, cx: &mut Context<Self>) {
        if caps != self.caps {
            self.caps = caps;
            cx.notify();
        }
    }

    /// Drops carry no file contents in the browser: nothing to do.
    pub fn add(&mut self, _paths: Vec<PathBuf>, _cx: &mut Context<Self>) {}

    pub fn add_held(&mut self, _paths: Vec<PathBuf>, _cx: &mut Context<Self>) {}
    fn choose(&mut self, cx: &mut Context<Self>) {
        let pick = client::pick_files();
        self.busy = true;
        cx.spawn(async move |this, cx| {
            let names = pick.await;
            let ok = names.as_ref().is_ok_and(|n| !n.is_empty());
            let _ = this.update(cx, |this, cx| {
                this.outcome = None;
                match names {
                    Ok(n) => {
                        this.names = n;
                        this.preview = None;
                    }
                    Err(e) => this.preview = Some(Err(e)),
                }
                this.busy = ok;
                cx.notify();
            });
            if !ok {
                return;
            }
            let r = client::send_picked("/api/preview").await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.preview = Some(json_of(r).map(|v| v["files"].as_array().cloned().unwrap_or_default()));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// *Add* in the paste box: the confirmations in the text, wrapped as an
    /// SMS backup file and sent through /upload like a chosen file.
    fn add_pasted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.paste.read(cx).value().to_string();
        let msgs = finance_core::paste::confirmations_in(&text);
        if msgs.is_empty() {
            self.paste_note = Some(Err("No HSBC FlexInvest confirmation found in that text.".into()));
            cx.notify();
            return;
        }
        let Ok(date) = chrono::NaiveDate::parse_from_str(self.paste_date.read(cx).value().trim(), "%Y-%m-%d") else {
            self.paste_note = Some(Err("The date should look like 2026-09-14.".into()));
            cx.notify();
            return;
        };
        let name = finance_core::paste::file_name();
        let fut = client::send(finance_core::paste::upload_request(&name, &finance_core::paste::sms_xml(&msgs, date)));
        self.busy = true;
        self.paste_note = None;
        cx.spawn_in(window, async move |this, cx| {
            let r = fut.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                this.paste_note = Some(json_of(r).map(|v| {
                    let list = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    if v["saved"].as_array().is_some_and(|a| !a.is_empty()) {
                        this.paste.update(cx, |p, cx| p.set_value("", window, cx));
                        cx.emit(Uploaded);
                        format!("Saved: {}. Reload to see the new rows.", list("saved"))
                    } else if !list("skipped").is_empty() {
                        format!("Already there: {}", list("skipped"))
                    } else {
                        format!("Refused: {}", list("errors"))
                    }
                }));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn upload(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        let fut = client::send_picked("/upload");
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                let o = json_of(r);
                if o.as_ref().is_ok_and(|v| v["saved"].as_array().is_some_and(|a| !a.is_empty())) {
                    cx.emit(Uploaded);
                }
                this.outcome = Some(o);
                this.names.clear();
                this.preview = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for Uploader {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        if !client::can_pick_files() {
            return ui::empty_with(IconName::ArrowUp, "Upload from the desktop app",
                "This build can't read files. Drop bank, card or crypto exports on the desktop app's Upload page.", cx);
        }
        let caption = |s: String| div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(s);
        let rows = match &self.preview {
            None => None,
            Some(Err(e)) => Some(v_flex().child(div().text_sm().text_color(fmt::money_out()).child(e.clone()))),
            Some(Ok(files)) => Some(v_flex().gap_2().children(files.iter().map(|f| {
                let ok = f["ok"].as_bool().unwrap_or(false);
                let detail = if ok {
                    let range = f["range"].as_array().map(|r| format!(", {} to {}", r[0].as_str().unwrap_or(""), r[1].as_str().unwrap_or(""))).unwrap_or_default();
                    format!("{}: {} rows, {} new{range}", f["label"].as_str().unwrap_or(""), f["rows"], f["new"])
                } else {
                    f["error"].as_str().unwrap_or("not readable").to_string()
                };
                h_flex().gap_2().items_center()
                    .child(ui::dot(if ok { fmt::money_in() } else { fmt::money_out() }, 7.))
                    .child(div().text_sm().font_medium().child(f["name"].as_str().unwrap_or("").to_string()))
                    .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(detail))
            }))),
        };
        let can_upload = matches!(&self.preview, Some(Ok(f)) if f.iter().any(|x| x["ok"] == Value::Bool(true)));
        let outcome = self.outcome.as_ref().map(|o| match o {
            Ok(v) => {
                let list = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                let mut s = Vec::new();
                for (k, label) in [("saved", "Saved"), ("skipped", "Already there"), ("errors", "Refused")] {
                    let l = list(k);
                    if !l.is_empty() {
                        s.push(format!("{label}: {l}"));
                    }
                }
                div().text_sm().child(format!("{}. Reload to see the new rows.", s.join(". ")))
            }
            Err(e) => div().text_sm().text_color(fmt::money_out()).child(e.clone()),
        });
        let pasted = self.paste_note.as_ref().map(|n| match n {
            Ok(s) => div().text_sm().child(s.clone()),
            Err(e) => div().text_sm().text_color(fmt::money_out()).child(e.clone()),
        });
        let paste = ui::card(cx).gap_3().max_w(px(760.))
            .child(div().text_lg().font_semibold().child("Paste a message"))
            .child(caption("An HSBC FlexInvest confirmation SMS, copied as text (one or several). It goes through the same check and upload as an SMS backup.".into()))
            .child(Textarea::new(&self.paste).w_full())
            .child(h_flex().gap_2().items_center()
                .child(caption("SMS date".into()))
                .child(div().w(px(130.)).child(Input::new(&self.paste_date).small()))
                .child(Button::new("web-paste-add").small().primary().icon(IconName::ArrowUp).label("Add").loading(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.add_pasted(window, cx)))))
            .children(pasted);
        v_flex().gap_4().child(ui::card(cx).gap_3().max_w(px(760.))
            .child(div().text_lg().font_semibold().child("Send exports to the server"))
            .child(caption("Bank, card or crypto exports (.csv, .xml, .pdf). Each file is checked first: what it is and how many of its rows are new. Nothing is written until you upload.".into()))
            .child(h_flex().gap_2()
                .child(Button::new("web-choose").small().outline().icon(IconName::Folder).label("Choose files").loading(self.busy && self.names.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.choose(cx))))
                .when(can_upload, |d| d.child(Button::new("web-upload").small().primary().icon(IconName::ArrowUp)
                    .label(format!("Upload {} file(s)", self.names.len())).loading(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.upload(cx))))))
            .when(self.busy && !self.names.is_empty() && self.preview.is_none(), |d| d.child(caption("Checking the files on the server…".into())))
            .children(rows)
            .children(outcome)
            .child(h_flex().child(Button::new("which-banks").xsmall().ghost().label("Which banks are supported?")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::app::GoSupported), cx)))))
            .when(self.caps.paste_sms(), |d| d.child(paste))
    }
}
