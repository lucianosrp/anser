//! The shot list (shots.toml): what to capture and how to frame it.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    #[serde(default)]
    pub shot: Vec<Shot>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shot {
    /// the output file's stem
    pub name: String,
    /// a sidebar page, by label or id (overview, spending, travel, ...)
    #[serde(default = "overview")]
    pub page: String,
    /// the page's tab: investments (overview, holdings, contributions,
    /// income, habits, allocation), broker (monitoring, deep), categories
    /// (rules, suggest)
    pub tab: Option<String>,
    /// desktop (1440x900 @2x), wide (1680x1050 @2x), phone (390x844 @3x), tall (1440x2400 @1x) or phone-tall (390x1800 @2x)
    #[serde(default = "desktop")]
    pub size: String,
    /// dark or light
    #[serde(default = "dark")]
    pub mode: String,
    /// an app palette: warm, nord, catppuccin, gruvbox, tokyo-night, rose-pine, terminal
    #[serde(default = "warm")]
    pub palette: String,
    /// pin the line charts' readout at this fraction of their width
    pub hover: Option<f32>,
    /// pin the travel map's readout on this stop
    pub map_hover: Option<u32>,
    /// false: hide the keyboard hints (?keys=0; default: the app's)
    pub keys: Option<bool>,
    /// anything else for the page's query, `key=value&key=value`
    pub query: Option<String>,
    /// window, phone or none (default: phone for the phone size, else window)
    pub frame: Option<String>,
    /// a backdrop preset (frame.rs BACKDROPS; default: the palette's)
    pub backdrop: Option<String>,
    /// also capture the other mode and show both, split diagonally
    #[serde(default)]
    pub pair: bool,
}

fn overview() -> String {
    "overview".into()
}
fn desktop() -> String {
    "desktop".into()
}
fn dark() -> String {
    "dark".into()
}
fn warm() -> String {
    "warm".into()
}

/// A viewport: CSS size, device scale, phone or not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub mobile: bool,
}

pub fn size(name: &str) -> Option<Size> {
    Some(match name {
        "desktop" => Size { width: 1440, height: 900, scale: 2., mobile: false },
        "wide" => Size { width: 1680, height: 1050, scale: 2., mobile: false },
        "phone" => Size { width: 390, height: 844, scale: 3., mobile: true },
        // tall pages in one image, to check a whole page (verification, not for the README)
        "tall" => Size { width: 1440, height: 2400, scale: 1., mobile: false },
        "phone-tall" => Size { width: 390, height: 1800, scale: 2., mobile: true },
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Frame {
    Window,
    Phone,
    None,
}

impl Shot {
    pub fn size(&self) -> Size {
        size(&self.size).expect("checked")
    }

    pub fn dark(&self) -> bool {
        self.mode != "light"
    }

    pub fn frame(&self) -> Frame {
        match self.frame.as_deref() {
            Some("window") => Frame::Window,
            Some("phone") => Frame::Phone,
            Some("none") => Frame::None,
            _ if self.size().mobile => Frame::Phone,
            _ => Frame::Window,
        }
    }

    pub fn backdrop(&self) -> &str {
        self.backdrop.as_deref().unwrap_or_else(|| crate::frame::backdrop_for(&self.palette, self.dark()))
    }

    fn check(&self) -> Result<()> {
        let ok_name = !self.name.is_empty() && self.name.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
        if !ok_name {
            bail!("shot {:?}: a name is letters, digits, - and _", self.name);
        }
        if size(&self.size).is_none() {
            bail!("shot {}: size {:?} (desktop, wide, phone, tall or phone-tall)", self.name, self.size);
        }
        if !matches!(self.mode.as_str(), "dark" | "light") {
            bail!("shot {}: mode {:?} (dark or light)", self.name, self.mode);
        }
        if !matches!(self.frame.as_deref(), None | Some("window" | "phone" | "none")) {
            bail!("shot {}: frame {:?} (window, phone or none)", self.name, self.frame);
        }
        if crate::frame::backdrop(self.backdrop()).is_none() {
            bail!("shot {}: no backdrop {:?}", self.name, self.backdrop());
        }
        if self.tab.is_some() {
            self.tab_key()?;
        }
        Ok(())
    }

    /// The query key for this page's tab.
    fn tab_key(&self) -> Result<&'static str> {
        Ok(match self.page.to_ascii_lowercase().as_str() {
            "investments" => "inv_tab",
            "broker" => "broker_tab",
            "categories" => "cats_tab",
            "spending" => "spend_tab",
            "settings" => "settings_tab",
            p => bail!("shot {}: page {p} has no tab aid (use query)", self.name),
        })
    }

    /// The web app's URL for this shot in `mode` (`?demo=1` and the aids
    /// app.rs / web/src/lib.rs read).
    pub fn url(&self, origin: &str, mode: &str) -> String {
        let mut q = vec![
            "demo=1".to_string(),
            // WebGL2 on SwiftShader: the same pixels on every run
            "gl=webgl".into(),
            format!("page={}", self.page),
            format!("mode={mode}"),
            format!("palette={}", self.palette),
        ];
        if self.size().mobile {
            q.push("dpr=0".into()); // the full 3x, not the phones' 2x cap
        }
        if let (Some(t), Ok(k)) = (&self.tab, self.tab_key()) {
            q.push(format!("{k}={t}"));
        }
        if let Some(h) = self.hover {
            q.push(format!("hover={h}"));
        }
        if let Some(m) = self.map_hover {
            q.push(format!("map_hover={m}"));
        }
        if self.keys == Some(false) {
            q.push("keys=0".into());
        }
        if let Some(extra) = &self.query {
            q.push(extra.trim_start_matches(['?', '&']).to_string());
        }
        format!("{origin}/app/?{}", q.join("&"))
    }
}

pub fn parse(text: &str) -> Result<Vec<Shot>> {
    let list: List = toml::from_str(text).context("shots.toml")?;
    let mut seen = std::collections::BTreeSet::new();
    for s in &list.shot {
        s.check()?;
        if !seen.insert(&s.name) {
            bail!("two shots named {}", s.name);
        }
    }
    Ok(list.shot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_list_parses() {
        let shots = parse(include_str!("../shots.toml")).unwrap();
        assert!(shots.len() >= 8);
        assert!(shots.iter().any(|s| s.frame() == Frame::Phone));
    }

    #[test]
    fn urls_carry_the_aids() {
        let s = &parse(
            r#"[[shot]]
name = "jkb"
page = "investments"
tab = "jkb"
hover = 0.6
query = "month=2026-08"
"#,
        )
        .unwrap()[0];
        assert_eq!(s.frame(), Frame::Window);
        assert_eq!(s.backdrop(), "warm");
        assert_eq!(
            s.url("http://127.0.0.1:9", "light"),
            "http://127.0.0.1:9/app/?demo=1&gl=webgl&page=investments&mode=light&palette=warm&inv_tab=jkb&hover=0.6&month=2026-08"
        );
        let quiet = &parse("[[shot]]\nname = \"q\"\nkeys = false\n").unwrap()[0];
        assert!(quiet.url("http://x", "dark").contains("&keys=0"));
        assert!(!s.url("http://x", "dark").contains("keys"));
    }

    #[test]
    fn bad_entries_are_refused() {
        assert!(parse("[[shot]]\nname = \"a\"\nsize = \"huge\"").is_err());
        assert!(parse("[[shot]]\nname = \"a\"\npage = \"travel\"\ntab = \"x\"").is_err());
        assert!(parse("[[shot]]\nname = \"a b\"").is_err());
        assert!(parse("[[shot]]\nname = \"a\"\n[[shot]]\nname = \"a\"").is_err());
        assert!(parse("[[shot]]\nname = \"a\"\ncolour = 1").is_err());
    }
}
