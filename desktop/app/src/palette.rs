//! What the Ctrl+K palette lists for a query: pages, actions, themes (the
//! transaction hits come from the searcher). Pure, so it is unit-tested; the
//! dialog in app.rs only renders it and acts on the picked [`Entry`].

use crate::pages::Page;
use crate::theme::{ThemeConfig, PALETTES};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    SyncNow,
    ToggleMode,
    RefreshLive,
}

/// (action, label, extra keywords)
pub const ACTIONS: [(Action, &str, &[&str]); 3] = [
    (Action::SyncNow, "Sync now", &["refresh", "pull", "update"]),
    (Action::ToggleMode, "Toggle dark / light", &["theme", "mode", "dark", "light"]),
    (Action::RefreshLive, "Refresh live data", &["fetch", "crypto", "ibkr", "okx", "binance", "prices", "now"]),
];

/// A theme the palette can preview and apply.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Choice {
    Palette(&'static str),
    /// true = dark
    Mode(bool),
}

impl Choice {
    pub fn apply_to(self, cfg: &mut ThemeConfig) {
        match self {
            Choice::Palette(id) => cfg.palette = id.into(),
            Choice::Mode(dark) => cfg.mode = if dark { "dark" } else { "light" }.into(),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct ThemeEntry {
    pub choice: Choice,
    pub label: String,
    /// the saved palette / mode: shown with a check mark
    pub current: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Entry {
    Page(Page),
    Action(Action),
    Theme(ThemeEntry),
    /// index into the ledger's entries
    Hit(usize),
}

/// The palette's groups in display order; the dialog's section index is the
/// position in [`Results::groups`].
#[derive(Default, Clone)]
pub struct Results {
    pub pages: Vec<Entry>,
    pub actions: Vec<Entry>,
    pub themes: Vec<Entry>,
    pub hits: Vec<Entry>,
}

impl Results {
    pub fn groups(&self) -> [&[Entry]; 4] {
        [&self.pages, &self.actions, &self.themes, &self.hits]
    }

    pub fn get(&self, section: usize, row: usize) -> Option<&Entry> {
        self.groups().get(section)?.get(row)
    }
}

const THEME_WORDS: [&str; 5] = ["theme", "palette", "colors", "colours", "appearance"];

/// Pages (already limited to the visible ones), actions and themes matching
/// `q`. Themes only show for a non-empty query, to keep the empty palette tidy.
pub fn filter(q: &str, pages: impl IntoIterator<Item = Page>, cfg: &ThemeConfig) -> Results {
    let ql = q.trim().to_lowercase();
    let pages = pages.into_iter().filter(|p| ql.is_empty() || p.label().to_lowercase().contains(&ql)).map(Entry::Page).collect();
    let actions = ACTIONS.iter()
        .filter(|(_, label, kw)| ql.is_empty() || label.to_lowercase().contains(&ql) || kw.contains(&ql.as_str()))
        .map(|(a, ..)| Entry::Action(*a))
        .collect();
    Results { pages, actions, themes: themes(&ql, cfg), hits: Vec::new() }
}

fn themes(ql: &str, cfg: &ThemeConfig) -> Vec<Entry> {
    if ql.is_empty() {
        return Vec::new();
    }
    let by_word = THEME_WORDS.contains(&ql);
    let palettes = PALETTES.iter().map(|(id, label)| (Choice::Palette(id), label.to_string(), cfg.palette == *id, by_word));
    let modes = [(true, "Dark mode", "dark"), (false, "Light mode", "light")]
        .map(|(dark, label, word)| (Choice::Mode(dark), label.to_string(), cfg.mode == if dark { "dark" } else { "light" }, by_word || ql == word));
    palettes.chain(modes)
        .filter(|(_, label, _, kw)| *kw || label.to_lowercase().contains(ql))
        .map(|(choice, label, current, _)| Entry::Theme(ThemeEntry { choice, label, current }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(r: &Results) -> Vec<String> {
        r.themes.iter().filter_map(|e| match e { Entry::Theme(t) => Some(t.label.clone()), _ => None }).collect()
    }

    #[test]
    fn themes_show_for_theme_term_and_dark_but_not_an_empty_query() {
        let cfg = ThemeConfig::default();
        assert!(filter("", Page::ALL, &cfg).themes.is_empty());
        assert!(filter("  ", Page::ALL, &cfg).themes.is_empty());
        assert_eq!(labels(&filter("theme", Page::ALL, &cfg)).len(), PALETTES.len() + 2);
        assert_eq!(labels(&filter("term", Page::ALL, &cfg)), ["Terminal"]);
        assert_eq!(labels(&filter("dark", Page::ALL, &cfg)), ["Dark mode"]);
        assert_eq!(labels(&filter("LIGHT", Page::ALL, &cfg)), ["Light mode"]);
        assert!(labels(&filter("zzz", Page::ALL, &cfg)).is_empty());
    }

    #[test]
    fn the_saved_palette_and_mode_carry_the_check() {
        let cfg = ThemeConfig { palette: "nord".into(), mode: "light".into(), ..ThemeConfig::default() };
        let checked: Vec<_> = filter("theme", Page::ALL, &cfg).themes.into_iter()
            .filter_map(|e| match e { Entry::Theme(t) if t.current => Some(t.label), _ => None })
            .collect();
        assert_eq!(checked, ["Nord", "Light mode"]);
    }

    #[test]
    fn a_choice_changes_only_its_own_field() {
        let mut cfg = ThemeConfig::default();
        Choice::Palette("terminal").apply_to(&mut cfg);
        Choice::Mode(false).apply_to(&mut cfg);
        assert_eq!((cfg.palette.as_str(), cfg.dark()), ("terminal", false));
    }

    #[test]
    fn results_are_found_by_section_and_row() {
        let r = filter("sync", Page::ALL, &ThemeConfig::default());
        assert_eq!(r.get(1, 0), Some(&Entry::Action(Action::SyncNow)));
        assert_eq!(r.get(3, 0), None);
    }
}
