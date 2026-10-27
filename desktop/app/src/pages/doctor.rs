//! Doctor: is everything around the app healthy, and one click to fix what
//! isn't. The remote server comes first (reachable, version, round trip,
//! last sync, replica age), then the tailnet, the replica, the data sources
//! as the server reports them (IBKR, Binance, OKX, bank exports; GET
//! /api/sources), phone access and the web app, the desktop integration,
//! the GPU, the theme, the logos and the config files. An API source this
//! user never set up (no keys, no data) has no row (`dr::unused_api_checks`).
//!
//! Its own view (an entity): every check runs on the background executor
//! and its row fills in as it lands. Each run first takes stock of the
//! machine (`finance_core::tools::detect`: VPNs, package manager, Hyprland,
//! OS) and shows only the checks that apply (`CheckId::applies`). The verdicts live in
//! `finance_core::doctor` (with tests); this file gathers what only the app
//! knows (the theme, the icon, the logos, its config types) and draws.
//!
//! Fixes are one click when they stay inside the app's own files: install or
//! repair the launcher, sync, set up the server, start a local `anser-server`.
//! Anything that changes system settings (DNS, packages, the compositor) is
//! shown as a command to copy, never run.
//!
//! Dev aid: `FINANCE_DESKTOP_PAGE=doctor`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use finance_core::doctor::{self as dr, Action, Check, CheckId, Fix, InstallKind, Installer, Status};
use finance_core::tools::{self, Env};
use finance_core::local_server::{self, Ensured};
use finance_core::{cache, qr::Qr, webapp};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fmt;
use crate::theme::{self, ThemeConfig};
use crate::ui::{self, ts};
use crate::zoom::px;

/// Asks the app to sync (it owns the sync state and reloads every page),
/// or to open the Upload page.
pub enum DoctorEvent {
    SyncNow,
    OpenUpload,
}

/// A one-click install: its log as it runs, then how it ended.
struct InstallRun {
    lines: Arc<Mutex<Vec<String>>>,
    done: Option<Result<(), String>>,
}

enum LocalServer {
    Starting,
    Up(String),
    Failed(String),
}

pub struct Doctor {
    /// bumps on every run, so a slow result from an older run is dropped
    generation: u64,
    results: HashMap<CheckId, Check>,
    /// what this machine has (this run's), None while it's being detected
    env: Option<Arc<Env>>,
    /// the checks that apply to it and the server, in page order
    applicable: Vec<CheckId>,
    checked_at: Option<String>,
    server_input: Entity<InputState>,
    setup_open: bool,
    setup_note: Option<(bool, String)>,
    installs: HashMap<InstallKind, InstallRun>,
    local: Option<LocalServer>,
    copied: Option<String>,
    sync_pending: bool,
    /// what building the web app needs and this machine lacks (from the last run)
    webapp_missing: Option<webapp::Missing>,
    /// the Server row's pairing form (a code from `anser-server pair`)
    pair_open: bool,
    pair_code: Entity<InputState>,
    pairing: bool,
    pair_note: Option<(bool, String)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DoctorEvent> for Doctor {}

/// The installer behind each install action. `missing`: the web app's
/// toolchain gaps, as the last run found them.
pub fn installer(kind: InstallKind, cfg: &ThemeConfig, missing: Option<&webapp::Missing>) -> Box<dyn Installer> {
    match kind {
        InstallKind::Desktop => Box::new(dr::DesktopInstaller {
            paths: dr::DesktopPaths::user(),
            exe: this_exe(),
            icon_svg: crate::icon::svg(theme::accent(cfg)),
        }),
        InstallKind::WebApp => Box::new(dr::WebAppInstaller { server: cache::server_url(), missing: missing.cloned() }),
    }
}

/// An install log shows its last lines (a build prints hundreds).
const LOG_TAIL: usize = 14;

fn this_exe() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok()).unwrap_or_default()
}

/// The theme file as saved (never created here, unlike ThemeConfig::load).
fn theme_now() -> ThemeConfig {
    std::fs::read_to_string(theme::config_path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

fn parse<T: serde::de::DeserializeOwned>(s: &str) -> Result<(), String> {
    toml::from_str::<T>(s).map(|_| ()).map_err(|e| e.to_string().lines().next().unwrap_or("").to_string())
}

/// The checks that need nothing from the network, one closure each.
fn local_checks(env: Arc<Env>) -> Vec<(CheckId, Box<dyn FnOnce() -> Check + Send>)> {
    let (gpu_env, theme_env) = (env.clone(), env);
    vec![
        (CheckId::Replica, Box::new(|| dr::replica(&cache::db_path(), &cache::bundle_path()))),
        (CheckId::Sources, Box::new(|| {
            match finance_core::Bundle::load(&cache::bundle_path()) {
                Ok(b) => dr::sources_check(&finance_core::capabilities::Capabilities::of(&b), chrono::Utc::now().timestamp_millis()),
                Err(_) => Check::new(CheckId::Sources, Status::Missing, "No local copy yet: sync once to see what the server has.")
                    .fix(Fix::Action(Action::SyncNow)),
            }
        })),
        (CheckId::Desktop, Box::new(|| {
            let cfg = theme_now();
            dr::desktop_check(&dr::DesktopPaths::user(), &this_exe(), &crate::icon::svg(theme::accent(&cfg)))
        })),
        (CheckId::Gpu, Box::new(move || dr::gpu_check(&dr::gpu_facts(), gpu_env.pkg))),
        (CheckId::Theme, Box::new(move || {
            let cfg = theme_now();
            let desktop = match cfg.accent.as_str() {
                "matugen" | "matugen-accent" => match cfg.desktop_source() {
                    theme::DesktopSource::Omarchy { file, .. } => Some(("Omarchy", file)),
                    theme::DesktopSource::Matugen(file) => Some(("matugen", file)),
                    // nothing found: report the file that was expected
                    theme::DesktopSource::None if cfg.desktop_theme == "matugen" => Some(("matugen", cfg.matugen_path())),
                    theme::DesktopSource::None => Some(("Omarchy or matugen", cfg.matugen_path())),
                },
                _ => None,
            }
            .map(|(source, path)| {
                let text = std::fs::read_to_string(&path).map_err(|_| "not found".to_string());
                let read = text.and_then(|s| match source {
                    "Omarchy" => theme::parse_omarchy(&s, false).map(|_| ()).ok_or_else(|| "doesn't have a background, foreground and accent".to_string()),
                    _ => serde_json::from_str::<serde_json::Value>(&s).map(|_| ()).map_err(|e| format!("doesn't parse ({e})")),
                });
                (source, path, read)
            });
            let blur = if cfg.glass() { dr::hypr_blur(&theme_env) } else { None };
            let label = theme::PALETTES.iter().find(|(id, _)| *id == cfg.palette).map(|(_, l)| *l).unwrap_or(cfg.palette.as_str());
            dr::theme_check(label, desktop.as_ref().map(|(s, p, r)| (*s, p.as_path(), r.clone())), cfg.glass(), blur.as_ref())
        })),
        (CheckId::Logos, Box::new(|| dr::logos_check(crate::logo::LOGOS.len(), theme_now().logos))),
        (CheckId::Config, Box::new(|| {
            let files = vec![
                dr::ConfigFile { name: "theme", path: theme::config_path(), read: dr::read_with(&theme::config_path(), parse::<ThemeConfig>) },
                dr::ConfigFile { name: "server", path: cache::server_file(), read: dr::read_server_file(&cache::server_file()) },
                dr::ConfigFile { name: "budgets", path: crate::budgets::path(),
                    read: dr::read_with(&crate::budgets::path(), parse::<crate::budgets::Budgets>) },
                dr::ConfigFile { name: "calendar", path: super::cashflow::config_path(),
                    read: dr::read_with(&super::cashflow::config_path(), parse::<super::cashflow::Config>) },
            ];
            dr::config_check(&files)
        })),
    ]
}

impl Doctor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let server_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://your-server.tailXXXX.ts.net:8787").default_value(cache::server_url())
        });
        let subs = vec![cx.subscribe_in(&server_input, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.save_and_test(window, cx);
            }
        })];
        let pair_code = cx.new(|cx| InputState::new(window, cx).placeholder("Pairing code, ABCD-EFGH"));
        Doctor {
            // dev/screenshot aid: FINANCE_DESKTOP_PAIR=1 opens the pairing form
            pair_open: std::env::var_os("FINANCE_DESKTOP_PAIR").is_some(),
            pair_code,
            pairing: false,
            pair_note: None,
            generation: 0,
            results: HashMap::new(),
            env: None,
            applicable: Vec::new(),
            checked_at: None,
            server_input,
            setup_open: false,
            setup_note: None,
            installs: HashMap::new(),
            local: None,
            copied: None,
            sync_pending: false,
            webapp_missing: None,
            _subs: subs,
        }
    }

    fn running(&self) -> bool {
        self.generation > 0 && (self.env.is_none() || self.results.len() < self.applicable.len())
    }

    /// Every check again, each on the background executor; rows fill in as
    /// they land. The machine is taken stock of first (a few stat calls), so
    /// only the checks that apply run.
    pub fn run(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.results.clear();
        self.checked_at = None;
        self.env = None;
        let detect = cx.background_spawn(async { tools::detect() });
        cx.spawn(async move |this, cx| {
            let env = detect.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.start(generation, Arc::new(env), cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn start(&mut self, generation: u64, env: Arc<Env>, cx: &mut Context<Self>) {
        // the server, phone and web-app rows share one probe
        let url = cache::server_url();
        self.applicable = CheckId::applicable(&env, &url);
        self.env = Some(env.clone());
        let source = dr::url_source();
        let can_start = local_server::local_server_bin(&this_exe()).is_some();
        let probe_url = url.clone();
        let remote = cx.background_spawn(async move {
            let probe = dr::probe(&probe_url);
            let local = dr::local_bundle();
            let missing = webapp::toolchain();
            let sources = match &probe {
                Ok(_) => dr::sources_probe(&probe_url),
                Err(_) => Err("the server doesn't answer".to_string()),
            };
            // what the server thinks of this device: paired as X, or not
            let who = probe.as_ref().ok().map(|_| finance_core::auth::whoami(&probe_url).map_err(|e| format!("{e:#}")));
            let now = chrono::Utc::now().timestamp_millis();
            let unused = dr::unused_api_checks(&sources);
            let checks = vec![
                dr::with_pairing(dr::server_check(&probe_url, source, &probe, local.as_ref(), can_start, now), who.as_ref()),
                dr::phone_check(&probe_url, &probe),
                dr::webapp_check(&probe_url, &probe, missing.as_ref()),
                dr::ibkr_check(&sources, fmt::hkd),
                dr::exchange_check(CheckId::Binance, "binance", &sources),
                dr::exchange_check(CheckId::Okx, "okx", &sources),
                dr::exports_check(&sources),
            ];
            (checks, missing, unused)
        });
        cx.spawn(async move |this, cx| {
            let (checks, missing, unused) = remote.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.webapp_missing = missing;
                    // a source the user doesn't use: no row
                    this.applicable.retain(|id| !unused.contains(id));
                    this.accept(checks, cx);
                }
            });
        })
        .detach();
        if self.applicable.contains(&CheckId::Tailnet) {
            let (tail_url, tail_env) = (url, env.clone());
            self.land(generation, cx.background_spawn(async move { vec![dr::tailnet(&tail_url, &tail_env)] }), cx);
        }
        for (id, check) in local_checks(env) {
            if self.applicable.contains(&id) {
                self.land(generation, cx.background_spawn(async move { vec![check()] }), cx);
            }
        }
    }

    fn land(&mut self, generation: u64, task: Task<Vec<Check>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let checks = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.accept(checks, cx);
                }
            });
        })
        .detach();
    }

    fn accept(&mut self, checks: Vec<Check>, cx: &mut Context<Self>) {
        // the remote batch carries every row; keep the ones that apply
        for c in checks.into_iter().filter(|c| self.applicable.contains(&c.id)) {
            if c.id == CheckId::Server && matches!(c.status, Status::Fail | Status::Missing) {
                self.setup_open = true;
            }
            self.results.insert(c.id, c);
        }
        if self.results.len() == self.applicable.len() {
            self.checked_at = Some(chrono::Local::now().format("%H:%M:%S").to_string());
        }
        cx.notify();
    }

    /// Re-run one check (after its fix ran).
    fn rerun(&mut self, id: CheckId, cx: &mut Context<Self>) {
        let local = self.env.clone().and_then(|env| local_checks(env).into_iter().find(|(i, _)| *i == id));
        if let Some((_, check)) = local {
            self.results.remove(&id);
            let generation = self.generation;
            self.land(generation, cx.background_spawn(async move { vec![check()] }), cx);
        } else {
            self.run(cx);
        }
    }

    /// The app finished a sync this page asked for.
    pub fn synced(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.sync_pending) {
            self.run(cx);
        }
    }

    /// A fix button on row `from` was clicked.
    fn act(&mut self, from: CheckId, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::SyncNow => {
                self.sync_pending = true;
                cx.emit(DoctorEvent::SyncNow);
            }
            // the Server row toggles its form; any other row opens it
            Action::SetupServer => self.setup_open = from != CheckId::Server || !self.setup_open,
            Action::StartLocalServer => self.start_local(cx),
            Action::OpenUpload => cx.emit(DoctorEvent::OpenUpload),
            Action::Install(kind) => self.install(kind, cx),
            Action::Pair => self.pair_open = !self.pair_open,
            Action::LockReads => self.lock_reads(cx),
        }
        let _ = window;
        cx.notify();
    }

    fn install(&mut self, kind: InstallKind, cx: &mut Context<Self>) {
        if self.installs.get(&kind).is_some_and(|r| r.done.is_none()) {
            return;
        }
        let inst = installer(kind, &theme_now(), self.webapp_missing.as_ref());
        if inst.unavailable().is_some() {
            return;
        }
        let lines = Arc::new(Mutex::new(Vec::new()));
        self.installs.insert(kind, InstallRun { lines: lines.clone(), done: None });
        let log = lines.clone();
        let task = cx.background_spawn(async move {
            inst.run(&|l| log.lock().unwrap_or_else(|e| e.into_inner()).push(l)).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            // the log ticks while it runs
            let tick = this.clone();
            let ticker = cx.spawn(async move |cx| loop {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                if tick.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            });
            let res = task.await;
            drop(ticker);
            let _ = this.update(cx, |this, cx| {
                if let Some(r) = this.installs.get_mut(&kind) {
                    r.done = Some(res);
                }
                match kind {
                    InstallKind::Desktop => this.rerun(CheckId::Desktop, cx),
                    InstallKind::WebApp => this.run(cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_local(&mut self, cx: &mut Context<Self>) {
        if matches!(self.local, Some(LocalServer::Starting)) {
            return;
        }
        self.local = Some(LocalServer::Starting);
        let log = cache::dir().join("anser-server.log");
        let task = cx.background_spawn(async move {
            local_server::ensure(dr::LOCAL_SERVER, &this_exe(), &log).map(|e| match e {
                Ensured::Started(pid) => format!("anser-server (pid {pid}) answers on {}. It stops when the app quits; log: {}.",
                    dr::LOCAL_SERVER, log.display()),
                _ => format!("anser-server already answers on {}.", dr::LOCAL_SERVER),
            })
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| {
                this.local = Some(match res {
                    Ok(s) => LocalServer::Up(s),
                    Err(e) => LocalServer::Failed(e),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn use_url(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.server_input.update(cx, |s, cx| s.set_value(url.to_string(), window, cx));
        self.save_and_test(window, cx);
    }

    /// Save the URL in the field, then test it (and everything that hangs on it).
    fn save_and_test(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let url = self.server_input.read(cx).value().trim().trim_end_matches('/').to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.setup_note = Some((false, "Enter a URL starting with http:// or https://.".into()));
            cx.notify();
            return;
        }
        match cache::save_server_url(&url) {
            Err(e) => self.setup_note = Some((false, format!("Couldn't save: {e:#}"))),
            Ok(()) => {
                let env = std::env::var("FINANCE_SERVER").ok().map(|e| format!(" $FINANCE_SERVER ({e}) still wins for this session.")).unwrap_or_default();
                self.setup_note = Some((true, format!("Saved to {}. Testing…{env}", cache::server_file().display())));
                self.run(cx);
            }
        }
        cx.notify();
    }

    fn copy(&mut self, cmd: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(cmd.to_string()));
        self.copied = Some(cmd.to_string());
        cx.notify();
    }
}

// ---------- drawing ----------

fn status_color(s: Status, cx: &App) -> Hsla {
    let t = cx.theme();
    match s {
        Status::Ok => fmt::money_in(),
        Status::Warn => t.warning,
        Status::Missing => t.primary,
        Status::Fail => fmt::money_out(),
    }
}

/// The QR code, drawn module by module: dark on white whatever the theme
/// (scanners want that contrast), with the standard 4-module quiet zone.
pub(crate) fn qr_view(qr: Rc<Qr>, side: f32) -> impl IntoElement {
    canvas(|_, _, _| (), move |b, _, window, _| {
        let n = qr.size + 8;
        let m = (f32::from(b.size.width) / n as f32).floor().max(1.);
        let off = ((f32::from(b.size.width) - m * n as f32) / 2.).floor();
        window.paint_quad(fill(b, gpui_kit::white()));
        for y in 0..qr.size {
            for x in 0..qr.size {
                if qr.dark(x, y) {
                    let at = point(b.origin.x + gpui_kit::px(off + m * (x + 4) as f32), b.origin.y + gpui_kit::px(off + m * (y + 4) as f32));
                    window.paint_quad(fill(Bounds::new(at, size(gpui_kit::px(m), gpui_kit::px(m))), gpui_kit::black()));
                }
            }
        }
    })
    .size(px(side))
    .flex_shrink_0()
}

fn command_box(id: SharedString, cmd: &str, copied: bool, cx: &mut Context<Doctor>) -> impl IntoElement {
    let t = cx.theme();
    let c = cmd.to_string();
    h_flex()
        .gap_2()
        .items_center()
        .flex_wrap()
        .child(div().px_2().py(px(3.)).rounded(t.radius).border_1().border_color(ui::hairline(cx)).bg(t.muted)
            .font_family(t.mono_font_family.clone()).text_size(px(ts::CAPTION)).child(cmd.to_string()))
        .child(Button::new(id).xsmall().ghost().label(if copied { "Copied" } else { "Copy" })
            .on_click(cx.listener(move |this, _, _, cx| this.copy(&c, cx))))
}

impl Doctor {
    fn fix_row(&self, check: &Check, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let mut buttons = h_flex().gap_2().flex_wrap().items_center();
        let mut commands = v_flex().gap_1();
        for (i, fix) in check.fixes.iter().enumerate() {
            let id = SharedString::from(format!("fix-{:?}-{i}", check.id));
            match fix {
                Fix::Command(cmd) => {
                    let copied = self.copied.as_deref() == Some(cmd.as_str());
                    commands = commands.child(command_box(id, cmd, copied, cx));
                }
                // the open setup form has its own start button
                Fix::Action(Action::StartLocalServer) if self.setup_open && check.id == CheckId::Server => {}
                Fix::Action(action) => {
                    let (action, from) = (*action, check.id);
                    let (label, primary, loading, disabled, note) = match action {
                        Action::SyncNow => ("Sync now", true, self.sync_pending, false, None),
                        Action::SetupServer if from != CheckId::Server => ("Set up server", false, false, false, None),
                        Action::SetupServer => (if self.setup_open { "Hide setup" } else { "Set up server" }, !self.setup_open, false, false, None),
                        Action::StartLocalServer => ("Start a local anser-server", false, matches!(self.local, Some(LocalServer::Starting)), false, None),
                        Action::OpenUpload => ("Open Upload", true, false, false, None),
                        Action::Pair => (if self.pair_open { "Hide pairing" } else { "Pair this device" }, !self.pair_open, false, false, None),
                        Action::LockReads => ("Lock reads (FINANCE_AUTH=all)", false, false, false, None),
                        Action::Install(kind) => {
                            let inst = installer(kind, &theme_now(), self.webapp_missing.as_ref());
                            let running = self.installs.get(&kind).is_some_and(|r| r.done.is_none());
                            let label = match (kind, check.status) {
                                (InstallKind::WebApp, Status::Ok) => "Update web app on the server",
                                (InstallKind::WebApp, _) => "Install web app on the server",
                                (_, Status::Missing) => "Install",
                                _ => "Repair",
                            };
                            let why = inst.unavailable();
                            (label, why.is_none(), running, why.is_some(), why)
                        }
                    };
                    let b = Button::new(id).small().label(label).loading(loading).disabled(disabled)
                        .on_click(cx.listener(move |this, _, window, cx| this.act(from, action, window, cx)));
                    buttons = buttons.child(if primary { b.primary() } else { b.outline() });
                    if let Some(n) = note {
                        buttons = buttons.child(ui::tag(n, cx));
                    }
                }
            }
        }
        // install logs
        let logs = check.fixes.iter().filter_map(|f| match f {
            Fix::Action(Action::Install(kind)) => self.installs.get(kind),
            _ => None,
        }).map(|run| {
            let lines = run.lines.lock().map(|l| l.clone()).unwrap_or_default();
            let earlier = lines.len().saturating_sub(LOG_TAIL);
            let tail = match &run.done {
                None => None,
                Some(Ok(())) => Some(("finished".to_string(), fmt::money_in())),
                Some(Err(e)) => Some((format!("failed: {e}"), fmt::money_out())),
            };
            v_flex().gap(px(2.)).p_2().rounded(t.radius).bg(t.muted).font_family(t.mono_font_family.clone())
                .text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
                .when(earlier > 0, |d| d.child(div().child(format!("… {earlier} earlier lines"))))
                .children(lines.into_iter().skip(earlier).map(|l| div().child(l)))
                .when_some(tail, |d, (s, c)| d.child(div().text_color(c).child(s)))
        });
        v_flex().gap_2().child(buttons).child(commands).children(logs)
    }

    /// Pair with the server: a code from `anser-server pair` (or an admin's
    /// Pair another device in Settings), traded for this machine's token.
    fn pair_form(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        v_flex().gap_2().p(px(12.)).rounded(t.radius).border_1().border_color(ui::hairline(cx))
            .child(div().text_size(px(ts::BODY)).font_semibold().child("Pair this device"))
            .child(ui::caption("Run anser-server pair on the server (or Settings, Account, Pair another device on a signed-in device), then enter the code.", cx))
            .child(h_flex().gap_2().flex_wrap().items_center()
                .child(div().w(px(180.)).child(Input::new(&self.pair_code).small()))
                .child(Button::new("pair-go").small().primary().label("Pair").loading(self.pairing)
                    .on_click(cx.listener(|this, _, _, cx| this.pair(cx)))))
            .when_some(self.pair_note.clone(), |d, (ok, n)| d.child(div().text_size(px(ts::CAPTION))
                .text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(n)))
    }

    fn pair(&mut self, cx: &mut Context<Self>) {
        let code = self.pair_code.read(cx).value().trim().to_string();
        let server = cache::server_url();
        self.pairing = true;
        let task = cx.background_spawn(async move {
            finance_core::auth::pair(&server, &code, &finance_core::auth::default_device_name()).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.pairing = false;
                match r {
                    Ok(d) => {
                        this.pair_note = Some((true, format!("Paired as {}.", d.name)));
                        this.pair_open = false;
                        this.run(cx);
                    }
                    Err(e) => this.pair_note = Some((false, e)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn lock_reads(&mut self, cx: &mut Context<Self>) {
        let req = finance_core::client::Request::new("POST", "/api/admin/auth-mode").json(&serde_json::json!({"mode": "all"}));
        let fut = finance_core::client::send(req);
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(resp) = &r {
                    if !resp.ok() {
                        this.pair_note = Some((false, resp.error()));
                    }
                }
                this.run(cx);
            });
        })
        .detach();
    }

    fn setup_form(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.theme().clone();
        let local_bin = local_server::local_server_bin(&this_exe());
        let local = match &self.local {
            None => None,
            Some(LocalServer::Starting) => Some(div().text_color(t.muted_foreground).child("starting anser-server…").into_any_element()),
            Some(LocalServer::Failed(e)) => Some(div().text_color(fmt::money_out()).child(e.clone()).into_any_element()),
            Some(LocalServer::Up(s)) => Some(h_flex().gap_2().flex_wrap().items_center()
                .child(div().text_color(fmt::money_in()).child(s.clone()))
                .child(Button::new("use-local").small().outline().label("Use it")
                    .on_click(cx.listener(|this, _, window, cx| this.use_url(dr::LOCAL_SERVER, window, cx))))
                .into_any_element()),
        };
        v_flex()
            .gap_2()
            .p(px(12.))
            .rounded(t.radius)
            .border_1()
            .border_color(ui::hairline(cx))
            .child(div().text_size(px(ts::BODY)).font_semibold().child("Server setup"))
            .child(ui::caption("1. The URL of the finance server: your server's tailnet address, or a local anser-server.", cx))
            .child(h_flex().gap_2().flex_wrap().items_center()
                .child(div().flex_1().min_w(px(220.)).child(Input::new(&self.server_input).small()))
                .child(Button::new("setup-save").small().primary().label("Save and test")
                    .on_click(cx.listener(|this, _, window, cx| this.save_and_test(window, cx)))))
            .when_some(self.setup_note.clone(), |d, (ok, n)| d.child(div().text_size(px(ts::CAPTION))
                .text_color(if ok { t.muted_foreground } else { fmt::money_out() }).child(n)))
            .child(ui::caption(match &local_bin {
                Some(b) => format!("2. Or run a server on this machine: {}", b.display()),
                None => "2. Or run a server on this machine: install anser-server with curl -fsSL https://raw.githubusercontent.com/lucianosrp/anser/main/install.sh | sh -s -- --server, or build it (cargo build -p finance-server --release).".into(),
            }, cx))
            .when(local_bin.is_some(), |d| d.child(h_flex().gap_2().child(
                Button::new("setup-local").small().outline().label("Start a local anser-server")
                    .loading(matches!(self.local, Some(LocalServer::Starting)))
                    .on_click(cx.listener(|this, _, _, cx| { this.start_local(cx); cx.notify(); })))))
            .children(local.map(|l| div().text_size(px(ts::CAPTION)).child(l)))
    }

    fn row(&self, id: CheckId, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().clone();
        let Some(check) = self.results.get(&id).cloned() else {
            return ui::card(cx)
                .child(h_flex().gap_3().items_center()
                    .child(ui::pulse_dot(SharedString::from(format!("doc-pending-{id:?}")), t.muted_foreground, 9.))
                    .child(div().text_size(px(ts::TITLE)).font_semibold().child(id.title()))
                    .child(ui::caption("checking…", cx)))
                .into_any_element();
        };
        let color = status_color(check.status, cx);
        let facts = v_flex().gap(px(2.)).children(check.facts.iter().map(|(k, v)| {
            h_flex().gap_3().items_start().text_size(px(ts::CAPTION))
                .child(div().w(px(96.)).flex_shrink_0().text_color(t.muted_foreground).child(k.clone()))
                .child(div().flex_1().min_w_0().child(v.clone()))
        }));
        let qr = (id == CheckId::Phone)
            .then(|| check.facts.iter().find(|(k, _)| k == "open").and_then(|(_, url)| Qr::encode(url)))
            .flatten()
            .map(|q| qr_view(Rc::new(q), 132.));
        let body = v_flex()
            .flex_1()
            .min_w(px(240.))
            .gap_2()
            .child(h_flex().gap_2().items_center()
                .child(div().text_size(px(ts::TITLE)).font_semibold().child(id.title()))
                .child(ui::chip(check.status.label(), color, cx)))
            .child(div().text_size(px(ts::BODY)).child(check.summary.clone()))
            .child(facts)
            .when(!check.fixes.is_empty(), |d| d.child(self.fix_row(&check, cx)))
            .when(id == CheckId::Server && self.setup_open, |d| d.child(self.setup_form(cx)))
            .when(id == CheckId::Server && self.pair_open, |d| d.child(self.pair_form(cx)));
        ui::card(cx)
            .child(h_flex().gap_3().items_start().flex_wrap()
                .child(div().pt(px(6.)).child(ui::dot(color, 9.)))
                .child(body)
                .children(qr))
            .into_any_element()
    }
}

/// Dev aid: `FINANCE_DESKTOP_DOCTOR=gpu,theme` shows only those rows (by
/// title prefix), so a screenshot reaches the bottom of the page.
fn shown(id: &CheckId) -> bool {
    match std::env::var("FINANCE_DESKTOP_DOCTOR") {
        Ok(only) => only.split(',').any(|w| !w.trim().is_empty() && id.title().to_lowercase().starts_with(&w.trim().to_lowercase())),
        Err(_) => true,
    }
}

impl Render for Doctor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.generation == 0 {
            self.run(cx); // first time the page shows
        }
        let t = cx.theme().clone();
        let mut counts: HashMap<Status, usize> = HashMap::new();
        for c in self.results.values() {
            *counts.entry(c.status).or_default() += 1;
        }
        let worst = self.results.values().map(|c| c.status).max();
        let headline = match (self.running(), worst) {
            (true, _) => "Checking…".to_string(),
            (false, Some(Status::Ok)) => "Everything is healthy.".into(),
            (false, Some(Status::Warn)) => "Working, with something to look at.".into(),
            (false, Some(Status::Missing)) => "Working; some optional parts aren't installed.".into(),
            (false, Some(Status::Fail)) => "Something is broken.".into(),
            (false, None) => String::new(),
        };
        let chips = h_flex().gap_2().flex_wrap().children([Status::Ok, Status::Warn, Status::Missing, Status::Fail].into_iter()
            .filter_map(|s| counts.get(&s).map(|n| ui::chip(format!("{n} {}", s.label()), status_color(s, cx), cx))));
        let summary = ui::card(cx).child(h_flex().gap_3().items_center().flex_wrap()
            .child(v_flex().flex_1().min_w(px(200.)).gap_1()
                .child(div().text_size(px(ts::TITLE)).font_semibold().child(headline))
                .child(chips))
            .child(v_flex().items_end().gap_1()
                .child(Button::new("doctor-run").small().outline().label("Run again").loading(self.running())
                    .on_click(cx.listener(|this, _, _, cx| { this.run(cx); cx.notify(); })))
                .child(ui::caption(match &self.checked_at {
                    Some(at) => format!("checked at {at}"),
                    None => "running every check…".into(),
                }, cx))));
        let note = div().text_size(px(ts::CAPTION)).text_color(t.muted_foreground)
            .child("Buttons only touch the app's own files. Commands that change system settings are shown for you to run.");
        v_flex()
            .w_full()
            .gap(px(ui::sp::GRID))
            .child(summary)
            .children(self.applicable.clone().into_iter().filter(shown).map(|id| self.row(id, cx)))
            .child(note)
    }
}
