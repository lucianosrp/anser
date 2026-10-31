//! Settings: theme (mode and accent, applied live and saved), the server the
//! replica syncs from (with a connection test), the local cache, and the keys.
//! Stateful, so it renders from a snapshot of the app's state and calls back
//! into the app through listeners.

use std::path::PathBuf;

use gpui_kit::component::{button::ButtonVariants as _, 
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::Button,
    h_flex,
    input::{Input, InputState},
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{FinanceApp, GoDoctor, SyncNow};
use crate::fmt;
use crate::theme::{self, ThemeConfig};
use crate::ui;
use crate::zoom::px;

/// Accent presets offered as swatches next to the custom hex field.
pub const SWATCHES: [&str; 6] = ["#7aa2f7", "#7dcfff", "#9ece6a", "#e0af68", "#f7768e", "#bb9af7"];

pub enum ConnTest {
    Idle,
    Running,
    Ok(String),
    Failed(String),
}

/// What the page shows, gathered by the app on each render (all cheap).
/// Settings' tabs: each says what it is for, so a card is easy to find.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Tab {
    #[default]
    General,
    Account,
    Data,
    /// how to get each source's data in (pages::supported)
    Supported,
    Home,
    Sync,
    /// MCP: assistants that read the finances (pages::assistants)
    Assistants,
    Admin,
}

impl Tab {
    pub const ALL: [Tab; 8] = [Tab::General, Tab::Account, Tab::Data, Tab::Supported, Tab::Home, Tab::Sync, Tab::Assistants, Tab::Admin];

    pub fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Account => "Account",
            Tab::Data => "Data sources",
            Tab::Supported => "Supported sources",
            Tab::Home => "Home",
            Tab::Sync => "Sync & storage",
            Tab::Assistants => "Assistants",
            Tab::Admin => "Admin",
        }
    }

    /// `FINANCE_DESKTOP_SETTINGS_TAB=sync` (dev/screenshot aid; the browser:
    /// `?settings_tab=sync`)
    pub fn from_env() -> Tab {
        let v = crate::app::dev_aid("FINANCE_DESKTOP_SETTINGS_TAB").unwrap_or_default().to_lowercase();
        Tab::ALL.into_iter().find(|t| t.label().to_lowercase().starts_with(&v) && !v.is_empty()).unwrap_or_default()
    }
}

pub struct Model<'a> {
    /// the open tab, and whether Admin is one (admins only)
    pub tab: Tab,
    pub admin: bool,
    pub theme: &'a ThemeConfig,
    pub accent_input: &'a Entity<InputState>,
    pub server_input: &'a Entity<InputState>,
    pub conn: &'a ConnTest,
    /// the URL in use and, when `$FINANCE_SERVER` overrides the file, that value
    pub server_in_use: String,
    pub server_env: Option<String>,
    pub server_file: PathBuf,
    /// the server's build version, from the bundle it last sent (None: not synced yet, or an older server)
    pub server_version: Option<String>,
    pub note: Option<&'a str>,
    pub syncing: bool,
    pub sync_error: Option<&'a str>,
    pub synced_ms: Option<i64>,
    pub cache_dir: PathBuf,
    pub cache_bytes: u64,
    /// the Downloads watcher (its card; none in the web build)
    pub downloads: &'a Entity<crate::pages::downloads::Downloads>,
    /// the app's own update (pages::updates; none in the web build)
    pub updates: Option<AnyElement>,
    /// "Encrypt this computer's copy" (pages::vault; nothing in the browser)
    pub vault: AnyElement,
    /// Settings, Backups (pages::backups)
    pub backups: AnyElement,
    /// the Home tab's form (pages::home_setup)
    pub home: AnyElement,
    /// the AI assistants tab (pages::assistants)
    pub assistants: AnyElement,
}

const KEYS: [(&str, &str); 16] = [
    ("Ctrl+K", "search everything / command palette"),
    ("Ctrl+1 … Ctrl+9", "Overview, Spending, Budgets, Travel, Transactions, Investments, Banks, Crypto, Broker"),
    ("Ctrl+U", "Upload"),
    ("Ctrl+,", "Settings"),
    ("Ctrl+F", "focus the transactions search"),
    ("Ctrl+R", "sync now"),
    ("Ctrl+Shift+K", "refresh live data: the server fetches the exchanges, broker and prices now, then syncs"),
    ("Ctrl+T", "toggle dark / light"),
    ("Ctrl+Q", "quit"),
    ("Ctrl+O / Ctrl+I", "back / forward through where you've been (also the mouse's side buttons)"),
    ("Ctrl+B", "collapse the sidebar to icons, or expand it"),
    ("← →  or  [ ]", "Spending: previous / next month"),
    ("↑ ↓", "Transactions: move the selection"),
    ("Enter", "Transactions: open the selected row"),
    ("Esc", "close the detail sheet or a dialog"),
    ("Enter", "Settings: apply the hex / save the server (in its field)"),
];

const MOUSE: &str = "Mouse: on Spending, click a category to see its rows, or a transaction for its detail. \
In the table, double-click a row. Drop bank, card or crypto exports anywhere to upload them.";

fn bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.0} KB", n as f64 / 1024.),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.),
    }
}

fn row(label: &str, value: impl IntoElement, cx: &App) -> Div {
    ui::field_row(label, value, cx)
}

fn key_chip(k: &str, cx: &App) -> Div {
    let t = cx.theme();
    div()
        .px_2()
        .py(px(1.))
        .rounded(t.radius)
        .border_1()
        .border_color(t.border)
        .bg(t.muted)
        .text_xs()
        .font_family(t.mono_font_family.clone())
        .whitespace_nowrap()
        .child(k.to_string())
}

fn column() -> Div {
    v_flex().flex_1().min_w(crate::layout::col(360.)).gap_2()
}

/// `account`: the server account card (pages::account), at the top of the
/// right column.
pub fn render(m: Model, account: AnyElement, cx: &mut Context<FinanceApp>) -> impl IntoElement {
    let t = cx.theme().clone();
    let dark = m.theme.dark();
    let accent = m.theme.accent.as_str();
    let custom = !matches!(accent, "warm" | "matugen" | "matugen-accent");

    // ---- appearance ----
    let saved_mode = m.theme.mode.as_str();
    let mode_choice = |id: &'static str, label: &'static str, value: &'static str, cx: &mut Context<FinanceApp>| {
        // anything but dark / light is "system"
        let sel = saved_mode == value || (value == "system" && !matches!(saved_mode, "dark" | "light"));
        Button::new(id)
            .small()
            .label(label)
            .outline()
            .when(sel, |b| b.icon(IconName::Check))
            .on_click(cx.listener(move |this, _, _, cx| this.update_theme(move |c| c.mode = value.into(), cx)))
    };
    let mode = h_flex().gap_2().flex_wrap().items_center()
        .child(mode_choice("mode-dark", "Dark", "dark", cx))
        .child(mode_choice("mode-light", "Light", "light", cx))
        .child(mode_choice("mode-system", "System", "system", cx))
        .when(!matches!(saved_mode, "dark" | "light"), |d| d.child(div().text_xs().text_color(t.muted_foreground)
            .child(format!("follows the system: now {}", if dark { "dark" } else { "light" }))));

    let logos_on = m.theme.logos;
    let logos = Switch::new("logos-switch").with_size(ui::switch_size())
        .checked(logos_on)
        .label(if logos_on { "On — single-colour, follow the theme" } else { "Off" })
        .on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.update_theme(move |c| c.logos = on, cx)
        }));

    let choice = |id: &'static str, label: &'static str, sel: bool, value: Option<&'static str>, cx: &mut Context<FinanceApp>| {
        Button::new(id)
            .small()
            .label(label)
            .outline()
            .when(sel, |b| b.icon(IconName::Check))
            .on_click(cx.listener(move |this, _, window, cx| match value {
                Some(v) => this.update_theme(move |c| c.accent = v.into(), cx),
                None => this.apply_custom_accent(window, cx),
            }))
    };
    let choices = h_flex()
        .gap_2()
        .flex_wrap()
        .child(choice("accent-warm", "Theme's own", accent == "warm", Some("warm"), cx))
        .child(choice("accent-matugen", "Desktop theme", accent == "matugen", Some("matugen"), cx))
        .child(choice("accent-custom", "Custom", custom, None, cx));

    // the desktop theme's (Omarchy's or matugen's) primary over the chosen palette (absent on the web: no file)
    let matugen_swatch = theme::desktop_accent(&m.theme, dark).map(|c| {
        div()
            .id("swatch-matugen")
            .size(px(22.))
            .rounded_full()
            .bg(c)
            .border_2()
            .border_color(if accent == "matugen-accent" { t.foreground } else { t.border })
            .cursor_pointer()
            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Desktop accent").build(w, cx))
            .on_click(cx.listener(|this, _, _, cx| this.update_theme(|c| c.accent = "matugen-accent".into(), cx)))
    });
    let swatches = h_flex().gap_2().children(matugen_swatch).children(SWATCHES.iter().enumerate().map(|(i, hex)| {
        let c = theme::hex(hex).unwrap_or(t.primary);
        let sel = accent.eq_ignore_ascii_case(hex);
        div()
            .id(("swatch", i))
            .size(px(22.))
            .rounded_full()
            .bg(c)
            .border_2()
            .border_color(if sel { t.foreground } else { t.border })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.set_accent_hex(hex, window, cx)))
    }));
    let hex_ok = theme::hex(&m.accent_input.read(cx).value()).is_some();
    let custom_row = h_flex()
        .gap_2()
        .flex_wrap()
        .child(div().w(px(130.)).child(Input::new(m.accent_input).small()))
        .child(Button::new("accent-apply").small().outline().label("Apply").disabled(!hex_ok)
            .on_click(cx.listener(|this, _, window, cx| this.apply_custom_accent(window, cx))))
        .child(swatches);

    // ---- palette picker: a mini card per palette in the current mode ----
    let current = m.theme.palette.clone();
    let palettes = h_flex().gap_2().flex_wrap().children(theme::PALETTES.iter().map(|&(id, label)| {
        let [bg, card, accent_c, chart] = theme::preview(id, dark);
        let sel = current == id;
        let swatch = h_flex().h(px(26.)).w(px(64.)).rounded(px(6.)).overflow_hidden().border_1()
            .border_color(t.border).bg(bg).p(px(4.)).gap(px(3.))
            .child(div().flex_1().h_full().rounded(px(3.)).bg(card))
            .child(div().w(px(8.)).h_full().rounded(px(3.)).bg(accent_c))
            .child(div().w(px(8.)).h_full().rounded(px(3.)).bg(chart));
        v_flex()
            .id(SharedString::from(format!("palette-{id}")))
            .gap(px(4.))
            .p(px(5.))
            .rounded(px(8.))
            .border_1()
            .border_color(if sel { t.primary } else { gpui_kit::transparent_black() })
            .cursor_pointer()
            .hover(|s| s.bg(t.accent))
            .on_click(cx.listener(move |this, _, _, cx| this.update_theme(move |c| c.palette = id.into(), cx)))
            .child(swatch)
            .child(div().text_xs().text_center().text_color(if sel { t.foreground } else { t.muted_foreground }).child(label))
    }));
    let glass_note = m.theme.glass().then(|| div().text_xs().text_color(t.muted_foreground)
        .child("Glass shows the desktop through the window, frosted by the compositor (Hyprland: decoration:blur)."));

    // ---- zoom ----
    let z = crate::zoom::get();
    let zoom = h_flex().gap_2().items_center()
        .child(Button::new("zoom-out").small().outline().label("−")
            .on_click(cx.listener(|this, _, window, cx| this.zoom_by(-crate::zoom::STEP, window, cx))))
        .child(div().w(px(48.)).text_center().text_sm().child(format!("{:.0}%", z * 100.)))
        .child(Button::new("zoom-in").small().outline().label("+")
            .on_click(cx.listener(|this, _, window, cx| this.zoom_by(crate::zoom::STEP, window, cx))))
        .child(Button::new("zoom-reset").small().ghost().label("Reset")
            .on_click(cx.listener(|this, _, window, cx| this.zoom_by(0., window, cx))))
        .when(ui::show_shortcuts(), |d| d.child(div().text_xs().text_color(t.muted_foreground).child("Ctrl + / − / 0, or Ctrl + scroll")));

    let source = m.theme.desktop_source();
    let source_line = h_flex()
        .gap_2()
        .child(div().size(px(8.)).rounded_full().bg(if source.found() { fmt::money_in() } else { t.muted_foreground }))
        .child(div().text_xs().text_color(t.muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap()
            .child(source.describe()));

    let appearance = ui::card(cx)
        .gap_2()
        .child(row("Mode", mode, cx))
        .child(row("Theme", v_flex().gap_2().child(palettes).children(glass_note), cx))
        .child(row("Accent", v_flex().gap_2().child(choices).child(custom_row), cx))
        .child(row("Zoom", zoom, cx))
        .child(row("Desktop theme source", source_line, cx))
        .child(row("Logos", logos, cx))
        .child(div().text_xs().text_color(t.muted_foreground)
            .child(if cfg!(target_family = "wasm") {
                "Applied live and saved in this browser. Money in and out stay green and red whatever the accent."
            } else {
                "Applied live and saved to the theme file. Money in and out stay green and red whatever the accent."
            }));

    // ---- server ----
    let conn = match m.conn {
        ConnTest::Idle => div(),
        ConnTest::Running => div().text_xs().text_color(t.muted_foreground).child("testing…"),
        ConnTest::Ok(s) => div().text_xs().text_color(fmt::money_in()).child(s.clone()),
        ConnTest::Failed(s) => div().text_xs().text_color(fmt::money_out()).child(s.clone()),
    };
    let server = ui::card(cx)
        .gap_2()
        .child(row("URL", Input::new(m.server_input).small(), cx))
        .child(h_flex().gap_2().flex_wrap()
            .child(Button::new("server-save").small().outline().label("Save")
                .on_click(cx.listener(|this, _, _, cx| this.save_server(cx))))
            .child(Button::new("server-test").small().outline().icon(IconName::Globe).label("Test connection")
                .loading(matches!(m.conn, ConnTest::Running))
                .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))))
            .child(Button::new("server-doctor").small().ghost().label("Doctor…")
                .tooltip("Every health check, with one-click fixes")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(GoDoctor), cx)))
            .child(conn))
        .child(row("In use", div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(m.server_in_use.clone()), cx))
        .child(row("App", div().text_xs().text_color(t.muted_foreground).child(finance_core::VERSION), cx))
        .when_some(m.server_version, |d, v| d.child(row("Server", div().text_xs().text_color(t.muted_foreground).child(v), cx)))
        .child(div().text_xs().text_color(t.muted_foreground)
            .child("Not affiliated with the banks, brokers or brands shown: logos only mark where your data comes from. Not financial advice. Never places trades or moves money. MIT licence; third-party notices in THIRD_PARTY_NOTICES.md."))
        .child(row("Saved in", div().text_xs().text_color(t.muted_foreground).child(m.server_file.display().to_string()), cx))
        .when_some(m.server_env, |d, env| d.child(div().text_xs().text_color(t.warning)
            .child(format!("$FINANCE_SERVER is set ({env}) and wins over the file for this session."))))
        .when_some(m.note, |d, n| d.child(div().text_xs().text_color(t.muted_foreground).child(n.to_string())));

    // ---- sync & cache ----
    let last = match (m.syncing, m.sync_error, m.synced_ms) {
        (true, _, _) => div().text_color(t.muted_foreground).child("syncing…"),
        (_, Some(e), _) => div().text_color(fmt::money_out()).child(format!("failed — {}", e.chars().take(120).collect::<String>())),
        (_, _, Some(ms)) => {
            let at = chrono::DateTime::from_timestamp_millis(ms)
                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            div().child(format!("{} · {at}", fmt::ago(ms)))
        }
        _ => div().text_color(t.muted_foreground).child("never"),
    };
    let sync = ui::card(cx)
        .gap_2()
        .child(row("Last sync", last, cx))
        .child(row("Cache", div().text_xs().text_color(t.muted_foreground).child(m.cache_dir.display().to_string()), cx))
        .child(row("Size", bytes(m.cache_bytes), cx))
        .child(h_flex().child(Button::new("settings-sync").small().outline().icon(IconName::RefreshCw).label("Sync now")
            .loading(m.syncing)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(SyncNow), cx))))
        .child(div().text_xs().text_color(t.muted_foreground)
            .child("Syncs on launch when older than 15 min, then every 30 min. The cache is a full replica; delete it to start over."));

    // ---- keys ----
    let keys = ui::card(cx).gap_0().children(KEYS.iter().map(|(k, what)| {
        h_flex()
            .gap_3()
            .py_1()
            .child(h_flex().w(px(130.)).flex_shrink_0().child(key_chip(k, cx)))
            .child(div().flex_1().min_w_0().text_sm().text_color(t.muted_foreground).child(what.to_string()))
    })).child(div().pt_2().text_xs().text_color(t.muted_foreground).child(MOUSE));


    // the tab bar, then the open tab: two columns when wide, one at
    // half-screen width
    let tab = if m.tab == Tab::Admin && !m.admin { Tab::General } else { m.tab };
    let mut bar = h_flex().w_full().flex_wrap().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(ui::hairline(cx));
    for this in Tab::ALL.into_iter().filter(|x| *x != Tab::Admin || m.admin) {
        let on = tab == this;
        bar = bar.child(div().id(SharedString::from(format!("settings-tab-{}", this.label()))).flex_shrink_0().px(px(12.)).py(px(4.)).rounded(px(6.))
            .text_sm().cursor_pointer().whitespace_nowrap()
            .when(on, |d| d.bg(t.secondary).text_color(t.foreground).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted_foreground).hover(|s| s.text_color(t.foreground)))
            .on_click(cx.listener(move |app, _, _, cx| if !on {
                app.open_settings_tab(this, cx);
            }))
            .child(this.label()));
    }
    // the Updates card under Appearance (the desktop build only)
    let updates = m.updates.map(|card| [ui::section("Updates", "this app's version", cx), div().child(card)]);
    let two = |a: Div, b: Div| h_flex().w_full().gap_4().items_start().flex_wrap().child(a).child(b);
    let body = match tab {
        // no Keys card without a keyboard (a phone)
        Tab::General if !ui::show_shortcuts() => h_flex().w_full().child(column().child(ui::section("Appearance", "theme.toml", cx)).child(appearance).children(updates.into_iter().flatten())),
        Tab::General => two(
            column().child(ui::section("Appearance", "theme.toml", cx)).child(appearance).children(updates.into_iter().flatten()),
            column().child(ui::section("Keys", "everything is reachable from the keyboard", cx)).child(keys)),
        // the account card shows its own part of each tab (pages::account)
        Tab::Account | Tab::Supported | Tab::Admin => h_flex().w_full().child(div().w_full().child(account)),
        Tab::Data => {
            let downloads = crate::pages::downloads::settings_card(m.downloads, cx);
            h_flex().w_full().gap_4().items_start().flex_wrap()
                .child(div().w_full().child(account))
                .when_some(downloads, |c, card| c.child(column().child(ui::section("Downloads", "new bank exports, suggested", cx)).child(card)))
        }
        Tab::Home => h_flex().w_full().child(column().child(m.home)),
        Tab::Assistants => h_flex().w_full().child(div().w_full().child(m.assistants)),
        Tab::Sync => two(
            column().child(ui::section("Server", "where the replica syncs from", cx)).child(server)
                .child(ui::section("Sync", "local replica", cx)).child(sync),
            column().when(!cfg!(target_family = "wasm"), |c| c.child(ui::section("This computer's copy", "encryption at rest", cx)))
                .child(m.vault).child(m.backups)),
    };
    v_flex().w_full().gap(px(ui::sp::L)).child(bar).child(body)
}

#[cfg(test)]
mod tests {
    use super::{SWATCHES, bytes, theme};

    #[test]
    fn sizes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2 KB");
        assert_eq!(bytes(3 * 1_048_576), "3.0 MB");
        assert!(SWATCHES.iter().all(|h| theme::hex(h).is_some()));
    }
}
