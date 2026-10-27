//! finance-web: the desktop app in a browser tab.
//!
//! The pages are the desktop app's own source files (`desktop/app/src`),
//! compiled a second time for wasm32 on gpui's web backend (WebGPU, falling
//! back to WebGL2; see `backend`). Only the edges differ:
//!
//! * data: no SQLite in the browser. Before the app starts, this entry point
//!   fetches `/api/ledger.json` and `/api/export.json` from the server that
//!   served the page and parks them in `finance_core::preload`, where the
//!   app's ordinary `Ledger::load` / `Bundle::load` find them.
//! * no filesystem: the theme (mode, palette, accent, zoom...) is kept in
//!   localStorage, read before the first frame, so a reload keeps it; the
//!   URL's `?mode=` / `?palette=` win over it (and that load saves nothing). Budgets
//!   and the calendar come from the server.
//! * fonts: the browser exposes no system fonts to gpui, so Noto Sans
//!   (what the desktop resolves "sans-serif" to) is bundled, subset.
//! * one thread: gpui's web dispatcher runs background tasks on the page's
//!   thread unless the page is cross-origin isolated (it isn't).
//! * Upload picks files with the browser's dialog, previews them on the
//!   server (POST /api/preview) and uploads them (net.rs); "Sync now"
//!   reloads the page.
//! * accounts: requests carry the session cookie and X-Finance-Request
//!   (net.rs); the resume key (a sign-out's code to jump back in works only
//!   with it) is kept in localStorage. A 401 on the data never navigates and never reloads: the app
//!   opens on its sign-in screen (the wizard's first step), and after
//!   signing in fetches the data in the page (`finance_core::signin` keeps
//!   it to one try per user action).
//! * the avatar: fetched after the data (`/api/profile/avatar`, 404 when
//!   it's initials) and kept in `finance_core::preload`.
//! * "Just try it" (`?demo=1`): the data is the server's made-up household
//!   (`/api/demo/*`, no sign-in) and `finance_core::demo` answers every
//!   call the pages make; leaving it is a navigation back without the query.
//!
//! On the host target the crate is empty.

#![cfg(target_family = "wasm")]
#![allow(dead_code, unused_imports)]

#[path = "../../app/src/app.rs"]
mod app;
#[path = "../../app/src/avatar.rs"]
mod avatar;
#[path = "../../app/src/budgets.rs"]
mod budgets;
#[path = "../../app/src/category.rs"]
mod category;
#[path = "../../app/src/catpicker.rs"]
mod catpicker;
#[path = "../../app/src/codeslots.rs"]
mod codeslots;
#[path = "../../app/src/danger.rs"]
mod danger;
#[path = "../../app/src/dive.rs"]
mod dive;
#[path = "../../app/src/filterbar.rs"]
mod filterbar;
#[path = "../../app/src/fmt.rs"]
mod fmt;
#[path = "../../app/src/heatmap.rs"]
mod heatmap;
#[path = "../../app/src/history.rs"]
mod history;
#[path = "../../app/src/icon.rs"]
mod icon;
#[path = "../../app/src/layout.rs"]
mod layout;
#[path = "../../app/src/linechart.rs"]
mod linechart;
#[path = "../../app/src/logo.rs"]
mod logo;
#[path = "../../app/src/map.rs"]
mod map;
#[path = "../../app/src/memo.rs"]
mod memo;
#[path = "../../app/src/pages/mod.rs"]
mod pages;
#[path = "../../app/src/palette.rs"]
mod palette;
#[path = "../../app/src/plot.rs"]
mod plot;
#[path = "../../app/src/settings_sync.rs"]
mod settings_sync;
#[path = "../../app/src/targets.rs"]
mod targets;
#[path = "../../app/src/theme.rs"]
mod theme;
#[path = "../../app/src/timing.rs"]
mod timing;
#[path = "../../app/src/ui.rs"]
mod ui;
#[path = "../../app/src/zoom.rs"]
mod zoom;

mod net;
mod web_assets;

use std::borrow::Cow;

use gpui_kit::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use app::{FinanceApp, FocusSearch, GoReview, GoUpload, Go0, Go1, Go2, Go3, Go4, Go5, Go6, Go7, Go8, Go9, OpenPalette, RefreshLive, SyncNow, ToggleMode, TogglePrivacy, ZoomIn, ZoomOut, ZoomReset};

const FONTS: [&[u8]; 5] = [
    include_bytes!("../fonts/NotoSans-Regular.ttf"),
    include_bytes!("../fonts/NotoSans-Medium.ttf"),
    include_bytes!("../fonts/NotoSans-SemiBold.ttf"),
    include_bytes!("../fonts/NotoSans-Bold.ttf"),
    include_bytes!("../fonts/NotoSansMono-Regular.ttf"),
];

async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let resp = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await.map_err(|e| format!("{url}: {e:?}"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| format!("{url}: not a Response"))?;
    // a 401 (FINANCE_AUTH=all, this browser not signed in) is remembered,
    // and the app shows its sign-in screen: no navigation, no reload
    finance_core::signin::note_status(resp.status());
    if resp.status() == 401 {
        return Err(format!("{url}: this browser isn't signed in"));
    }
    if !resp.ok() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?).await.map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Set the boot status line in index.html (until the canvas covers it).
fn status(msg: &str) {
    if let Some(el) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("boot")) {
        el.set_text_content(Some(msg));
    }
}

#[wasm_bindgen(start)]
pub fn start() {
    // before web_init: gpui's own console logger then stays out (one logger
    // per process), and ours also watches for a lost GPU device
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    gpui_kit::platform::web_init();
    cap_pixel_ratio();
    // ?keys=0 hides the key hints (hero screenshots)
    layout::set_touch_only(touch_only() || query_value("keys") == "0");
    theme::set_store(theme_load, theme_save);
    // dev/screenshot aid: ?onboarding=1 opens the wizard, ?onboarding=money on that step
    pages::onboarding::set_dev_aid(query_value("onboarding"));
    // a new user's currency and country come from the browser's language
    if let Some(l) = web_sys::window().and_then(|w| w.navigator().language()) {
        finance_core::locale::set_browser_tag(l);
    }
    // ?page=settings, ?account_menu=1 (app::dev_aid)
    app::set_query(web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default());
    let timing = query_value("timing");
    if !timing.is_empty() {
        timing::set_render(timing::Render::parse(&timing), |line| web_sys::console::log_1(&line.into()));
    }
    wasm_bindgen_futures::spawn_local(async {
        let origin = web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
        finance_core::cache::set_server_url(&origin);
        net::install();
        finance_core::signin::set_resume_hooks(resume_held, resume_ensure);
        finance_core::sync::set_reload_hook(|| {
            if let Some(w) = web_sys::window() {
                let _ = w.location().reload();
            }
        });
        finance_core::demo::set_web_hooks(|| navigate(true), || navigate(false));
        let demo = query_value("demo") == "1";
        finance_core::demo::set_active(demo);
        status(if demo { "Making up a household…" } else { "Fetching your data…" });
        let (lp, bp) = if demo { ("/api/demo/ledger.json", "/api/demo/export.json") } else { ("/api/ledger.json", "/api/export.json") };
        let (ledger, bundle) = futures_join(fetch_bytes(lp), fetch_bytes(bp)).await;
        match (ledger, bundle) {
            (Ok(l), Ok(b)) => {
                if let Err(e) = finance_core::preload::set(&l, &b) {
                    web_sys::console::error_1(&format!("finance-web: {e:#}").into());
                }
                // the avatar, best effort (404: initials; the demo has none)
                if let Ok(png) = if demo { Err(String::new()) } else { fetch_bytes("/api/profile/avatar").await } {
                    if png.starts_with(b"\x89PNG") {
                        finance_core::preload::set_avatar(Some(png));
                    }
                }
            }
            (l, b) => {
                let e = l.err().or(b.err()).unwrap_or_default();
                web_sys::console::error_1(&format!("finance-web: {e}").into());
            }
        }
        status("Starting…");
        run();
    });
}

/// Both fetches in flight at once (no futures crate needed for two).
async fn futures_join<A: std::future::Future, B: std::future::Future>(a: A, b: B) -> (A::Output, B::Output) {
    let a = std::pin::pin!(a);
    let b = std::pin::pin!(b);
    let mut a = Some(a);
    let mut b = Some(b);
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(move |cx| {
        use std::task::Poll;
        if ra.is_none() {
            if let Some(f) = a.as_mut() {
                if let Poll::Ready(v) = f.as_mut().poll(cx) {
                    ra = Some(v);
                    a = None;
                }
            }
        }
        if rb.is_none() {
            if let Some(f) = b.as_mut() {
                if let Poll::Ready(v) = f.as_mut().poll(cx) {
                    rb = Some(v);
                    b = None;
                }
            }
        }
        if ra.is_some() && rb.is_some() {
            Poll::Ready((ra.take().unwrap(), rb.take().unwrap()))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// The demo in (`?demo=1`) or out: the page again, with or without it.
fn navigate(demo: bool) {
    if let Some(w) = web_sys::window() {
        let path = w.location().pathname().unwrap_or_else(|_| "/".into());
        let _ = w.location().set_href(&if demo { format!("{path}?demo=1") } else { path });
    }
}

/// `?name=value` from the page's URL ("" when absent).
fn query_value(name: &str) -> String {
    let search = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
    search.trim_start_matches('?').split('&')
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=').map(str::to_string))
        .unwrap_or_default()
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// localStorage key: this browser's resume key for the server it was loaded
/// from (finance_core::signin; the origin is the server, so one per server).
/// Never the session cookie, which the server alone sets and ends.
const RESUME_KEY: &str = "finance-resume-key";

fn resume_held() -> Option<String> {
    storage()?.get_item(RESUME_KEY).ok().flatten().filter(|k| !k.is_empty())
}

/// [`resume_held`], or 32 random bytes (hex) kept now.
fn resume_ensure() -> Option<String> {
    if let Some(k) = resume_held() {
        return Some(k);
    }
    let mut b = [0u8; 32];
    web_sys::window()?.crypto().ok()?.get_random_values_with_u8_array(&mut b).ok()?;
    let k: String = b.iter().map(|x| format!("{x:02x}")).collect();
    storage()?.set_item(RESUME_KEY, &k).ok()?;
    Some(k)
}

/// localStorage key: this browser's theme, theme.toml's content (the web
/// has no file). Settings, Ctrl+T and the zoom keys write it.
const THEME_KEY: &str = "finance-theme";
/// localStorage key: that theme's background ("#rrggbb"), which index.html
/// paints before the wasm has loaded.
const THEME_BG_KEY: &str = "finance-theme-bg";

fn theme_load() -> Option<String> {
    storage()?.get_item(THEME_KEY).ok().flatten()
}

/// The URL set the mode or palette (`?mode=`, `?palette=`: screenshots):
/// this page load saves no theme, so the aid never sticks.
static THEME_FROM_URL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn theme_save(toml_text: &str) {
    if THEME_FROM_URL.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(s) = storage() else { return };
    let _ = s.set_item(THEME_KEY, toml_text);
    if let Ok(cfg) = toml::from_str::<theme::ThemeConfig>(toml_text) {
        let c = Rgba::from(theme::preview(&cfg.palette, cfg.dark())[0]);
        let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        let _ = s.set_item(THEME_BG_KEY, &format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b)));
    }
}

/// localStorage key: when WebGPU last lost its device (ms since the epoch).
/// For a week after, the page starts on WebGL2.
const GL_FALLBACK_KEY: &str = "finance-gl-fallback";
const GL_FALLBACK_MS: f64 = 7. * 24. * 3600. * 1000.;

/// The graphics backend. WebGPU when the browser has `navigator.gpu`
/// (gpui's Auto: WebGPU, else WebGL2 when no adapter or device comes up);
/// WebGL2 without it, or for a week after WebGPU lost its device on this
/// browser (see `Logger`). `?gl=webgpu` / `?gl=webgl` force one, `?gl=auto`
/// also forgets a fallback.
fn backend() -> gpui_kit::web::WebBackendPreference {
    use gpui_kit::web::WebBackendPreference as B;
    let store = storage();
    match query_value("gl").as_str() {
        "webgpu" => return B::WebGpu,
        "webgl" | "gl" => return B::WebGl,
        "auto" => {
            if let Some(s) = &store {
                let _ = s.remove_item(GL_FALLBACK_KEY);
            }
        }
        _ => {}
    }
    let fell_back = store.and_then(|s| s.get_item(GL_FALLBACK_KEY).ok().flatten())
        .and_then(|v| v.parse::<f64>().ok())
        .is_some_and(|at| js_sys::Date::now() - at < GL_FALLBACK_MS);
    let has_gpu = web_sys::window()
        .is_some_and(|w| js_sys::Reflect::has(&w.navigator(), &"gpu".into()).unwrap_or(false));
    let pick = if has_gpu && !fell_back { B::Auto } else { B::WebGl };
    BACKEND.with(|b| b.set(Some(pick)));
    pick
}

thread_local! {
    static BACKEND: std::cell::Cell<Option<gpui_kit::web::WebBackendPreference>> = const { std::cell::Cell::new(None) };
}

/// gpui's console logger, plus: a lost GPU device stops rendering for good
/// (gpui: "reload the page to recover"), so reload. When the device was
/// WebGPU's, remember it and come back on WebGL2. At most three reloads
/// per tab session, so a device that keeps dying can't loop the page.
struct Logger;
static LOGGER: Logger = Logger;

impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let msg = format!("[{}] {}: {}", record.level(), record.target(), record.args());
        let js = wasm_bindgen::JsValue::from_str(&msg);
        match record.level() {
            log::Level::Error => web_sys::console::error_1(&js),
            log::Level::Warn => web_sys::console::warn_1(&js),
            log::Level::Info => web_sys::console::info_1(&js),
            _ => web_sys::console::log_1(&js),
        }
        let lost = (msg.contains("device lost") && !msg.contains("Destroyed")) || msg.contains("graphics context was lost");
        if lost {
            device_lost();
        }
    }

    fn flush(&self) {}
}

fn device_lost() {
    static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(w) = web_sys::window() else { return };
    let session = w.session_storage().ok().flatten();
    let tries = session.as_ref().and_then(|s| s.get_item("finance-gl-reloads").ok().flatten())
        .and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    if tries >= 3 {
        status("The graphics device was lost. Reload the page to try again.");
        return;
    }
    if let Some(s) = &session {
        let _ = s.set_item("finance-gl-reloads", &(tries + 1).to_string());
    }
    let was_webgpu = BACKEND.with(|b| b.get()) != Some(gpui_kit::web::WebBackendPreference::WebGl);
    if was_webgpu {
        if let Some(s) = storage() {
            let _ = s.set_item(GL_FALLBACK_KEY, &js_sys::Date::now().to_string());
        }
    }
    let _ = w.location().reload();
}

/// A touch screen with no mouse: no key hints (layout::has_keyboard).
fn touch_only() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(pointer: coarse) and (hover: none)").ok().flatten())
        .is_some_and(|m| m.matches())
}

/// Phones: render at most 2 device pixels per CSS pixel. A 3x screen then
/// fills 44% fewer pixels a frame, which costs nothing you can see at arm's
/// length. gpui reads `devicePixelRatio` for its scale and sizes the canvas
/// from ResizeObserver; with the ratio capped it must take the CSS-pixel
/// box (× ratio, as on Safari) instead of the device-pixel one, or the
/// layout would come out 1.5x too wide. `?dpr=N` sets the cap (0 = none).
fn cap_pixel_ratio() {
    let Some(w) = web_sys::window() else { return };
    let coarse = w.match_media("(pointer: coarse)").ok().flatten().is_some_and(|m| m.matches());
    let cap = match query_value("dpr").parse::<f64>() {
        Ok(v) => v,
        Err(_) if coarse => 2.,
        Err(_) => return,
    };
    let real = w.device_pixel_ratio();
    if cap <= 0. || real <= cap {
        return;
    }
    let desc = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&desc, &"value".into(), &cap.into());
    let _ = js_sys::Reflect::set(&desc, &"configurable".into(), &true.into());
    let _ = js_sys::Reflect::define_property(&w, &"devicePixelRatio".into(), &desc);
    if let Ok(entry) = js_sys::Reflect::get(&w, &"ResizeObserverEntry".into()) {
        if let Ok(proto) = js_sys::Reflect::get(&entry, &"prototype".into()) {
            let _ = js_sys::Reflect::delete_property(proto.unchecked_ref::<js_sys::Object>(), &"devicePixelContentBoxSize".into());
        }
    }
    web_sys::console::info_1(&format!("finance-web: rendering at {cap}x (screen {real}x)").into());
}

/// `window.__financeReady = true` and a console line: the app has drawn
/// its first frames (desktop/shots polls for it rather than sleeping).
fn mark_ready() {
    if let Some(w) = web_sys::window() {
        let _ = js_sys::Reflect::set(&w, &"__financeReady".into(), &true.into());
    }
    web_sys::console::info_1(&"finance-web: ready".into());
}

fn application() -> Application {
    // single-threaded: the page isn't cross-origin isolated, so no
    // SharedArrayBuffer; gpui then runs background tasks on this thread
    let platform = std::rc::Rc::new(gpui_kit::web::WebPlatform::new_with_backend(false, backend()));
    let http = std::sync::Arc::new(platform.fetch_http_client());
    Application::with_platform(platform).with_http_client(http)
}

fn run() {
    application()
        .with_assets(logo::AppAssets)
        .run(|cx| {
            if let Err(e) = cx.text_system().add_fonts(FONTS.iter().map(|b| Cow::Borrowed(*b)).collect()) {
                web_sys::console::error_1(&format!("finance-web: fonts: {e:#}").into());
            }
            gpui_kit::init(cx);
            theme::register_fonts(cx);
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
                KeyBinding::new("ctrl--", ZoomOut, Some("FinanceApp")),
                KeyBinding::new("ctrl-0", ZoomReset, Some("FinanceApp")),
                KeyBinding::new("ctrl-h", TogglePrivacy, Some("FinanceApp")),
            ]);

            // this browser's saved theme (localStorage, theme::set_store)
            let mut theme = theme::ThemeConfig::load();
            // dev/screenshot aid: ?mode=light, ?palette=nord (theme.rs PALETTES),
            // over the saved theme and never saved
            for (key, slot) in [("mode", &mut theme.mode), ("palette", &mut theme.palette)] {
                let v = query_value(key);
                if !v.is_empty() {
                    *slot = v;
                    THEME_FROM_URL.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            theme::apply(&theme, cx);
            cx.set_global(budgets::Budgets::load());

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions { title: Some("Anser".into()), ..Default::default() }),
                window_background: theme::window_background(&theme),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                zoom::set(theme.zoom);
                zoom::apply(window, cx);
                let view = cx.new(|cx| FinanceApp::new(theme, window, cx));
                window.focus(&view.focus_handle(cx), cx);
                // screenshots (desktop/shots) wait for this: the first frame
                // with the data drawn, a frame after the window's first one
                window.on_next_frame(|window, _| window.on_next_frame(|_, _| mark_ready()));
                view
            })
            .expect("open window");
            status("");
        });
}
