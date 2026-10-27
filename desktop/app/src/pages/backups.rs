//! Settings, Backups: the encrypted `.finance` archives (finance_engine::archive).
//!
//! Desktop:
//! * Backup key: an age key pair made here. The private half stays on this
//!   computer (the OS keyring, else a 0600 file, with a warning); the public
//!   half goes to the server, which seals its nightly archive to it. A copy
//!   of the private half can be saved anywhere: it's the only way to open
//!   those archives.
//! * Fetch latest backup: the newest nightly archive into the archives
//!   folder (~/Backups/finance/archives by default).
//! * Export: the whole account under a passphrase (typed twice, a strength
//!   hint), the API keys only when asked.
//! * Import: an archive, opened with its passphrase or the backup key; a dry
//!   run first, then the import in the chosen mode.
//! * Verify a backup: opened and checked here, nothing sent.
//!
//! In the browser: Export only (the passphrase, then a download).

use finance_core::client::Request;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{json, Value};

use super::sources::{call, json_of};
use crate::fmt;
use crate::ui::{self, ts};
use crate::zoom::px;

#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

type Note = Option<(bool, String)>;

pub struct Backups {
    pass1: Entity<InputState>,
    pass2: Entity<InputState>,
    with_secrets: bool,
    exporting: bool,
    export_note: Note,
    #[cfg(not(target_family = "wasm"))]
    native: Native,
}

#[cfg(not(target_family = "wasm"))]
struct Native {
    recipient: Option<String>,
    public: Option<String>,
    kept_in: Option<finance_core::backups::KeptIn>,
    key_note: Note,
    confirm_replace: bool,
    dir: Entity<InputState>,
    fetching: bool,
    fetch_note: Note,
    import_file: Option<PathBuf>,
    /// the picked archive wants a passphrase (else the backup key)
    import_pass: bool,
    import_secret: Entity<InputState>,
    mode: &'static str,
    prefer_archive: bool,
    /// the dry run's report, for (file, mode)
    dry: Option<(PathBuf, &'static str, String)>,
    importing: bool,
    import_note: Note,
    verifying: bool,
    verify_note: Note,
}

fn web() -> bool {
    cfg!(target_family = "wasm")
}

fn note_el(n: &Note, cx: &App) -> Option<Div> {
    n.as_ref().map(|(ok, s)| div().text_size(px(ts::CAPTION)).whitespace_normal()
        .text_color(if *ok { cx.theme().muted_foreground } else { fmt::money_out() }).child(s.clone()))
}

fn row(label: &str, value: impl IntoElement, cx: &App) -> Div {
    ui::field_row(label, value, cx)
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

impl Backups {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let masked = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).masked(true))
        };
        let pass1 = masked("Passphrase", window, cx);
        let pass2 = masked("Once more", window, cx);
        #[cfg(not(target_family = "wasm"))]
        let native = {
            let dir = finance_core::backups::archives_dir().display().to_string();
            Native {
                recipient: None,
                public: finance_core::backups::public_key(),
                kept_in: finance_core::backups::identity().map(|(_, k)| k),
                key_note: None,
                confirm_replace: false,
                dir: cx.new(|cx| InputState::new(window, cx).default_value(dir)),
                fetching: false,
                fetch_note: None,
                import_file: None,
                import_pass: true,
                import_secret: masked("The archive's passphrase", window, cx),
                mode: "merge",
                prefer_archive: false,
                dry: None,
                importing: false,
                import_note: None,
                verifying: false,
                verify_note: None,
            }
        };
        let mut b = Backups {
            pass1, pass2, with_secrets: false, exporting: false, export_note: None,
            #[cfg(not(target_family = "wasm"))]
            native,
        };
        b.refresh(cx);
        b
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        #[cfg(not(target_family = "wasm"))]
        call(Request::new("GET", "/api/admin/backup-recipient"), cx, |this, r, _| {
            if let Ok(v) = json_of(r) {
                this.native.recipient = v["recipient"].as_str().map(String::from);
            }
        });
        #[cfg(target_family = "wasm")]
        let _ = cx;
    }

    /// The export passphrase, checked: Err(why) when it can't be used.
    fn passphrase(&self, cx: &App) -> Result<String, String> {
        let (a, b) = (self.pass1.read(cx).value().to_string(), self.pass2.read(cx).value().to_string());
        if a.chars().count() < 8 {
            return Err("8 characters at least (four random words is better)".into());
        }
        if a != b {
            return Err("the two passphrases differ".into());
        }
        Ok(a)
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let pass = match self.passphrase(cx) {
            Ok(p) => p,
            Err(e) => {
                self.export_note = Some((false, e));
                cx.notify();
                return;
            }
        };
        let with = self.with_secrets;
        self.export_note = None;
        #[cfg(target_family = "wasm")]
        {
            self.exporting = true;
            call(Request::new("POST", "/api/account/export").json(&json!({"passphrase": pass, "with_secrets": with})), cx, |this, r, _| {
                this.exporting = false;
                this.export_note = Some(match r {
                    Ok(resp) if resp.ok() => match finance_core::client::save_download(&format!("finance-{}.finance", stamp()), &resp.body) {
                        Ok(()) => (true, format!("Saved finance-….finance ({:.1} MB). Keep the passphrase: nothing else opens it.", resp.body.len() as f64 / 1e6)),
                        Err(e) => (false, e),
                    },
                    Ok(resp) => (false, resp.error()),
                    Err(e) => (false, e),
                });
            });
        }
        #[cfg(not(target_family = "wasm"))]
        {
            let dir = self.archives_dir(cx);
            let _ = std::fs::create_dir_all(&dir);
            let picked = cx.prompt_for_new_path(&dir, Some(&format!("finance-{}.finance", stamp())));
            cx.spawn(async move |this, cx| {
                let Ok(Ok(Some(path))) = picked.await else { return };
                let _ = this.update(cx, |this, cx| {
                    this.exporting = true;
                    cx.notify();
                });
                let p = path.clone();
                let task = cx.background_spawn(async move { finance_core::backups::export_to(&p, &pass, with).map_err(|e| format!("{e:#}")) });
                let r = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.exporting = false;
                    this.export_note = Some(match r {
                        Ok(n) => (true, format!("Saved {} ({:.1} MB). Keep the passphrase: nothing else opens it.", path.display(), n as f64 / 1e6)),
                        Err(e) => (false, e),
                    });
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn export_card(&mut self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let a = self.pass1.read(cx).value().to_string();
        let b = self.pass2.read(cx).value().to_string();
        let (level, hint) = finance_core_strength(&a);
        let hint_color = match level { 0 => fmt::money_out(), 1 => t.warning, _ => fmt::money_in() };
        ui::card(cx).gap_2()
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(
                "One encrypted .finance file with everything on the server: the database, your bank exports, the profile and settings. Opens with this passphrase only."))
            .child(h_flex().gap_2().flex_wrap()
                .child(div().w(px(200.)).child(Input::new(&self.pass1).small().mask_toggle()))
                .child(div().w(px(200.)).child(Input::new(&self.pass2).small().mask_toggle())))
            .when(!a.is_empty(), |d| d.child(div().text_size(px(ts::CAPTION))
                .text_color(if !b.is_empty() && a != b { fmt::money_out() } else { hint_color })
                .child(if !b.is_empty() && a != b { "the two differ".to_string() } else { hint.to_string() })))
            .child(Checkbox::new("bk-secrets").label("Include the API keys").checked(self.with_secrets)
                .on_click(cx.listener(|this, v: &bool, _, cx| { this.with_secrets = *v; cx.notify(); })))
            .when(self.with_secrets, |d| d.child(div().text_size(px(ts::CAPTION)).text_color(t.warning).child(
                "The file then holds your exchange and broker keys (read-only ones, but still keys). Keep it somewhere private.")))
            .child(h_flex().child(Button::new("bk-export").small().primary().icon(IconName::ArrowDown)
                .label(if web() { "Export" } else { "Export…" }).loading(self.exporting)
                .on_click(cx.listener(|this, _, _, cx| this.export(cx)))))
            .children(note_el(&self.export_note, cx))
    }
}

fn finance_core_strength(p: &str) -> (u8, &'static str) {
    // the same rule on both builds (core's is native-only)
    let n = p.chars().count();
    let classes = [p.chars().any(|c| c.is_lowercase()), p.chars().any(|c| c.is_uppercase()), p.chars().any(|c| c.is_ascii_digit()),
        p.chars().any(|c| !c.is_alphanumeric())].iter().filter(|b| **b).count();
    let words = p.split_whitespace().count();
    match (n, classes, words) {
        (0..=7, _, _) => (0, "too short: 8 characters at least"),
        (_, _, 4..) if n >= 20 => (3, "strong: a few random words is the best kind"),
        (20.., _, _) => (3, "strong"),
        (12..=19, 3.., _) => (2, "good"),
        (12..=19, _, _) => (1, "fair: longer is better (four random words)"),
        _ => (1, "weak: longer is better (four random words)"),
    }
}

// ---------- the desktop's part ----------

#[cfg(not(target_family = "wasm"))]
impl Backups {
    fn archives_dir(&self, cx: &App) -> PathBuf {
        let d = self.native.dir.read(cx).value().trim().to_string();
        if d.is_empty() {
            return finance_core::backups::archives_dir();
        }
        match d.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
            None => PathBuf::from(d),
        }
    }

    fn generate_key(&mut self, cx: &mut Context<Self>) {
        self.native.confirm_replace = false;
        let (text, public) = finance_engine::archive::generate_identity();
        match finance_core::backups::store_identity(&text) {
            Ok(kept) => {
                self.native.kept_in = Some(kept);
                self.native.public = Some(public.clone());
            }
            Err(e) => {
                self.native.key_note = Some((false, format!("couldn't keep the key: {e:#}")));
                cx.notify();
                return;
            }
        }
        call(Request::new("PUT", "/api/admin/backup-recipient").json(&json!({"recipient": public})), cx, |this, r, _| match json_of(r) {
            Ok(v) => {
                this.native.recipient = v["recipient"].as_str().map(String::from);
                this.native.key_note = Some((true, "The server seals its nightly backup to this key from now on. Save a copy of the key somewhere safe: it's the only way to open those backups.".into()));
            }
            Err(e) => this.native.key_note = Some((false, format!("the key is kept here, but the server didn't take it: {e}"))),
        });
        cx.notify();
    }

    fn send_key(&mut self, cx: &mut Context<Self>) {
        let Some(public) = self.native.public.clone() else { return };
        call(Request::new("PUT", "/api/admin/backup-recipient").json(&json!({"recipient": public})), cx, |this, r, _| match json_of(r) {
            Ok(v) => {
                this.native.recipient = v["recipient"].as_str().map(String::from);
                this.native.key_note = Some((true, "The server has this computer's backup key.".into()));
            }
            Err(e) => this.native.key_note = Some((false, e)),
        });
    }

    fn save_key_copy(&mut self, cx: &mut Context<Self>) {
        let home = dirs::home_dir().unwrap_or_default();
        let picked = cx.prompt_for_new_path(&home, Some("finance-backup-key.txt"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let r = finance_core::backups::save_copy(&path);
            let _ = this.update(cx, |this, cx| {
                this.native.key_note = Some(match r {
                    Ok(()) => (true, format!("Saved to {} (readable by you only). Keep it off this computer too: a password manager, a USB stick.", path.display())),
                    Err(e) => (false, format!("{e:#}")),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn fetch_latest(&mut self, cx: &mut Context<Self>) {
        let dir = self.archives_dir(cx);
        let typed = self.native.dir.read(cx).value().trim().to_string();
        if !typed.is_empty() {
            let _ = finance_core::backups::set_archives_dir(&typed);
        }
        self.native.fetching = true;
        self.native.fetch_note = None;
        let task = cx.background_spawn(async move { finance_core::backups::fetch_latest(&dir).map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.native.fetching = false;
                this.native.fetch_note = Some(match r {
                    Ok((p, n)) => (true, format!("Saved {} ({:.1} MB).", p.display(), n as f64 / 1e6)),
                    Err(e) => (false, e),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pick_archive(&mut self, then_verify: bool, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Open".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                let pass = std::fs::File::open(&path).map_err(anyhow::Error::from).and_then(finance_engine::archive::is_passphrase);
                match pass {
                    Ok(p) => {
                        this.native.import_pass = p;
                        this.native.import_file = Some(path);
                        this.native.dry = None;
                        this.native.import_note = None;
                        this.native.verify_note = None;
                        if then_verify && (!p || !this.native.import_secret.read(cx).value().is_empty()) {
                            this.verify(cx);
                        } else if then_verify {
                            this.native.verify_note = Some((true, "Enter the archive's passphrase, then Verify.".into()));
                        }
                    }
                    Err(e) => this.native.import_note = Some((false, format!("{}: {e:#}", path.display()))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn unseal(&self, cx: &App) -> Result<finance_engine::archive::Unseal, String> {
        if self.native.import_pass {
            let p = self.native.import_secret.read(cx).value().to_string();
            if p.is_empty() {
                return Err("enter the archive's passphrase".into());
            }
            Ok(finance_engine::archive::Unseal::Passphrase(p))
        } else {
            finance_core::backups::identity()
                .map(|(t, _)| finance_engine::archive::Unseal::Identity(t))
                .ok_or_else(|| "this archive is sealed to a backup key, and this computer has none".into())
        }
    }

    fn verify(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.native.import_file.clone() else { return };
        let unseal = match self.unseal(cx) {
            Ok(u) => u,
            Err(e) => {
                self.native.verify_note = Some((false, e));
                cx.notify();
                return;
            }
        };
        self.native.verifying = true;
        let task = cx.background_spawn(async move {
            std::fs::File::open(&file).map_err(anyhow::Error::from)
                .and_then(|f| finance_engine::archive::verify(f, &unseal)).map(|r| r.summary()).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.native.verifying = false;
                this.native.verify_note = Some(match r {
                    Ok(s) => (true, s),
                    Err(e) => (false, format!("Not a good backup: {e}")),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn import(&mut self, dry_run: bool, cx: &mut Context<Self>) {
        let Some(file) = self.native.import_file.clone() else { return };
        let key = if self.native.import_pass {
            let p = self.native.import_secret.read(cx).value().to_string();
            if p.is_empty() {
                self.native.import_note = Some((false, "enter the archive's passphrase".into()));
                cx.notify();
                return;
            }
            finance_core::backups::Key::Passphrase(p)
        } else {
            finance_core::backups::Key::Identity
        };
        let (mode, prefer) = (self.native.mode, self.native.prefer_archive);
        self.native.importing = true;
        self.native.import_note = None;
        let f = file.clone();
        let task = cx.background_spawn(async move {
            finance_core::backups::import(&f, &key, mode, dry_run, prefer).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.native.importing = false;
                match r {
                    Ok(v) => {
                        let text = v["text"].as_str().unwrap_or("").to_string();
                        if dry_run {
                            this.native.dry = Some((file, mode, text));
                        } else {
                            this.native.dry = None;
                            this.native.import_note = Some((true, format!("Imported. {}", summary(&v))));
                            cx.emit(BackupsEvent::Imported);
                        }
                    }
                    Err(e) => this.native.import_note = Some((false, e)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn key_card(&mut self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let n = &self.native;
        let on_server = n.public.is_some() && n.public == n.recipient;
        let mut card = ui::card(cx).gap_2()
            .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(
                "The server seals a backup every night to this computer's backup key, and only that key opens it. Off until you make one."));
        card = card.child(row("Backup key", match &n.public {
            Some(p) => div().text_xs().font_family(t.mono_font_family.clone()).whitespace_normal().child(p.clone()),
            None => div().text_color(t.muted_foreground).child("none yet"),
        }, cx));
        if let Some(k) = &n.kept_in {
            card = card.child(row("Kept in", match k {
                finance_core::backups::KeptIn::Keyring => div().child("the OS keyring"),
                finance_core::backups::KeptIn::File(p) => div().text_color(t.warning).whitespace_normal()
                    .child(format!("{} (readable by you only; there's no keyring here, so anyone with your login can read it)", p.display())),
            }, cx));
        }
        card = card.child(row("Server", div().whitespace_normal().child(match (&n.recipient, on_server) {
            (None, _) => "no backup key: no nightly archive".to_string(),
            (Some(_), true) => "seals its nightly backup to this key".to_string(),
            (Some(r), false) => format!("seals to another key ({}…)", r.chars().take(16).collect::<String>()),
        }), cx));
        let mut buttons = h_flex().gap_2().flex_wrap();
        if n.public.is_none() {
            buttons = buttons.child(Button::new("bk-gen").small().primary().icon(IconName::Plus).label("Generate backup key")
                .on_click(cx.listener(|this, _, _, cx| this.generate_key(cx))));
        } else {
            buttons = buttons
                .child(Button::new("bk-copy").small().outline().label("Save a copy of the key…")
                    .on_click(cx.listener(|this, _, _, cx| this.save_key_copy(cx))))
                .when(!on_server, |d| d.child(Button::new("bk-send").small().outline().label("Give the server this key")
                    .on_click(cx.listener(|this, _, _, cx| this.send_key(cx)))))
                .child(if n.confirm_replace {
                    Button::new("bk-regen-yes").small().danger().label("Replace it (old backups need the old key)")
                        .on_click(cx.listener(|this, _, _, cx| this.generate_key(cx)))
                } else {
                    Button::new("bk-regen").small().ghost().label("New key…")
                        .on_click(cx.listener(|this, _, _, cx| { this.native.confirm_replace = true; cx.notify(); }))
                });
        }
        card.child(buttons).children(note_el(&self.native.key_note, cx))
    }

    fn fetch_card(&mut self, cx: &mut Context<Self>) -> Div {
        ui::card(cx).gap_2()
            .child(row("Folder", Input::new(&self.native.dir).small(), cx))
            .child(h_flex().gap_2()
                .child(Button::new("bk-fetch").small().outline().icon(IconName::ArrowDown).label("Fetch latest backup")
                    .loading(self.native.fetching).disabled(self.native.recipient.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_latest(cx)))))
            .children(note_el(&self.native.fetch_note, cx))
    }

    fn import_card(&mut self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let n = &self.native;
        let file = n.import_file.as_ref().map(|p| p.display().to_string());
        let mode_btn = |id: &'static str, label: &'static str, m: &'static str, sel: bool, cx: &mut Context<Self>| {
            Button::new(id).small().outline().label(label).when(sel, |b| b.icon(IconName::Check))
                .on_click(cx.listener(move |this, _, _, cx| { this.native.mode = m; this.native.dry = None; cx.notify(); }))
        };
        let mode = n.mode;
        let explain = match mode {
            "merge" => "Merge: adds what this server lacks; rows it has are skipped, its own overrides, rules and settings win.",
            "replace" => "Replace: the archive becomes this server's data. A backup of the current database is taken first.",
            _ => "Fresh: only into an empty server (a new install).",
        };
        let dry_ok = n.dry.as_ref().is_some_and(|(f, m, _)| Some(f) == n.import_file.as_ref() && *m == mode);
        let mut card = ui::card(cx).gap_2()
            .child(h_flex().gap_2().flex_wrap().items_center()
                .child(Button::new("bk-pick").small().outline().label("Choose an archive…").on_click(cx.listener(|this, _, _, cx| this.pick_archive(false, cx))))
                .child(Button::new("bk-verify").small().ghost().label("Verify a backup…").loading(n.verifying)
                    .on_click(cx.listener(|this, _, _, cx| if this.native.import_file.is_some() && this.native.verify_note.is_some() {
                        this.verify(cx)
                    } else {
                        this.pick_archive(true, cx)
                    }))))
            .when_some(file, |d, f| d.child(row("Archive", div().text_xs().whitespace_normal().child(f), cx)));
        if n.import_file.is_some() {
            card = card
                .child(if n.import_pass {
                    row("Passphrase", div().w(px(240.)).child(Input::new(&n.import_secret).small().mask_toggle()), cx)
                } else {
                    row("Opens with", div().child(if n.public.is_some() { "this computer's backup key" } else { "a backup key (this computer has none)" }), cx)
                })
                .child(row("Mode", h_flex().gap_2().flex_wrap()
                    .child(mode_btn("bk-merge", "Merge", "merge", mode == "merge", cx))
                    .child(mode_btn("bk-replace", "Replace", "replace", mode == "replace", cx))
                    .child(mode_btn("bk-fresh", "Fresh", "fresh", mode == "fresh", cx)), cx))
                .child(div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground).child(explain))
                .when(mode == "merge", |d| d.child(Checkbox::new("bk-prefer").label("The archive wins conflicts").checked(n.prefer_archive)
                    .on_click(cx.listener(|this, v: &bool, _, cx| { this.native.prefer_archive = *v; this.native.dry = None; cx.notify(); }))))
                .child(h_flex().gap_2()
                    .child(Button::new("bk-dry").small().outline().label("Check first (dry run)").loading(n.importing && !dry_ok)
                        .on_click(cx.listener(|this, _, _, cx| this.import(true, cx))))
                    .when(dry_ok, |d| d.child(Button::new("bk-apply").small().map(|b| if mode == "merge" { b.primary() } else { b.danger() })
                        .label(format!("Import ({mode})")).loading(n.importing)
                        .on_click(cx.listener(|this, _, _, cx| this.import(false, cx))))));
            if let Some((_, _, text)) = n.dry.as_ref().filter(|_| dry_ok) {
                card = card.child(div().p_2().rounded(t.radius).bg(t.muted).text_xs().font_family(t.mono_font_family.clone())
                    .whitespace_normal().children(text.lines().map(|l| div().child(l.to_string()))));
            }
        }
        card.children(note_el(&self.native.import_note, cx)).children(note_el(&self.native.verify_note, cx))
    }
}

#[cfg(not(target_family = "wasm"))]
fn summary(v: &Value) -> String {
    let tables = v["report"]["tables"].as_array().cloned().unwrap_or_default();
    let added: i64 = tables.iter().filter_map(|t| t["added"].as_i64()).sum();
    let conflicts: usize = tables.iter().map(|t| t["conflicts"].as_array().map(|c| c.len()).unwrap_or(0)).sum();
    format!("{} rows added, {conflicts} conflicts. Sync to see them here.", fmt::count(added as f64))
}

pub enum BackupsEvent {
    /// an import changed the server's data: sync
    Imported,
}

impl EventEmitter<BackupsEvent> for Backups {}

impl Render for Backups {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let export = self.export_card(cx);
        #[cfg(target_family = "wasm")]
        {
            v_flex().gap_2().child(ui::section("Export", "everything, in one encrypted file", cx)).child(export)
        }
        #[cfg(not(target_family = "wasm"))]
        {
            let key = self.key_card(cx);
            let fetch = self.fetch_card(cx);
            let import = self.import_card(cx);
            v_flex().gap_2()
                .child(ui::section("Backups", "nightly, encrypted to this computer's key", cx))
                .child(key)
                .child(fetch)
                .child(ui::section("Export", "everything, in one encrypted file", cx))
                .child(export)
                .child(ui::section("Import", "an archive into the server, checked first", cx))
                .child(import)
        }
    }
}
