//! The profile picture, as the clients see it (no UI here; app/src/avatar.rs
//! draws it): the profile's avatar fields, the initials and their colour.
//!
//! * upload / gravatar: a picture the server keeps (`GET /api/profile/avatar`;
//!   the desktop caches it next to the replica, the web app fetches it with
//!   the data). Without the picture, it falls back to initials.
//! * initials: one or two letters (the profile's, else from the name) on a
//!   colour: a theme chart colour (`chart-1` .. `chart-5`) or `#rrggbb`.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    Upload,
    Gravatar,
    #[default]
    Initials,
}

impl Kind {
    pub fn parse(s: &str) -> Kind {
        match s {
            "upload" => Kind::Upload,
            "gravatar" => Kind::Gravatar,
            _ => Kind::Initials,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Upload => "upload",
            Kind::Gravatar => "gravatar",
            Kind::Initials => "initials",
        }
    }
}

/// The initials' background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// the theme's chart colour 1..=5
    Chart(u8),
    /// 0xRRGGBB
    Hex(u32),
}

impl Color {
    pub fn parse(s: &str) -> Option<Color> {
        let s = s.trim();
        if let Some(n) = s.strip_prefix("chart-") {
            return n.parse::<u8>().ok().filter(|n| (1..=5).contains(n)).map(Color::Chart);
        }
        let h = s.strip_prefix('#')?;
        (h.len() == 6).then(|| u32::from_str_radix(h, 16).ok()).flatten().map(Color::Hex)
    }

    pub fn key(self) -> String {
        match self {
            Color::Chart(n) => format!("chart-{n}"),
            Color::Hex(h) => format!("#{h:06x}"),
        }
    }

    /// A colour of its own for a name with none chosen: the same name
    /// always gets the same chart colour.
    pub fn for_name(name: &str) -> Color {
        let sum = name.trim().to_lowercase().bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        Color::Chart((sum % 5) as u8 + 1)
    }
}

/// The presets offered beside a custom hex.
pub const PRESETS: [Color; 5] = [Color::Chart(1), Color::Chart(2), Color::Chart(3), Color::Chart(4), Color::Chart(5)];

/// "Jane Quinn Doe" -> "JD", "jane" -> "J", "  " -> "". The first letter of
/// the first and of the last word; words that start with no letter or
/// digit ("&", "-") are skipped.
pub fn initials(name: &str) -> String {
    let firsts: Vec<char> = name.split_whitespace().filter_map(|w| w.chars().next().filter(|c| c.is_alphanumeric())).collect();
    let pick: Vec<char> = match firsts.as_slice() {
        [] => vec![],
        [one] => vec![*one],
        [first, .., last] => vec![*first, *last],
    };
    pick.into_iter().flat_map(char::to_uppercase).collect()
}

/// What the profile says about the avatar, ready to draw.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Spec {
    pub kind: Kind,
    pub name: String,
    /// as set, uppercase; empty = from the name
    pub initials: String,
    /// as set; None = from the name
    pub color: Option<Color>,
    /// the Gravatar address
    pub email: String,
    /// moves when the picture changes
    pub updated: Option<i64>,
}

impl Spec {
    /// From the profile (the bundle's `profile`, or `GET /api/profile`).
    pub fn from_profile(p: &Value) -> Spec {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        Spec {
            kind: Kind::parse(&s("avatar_kind")),
            name: s("name"),
            initials: s("avatar_initials").to_uppercase(),
            color: Color::parse(&s("avatar_color")),
            email: s("avatar_email"),
            updated: p.get("avatar_updated").and_then(Value::as_i64),
        }
    }

    /// The letters to draw: the chosen ones, else the name's.
    pub fn letters(&self) -> String {
        if self.initials.is_empty() { initials(&self.name) } else { self.initials.clone() }
    }

    pub fn background(&self) -> Color {
        self.color.unwrap_or_else(|| Color::for_name(&self.name))
    }

    /// Is there a picture to fetch from the server?
    pub fn has_picture(&self) -> bool {
        matches!(self.kind, Kind::Upload | Kind::Gravatar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("Jane Quinn Doe"), "JD");
        assert_eq!(initials("jane"), "J");
        assert_eq!(initials("  "), "");
        assert_eq!(initials("Ana & Luís"), "AL");
        assert_eq!(initials("élodie martin"), "ÉM");
        assert_eq!(initials("李 小龙"), "李小");
    }

    #[test]
    fn colors_and_the_profile() {
        assert_eq!(Color::parse("chart-3"), Some(Color::Chart(3)));
        assert_eq!(Color::parse("chart-7"), None);
        assert_eq!(Color::parse("#7aA2f7"), Some(Color::Hex(0x7aa2f7)));
        assert_eq!(Color::parse("#7aa2f"), None);
        assert_eq!(Color::Hex(0x00ff00).key(), "#00ff00");
        assert_eq!(Color::for_name("Jane"), Color::for_name(" jane "));
        assert!(matches!(Color::for_name("x"), Color::Chart(1..=5)));

        let s = Spec::from_profile(&json!({"name": "Jane Doe", "avatar_kind": "initials", "avatar_color": "chart-2"}));
        assert_eq!((s.letters(), s.background(), s.has_picture()), ("JD".to_string(), Color::Chart(2), false));
        let s = Spec::from_profile(&json!({"name": "Jane Doe", "avatar_kind": "gravatar", "avatar_initials": "jq"}));
        assert_eq!((s.letters().as_str(), s.has_picture()), ("JQ", true));
        // an older server: no avatar fields at all
        let s = Spec::from_profile(&json!({"name": "Jane Doe"}));
        assert_eq!((s.kind, s.letters().as_str()), (Kind::Initials, "JD"));
        assert_eq!(Spec::from_profile(&Value::Null).letters(), "");
    }
}
