//! anser: native, local-first view of the finance replica.
//!
//!   cargo run -p finance-desktop --release
//!
//!   anser mcp [--account NAME]   (no window: MCP on stdio, mcp_local.rs)
//!   anser server <cmd> [args]    (no window: the server CLI, as anser-server)
//!
//! Server: $FINANCE_SERVER, else the active account's in
//!         ~/.config/finance-desktop/profiles.json (the `server` file beside it
//!         mirrors its URL).
//! Theme:  ~/.config/finance-desktop/theme.toml (see theme.rs).

mod app;
mod avatar;
mod budgets;
mod category;
mod catpicker;
mod codeslots;
mod danger;
mod dive;
mod downloads;
mod filterbar;
mod formats;
mod fmt;
mod heatmap;
mod history;
mod icon;
mod layout;
mod linechart;
mod logo;
mod map;
mod mcp_local;
mod memo;
mod plot;
mod settings_sync;
mod pages;
mod palette;
mod theme;
mod timing;
mod targets;
mod ui;
mod zoom;
mod upload;

use app::{FinanceApp, FocusSearch, GoReview, GoUpload, Go0, Go1, Go2, Go3, Go4, Go5, Go6, Go7, Go8, Go9, ZoomIn, ZoomOut, ZoomReset, TogglePrivacy, OpenPalette, RefreshLive, SyncNow, ToggleMode};
use gpui_kit::*;
use crate::zoom::px;

actions!(finance, [Quit]);

/// Hybrid laptops: GPU init enumerates every Vulkan driver, and probing the
/// discrete NVIDIA one wakes it from power-off (~120 ms of startup, then
/// battery for as long as the app runs). A dashboard needs no dGPU, so when
/// an integrated GPU's Mesa driver is installed, show Vulkan only that one,
/// and EGL only Mesa (glvnd; an explicit `__EGL_VENDOR_LIBRARY_*` wins).
/// An explicit VK_DRIVER_FILES / VK_ICD_FILENAMES, or
/// FINANCE_DESKTOP_GPU=any, keeps the default enumeration.
fn prefer_integrated_gpu() {
    if ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"].iter().any(|k| std::env::var_os(k).is_some())
        || std::env::var("FINANCE_DESKTOP_GPU").is_ok_and(|v| v == "any")
    {
        return;
    }
    let icds = ["radeon_icd.x86_64.json", "radeon_icd.json", "intel_icd.x86_64.json", "intel_icd.json"];
    if let Some(icd) = icds.iter().map(|f| std::path::Path::new("/usr/share/vulkan/icd.d").join(f)).find(|p| p.exists()) {
        // SAFETY: first thing in main, before any thread exists
        unsafe { std::env::set_var("VK_DRIVER_FILES", icd) };
        // gpui also starts wgpu's GL backend, whose EGL loads every vendor's
        // library: NVIDIA's were ~35 MB of the app's resident memory, for an
        // adapter it never picks. Mesa's stays, the GL fallback.
        let mesa = std::path::Path::new("/usr/share/glvnd/egl_vendor.d/50_mesa.json");
        if mesa.exists() && std::env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").is_none() && std::env::var_os("__EGL_VENDOR_LIBRARY_DIRS").is_none() {
            // SAFETY: as above
            unsafe { std::env::set_var("__EGL_VENDOR_LIBRARY_FILENAMES", mesa) };
        }
    }
}

fn main() {
    // `anser --version`: before anything touches the GPU or the disk
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("anser {}", finance_core::VERSION);
        return;
    }
    // `anser server <cmd>`: the server's whole CLI (finance_server::cli),
    // what local mode runs as its child; no GPU, profiles or keyring setup
    if std::env::args().nth(1).as_deref() == Some("server") {
        let rest: Vec<String> = std::env::args().skip(2).collect();
        if let Err(e) = finance_server::cli::run_main("anser server", &rest) {
            // what anser-server's `main() -> Result` prints
            eprintln!("Error: {e:?}");
            std::process::exit(1);
        }
        return;
    }
    // `anser mcp`: MCP on stdio over the local copy, before
    // anything that could touch stdout, the GPU or the profiles
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("mcp") {
        if let Err(e) = mcp_local::run(&argv[2..]) {
            eprintln!("anser mcp: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    timing::start();
    prefer_integrated_gpu();
    // format definitions: this machine's and its copy of the server's
    formats::init();
    // `anser --icon PATH`: write the accent-colored icon and exit
    // (used by scripts/install.sh; no window)
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--icon") {
        let path = args.get(2).map(std::path::PathBuf::from).unwrap_or_else(icon::installed_path);
        let cfg = theme::ThemeConfig::load();
        if let Err(e) = icon::write(&path, theme::accent(&cfg)) {
            eprintln!("writing {}: {e}", path.display());
            std::process::exit(1);
        }
        println!("{}", path.display());
        return;
    }
    // accounts that never signed in go before anything reads the profiles
    // (the one "Add an account…" restarted on stays, for this start only)
    let adding = std::env::var(app::accounts_menu::ADDING_ENV).ok();
    let pruned = finance_core::profiles::prune_unfinished(adding.as_deref());
    if !pruned.is_empty() {
        eprintln!("anser: removed accounts that never finished signing in: {}", pruned.join(", "));
    }
    // a local install owns its server: started here when nothing answers,
    // stopped when the app quits (never one this app didn't start)
    if let Some(url) = finance_core::cache::saved_server_url().filter(|u| finance_core::local_server::is_loopback(u)) {
        let log = finance_core::cache::dir().join("anser-server.log");
        if let Err(e) = finance_core::local_server::ensure(&url, &log) {
            eprintln!("anser: local server: {e}");
        }
    }
    gpui_kit::application()
        .with_assets(logo::AppAssets) // gpui-kit's icons + the bundled logos
        .run(|cx| {
            timing::mark("platform up");
            gpui_kit::init(cx);
            timing::mark("kit init");
            cx.bind_keys([
                KeyBinding::new("ctrl-k", OpenPalette, None),
                KeyBinding::new("ctrl-r", SyncNow, Some("FinanceApp")),
                KeyBinding::new("ctrl-shift-k", RefreshLive, Some("FinanceApp")),
                KeyBinding::new("ctrl-t", ToggleMode, Some("FinanceApp")),
                KeyBinding::new("ctrl-f", FocusSearch, Some("FinanceApp")),
                KeyBinding::new("ctrl-1", Go1, Some("FinanceApp")),
                KeyBinding::new("ctrl-2", Go2, Some("FinanceApp")),
                KeyBinding::new("ctrl-3", Go3, Some("FinanceApp")),
                KeyBinding::new("ctrl-4", Go4, Some("FinanceApp")),
                KeyBinding::new("ctrl-5", Go5, Some("FinanceApp")),
                KeyBinding::new("ctrl-6", Go6, Some("FinanceApp")),
                KeyBinding::new("ctrl-7", Go7, Some("FinanceApp")),
                KeyBinding::new("ctrl-8", Go8, Some("FinanceApp")),
                KeyBinding::new("ctrl-9", Go9, Some("FinanceApp")),
                KeyBinding::new("ctrl-,", Go0, Some("FinanceApp")),
                KeyBinding::new("ctrl-y", GoReview, Some("FinanceApp")),
                KeyBinding::new("ctrl-u", GoUpload, Some("FinanceApp")),
                KeyBinding::new("ctrl-=", ZoomIn, Some("FinanceApp")),
                KeyBinding::new("ctrl-+", ZoomIn, Some("FinanceApp")),
                KeyBinding::new("ctrl-shift-=", ZoomIn, Some("FinanceApp")),
                KeyBinding::new("ctrl--", ZoomOut, Some("FinanceApp")),
                KeyBinding::new("ctrl-0", ZoomReset, Some("FinanceApp")),
                KeyBinding::new("ctrl-h", TogglePrivacy, Some("FinanceApp")),
                KeyBinding::new("ctrl-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            // the quit hook gets 200 ms: the stop is synchronous (SIGTERM, a short wait)
            cx.on_app_quit(|_| {
                finance_core::local_server::stop_owned();
                async {}
            })
            .detach();

            theme::register_fonts(cx);
            let theme = theme::ThemeConfig::load();
            theme::apply(&theme, cx);
            cx.set_global(budgets::Budgets::load());
            timing::mark("theme applied");

            let bounds = Bounds::centered(None, size(px(1360.), px(880.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Anser".into()), ..Default::default() }),
                window_min_size: Some(size(px(900.), px(560.))),
                app_id: Some("anser".into()),
                window_background: theme::window_background(&theme),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                zoom::set(theme.zoom);
                zoom::apply(window, cx);
                let view = cx.new(|cx| FinanceApp::new(theme, window, cx));
                window.focus(&view.focus_handle(cx), cx);
                view
            })
            .expect("open window");
            timing::mark("window opened");
            cx.activate(true);
        });
}
