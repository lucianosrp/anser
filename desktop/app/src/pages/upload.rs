//! Upload: drop bank, card or crypto exports (anywhere in the window, or
//! "Choose files…"), see what each one is and how many of its rows are new,
//! then send the server only what it doesn't have and sync.
//!
//! Its own view (an entity), so the stages tick without the app's help:
//! * up to one file per core parses at once, on the background executor;
//!   each row updates as its file finishes ([`crate::upload::parse_one`])
//! * once every file of a drop has parsed, the drop uploads, one file per
//!   request, skipping any whose md5 the server already holds
//! * a drop that saved anything emits [`Uploaded`]: the app then syncs, so
//!   the new rows reach every page
//! * a CSV or spreadsheet no source reads offers *Map columns…* (the
//!   column mapper, `pages::mapper`); the format it saves goes to the
//!   server, and the file is read again as a known source
//! * files from the Downloads banner's *Review* wait, parsed, for *Upload*
//! * *Paste a message*: an HSBC FlexInvest text, wrapped as an SMS-backup
//!   file (`finance_core::paste`) and queued like any dropped file; shown
//!   only to a user with an HSBC source (`Capabilities::paste_sms`)
//!
//! The drop zone names only the formats of the user's own sources
//! (`Capabilities::upload_hint`), from the capabilities of the last load.
//!
//! Dev aid: `FINANCE_DESKTOP_UPLOAD=path1:path2` enqueues files at start
//! (with `FINANCE_DESKTOP_PAGE=upload`), `FINANCE_DESKTOP_UPLOAD_STEP_MS`
//! slows each stage down for a mid-flow screenshot.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use finance_core::cache;
use finance_core::capabilities::Capabilities;
use finance_core::upload::Ingested;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::logo;
use crate::ui::{self, Figures as _, sp, ts};
use crate::upload::{self as model, FileRow, Queue, Scratch, Sent, Stage};
use crate::zoom::px;

/// Emitted when a drop saved at least one file on the server.
pub struct Uploaded;

pub struct Uploader {
    queue: Queue,
    /// the open column mapper's subscription
    mapper: Option<Subscription>,
    /// dev aid FINANCE_DESKTOP_MAP: open the mapper on the first unknown file
    auto_map: bool,
    /// each parsing drop's private replica copy, dropped once it has parsed
    scratch: HashMap<usize, Arc<Scratch>>,
    history: Vec<Ingested>,
    history_stamp: Option<SystemTime>,
    note: Option<String>,
    /// *Paste a message*: the pasted text and its date
    paste: Entity<TextareaState>,
    paste_date: Entity<InputState>,
    /// what the server has (the last load's): which hints and cards apply
    caps: Capabilities,
}

impl EventEmitter<Uploaded> for Uploader {}

const HISTORY: usize = 12;

fn parallel() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8)
}

fn size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.0} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

fn plural(n: usize, one: &str) -> String {
    format!("{} {one}{}", fmt::count(n as f64), if n == 1 { "" } else { "s" })
}

impl Uploader {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paste = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder("Paste the HSBC FlexInvest SMS here, or several"));
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let paste_date = cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD").default_value(today));
        let mut this = Uploader { queue: Queue::default(), mapper: None, auto_map: std::env::var_os("FINANCE_DESKTOP_MAP").is_some(), scratch: HashMap::new(), history: vec![], history_stamp: None, note: None, paste, paste_date, caps: Capabilities::default() };
        this.refresh_history();
        // the server's own format definitions, so files they read preview here
        let server = cache::server_url();
        cx.background_spawn(async move {
            let _ = crate::formats::pull(&server);
        })
        .detach();
        if let Some(v) = std::env::var_os("FINANCE_DESKTOP_UPLOAD") {
            this.add(std::env::split_paths(&v).collect(), cx);
        }
        this
    }

    /// The capabilities of a new load: the paste card and the drop zone's
    /// hint follow the user's sources.
    pub fn set_capabilities(&mut self, caps: Capabilities, cx: &mut Context<Self>) {
        if caps != self.caps {
            self.caps = caps;
            cx.notify();
        }
    }

    /// The replica's ingest history, re-read whenever a sync replaced it.
    fn refresh_history(&mut self) {
        let db = cache::db_path();
        let stamp = std::fs::metadata(finance_core::replica::stored_path(&db)).and_then(|m| m.modified()).ok();
        if stamp != self.history_stamp || self.history_stamp.is_none() {
            self.history_stamp = stamp;
            self.history = finance_core::upload::replica_history(&db, HISTORY);
        }
    }

    /// Queue dropped or picked files (a folder stands for its files) and
    /// start on them.
    pub fn add(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.note = None;
        let (batch, ids) = self.queue.enqueue(&paths);
        if !ids.is_empty() {
            self.scratch.insert(batch, Arc::new(Mutex::new(None)));
        }
        self.pump(cx);
        cx.notify();
    }

    /// Queue files that wait, parsed, for the user's *Upload* (the
    /// Downloads banner's *Review*).
    pub fn add_held(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.note = None;
        let (batch, ids) = self.queue.enqueue_held(&paths);
        if !ids.is_empty() {
            self.scratch.insert(batch, Arc::new(Mutex::new(None)));
        }
        self.pump(cx);
        cx.notify();
    }

    /// Send a held batch.
    fn release(&mut self, batch: usize, cx: &mut Context<Self>) {
        if self.queue.release(batch) {
            self.send(batch, cx);
            cx.notify();
        }
    }

    /// Open the column mapper on an unknown-format file.
    fn open_mapper(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.queue.get(id) else { return };
        let path = row.path.clone();
        let mapper = cx.new(|cx| super::mapper::Mapper::new(path, window, cx));
        self.mapper = Some(cx.subscribe(&mapper, move |this, _, ev: &super::mapper::Saved, cx| this.mapped(id, ev, cx)));
        let title = format!("Map columns · {}", row.name);
        window.open_dialog(cx, move |dialog, _, _| dialog.w(px(900.)).title(title.clone()).child(mapper.clone()));
    }

    /// A format was saved for row `id`'s file: send it to the server, and
    /// read the file again (now a known source).
    fn mapped(&mut self, id: usize, ev: &super::mapper::Saved, cx: &mut Context<Self>) {
        let (def, path) = (ev.def.clone(), ev.path.clone());
        self.note = Some(format!("Saved the {} format; sending it to the server…", def.label));
        self.queue.remove(id);
        let server = cache::server_url();
        let label = def.label.clone();
        let task = cx.background_spawn(async move { crate::formats::push(&def.id, &server) });
        cx.spawn(async move |this, cx| {
            let sent = task.await;
            let _ = this.update(cx, |this, cx| {
                this.note = Some(match sent {
                    Ok(()) => format!("The {label} format is on the server too: its ingest reads these files now."),
                    Err(e) => format!("The {label} format is saved here, but the server didn't take it ({e:#}). Its files upload, and are read once it has the format."),
                });
                // the file again, as a known source: previewed, then sent
                this.add(vec![path], cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Start queued files while fewer than [`parallel`] are parsing.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let running = self.queue.rows.iter().filter(|r| r.stage == Stage::Parsing).count();
        let next: Vec<(usize, usize, PathBuf)> = self
            .queue
            .rows
            .iter()
            .filter(|r| r.stage == Stage::Queued)
            .take(parallel().saturating_sub(running))
            .map(|r| (r.id, r.batch, r.path.clone()))
            .collect();
        for (id, batch, path) in next {
            self.queue.set(id, Stage::Parsing);
            let scratch = self.scratch.entry(batch).or_insert_with(|| Arc::new(Mutex::new(None))).clone();
            let replica = cache::db_path();
            let task = cx.background_spawn(async move { model::parse_one(&path, &replica, &scratch) });
            cx.spawn(async move |this, cx| {
                let (md5, stage) = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.queue.set_md5(id, md5);
                    this.queue.set(id, stage);
                    if this.queue.batch_parsed(batch) && this.scratch.remove(&batch).is_some() && !this.queue.is_held(batch) {
                        this.send(batch, cx);
                    }
                    this.pump(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Upload a parsed drop, one file at a time, never a file whose content
    /// the server already holds.
    fn send(&mut self, batch: usize, cx: &mut Context<Self>) {
        let ids = self.queue.to_upload(batch);
        if ids.is_empty() {
            return;
        }
        let server = cache::server_url();
        let (s, replica) = (server.clone(), cache::db_path());
        let known = cx.background_spawn(async move { model::known_hashes(&s, &replica) });
        cx.spawn(async move |this, cx| {
            let mut known = known.await;
            let mut saved = 0;
            for id in ids {
                let next = this.update(cx, |this, cx| {
                    let row = this.queue.get(id)?;
                    let Stage::Parsed(p) = &row.stage else { return None };
                    let (p, path, md5) = (p.clone(), row.path.clone(), row.md5.clone());
                    if md5.as_ref().is_some_and(|m| known.contains(m)) {
                        this.queue.set(id, Stage::Done(p, Sent::AlreadyOnServer));
                        cx.notify();
                        return None;
                    }
                    this.queue.set(id, Stage::Uploading(p.clone()));
                    cx.notify();
                    Some((p, path, md5))
                });
                let Ok(Some((p, path, md5))) = next else { continue };
                let s = server.clone();
                let sent = cx.background_spawn(async move { model::send_one(&s, &path) }).await;
                if matches!(sent, Sent::Saved(_) | Sent::SameContent) {
                    known.extend(md5);
                }
                saved += matches!(sent, Sent::Saved(_)) as usize;
                let _ = this.update(cx, |this, cx| {
                    this.queue.set(id, Stage::Done(p, sent));
                    cx.notify();
                });
            }
            if saved > 0 {
                let _ = this.update(cx, |_, cx| cx.emit(Uploaded));
            }
        })
        .detach();
    }

    /// *Add* in the paste box: the confirmations in the text, as an SMS
    /// backup file in a scratch folder, queued like a dropped file.
    fn add_pasted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.paste.read(cx).value().to_string();
        let msgs = finance_core::paste::confirmations_in(&text);
        if msgs.is_empty() {
            self.note = Some("No HSBC FlexInvest confirmation found in that text.".into());
            cx.notify();
            return;
        }
        let Ok(date) = chrono::NaiveDate::parse_from_str(self.paste_date.read(cx).value().trim(), "%Y-%m-%d") else {
            self.note = Some("The date should look like 2026-09-14.".into());
            cx.notify();
            return;
        };
        let dir = std::env::temp_dir().join("anser-paste");
        let path = dir.join(finance_core::paste::file_name());
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, finance_core::paste::sms_xml(&msgs, date))) {
            self.note = Some(format!("Couldn't prepare the message for upload ({e})."));
            cx.notify();
            return;
        }
        self.paste.update(cx, |p, cx| p.set_value("", window, cx));
        self.add(vec![path], cx);
    }

    fn choose_files(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn(async move |this, cx| {
            let res = picked.await;
            let _ = this.update(cx, |this, cx| match res {
                Ok(Ok(Some(paths))) => this.add(paths, cx),
                Ok(Err(e)) => {
                    this.note = Some(format!("The file picker didn't open ({e}). Drop the files on the window instead."));
                    cx.notify();
                }
                _ => {}
            });
        })
        .detach();
    }
}

// ---------- rendering ----------

fn drop_zone(server: &str, hint: String, cx: &mut Context<Uploader>) -> impl IntoElement {
    let t = cx.theme().clone();
    let (accent, wash) = (t.primary, t.primary.opacity(0.07));
    v_flex()
        .id("dropzone")
        .w_full()
        .items_center()
        .gap(px(sp::S))
        .py(px(34.))
        .px(px(sp::L))
        .rounded(t.radius_lg)
        .border_2()
        .border_dashed()
        .border_color(t.muted_foreground.opacity(0.35))
        .bg(t.secondary)
        .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(accent).bg(wash))
        .child(
            div()
                .size(px(44.))
                .rounded_full()
                .bg(t.primary.opacity(0.14))
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(IconName::ArrowUp).text_color(t.primary)),
        )
        .child(div().mt_1().text_size(px(15.)).font_semibold().text_center()
            // a phone-sized window: the button is the way in, not dragging
            .child(if crate::layout::compact() { "Add bank, card or crypto exports" } else { "Drop bank, card or crypto exports here" }))
        .child(
            div()
                .max_w(px(520.))
                .text_size(px(ts::CAPTION))
                .text_color(t.muted_foreground)
                .text_center()
                .child(format!("{hint} Each file is read here first; only what the server doesn't have is sent.")),
        )
        .child(
            h_flex()
                .mt_2()
                .gap_3()
                .flex_wrap()
                .justify_center()
                .child(
                    Button::new("choose-files")
                        .small()
                        .primary()
                        .icon(IconName::FolderOpen)
                        .label("Choose files…")
                        .on_click(cx.listener(|this, _, _, cx| this.choose_files(cx))),
                )
                .when(!crate::layout::compact(), |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child("or drop them anywhere in the window"))),
        )
        .child(Button::new("which-banks").xsmall().ghost().label("Which banks are supported?")
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::app::GoSupported), cx)))
        .child(
            div()
                .mt_1()
                .max_w_full()
                .text_size(px(11.))
                .text_color(t.muted_foreground.opacity(0.8))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(format!("Uploads to {server}")),
        )
}

/// *Paste a message*: the SMS text instead of an SMS Backup XML.
fn paste_card(up: &Uploader, cx: &mut Context<Uploader>) -> Div {
    ui::card(cx)
        .gap_3()
        .child(div().text_size(px(ts::TITLE)).font_semibold().child("Paste a message"))
        .child(ui::caption_wrap("An HSBC FlexInvest confirmation SMS, copied as text (one or several). It goes through the same check and upload as an SMS backup.", cx))
        .child(Textarea::new(&up.paste).w_full())
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::caption("SMS date", cx))
                .child(div().w(px(130.)).child(Input::new(&up.paste_date).small()))
                .child(
                    Button::new("paste-add")
                        .small()
                        .primary()
                        .icon(IconName::ArrowUp)
                        .label("Add")
                        .on_click(cx.listener(|this, _, window, cx| this.add_pasted(window, cx))),
                ),
        )
}

/// The status chip at the end of a row.
fn status(row: &FileRow, cx: &App) -> AnyElement {
    let t = cx.theme();
    let label = row.stage.label();
    let busy = |color: Hsla| {
        h_flex()
            .gap(px(5.))
            .px(px(6.))
            .h(px(18.))
            .rounded(px(5.))
            .bg(color.opacity(0.12))
            .text_color(color)
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .child(ui::braille_spinner(("busy", row.id), color, 11.))
            .child(label)
            .into_any_element()
    };
    match &row.stage {
        Stage::Queued => ui::tag(label, cx).into_any_element(),
        Stage::Parsing | Stage::Uploading(_) => busy(t.primary),
        Stage::Parsed(_) => ui::chip(label, t.primary, cx).into_any_element(),
        Stage::Done(_, Sent::Saved(_)) => {
            ui::chip(label, fmt::money_in(), cx).child(Icon::new(IconName::Check).xsmall()).into_any_element()
        }
        Stage::Done(_, Sent::SameContent | Sent::AlreadyOnServer) => ui::tag(label, cx).into_any_element(),
        Stage::Unsupported(_) | Stage::Unmatched(_) => ui::chip(label, t.warning, cx).into_any_element(),
        Stage::Error(_) | Stage::Done(_, Sent::Failed(_)) => ui::chip(label, fmt::money_out(), cx).into_any_element(),
    }
}

/// What the file is (line 1) and what happened to it (line 2).
fn lines(row: &FileRow, cx: &App) -> (String, Option<(String, Hsla)>) {
    let t = cx.theme();
    let muted = t.muted_foreground;
    let what = match row.stage.preview() {
        Some(p) => {
            let mut parts = vec![p.label.clone(), p.kind.to_string(), plural(p.rows, "row")];
            parts.extend(model::range_label(&p.range));
            parts.join(" · ")
        }
        None => size(row.bytes),
    };
    let result = match &row.stage {
        Stage::Queued => Some(("waiting for a free core".into(), muted)),
        Stage::Parsing => Some(("reading and checking against the local replica…".into(), muted)),
        Stage::Parsed(p) if p.new == 0 => Some(("nothing new: every row is already known".into(), muted)),
        Stage::Parsed(_) => Some(("waiting to upload".into(), muted)),
        Stage::Uploading(_) => Some(("sending to the server…".into(), muted)),
        Stage::Unsupported(why) | Stage::Unmatched(why) => Some((why.clone(), t.warning)),
        Stage::Error(e) => Some((e.clone(), fmt::money_out())),
        Stage::Done(_, Sent::Saved(name)) if *name == finance_core::upload::upload_name(&row.name) => {
            Some(("saved on the server; the sync brings its rows here".into(), fmt::money_in()))
        }
        Stage::Done(_, Sent::Saved(name)) => Some((format!("saved on the server as {name}"), fmt::money_in())),
        Stage::Done(_, Sent::SameContent) => Some(("the server had this exact file already".into(), muted)),
        Stage::Done(_, Sent::AlreadyOnServer) => Some(("the server has ingested this content: not sent".into(), muted)),
        Stage::Done(_, Sent::Failed(e)) => Some((e.clone(), fmt::money_out())),
    };
    (what, result)
}

fn file_row(row: &FileRow, held: bool, cx: &mut Context<Uploader>) -> Div {
    let t = cx.theme().clone();
    let id = row.id;
    let action = match &row.stage {
        Stage::Unmatched(_) => Some(
            Button::new(("map-columns", id))
                .xsmall()
                .primary()
                .label("Map columns…")
                .on_click(cx.listener(move |this, _, window, cx| this.open_mapper(id, window, cx)))
                .into_any_element(),
        ),
        Stage::Parsed(_) if held => Some(
            Button::new(("held-skip", id))
                .xsmall()
                .ghost()
                .label("Skip")
                .tooltip("Leave this file out")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.queue.remove(id);
                    cx.notify();
                }))
                .into_any_element(),
        ),
        _ => None,
    };
    let mark = match row.stage.preview() {
        Some(p) => logo::tile(logo::for_source(&p.source), 30., cx),
        None => None,
    }
    .unwrap_or_else(|| {
        div()
            .size(px(30.))
            .flex_shrink_0()
            .rounded(px(8.))
            .bg(t.muted)
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::FileText).small().text_color(t.muted_foreground))
            .into_any_element()
    });
    let (what, result) = lines(row, cx);
    let counts = row.stage.preview().map(|p| {
        h_flex()
            .gap(px(6.))
            .when(p.rows == 0, |d| d.child(ui::tag("no rows", cx)))
            .when(p.new > 0, |d| d.child(ui::chip(format!("{} new", fmt::count(p.new as f64)), fmt::money_in(), cx)))
            .when(p.known() > 0, |d| d.child(ui::tag(format!("{} known", fmt::count(p.known() as f64)), cx)))
    });
    h_flex()
        .gap(px(sp::M))
        .py(px(10.))
        .items_start()
        .child(mark)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_size(px(ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(row.name.clone()))
                .child(ui::caption(what, cx).figures())
                .when_some(result, |d, (text, color)| {
                    d.child(div().text_size(px(ts::CAPTION)).text_color(color).line_clamp(2).child(text))
                }),
        )
        .child(h_flex().flex_shrink_0().gap(px(6.)).pt(px(2.)).children(counts).children(action).child(status(row, cx)))
}

fn files_card(queue: &Queue, cx: &mut Context<Uploader>) -> Div {
    let s = queue.summary();
    let mut bits = vec![plural(s.files, "file")];
    if s.rows > 0 {
        bits.push(plural(s.rows, "row"));
        bits.push(format!("{} new", fmt::count(s.new as f64)));
    }
    if s.uploaded > 0 {
        bits.push(format!("{} uploaded", s.uploaded));
    }
    if s.failed > 0 {
        bits.push(format!("{} not taken", s.failed));
    }
    let n = queue.rows.len();
    let rows: Vec<Div> = queue
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let hairline = ui::hairline(cx);
            file_row(r, queue.is_held(r.batch), cx).when(i + 1 < n, |d| d.border_b_1().border_color(hairline))
        })
        .collect();
    let list = v_flex().children(rows);
    let held: Vec<AnyElement> = queue
        .held_ready()
        .into_iter()
        .map(|(batch, k)| {
            Button::new(("release", batch))
                .small()
                .primary()
                .icon(IconName::ArrowUp)
                .label(format!("Upload {}", plural(k, "file")))
                .on_click(cx.listener(move |this, _, _, cx| this.release(batch, cx)))
                .into_any_element()
        })
        .collect();
    ui::card(cx)
        .gap_0()
        .child(
            h_flex()
                .gap_3()
                .justify_between()
                .pb(px(4.))
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .when(s.busy > 0, |d| d.child(ui::pulse_dot("upload-busy", cx.theme().primary, 7.)))
                        .child(div().text_size(px(ts::TITLE)).font_semibold().whitespace_nowrap().child("This session"))
                        .child(ui::caption(bits.join(" · "), cx).figures()),
                )
                .children(held)
                .when(s.busy < s.files, |d| {
                    d.child(
                        Button::new("upload-clear")
                            .small()
                            .ghost()
                            .label("Clear finished")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queue.clear_finished();
                                cx.notify();
                            })),
                    )
                }),
        )
        .child(list)
}

fn history_card(history: &[Ingested], cx: &App) -> Div {
    let t = cx.theme();
    if history.is_empty() {
        return ui::card(cx).child(ui::empty_with(
            IconName::Inbox,
            "Nothing ingested yet",
            "Files the server has ingested show up here after a sync.",
            cx,
        ));
    }
    let n = history.len();
    ui::card(cx).gap_0().py(px(6.)).children(history.iter().enumerate().map(|(i, f)| {
        let added = match f.added {
            Some(0) => "nothing new".to_string(),
            Some(a) => format!("{} new", fmt::count(a as f64)),
            None => "—".into(),
        };
        let label = finance_engine::sources::spec(&f.source).map(|s| s.label.to_string()).unwrap_or_else(|| f.source.clone());
        h_flex()
            .gap(px(sp::M))
            .py(px(8.))
            .when(i + 1 < n, |d| d.border_b_1().border_color(ui::hairline(cx)))
            .children(logo::tile(logo::for_source(&f.source), 24., cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(ts::BODY)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(f.name.clone()))
                    .child(ui::caption(format!("{label} · {} · {added}", plural(f.rows as usize, "row")), cx).figures()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(ts::CAPTION))
                    .text_color(t.muted_foreground)
                    .whitespace_nowrap()
                    .child(if f.at_ms > 0 { fmt::ago(f.at_ms) } else { "—".into() }),
            )
    }))
}

impl Render for Uploader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_history();
        if self.auto_map {
            if let Some(id) = self.queue.rows.iter().find(|r| matches!(r.stage, Stage::Unmatched(_))).map(|r| r.id) {
                self.auto_map = false;
                cx.defer_in(window, move |this, window, cx| this.open_mapper(id, window, cx));
            }
        }
        let t = cx.theme().clone();
        let server = cache::server_url();
        v_flex()
            .w_full()
            .gap(px(sp::GRID))
            .child(drop_zone(&server, self.caps.upload_hint(), cx))
            .when(self.caps.paste_sms(), |d| d.child(paste_card(self, cx)))
            .when_some(self.note.clone(), |d, n| d.child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child(n)))
            .when(!self.queue.rows.is_empty(), |d| d.child(files_card(&self.queue, cx)))
            .child(ui::section("Ingested on the server", "newest first, as of the last sync", cx))
            .child(history_card(&self.history, cx))
    }
}
