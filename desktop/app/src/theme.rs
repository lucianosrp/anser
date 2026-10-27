//! Theme: dark by default; a palette, then an accent source over it.
//!
//! `~/.config/finance-desktop/theme.toml` (written with defaults on first run):
//!
//!   mode = "system"          # "system" (the OS appearance, live) | "dark" | "light"
//!   palette = "warm"         # warm | nord | catppuccin | gruvbox | tokyo-night | rose-pine | glass | terminal
//!   accent = "warm"          # "warm" (the palette's own) | "matugen" | "matugen-accent" | "#7aa2f7"
//!   matugen_file = "~/.local/state/quickshell/user/generated/colors.json"
//!   omarchy_file = ""        # "" = auto: Omarchy's current theme (see below)
//!   desktop_theme = "auto"   # "auto" | "omarchy" | "matugen": who "matugen" follows
//!   zoom = 1.0               # UI scale, Ctrl +/-/0 and Ctrl+scroll
//!   privacy = false          # mask every amount (Ctrl+H)
//!
//! Every palette has a dark and a light variant. "glass" is translucent: the
//! window is transparent and the compositor (Hyprland's blur, KDE's) frosts
//! whatever is behind it; surfaces are tinted veils over that. "terminal"
//! (a quiet take on a trading terminal: near-black, amber used sparingly)
//! also changes the structure a little, see [`Look`].
//!
//! `mode = "system"` (the default for a new config; a file without `mode` keeps
//! meaning dark) follows the OS appearance live: GPUI reports it (the
//! xdg-desktop-portal color-scheme on Linux, prefers-color-scheme in the
//! browser) and calls back on a change ([`set_os_dark`], fed by [`apply`] and
//! the app's appearance observer). Without an OS answer it falls back to the
//! desktop theme's declared mode (Omarchy's), else dark: see [`decide_dark`].
//!
//! The web build has no file: it keeps the same toml in localStorage
//! ([`set_store`]).
//!
//! "matugen" maps a Material 3 palette (matugen's JSON output, hex per role)
//! onto the whole UI and is re-read live when the file changes, so a new
//! wallpaper re-themes the app. A hex accent only recolors the accent roles, and
//! "matugen-accent" is that with matugen's primary, so the palette stays yours.
//! Money in/out keep their fixed green/red (fmt.rs) whatever the accent.
//!
//! "matugen" and "matugen-accent" (the stored values; Settings calls them
//! "Desktop theme" / "Desktop accent") follow the desktop theme: Omarchy's
//! `colors.toml` when present, else matugen's file. Omarchy is looked for at
//! `~/.local/state/omarchy/current/theme/colors.toml`, then
//! `~/.config/omarchy/current/theme/colors.toml` (or `omarchy_file`);
//! `desktop_theme = "matugen"` ignores Omarchy, `"omarchy"` ignores matugen.
//! Omarchy needs only background, foreground and accent (accent falls back to
//! color4); the other roles come from its keys when present, else from mixes
//! of background and foreground. Omarchy is one scheme, in one mode: when the
//! effective mode differs (the OS is light, Omarchy's theme dark) only its accent
//! is borrowed, over the palette's own surfaces; switching the Omarchy theme
//! (which repoints `current/theme`, with a `light.mode` file for light ones)
//! brings the matching colors back. The theme re-applies live when the resolved file (a switched theme repoints the
//! `current/theme` symlink) or its mtime changes ([`ThemeConfig::theme_stamp`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Hsla, Rgba};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// "system" | "dark" | "light"
    #[serde(default = "dark_mode")]
    pub mode: String,
    pub accent: String,
    pub matugen_file: String,
    /// Omarchy's colors.toml; empty = auto (its two usual locations)
    pub omarchy_file: String,
    /// who the "matugen" accents follow: "auto" (Omarchy when present, else matugen) | "omarchy" | "matugen"
    pub desktop_theme: String,
    /// Bank / merchant / coin logos in lists (single-colour, theme-tinted)
    pub logos: bool,
    /// one of [`PALETTES`]
    pub palette: String,
    /// UI scale (1.0 = 100%)
    pub zoom: f32,
    /// privacy mode: every amount masked (Ctrl+H)
    pub privacy: bool,
    /// the sidebar as a narrow icon rail (Ctrl+B)
    pub sidebar_collapsed: bool,
}

/// A saved config without `mode` predates "system": it was dark.
fn dark_mode() -> String {
    "dark".into()
}

/// The OS appearance as last reported by GPUI: 0 unknown, 1 dark, 2 light.
static OS_DARK: AtomicU8 = AtomicU8::new(0);

/// Record the OS appearance (from `cx.window_appearance()`).
pub fn set_os_dark(dark: bool) {
    OS_DARK.store(if dark { 1 } else { 2 }, Ordering::Relaxed);
}

fn os_dark() -> Option<bool> {
    match OS_DARK.load(Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// Dark or light: an explicit mode wins; "system" follows the OS, else the
/// desktop theme's declared mode, else dark.
pub fn decide_dark(mode: &str, os: Option<bool>, desktop: Option<bool>) -> bool {
    match mode {
        "dark" => true,
        "light" => false,
        _ => os.or(desktop).unwrap_or(true),
    }
}

/// (id, label) of the built-in palettes, in picker order.
pub const PALETTES: [(&str, &str); 8] = [
    ("warm", "Warm"), ("nord", "Nord"), ("catppuccin", "Catppuccin"), ("gruvbox", "Gruvbox"),
    ("tokyo-night", "Tokyo Night"), ("rose-pine", "Rosé Pine"), ("glass", "Glass"),
    ("terminal", "Terminal"),
];

/// Where the theme lives when there is no file (the web build: the
/// browser's localStorage): read the saved toml, write it.
type Store = (fn() -> Option<String>, fn(&str));
static STORE: OnceLock<Store> = OnceLock::new();

/// Keep the theme with `load`/`save` instead of the theme file. Set it
/// before the first [`ThemeConfig::load`].
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub fn set_store(load: fn() -> Option<String>, save: fn(&str)) {
    let _ = STORE.set((load, save));
}

impl Default for ThemeConfig {
    fn default() -> Self {
        ThemeConfig {
            mode: "system".into(),
            accent: "warm".into(),
            matugen_file: "~/.local/state/quickshell/user/generated/colors.json".into(),
            omarchy_file: String::new(),
            desktop_theme: "auto".into(),
            logos: true,
            palette: "warm".into(),
            zoom: 1.0,
            privacy: false,
            sidebar_collapsed: false,
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("FINANCE_DESKTOP_THEME") {
        return PathBuf::from(p); // dev/screenshots: a theme file of its own
    }
    dirs::config_dir().unwrap_or_default().join("finance-desktop").join("theme.toml")
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

impl ThemeConfig {
    pub fn load() -> Self {
        if let Some((load, _)) = STORE.get() {
            return load().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
        }
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let cfg = ThemeConfig::default();
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, toml::to_string(&cfg).unwrap_or_default());
                cfg
            }
        }
    }

    pub fn save(&self) {
        if let Some((_, save)) = STORE.get() {
            return save(&toml::to_string(self).unwrap_or_default());
        }
        let _ = std::fs::write(config_path(), toml::to_string(self).unwrap_or_default());
    }

    /// The mode in effect: the choice, or for "system" the OS (see [`decide_dark`]).
    pub fn dark(&self) -> bool {
        let system = self.mode != "dark" && self.mode != "light";
        let os = if system { os_dark() } else { None };
        let desktop = (system && os.is_none()).then(|| self.omarchy_colors().map(|o| o.dark)).flatten();
        decide_dark(&self.mode, os, desktop)
    }

    /// Translucent palette: the window must be transparent for it.
    pub fn glass(&self) -> bool {
        self.palette == "glass"
    }

    /// The palette's structure beyond its colors.
    pub fn look(&self) -> Look {
        Look { terminal: self.palette == "terminal" }
    }

    pub fn matugen_path(&self) -> PathBuf {
        expand(&self.matugen_file)
    }

    /// The accent follows the desktop theme ("matugen" / "matugen-accent").
    fn follows_desktop(&self) -> bool {
        matches!(self.accent.as_str(), "matugen" | "matugen-accent")
    }

    /// Where the desktop theme comes from right now.
    pub fn desktop_source(&self) -> DesktopSource {
        self.desktop_source_in(&dirs::home_dir().unwrap_or_default())
    }

    fn desktop_source_in(&self, home: &Path) -> DesktopSource {
        let omarchy = || {
            let file = if self.omarchy_file.trim().is_empty() {
                omarchy_locations(home).into_iter().find(|p| p.is_file())?
            } else {
                let f = expand(&self.omarchy_file);
                f.is_file().then_some(f)?
            };
            Some(DesktopSource::Omarchy { name: omarchy_name(&file), file })
        };
        let matugen = || Some(self.matugen_path()).filter(|p| p.is_file()).map(DesktopSource::Matugen);
        let found = match self.desktop_theme.as_str() {
            "matugen" => matugen(),
            "omarchy" => omarchy(),
            _ => omarchy().or_else(matugen),
        };
        found.unwrap_or(DesktopSource::None)
    }

    /// Omarchy's colors, when the accent follows the desktop and Omarchy is the source.
    fn omarchy_colors(&self) -> Option<Arc<Omarchy>> {
        if !self.follows_desktop() {
            return None;
        }
        match self.desktop_source() {
            DesktopSource::Omarchy { file, .. } => load_omarchy(&file),
            _ => None,
        }
    }

    /// What the desktop theme currently is, to notice a regenerated palette, a
    /// switched Omarchy theme (the symlink repointed) or a mode change: the
    /// resolved file, its mtime, and whether `light.mode` exists.
    pub fn theme_stamp(&self) -> Option<ThemeStamp> {
        if !self.follows_desktop() {
            return None;
        }
        let (file, light) = match self.desktop_source() {
            DesktopSource::Omarchy { file, .. } => {
                let light = file.with_file_name("light.mode").exists();
                (file, light)
            }
            DesktopSource::Matugen(file) => (file, false),
            DesktopSource::None => return None,
        };
        let real = std::fs::canonicalize(&file).unwrap_or(file);
        Some((std::fs::metadata(&real).ok()?.modified().ok()?, real, light))
    }
}

/// See [`ThemeConfig::theme_stamp`].
pub type ThemeStamp = (SystemTime, PathBuf, bool);

/// Where the desktop theme comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopSource {
    Omarchy { file: PathBuf, name: Option<String> },
    Matugen(PathBuf),
    None,
}

impl DesktopSource {
    /// One line for Settings.
    pub fn describe(&self) -> String {
        match self {
            DesktopSource::Omarchy { file, name: Some(n) } => format!("Omarchy: {n}, {}", file.display()),
            DesktopSource::Omarchy { file, name: None } => format!("Omarchy: {}", file.display()),
            DesktopSource::Matugen(f) => format!("matugen: {}", f.display()),
            DesktopSource::None => "none found".into(),
        }
    }

    pub fn found(&self) -> bool {
        *self != DesktopSource::None
    }
}

/// Omarchy's colors.toml, newest layout first.
fn omarchy_locations(home: &Path) -> [PathBuf; 2] {
    [home.join(".local/state/omarchy/current/theme/colors.toml"),
     home.join(".config/omarchy/current/theme/colors.toml")]
}

/// The theme's name: `current/theme.name`, next to the `theme` directory.
fn omarchy_name(file: &Path) -> Option<String> {
    let n = std::fs::read_to_string(file.parent()?.parent()?.join("theme.name")).ok()?;
    Some(n.trim().to_string()).filter(|n| !n.is_empty())
}

/// An Omarchy colors.toml: its colors by key (invalid hex dropped) and mode.
#[derive(Debug, Clone, PartialEq)]
pub struct Omarchy {
    colors: HashMap<String, Hsla>,
    dark: bool,
}

/// Parse a colors.toml (the stock keys, or matugen's richer template).
/// `light_marker`: a `light.mode` file sits next to it. A `mode` key wins over
/// the marker; neither means dark. None without a valid background and
/// foreground, or without an accent (or color4).
pub fn parse_omarchy(text: &str, light_marker: bool) -> Option<Omarchy> {
    let t: toml::Table = toml::from_str(text).ok()?;
    let colors: HashMap<String, Hsla> = t.iter()
        .filter_map(|(k, v)| Some((k.clone(), hex(v.as_str()?)?)))
        .collect();
    let dark = match t.get("mode").and_then(|m| m.as_str()) {
        Some("light") => false,
        Some("dark") => true,
        _ => !light_marker,
    };
    let o = Omarchy { colors, dark };
    (o.get(&["background"]).is_some() && o.get(&["foreground"]).is_some() && o.get(&["accent", "color4"]).is_some())
        .then_some(o)
}

impl Omarchy {
    /// The first of `keys` present.
    fn get(&self, keys: &[&str]) -> Option<Hsla> {
        keys.iter().find_map(|k| self.colors.get(*k).copied())
    }
}

type OmarchyCache = Mutex<Option<((PathBuf, SystemTime, u64, bool), Arc<Omarchy>)>>;
static OMARCHY: OnceLock<OmarchyCache> = OnceLock::new();

/// Read and parse the file, cached by path, mtime and size (the mode is asked
/// on every render).
fn load_omarchy(file: &Path) -> Option<Arc<Omarchy>> {
    let meta = std::fs::metadata(file).ok()?;
    // the resolved path: a repointed `current/theme` symlink is another file
    let real = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let light = file.with_file_name("light.mode").exists();
    let key = (real, meta.modified().ok()?, meta.len(), light);
    let cache = OMARCHY.get_or_init(Default::default);
    if let Some((k, o)) = cache.lock().ok()?.as_ref() {
        if *k == key {
            return Some(o.clone());
        }
    }
    let o = Arc::new(parse_omarchy(&std::fs::read_to_string(file).ok()?, light)?);
    *cache.lock().ok()? = Some((key, o.clone()));
    Some(o)
}

fn rgba(c: Hsla) -> Rgba {
    c.into()
}

/// `a` moved `t` of the way toward `b`, in sRGB (alpha from `a`).
fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (x, y) = (rgba(a), rgba(b));
    Rgba { r: x.r + (y.r - x.r) * t, g: x.g + (y.g - x.g) * t, b: x.b + (y.b - x.b) * t, a: x.a }.into()
}

fn luma(c: Hsla) -> f32 {
    let c = rgba(c);
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

/// Omarchy -> UI roles. Surfaces are mixes of background and foreground (the
/// two formats name their "lighter/darker" backgrounds differently); the
/// accent, selection, muted ink and chart hues come from the keys when they
/// read on the background.
fn omarchy_roles(o: &Omarchy, dark: bool) -> Option<Palette> {
    let bg = o.get(&["background"])?;
    let fg = o.get(&["foreground"])?;
    let accent = o.get(&["accent", "color4"])?;
    let (black, white) = (hex("#000000")?, hex("#ffffff")?);
    let toward = |t: f32| mix(bg, fg, t);
    let (card, muted, hover, border, popover, sidebar) = if dark {
        (toward(0.04), toward(0.08), toward(0.12), toward(0.16), toward(0.09), mix(bg, black, 0.25))
    } else {
        (mix(bg, white, 0.55), toward(0.07), toward(0.10), toward(0.15), mix(bg, white, 0.7), toward(0.04))
    };
    let reads = |c: &Hsla| (luma(*c) - luma(bg)).abs() >= 0.2;
    let ink = |keys: &[&str]| keys.iter().filter_map(|k| o.colors.get(*k)).copied().find(reads);
    let mut m = HashMap::from([
        ("background", bg), ("foreground", fg), ("card", card), ("muted", muted), ("border", border),
        ("accent", hover), ("accent_foreground", fg), ("primary", accent),
        ("primary_foreground", if (luma(bg) - luma(accent)).abs() > (luma(fg) - luma(accent)).abs() { bg } else { fg }),
        ("sidebar", sidebar), ("sidebar_accent", mix(bg, accent, 0.2)), ("popover", popover),
        ("muted_foreground", ink(&["muted", "color8"]).unwrap_or_else(|| mix(fg, bg, 0.4))),
        ("selection", o.get(&["selection_background", "selection"]).unwrap_or_else(|| accent.opacity(0.35))),
    ]);
    for (role, keys) in [
        ("chart_2", ["cyan", "color6", "blue", "color4"]), ("chart_3", ["magenta", "color5", "red", "color1"]),
        ("chart_4", ["yellow", "color3", "green", "color2"]), ("chart_5", ["orange", "green", "color2", "color11"]),
    ] {
        if let Some(c) = ink(&keys) {
            m.insert(role, c);
        }
    }
    Some(Palette(m))
}

/// How the palette draws structure, beyond its colors: a global that
/// `ui.rs` reads. The default is soft cards and plain headings.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Look {
    /// terminal: monospace text ([`TERMINAL_FONT`]), square cards on 1px
    /// hairlines, section headings drawn with box rules, a prompt before
    /// the page title, chips and deltas in brackets
    pub terminal: bool,
}

impl gpui_kit::Global for Look {}

/// The terminal palette's font, bundled (JetBrains Mono NL, OFL, subset:
/// `web/fonts/`) for the desktop and the web alike.
pub const TERMINAL_FONT: &str = "JetBrains Mono NL";

/// Its regular, medium and bold faces; [`register_fonts`] adds them.
const TERMINAL_FONTS: [&[u8]; 3] = [
    include_bytes!("../../web/fonts/JetBrainsMonoNL-Regular.ttf"),
    include_bytes!("../../web/fonts/JetBrainsMonoNL-Medium.ttf"),
    include_bytes!("../../web/fonts/JetBrainsMonoNL-Bold.ttf"),
];

/// The braille loading animations' font (DejaVu Sans subset to U+2800-28FF,
/// equal advances): no other bundled font has braille, and the web has no
/// fallback.
pub const BRAILLE_FONT: &str = "DejaVu Sans";
const BRAILLE_BYTES: &[u8] = include_bytes!("../../web/fonts/DejaVuSans-Braille.ttf");

/// The welcome card's italic accent line (Noto Serif Italic, OFL, subset to
/// Latin-1 and common punctuation).
pub const ACCENT_FONT: &str = "Noto Serif";
const ACCENT_BYTES: &[u8] = include_bytes!("../../web/fonts/NotoSerif-Italic-subset.ttf");

/// Add the bundled fonts (terminal, braille, accent) to gpui's text system (once, at start).
pub fn register_fonts(cx: &mut App) {
    let fonts = TERMINAL_FONTS.iter().copied().chain([BRAILLE_BYTES, ACCENT_BYTES]).map(std::borrow::Cow::Borrowed).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("finance: terminal font: {e:#}");
    }
}

/// The theme's fonts before any palette touched them (see `apply`).
static DEFAULT_FONTS: OnceLock<(String, String)> = OnceLock::new();

/// "#rrggbb" or "#rrggbbaa"
pub fn hex(s: &str) -> Option<Hsla> {
    let s = s.trim().trim_start_matches('#');
    let (rgb, a) = match s.len() {
        6 => (s, 255),
        8 => (&s[..6], u32::from_str_radix(&s[6..], 16).ok()?),
        _ => return None,
    };
    let v = u32::from_str_radix(rgb, 16).ok()?;
    let rgba = Rgba {
        r: ((v >> 16) & 0xff) as f32 / 255.,
        g: ((v >> 8) & 0xff) as f32 / 255.,
        b: (v & 0xff) as f32 / 255.,
        a: a as f32 / 255.,
    };
    Some(rgba.into())
}

/// Role -> color, applied on top of gpui-kit's default theme for the mode.
struct Palette(HashMap<&'static str, Hsla>);

impl Palette {
    fn from_hex(pairs: &[(&'static str, &str)]) -> Self {
        Palette(pairs.iter().filter_map(|(k, v)| hex(v).map(|c| (*k, c))).collect())
    }
}

/// Ely-inspired: warm, quiet surfaces; color only where it means something.
fn warm(dark: bool) -> Palette {
    if dark {
        Palette::from_hex(&[
            ("background", "#14120f"), ("foreground", "#ece6da"), ("card", "#1c1915"),
            ("muted", "#24201a"), ("muted_foreground", "#a39b8c"), ("border", "#2e2921"),
            ("accent", "#2b261e"), ("accent_foreground", "#ece6da"), ("primary", "#d9a35f"),
            ("primary_foreground", "#1a1308"), ("sidebar", "#100e0b"), ("sidebar_accent", "#26211a"),
            ("popover", "#1f1b16"), ("selection", "#5a4426"), ("chart_2", "#8fb3a6"),
            ("chart_3", "#c98b7a"), ("chart_4", "#a59bd0"), ("chart_5", "#d6c27a"),
        ])
    } else {
        Palette::from_hex(&[
            ("background", "#f7f3ec"), ("foreground", "#2a2620"), ("card", "#fffdf9"),
            ("muted", "#ebe5d8"), ("muted_foreground", "#7b7262"), ("border", "#e0d8c8"),
            ("accent", "#ebe4d6"), ("accent_foreground", "#2a2620"), ("primary", "#b7791f"),
            ("primary_foreground", "#fffaf1"), ("sidebar", "#f3eee4"), ("sidebar_accent", "#e8e0cf"),
            ("popover", "#fffdf9"), ("selection", "#f0d9b5"), ("chart_2", "#4f8a78"),
            ("chart_3", "#b4644f"), ("chart_4", "#6f63a8"), ("chart_5", "#a8902d"),
        ])
    }
}

/// The roles every palette fills, in this order (see `pal!`).
const ROLES: [&str; 18] = [
    "background", "foreground", "card", "muted", "muted_foreground", "border", "accent",
    "accent_foreground", "primary", "primary_foreground", "sidebar", "sidebar_accent",
    "popover", "selection", "chart_2", "chart_3", "chart_4", "chart_5",
];

fn roles(v: [&str; 18]) -> Palette {
    let pairs: Vec<(&'static str, &str)> = ROLES.iter().copied().zip(v).collect();
    Palette::from_hex(&pairs)
}

/// The built-in palettes beyond warm, as role values in [`ROLES`] order.
fn named(id: &str, dark: bool) -> Option<Palette> {
    let v = match (id, dark) {
        ("nord", true) => ["#2e3440", "#eceff4", "#3b4252", "#434c5e", "#a3acbb", "#4c566a", "#434c5e",
            "#eceff4", "#88c0d0", "#2e3440", "#292e39", "#3b4252", "#3b4252", "#4c566a",
            "#a3be8c", "#b48ead", "#ebcb8b", "#d08770"],
        ("nord", false) => ["#eceff4", "#2e3440", "#f8f9fb", "#e5e9f0", "#4c566a", "#d8dee9", "#e5e9f0",
            "#2e3440", "#5e81ac", "#eceff4", "#e5e9f0", "#d8dee9", "#f8f9fb", "#c9d6e8",
            "#6f8f57", "#9a6d93", "#b8962e", "#b9694a"],
        ("catppuccin", true) => ["#1e1e2e", "#cdd6f4", "#252536", "#313244", "#a6adc8", "#3b3d52", "#313244",
            "#cdd6f4", "#cba6f7", "#1e1e2e", "#181825", "#313244", "#262637", "#45475a",
            "#89dceb", "#fab387", "#f5c2e7", "#f9e2af"],
        ("catppuccin", false) => ["#eff1f5", "#4c4f69", "#f7f8fa", "#e6e9ef", "#6c6f85", "#ccd0da", "#e6e9ef",
            "#4c4f69", "#8839ef", "#eff1f5", "#e6e9ef", "#dce0e8", "#f7f8fa", "#d4c4f5",
            "#04a5e5", "#fe640b", "#ea76cb", "#df8e1d"],
        ("gruvbox", true) => ["#282828", "#ebdbb2", "#32302f", "#3c3836", "#a89984", "#504945", "#3c3836",
            "#ebdbb2", "#fabd2f", "#282828", "#1d2021", "#3c3836", "#32302f", "#665c54",
            "#83a598", "#d3869b", "#8ec07c", "#fe8019"],
        ("gruvbox", false) => ["#fbf1c7", "#3c3836", "#f9f5d7", "#ebdbb2", "#7c6f64", "#d5c4a1", "#f2e5bc",
            "#3c3836", "#b57614", "#fbf1c7", "#f2e5bc", "#ebdbb2", "#f9f5d7", "#d5c4a1",
            "#076678", "#8f3f71", "#427b58", "#af3a03"],
        ("tokyo-night", true) => ["#1a1b26", "#c0caf5", "#1f2335", "#24283b", "#737aa2", "#292e42", "#292e42",
            "#c0caf5", "#7aa2f7", "#1a1b26", "#16161e", "#292e42", "#1f2335", "#33467c",
            "#bb9af7", "#7dcfff", "#e0af68", "#ff9e64"],
        ("tokyo-night", false) => ["#e1e2e7", "#3760bf", "#e9e9ed", "#d5d6db", "#6172b0", "#c4c8da", "#d5d6db",
            "#3760bf", "#2e7de9", "#e1e2e7", "#d0d5e3", "#c4c8da", "#e9e9ed", "#b7c1e3",
            "#9854f1", "#007197", "#8c6c3e", "#b15c00"],
        ("rose-pine", true) => ["#191724", "#e0def4", "#1f1d2e", "#26233a", "#908caa", "#2e2b44", "#26233a",
            "#e0def4", "#ebbcba", "#191724", "#16141f", "#26233a", "#1f1d2e", "#403d52",
            "#9ccfd8", "#c4a7e7", "#f6c177", "#31748f"],
        ("rose-pine", false) => ["#faf4ed", "#575279", "#fffaf3", "#f2e9e1", "#797593", "#dfdad9", "#f2e9e1",
            "#575279", "#d7827e", "#fffaf3", "#f4ede8", "#f2e9e1", "#fffaf3", "#dfdad9",
            "#56949f", "#907aa9", "#ea9d34", "#286983"],
        // veils over a blurred desktop: translucent surfaces thick enough to
        // read as frosted even under a light compositor blur, opaque-ish
        // popovers (menus and tooltips must stay legible over anything).
        // How blurry the desktop behind is, is the compositor's call
        // (Hyprland: decoration:blur:size / passes)
        ("glass", true) => ["#11141cbc", "#f2f4f8", "#ffffff14", "#ffffff18", "#b8bfcc", "#ffffff26", "#ffffff1e",
            "#f2f4f8", "#8ab4ff", "#0b1020", "#0a0c1294", "#ffffff24", "#1c202aee", "#8ab4ff59",
            "#7dd3c0", "#f0a6ca", "#c3b1ff", "#f5d48a"],
        ("glass", false) => ["#f4f6fabd", "#1a1d24", "#ffffff8c", "#ffffff66", "#4b5263", "#00000017", "#ffffff73",
            "#1a1d24", "#2f6fed", "#ffffff", "#ffffff6b", "#ffffff80", "#f8f9fcf2", "#2f6fed40",
            "#1f8a78", "#b4507e", "#6a52c4", "#a77b12"],
        // a refined trading terminal: near-black surfaces a step apart,
        // off-white ink, muted grey secondary text, amber only where it
        // points (titles, the active page, focus, key figures). Light is
        // warm paper and ink with the same amber, darkened to read on it
        ("terminal", true) => ["#0b0b0c", "#e9e6df", "#121214", "#1a1a1d", "#8e8a83", "#26262a", "#1c1c1f",
            "#e9e6df", "#f0a43a", "#0b0b0c", "#080809", "#1f1b15", "#151517", "#6a4a1c",
            "#6fa3c8", "#c9837a", "#9d94c9", "#7fb3ad"],
        ("terminal", false) => ["#f6f1e7", "#1c1a16", "#fbf8f1", "#ece5d6", "#6b655b", "#d8cfbe", "#eee7d9",
            "#1c1a16", "#a65e00", "#fffaf0", "#f0e9dc", "#e7dcc6", "#fbf8f1", "#efd2a2",
            "#3f6f93", "#a5584c", "#6a5fa3", "#3d7c75"],
        _ => return None,
    };
    Some(roles(v))
}

/// Material 3 roles (matugen) -> UI roles.
fn matugen(cfg: &ThemeConfig, dark: bool) -> Option<Palette> {
    let raw = std::fs::read_to_string(cfg.matugen_path()).ok()?;
    let m: HashMap<String, String> = serde_json::from_str(&raw).ok()?;
    let g = |k: &str| m.get(k).map(String::as_str).unwrap_or("");
    if !dark {
        // the generated file is one (dark) scheme; in light mode borrow only the accents
        return Some(Palette::from_hex(&[("primary", g("primary_container")),
                                         ("chart_2", g("tertiary_container"))]));
    }
    Some(Palette::from_hex(&[
        ("background", g("surface")), ("foreground", g("on_surface")),
        ("card", g("surface_container")), ("muted", g("surface_container_high")),
        ("muted_foreground", g("on_surface_variant")), ("border", g("outline_variant")),
        ("accent", g("surface_container_highest")), ("accent_foreground", g("on_surface")),
        ("primary", g("primary")), ("primary_foreground", g("on_primary")),
        ("sidebar", g("surface_container_low")), ("sidebar_accent", g("secondary_container")),
        ("popover", g("surface_container_high")), ("selection", g("primary_container")),
        ("chart_2", g("tertiary")), ("chart_3", g("secondary")), ("chart_4", g("primary_fixed_dim")),
        ("chart_5", g("tertiary_fixed_dim")),
    ]))
}

/// The desktop theme's roles: Omarchy's, else matugen's, per the config.
fn desktop(cfg: &ThemeConfig, dark: bool) -> Option<Palette> {
    match cfg.omarchy_colors() {
        Some(o) => {
            if o.dark != dark {
                // one scheme, other mode: borrow the accent only
                let a = o.get(&["accent", "color4"])?;
                let on = hex(if luma(a) > 0.5 { "#101010" } else { "#ffffff" })?;
                return Some(Palette(HashMap::from([("primary", a), ("primary_foreground", on), ("selection", a.opacity(0.35))])));
            }
            let mut p = omarchy_roles(&o, dark)?;
            if cfg.glass() {
                // keep glass translucent: Omarchy's colors under glass's own alpha
                let base = named("glass", dark)?;
                for role in ["background", "card", "muted", "border", "accent", "sidebar", "sidebar_accent", "popover"] {
                    if let (Some(c), Some(b)) = (p.0.get_mut(role), base.0.get(role)) {
                        *c = c.opacity(b.a);
                    }
                }
                if let Some(a) = p.0.get("primary").copied() {
                    p.0.insert("selection", a.opacity(0.35));
                }
            }
            Some(p)
        }
        None if matches!(cfg.desktop_source(), DesktopSource::Matugen(_)) => matugen(cfg, dark),
        None => None,
    }
}

/// The desktop theme's primary (the palette's accent role), when there is one.
pub fn desktop_accent(cfg: &ThemeConfig, dark: bool) -> Option<Hsla> {
    desktop(cfg, dark)?.0.get("primary").copied()
}

/// The configured palette: the warm base for the mode, with the accent
/// source (desktop theme / hex) painted over it.
fn palette(cfg: &ThemeConfig, dark: bool) -> Palette {
    let mut pal = warm(dark);
    if let Some(p) = named(&cfg.palette, dark) {
        pal.0.extend(p.0);
    }
    match cfg.accent.as_str() {
        "warm" => {}
        "matugen" => {
            if let Some(m) = desktop(cfg, dark) {
                pal.0.extend(m.0);
            }
        }
        "matugen-accent" => {
            if let Some(c) = desktop_accent(cfg, dark) {
                pal.0.insert("primary", c);
                pal.0.insert("selection", c.opacity(0.35));
            }
        }
        other => {
            if let Some(c) = hex(other) {
                pal.0.insert("primary", c);
                pal.0.insert("selection", c.opacity(0.35));
            }
        }
    }
    pal
}

/// A palette's look at a glance, for the picker: (background, surface, accent, chart).
pub fn preview(id: &str, dark: bool) -> [Hsla; 4] {
    let mut pal = warm(dark);
    if let Some(p) = named(id, dark) {
        pal.0.extend(p.0);
    }
    let g = |k: &str| pal.0.get(k).copied().unwrap_or_default();
    [g("background"), g("card"), g("primary"), g("chart_2")]
}

/// The window background the palette needs.
pub fn window_background(cfg: &ThemeConfig) -> gpui_kit::WindowBackgroundAppearance {
    if cfg.glass() { gpui_kit::WindowBackgroundAppearance::Transparent } else { gpui_kit::WindowBackgroundAppearance::Opaque }
}

/// The accent the UI paints with -- also what the app icon is drawn from.
pub fn accent(cfg: &ThemeConfig) -> Hsla {
    palette(cfg, cfg.dark()).0.get("primary").copied().unwrap_or_else(|| hex("#d9a35f").unwrap_or_default())
}

/// Switch mode and paint the configured palette over gpui-kit's defaults.
pub fn apply(cfg: &ThemeConfig, cx: &mut App) {
    set_os_dark(matches!(cx.window_appearance(), gpui_kit::WindowAppearance::Dark | gpui_kit::WindowAppearance::VibrantDark));
    let dark = cfg.dark();
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let pal = palette(cfg, dark);
    let p = &pal.0;
    let t = Theme::global_mut(cx);
    let set = |key: &str, slot: &mut Hsla| {
        if let Some(c) = p.get(key) {
            *slot = *c;
        }
    };
    set("background", &mut t.background);
    set("foreground", &mut t.foreground);
    set("muted", &mut t.muted);
    set("muted_foreground", &mut t.muted_foreground);
    set("border", &mut t.border);
    set("border", &mut t.input);
    set("accent", &mut t.accent);
    set("accent_foreground", &mut t.accent_foreground);
    set("primary", &mut t.primary);
    set("primary", &mut t.ring);
    set("primary", &mut t.chart_1);
    set("primary", &mut t.sidebar_primary);
    set("primary_foreground", &mut t.primary_foreground);
    set("primary_foreground", &mut t.sidebar_primary_foreground);
    set("card", &mut t.secondary);
    set("foreground", &mut t.secondary_foreground);
    set("sidebar", &mut t.sidebar);
    set("foreground", &mut t.sidebar_foreground);
    set("border", &mut t.sidebar_border);
    set("sidebar_accent", &mut t.sidebar_accent);
    set("foreground", &mut t.sidebar_accent_foreground);
    set("popover", &mut t.popover);
    set("foreground", &mut t.popover_foreground);
    set("selection", &mut t.selection);
    set("background", &mut t.table);
    set("background", &mut t.colors.list); // Theme has its own `list` settings field
    set("sidebar", &mut t.table_head);
    set("muted_foreground", &mut t.table_head_foreground);
    set("accent", &mut t.table_hover);
    set("accent", &mut t.list_hover);
    set("sidebar_accent", &mut t.table_active);
    set("sidebar_accent", &mut t.list_active);
    set("border", &mut t.table_row_border);
    set("background", &mut t.title_bar);
    set("chart_2", &mut t.chart_2);
    set("chart_3", &mut t.chart_3);
    set("chart_4", &mut t.chart_4);
    set("chart_5", &mut t.chart_5);
    // gpui-kit's Root paints the window from tokens.background, not `background`:
    // keep them in step, or glass stays opaque behind every translucent surface
    t.tokens.background = t.background.into();
    // the zoom drives the base font size: Root resets the window's rem to it every frame
    t.font_size = gpui_kit::px(16. * crate::zoom::get());
    // Theme::change keeps the last fonts: start from the first ones each time
    let (sans, mono) = DEFAULT_FONTS.get_or_init(|| (t.font_family.to_string(), t.mono_font_family.to_string()));
    t.font_family = sans.clone().into();
    t.mono_font_family = mono.clone().into();
    // the browser has no system fonts: the web build bundles Noto Sans (what
    // fontconfig resolves "sans-serif" to on the owner's desktop)
    #[cfg(target_family = "wasm")]
    {
        t.font_family = "Noto Sans".into();
        t.mono_font_family = "Noto Sans Mono".into();
    }
    let look = cfg.look();
    // terminal: monospace everywhere, text and figures
    if look.terminal {
        t.font_family = TERMINAL_FONT.into();
        t.mono_font_family = TERMINAL_FONT.into();
    }
    t.skeleton = t.muted_foreground.opacity(if dark { 0.13 } else { 0.16 });
    set("primary", &mut t.button_primary);
    set("primary", &mut t.button_primary_hover);
    set("primary", &mut t.button_primary_active);
    set("primary_foreground", &mut t.button_primary_foreground);
    t.button_primary_hover = t.button_primary_hover.opacity(0.9);
    // charts: quiet grid, axis labels in the muted ink
    t.chart_grid = t.border.opacity(0.55);
    // terminal: square corners, no soft shadows (Theme::change keeps the
    // last values, so every palette sets them)
    t.radius = gpui_kit::px(if look.terminal { 0. } else { 6. });
    t.radius_lg = gpui_kit::px(if look.terminal { 0. } else { 8. });
    t.shadow = !look.terminal;
    // money semantics win over any palette; terminal's greens and reds are
    // its own (a phosphor green), still in for green and out for red
    crate::fmt::set_money_tone(match (look.terminal, dark) {
        (false, _) => 0,
        (true, true) => 1,
        (true, false) => 2,
    });
    t.success = crate::fmt::money_in();
    t.danger = crate::fmt::money_out();
    cx.set_global(look);
    cx.set_global(crate::logo::LogoPref(cfg.logos));
    crate::fmt::set_private(cfg.privacy);
    // glass needs a see-through window for the compositor to blur behind it
    let bg = window_background(cfg);
    for w in cx.windows() {
        let _ = w.update(cx, |_, window, _| window.set_background_appearance(bg));
    }
    cx.refresh_windows();
    // the installed launcher icon follows the accent (no-op when not installed)
    crate::icon::sync_installed(accent(cfg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matugen_accent_only_recolors_the_accent() {
        let file = std::env::temp_dir().join(format!("matugen-accent-{}.json", std::process::id()));
        std::fs::write(&file, r##"{"primary": "#112233", "surface": "#ff00ff"}"##).unwrap();
        let mut cfg = ThemeConfig { palette: "nord".into(), matugen_file: file.display().to_string(),
            omarchy_file: "/nonexistent/colors.toml".into(), ..Default::default() };
        let own = palette(&cfg, true);
        cfg.accent = "matugen-accent".into();
        let pal = palette(&cfg, true);
        assert_eq!(pal.0.get("primary"), hex("#112233").as_ref());
        assert_eq!(pal.0.get("background"), own.0.get("background"));
        assert!(cfg.theme_stamp().is_some());
        cfg.matugen_file = "/nonexistent/colors.json".into();
        assert_eq!(palette(&cfg, true).0.get("primary"), own.0.get("primary"));
        std::fs::remove_file(file).ok();
    }

    const STOCK: &str = r##"
accent = "#7aa2f7"
cursor = "#c0caf5"
foreground = "#a9b1d6"
background = "#1a1b26"
selection_foreground = "#c0caf5"
selection_background = "#7aa2f7"
color0 = "#32344a"
color1 = "#f7768e"
color2 = "#9ece6a"
color3 = "#e0af68"
color4 = "#7aa2f7"
color5 = "#ad8ee6"
color6 = "#449dab"
color8 = "#444b6a"
"##;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("omarchy-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn omarchy_parses_the_stock_format() {
        let o = parse_omarchy(STOCK, false).unwrap();
        assert!(o.dark);
        let p = omarchy_roles(&o, true).unwrap();
        assert_eq!(p.0.get("background"), hex("#1a1b26").as_ref());
        assert_eq!(p.0.get("primary"), hex("#7aa2f7").as_ref());
        assert_eq!(p.0.get("selection"), hex("#7aa2f7").as_ref());
        // color8 is too close to the background to read: the muted ink is a mix
        assert_ne!(p.0.get("muted_foreground"), hex("#444b6a").as_ref());
        // the roles are all there, with chart hues from the ANSI colors
        assert_eq!(p.0.get("chart_2"), hex("#449dab").as_ref());
        assert_eq!(p.0.len(), ROLES.len());
    }

    #[test]
    fn omarchy_parses_the_matugen_format_and_its_mode_key() {
        let t = r##"mode = "light"
accent = "#336699"
muted = "#555566"
background = "#fafafa"
foreground = "#101010"
selection = "#bbccdd"
orange = "#fafafa"
"##;
        let o = parse_omarchy(t, false).unwrap();
        assert!(!o.dark);
        let p = omarchy_roles(&o, false).unwrap();
        assert_eq!(p.0.get("muted_foreground"), hex("#555566").as_ref());
        assert_eq!(p.0.get("selection"), hex("#bbccdd").as_ref());
        // light cards are lighter than the page; an orange that doesn't read is skipped
        assert!(luma(p.0["card"]) >= luma(p.0["background"]));
        assert!(!p.0.contains_key("chart_5"));
    }

    #[test]
    fn omarchy_minimal_invalid_and_light_marker() {
        // only the three required keys; accent from color4 when absent
        let o = parse_omarchy("background = \"#ffffff\"\nforeground = \"#000000\"\ncolor4 = \"#0000ff\"", true).unwrap();
        assert!(!o.dark, "light.mode marks a light theme");
        let p = omarchy_roles(&o, false).unwrap();
        assert_eq!(p.0.get("primary"), hex("#0000ff").as_ref());
        assert_eq!(p.0.get("primary_foreground"), hex("#ffffff").as_ref());
        // an invalid hex counts as missing; a missing required key is no theme
        assert!(parse_omarchy("background = \"nope\"\nforeground = \"#000000\"\naccent = \"#ff0000\"", false).is_none());
        assert!(parse_omarchy("background = \"#000000\"\nforeground = \"#ffffff\"", false).is_none());
        assert!(parse_omarchy("not toml [", false).is_none());
        // the key wins over the marker
        let o = parse_omarchy("mode = \"dark\"\nbackground = \"#000000\"\nforeground = \"#ffffff\"\naccent = \"#ff0000\"", true).unwrap();
        assert!(o.dark);
    }

    #[test]
    fn light_mode_file_decides_a_stock_theme() {
        let d = scratch("light");
        let file = d.join("colors.toml");
        std::fs::write(&file, STOCK).unwrap();
        assert!(load_omarchy(&file).unwrap().dark);
        std::fs::write(d.join("light.mode"), "").unwrap();
        assert!(!load_omarchy(&file).unwrap().dark);
        // "system" with no OS answer falls back to Omarchy's declared mode
        let cfg = ThemeConfig { accent: "matugen".into(), omarchy_file: file.display().to_string(), ..Default::default() };
        assert!(!cfg.dark());
        let own = ThemeConfig { omarchy_file: file.display().to_string(), ..Default::default() };
        assert!(own.dark(), "not following the desktop: dark");
        let forced = ThemeConfig { mode: "dark".into(), ..cfg };
        assert!(forced.dark(), "an explicit mode wins");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn omarchy_has_priority_over_matugen() {
        let d = scratch("prio");
        let (omarchy, matugen_f) = (d.join("colors.toml"), d.join("colors.json"));
        std::fs::write(&omarchy, STOCK).unwrap();
        std::fs::write(&matugen_f, r##"{"primary": "#112233"}"##).unwrap();
        let mut cfg = ThemeConfig {
            accent: "matugen-accent".into(),
            omarchy_file: omarchy.display().to_string(),
            matugen_file: matugen_f.display().to_string(),
            ..Default::default()
        };
        // auto, both present -> Omarchy
        assert!(matches!(cfg.desktop_source(), DesktopSource::Omarchy { .. }));
        assert_eq!(desktop_accent(&cfg, true), hex("#7aa2f7"));
        // desktop_theme = matugen -> matugen even with Omarchy present
        cfg.desktop_theme = "matugen".into();
        assert_eq!(cfg.desktop_source(), DesktopSource::Matugen(matugen_f.clone()));
        assert_eq!(desktop_accent(&cfg, true), hex("#112233"));
        assert!(cfg.dark(), "matugen doesn't set the mode");
        // auto, only matugen -> matugen
        cfg.desktop_theme = "auto".into();
        cfg.omarchy_file = d.join("missing.toml").display().to_string();
        assert_eq!(cfg.desktop_source(), DesktopSource::Matugen(matugen_f.clone()));
        assert_eq!(desktop_accent(&cfg, true), hex("#112233"));
        // desktop_theme = omarchy, no Omarchy -> nothing
        cfg.desktop_theme = "omarchy".into();
        assert_eq!(cfg.desktop_source(), DesktopSource::None);
        assert_eq!(cfg.theme_stamp(), None);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn default_locations_name_and_stamp_follow_the_symlink() {
        let home = scratch("home");
        let cur = home.join(".local/state/omarchy/current");
        let (a, b) = (home.join("themes/a"), home.join("themes/b"));
        for (d, c) in [(&a, "#ff0000"), (&b, "#00ff00")] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("colors.toml"), STOCK.replace("#7aa2f7\"\ncursor", &format!("{c}\"\ncursor"))).unwrap();
        }
        std::fs::create_dir_all(&cur).unwrap();
        std::fs::write(cur.join("theme.name"), "Quattro\n").unwrap();
        #[cfg(unix)]
        {
            let link = cur.join("theme");
            std::os::unix::fs::symlink(&a, &link).unwrap();
            let cfg = ThemeConfig { accent: "matugen".into(), ..Default::default() };
            let src = cfg.desktop_source_in(&home);
            assert_eq!(src.describe(), format!("Omarchy: Quattro, {}", link.join("colors.toml").display()));
            let file = link.join("colors.toml");
            let first = (std::fs::canonicalize(&file).unwrap(), load_omarchy(&file).unwrap());
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(&b, &link).unwrap();
            assert_ne!(first.0, std::fs::canonicalize(&file).unwrap(), "a switched theme resolves elsewhere (new stamp)");
            assert_ne!(load_omarchy(&file).unwrap().colors["accent"], first.1.colors["accent"]);
        }
        assert_eq!(DesktopSource::None.describe(), "none found");
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn mode_decision() {
        // explicit choices ignore everything
        assert!(decide_dark("dark", Some(false), Some(false)));
        assert!(!decide_dark("light", Some(true), Some(true)));
        // system: the OS, else the desktop theme's mode, else dark
        assert!(!decide_dark("system", Some(false), Some(true)));
        assert!(decide_dark("system", Some(true), Some(false)));
        assert!(!decide_dark("system", None, Some(false)));
        assert!(decide_dark("system", None, None));
        // an unknown word behaves like system
        assert!(!decide_dark("auto", Some(false), None));
        // new configs are system, a saved file without `mode` stays dark
        assert_eq!(ThemeConfig::default().mode, "system");
        let old: ThemeConfig = toml::from_str("palette = \"nord\"").unwrap();
        assert_eq!(old.mode, "dark");
        let new: ThemeConfig = toml::from_str("mode = \"system\"").unwrap();
        assert_eq!(new.mode, "system");
    }

    #[test]
    fn omarchy_in_the_other_mode_lends_only_its_accent() {
        let d = scratch("mismatch");
        let file = d.join("colors.toml");
        std::fs::write(&file, STOCK).unwrap(); // a dark scheme
        let cfg = ThemeConfig { mode: "light".into(), accent: "matugen".into(), palette: "nord".into(),
            omarchy_file: file.display().to_string(), ..Default::default() };
        let p = palette(&cfg, false);
        assert_eq!(p.0["primary"], hex("#7aa2f7").unwrap());
        assert_eq!(p.0["background"], named("nord", false).unwrap().0["background"]);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn omarchy_under_glass_stays_translucent() {
        let d = scratch("glass");
        let file = d.join("colors.toml");
        std::fs::write(&file, STOCK).unwrap();
        let cfg = ThemeConfig { accent: "matugen".into(), palette: "glass".into(), omarchy_file: file.display().to_string(), ..Default::default() };
        let p = palette(&cfg, true);
        assert!(p.0["background"].a < 1.0 && p.0["card"].a < 1.0);
        assert_eq!(p.0["primary"], hex("#7aa2f7").unwrap());
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn every_palette_fills_every_role_and_only_terminal_has_its_look() {
        for (id, _) in PALETTES.iter().filter(|(id, _)| *id != "warm") {
            for dark in [true, false] {
                assert_eq!(named(id, dark).map(|p| p.0.len()), Some(ROLES.len()), "{id} {dark}");
            }
            let cfg = ThemeConfig { palette: id.to_string(), ..Default::default() };
            assert_eq!(cfg.look().terminal, *id == "terminal", "{id}");
        }
        assert!(!ThemeConfig::default().look().terminal);
    }
}
