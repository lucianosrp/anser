//! Who the data belongs to, and the settings every device shares: two
//! tables in the user's finance.db. Neither holds a secret, so both are safe
//! to replicate (`/api/db.sqlite`) and both ride in the export bundle
//! (`profile`, `user_settings`; Rust-only sections).
//!
//! * `profile`: one row. base_currency, home_country, home_city, name,
//!   aliases (a JSON array: other spellings of the name on bank rows),
//!   benchmark, timezone, onboarding_step, and the avatar: avatar_kind
//!   (upload, gravatar or initials), avatar_email (the Gravatar address),
//!   avatar_initials and avatar_color (a theme chart colour, chart-1 to
//!   chart-5, or #rrggbb), avatar_updated (ms; moves when the picture does).
//!   The picture itself is a file in the user's data dir, never here.
//! * `user_settings`: key -> JSON. `budgets`, `calendar`, `filters` (moved
//!   up from the desktop's per-machine toml files, the owner's decision:
//!   shared by every device and the web app), and `accounts` (Phase 4:
//!   `{source: {"country": "HK" | null}}`, the country of each account, set
//!   in Settings → Sources; a source without one takes the catalog's
//!   default, catalog::default_country).
//!
//! The tables are created by [`ensure`], which the server runs at startup
//! after its pre-migration backup (and before any write here); the readers
//! treat a missing table as empty, so a database that never met this
//! server reads the same as before.

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS profile (
  id             INTEGER PRIMARY KEY CHECK (id = 1),
  base_currency  TEXT NOT NULL DEFAULT 'USD',
  home_country   TEXT,
  home_city      TEXT,
  name           TEXT,
  aliases        TEXT NOT NULL DEFAULT '[]',   -- JSON array of strings
  benchmark      TEXT,
  timezone       TEXT,
  onboarding_step TEXT,                         -- NULL = never started; 'done'
  updated_at     INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS user_settings (
  key        TEXT PRIMARY KEY,                  -- budgets, calendar, filters, accounts, home
  value      TEXT NOT NULL,                     -- JSON
  updated_at INTEGER NOT NULL
);
";

/// Columns added after the table first shipped: (name, type).
const LATER_COLUMNS: [(&str, &str); 5] =
    [("avatar_kind", "TEXT"), ("avatar_email", "TEXT"), ("avatar_initials", "TEXT"), ("avatar_color", "TEXT"), ("avatar_updated", "INTEGER")];

pub const AVATAR_KINDS: [&str; 3] = ["upload", "gravatar", "initials"];

/// The settings a client may write (PUT /api/settings/{key}).
pub const SETTING_KEYS: [&str; 5] = ["budgets", "calendar", "filters", "accounts", "home"];

/// `accounts`: {source: {"country": "XX" or null}}, nothing else.
fn check_accounts(v: &Value) -> Result<()> {
    let Some(m) = v.as_object() else { bail!("accounts is an object of source -> {{country}}") };
    for (src, a) in m {
        if src.is_empty() || src.len() > 40 || !src.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            bail!("{src:?} isn't a source key");
        }
        match a.get("country") {
            Some(Value::Null) | None => {}
            Some(Value::String(c)) if c.is_empty() || (c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic())) => {}
            _ => bail!("a country is a 2-letter code like HK, or null"),
        }
    }
    Ok(())
}

/// `home`: the home setup (finance_core::home::HomeSetup); mortgage mode
/// isn't accepted yet.
fn check_home(v: &Value) -> Result<()> {
    finance_core::home::HomeSetup::check(v).map(|_| ()).map_err(|e| anyhow::anyhow!(e))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub base_currency: String,
    pub home_country: Option<String>,
    pub home_city: Option<String>,
    pub name: Option<String>,
    pub aliases: Vec<String>,
    pub benchmark: Option<String>,
    pub timezone: Option<String>,
    pub onboarding_step: Option<String>,
    pub avatar_kind: Option<String>,
    pub avatar_email: Option<String>,
    pub avatar_initials: Option<String>,
    pub avatar_color: Option<String>,
    /// set by the server when the picture changes; a client's value is ignored
    pub avatar_updated: Option<i64>,
}

pub fn ensure(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    for (col, ty) in LATER_COLUMNS {
        let has: bool = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('profile') WHERE name = ?1", [col], |r| r.get::<_, i64>(0))? > 0;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE profile ADD COLUMN {col} {ty}"))?;
        }
    }
    Ok(())
}

fn has_column(conn: &Connection, col: &str) -> bool {
    conn.query_row("SELECT COUNT(*) FROM pragma_table_info('profile') WHERE name = ?1", [col], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0
}

/// "chart-1".."chart-5" or "#rrggbb".
pub fn valid_avatar_color(c: &str) -> bool {
    let c = c.trim();
    if let Some(n) = c.strip_prefix("chart-") {
        return matches!(n, "1" | "2" | "3" | "4" | "5");
    }
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

/// One or two letters or digits.
pub fn valid_initials(s: &str) -> bool {
    let n = s.trim().chars().count();
    (1..=2).contains(&n) && s.trim().chars().all(char::is_alphanumeric)
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", [name], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

/// The profile; None before one was written (or before [`ensure`]).
pub fn get(conn: &Connection) -> Result<Option<Profile>> {
    if !has_table(conn, "profile") {
        return Ok(None);
    }
    // a database this server hasn't migrated yet reads without the avatar
    let avatar = has_column(conn, "avatar_kind");
    let cols = if avatar { "avatar_kind, avatar_email, avatar_initials, avatar_color, avatar_updated" } else { "NULL, NULL, NULL, NULL, NULL" };
    Ok(conn
        .query_row(
            &format!("SELECT base_currency, home_country, home_city, name, aliases, benchmark, timezone, onboarding_step, {cols} FROM profile WHERE id = 1"),
            [],
            |r| {
                Ok(Profile {
                    base_currency: r.get(0)?,
                    home_country: r.get(1)?,
                    home_city: r.get(2)?,
                    name: r.get(3)?,
                    aliases: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                    benchmark: r.get(5)?,
                    timezone: r.get(6)?,
                    onboarding_step: r.get(7)?,
                    avatar_kind: r.get(8)?,
                    avatar_email: r.get(9)?,
                    avatar_initials: r.get(10)?,
                    avatar_color: r.get(11)?,
                    avatar_updated: r.get(12)?,
                })
            },
        )
        .optional()?)
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Write the whole profile (validated: a 3-letter currency, short fields).
/// The avatar fields change only when `avatar_kind` is given (a client that
/// predates them keeps what is there), and `avatar_updated` only through
/// [`picture_changed`].
pub fn put(conn: &Connection, p: &Profile) -> Result<()> {
    let ccy = p.base_currency.trim().to_uppercase();
    if ccy.len() != 3 || !ccy.chars().all(|c| c.is_ascii_alphabetic()) {
        bail!("base_currency must be a 3-letter code like USD");
    }
    for f in [&p.home_country, &p.home_city, &p.name, &p.benchmark, &p.timezone, &p.onboarding_step].into_iter().flatten() {
        if f.chars().count() > 200 {
            bail!("a profile field is too long");
        }
    }
    let aliases: Vec<String> = p.aliases.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).take(20).collect();
    ensure(conn)?;
    let old = get(conn)?.unwrap_or_default();
    let (kind, email, initials, color) = match clean(&p.avatar_kind) {
        None => (old.avatar_kind, old.avatar_email, old.avatar_initials, old.avatar_color),
        Some(k) => {
            if !AVATAR_KINDS.contains(&k.as_str()) {
                bail!("avatar_kind must be upload, gravatar or initials");
            }
            let initials = clean(&p.avatar_initials);
            if initials.as_deref().is_some_and(|i| !valid_initials(i)) {
                bail!("the initials are one or two letters");
            }
            let color = clean(&p.avatar_color);
            if color.as_deref().is_some_and(|c| !valid_avatar_color(c)) {
                bail!("the avatar colour is chart-1 to chart-5 or #rrggbb");
            }
            let email = clean(&p.avatar_email);
            if email.as_deref().is_some_and(|e| e.len() > 200 || !e.contains('@')) {
                bail!("that email doesn't look right");
            }
            (Some(k), email, initials.map(|i| i.to_uppercase()), color.map(|c| c.to_lowercase()))
        }
    };
    conn.execute(
        "INSERT INTO profile (id, base_currency, home_country, home_city, name, aliases, benchmark, timezone, onboarding_step, updated_at,
           avatar_kind, avatar_email, avatar_initials, avatar_color)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET base_currency=?1, home_country=?2, home_city=?3, name=?4, aliases=?5, benchmark=?6,
           timezone=?7, onboarding_step=?8, updated_at=?9, avatar_kind=?10, avatar_email=?11, avatar_initials=?12, avatar_color=?13",
        params![
            ccy,
            clean(&p.home_country).map(|c| c.to_uppercase()),
            clean(&p.home_city),
            clean(&p.name),
            serde_json::to_string(&aliases)?,
            clean(&p.benchmark),
            clean(&p.timezone),
            clean(&p.onboarding_step),
            crate::db::now_ms(),
            kind,
            email,
            initials,
            color
        ],
    )?;
    Ok(())
}

/// The picture changed (an upload, a Gravatar fetch, or back to initials):
/// set the kind (and the Gravatar address), and move `avatar_updated` so
/// clients fetch it again. Makes a default profile when there is none.
pub fn picture_changed(conn: &Connection, kind: &str, email: Option<&str>) -> Result<()> {
    if !AVATAR_KINDS.contains(&kind) {
        bail!("unknown avatar kind {kind}");
    }
    ensure(conn)?;
    if get(conn)?.is_none() {
        put(conn, &Profile { base_currency: crate::config::NEW_PROFILE_CURRENCY.into(), ..Default::default() })?;
    }
    conn.execute("UPDATE profile SET avatar_kind = ?1, avatar_updated = ?2 WHERE id = 1", params![kind, crate::db::now_ms()])?;
    if let Some(e) = email {
        conn.execute("UPDATE profile SET avatar_email = ?1 WHERE id = 1", [e])?;
    }
    Ok(())
}

/// The owner's name as the self-transfer rules spell it: rules of category
/// "Transfer: self/exchange" whose pattern is just a name (two or three
/// words of letters, none of the payment words). The first is the name,
/// the rest its aliases. None when no rule looks like a name.
pub fn name_from_rules(conn: &Connection) -> Option<(String, Vec<String>)> {
    const NOT_NAMES: [&str; 12] = ["from", "via", "with", "added", "top-up", "auto", "bought", "sold", "to", "payment", "transfer", "money"];
    let mut st = conn
        .prepare("SELECT pattern FROM category_rules WHERE category = 'Transfer: self/exchange' ORDER BY priority, id")
        .ok()?;
    let pats: Vec<String> = st.query_map([], |r| r.get(0)).ok()?.filter_map(|r| r.ok()).collect();
    let names: Vec<String> = pats
        .iter()
        .map(|p| p.trim_start_matches('=').trim().to_string())
        .filter(|p| {
            let words: Vec<&str> = p.split_whitespace().collect();
            (2..=3).contains(&words.len())
                && words.iter().all(|w| w.len() >= 2 && w.chars().all(|c| c.is_alphabetic()))
                && !words.iter().any(|w| NOT_NAMES.contains(&w.to_lowercase().as_str()))
        })
        .collect();
    let title = |s: &str| {
        s.split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let first = names.first()?;
    Some((title(first), names.iter().skip(1).map(|n| title(n)).collect()))
}

/// The owner's profile on an existing tree, written once: the defaults a
/// new user gets (config::BASE_CURRENCY, no home country) and the name from
/// the self-transfer rules. Onboarding is done (the tree has data).
/// Leaves an existing profile alone. Returns whether it wrote one.
pub fn seed_owner(conn: &Connection) -> Result<bool> {
    ensure(conn)?;
    if get(conn)?.is_some() {
        return Ok(false);
    }
    let (name, aliases) = name_from_rules(conn).unwrap_or_default();
    put(conn, &Profile {
        base_currency: crate::config::BASE_CURRENCY.into(),
        name: (!name.is_empty()).then_some(name),
        aliases,
        onboarding_step: Some("done".into()),
        ..Default::default()
    })?;
    Ok(true)
}

/// One setting's JSON value, if set.
pub fn setting(conn: &Connection, key: &str) -> Result<Option<Value>> {
    if !has_table(conn, "user_settings") {
        return Ok(None);
    }
    let raw: Option<String> = conn.query_row("SELECT value FROM user_settings WHERE key = ?", [key], |r| r.get(0)).optional()?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

/// The MCP switch, `{"enabled": bool}` in user_settings. Not one of
/// [`SETTING_KEYS`]: it stays out of the export bundle (so the owner's
/// identity output never changes with it) and is written only through
/// [`set_mcp_enabled`], by the `/api/mcp` route and `anser-server mcp-enable`.
pub const MCP_KEY: &str = "mcp";

/// Whether this user serves MCP over HTTP (`/mcp`). Off unless switched on.
pub fn mcp_enabled(conn: &Connection) -> bool {
    setting(conn, MCP_KEY).ok().flatten().and_then(|v| v.get("enabled").and_then(Value::as_bool)).unwrap_or(false)
}

pub fn set_mcp_enabled(conn: &Connection, enabled: bool) -> Result<()> {
    ensure(conn)?;
    conn.execute(
        "INSERT INTO user_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        params![MCP_KEY, serde_json::json!({"enabled": enabled}).to_string(), crate::db::now_ms()],
    )?;
    Ok(())
}

pub fn put_setting(conn: &Connection, key: &str, value: &Value) -> Result<()> {
    if !SETTING_KEYS.contains(&key) {
        bail!("unknown setting {key}");
    }
    match key {
        "accounts" => check_accounts(value)?,
        "home" => check_home(value)?,
        _ => {}
    }
    let raw = serde_json::to_string(value)?;
    if raw.len() > 1 << 20 {
        bail!("setting too large");
    }
    ensure(conn)?;
    conn.execute(
        "INSERT INTO user_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        params![key, raw, crate::db::now_ms()],
    )?;
    Ok(())
}

/// The bundle's `profile` section: the profile, or null before one exists.
pub fn bundle_profile(conn: &Connection) -> Value {
    match get(conn) {
        Ok(Some(p)) => serde_json::to_value(p).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// The bundle's `user_settings` section: {key: value} of what is set. An
/// object (possibly empty) once the table exists, so a client can tell "this
/// server keeps settings" from an older server (no section at all, or null).
pub fn bundle_settings(conn: &Connection) -> Value {
    if !has_table(conn, "user_settings") {
        return Value::Null;
    }
    let mut out = serde_json::Map::new();
    for k in SETTING_KEYS {
        if let Ok(Some(v)) = setting(conn, k) {
            out.insert(k.into(), v);
        }
    }
    json!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_round_trip_and_seed() {
        let c = crate::db::memory().unwrap();
        assert_eq!(get(&c).unwrap(), None, "no table yet: no profile");
        assert_eq!(bundle_settings(&c), Value::Null);
        c.execute_batch(
            "INSERT INTO category_rules (priority, pattern, category, is_transfer) VALUES
               (26, 'jane quinn doe', 'Transfer: self/exchange', 1),
               (26, 'payment from doe j', 'Transfer: self/exchange', 1),
               (26, 'money added via', 'Transfer: self/exchange', 1),
               (26, 'doe jane', 'Transfer: self/exchange', 1),
               (30, 'jane quinn doe', 'Rent', 0);",
        )
        .unwrap();
        assert_eq!(name_from_rules(&c), Some(("Jane Quinn Doe".into(), vec!["Doe Jane".into()])));
        assert!(seed_owner(&c).unwrap());
        let p = get(&c).unwrap().unwrap();
        assert_eq!((p.base_currency.as_str(), p.home_country.as_deref(), p.name.as_deref()), (crate::config::BASE_CURRENCY, None, Some("Jane Quinn Doe")));
        assert_eq!(p.onboarding_step.as_deref(), Some("done"));
        assert!(!seed_owner(&c).unwrap(), "seeded once");

        assert!(put(&c, &Profile { base_currency: "dollars".into(), ..Default::default() }).is_err());
        put(&c, &Profile { base_currency: "eur".into(), home_country: Some("de".into()), ..p.clone() }).unwrap();
        assert_eq!(get(&c).unwrap().unwrap().base_currency, "EUR");

        assert_eq!(bundle_settings(&c), json!({}));
        put_setting(&c, "budgets", &json!({"monthly": {"Dining": 2200.0}})).unwrap();
        assert!(put_setting(&c, "secrets", &json!({})).is_err());
        assert_eq!(bundle_settings(&c), json!({"budgets": {"monthly": {"Dining": 2200.0}}}));
    }

    #[test]
    fn the_home_setting_is_checked() {
        let c = crate::db::memory().unwrap();
        let ok = json!({"mode": "rent", "currency": "HKD", "rent": {"monthly": 18000.0, "landlord_match": "linden", "lease": {"start": "2026-01-01", "end": "2026-12-31"}},
            "oneoffs": [{"kind": "deposit", "amount": 36000.0, "date": "2026-01-02", "returned_on": null}]});
        put_setting(&c, "home", &ok).unwrap();
        assert_eq!(setting(&c, "home").unwrap(), Some(ok.clone()));
        assert_eq!(bundle_settings(&c)["home"], ok);
        put_setting(&c, "home", &json!({"mode": "none"})).unwrap();
        let err = |v: Value| put_setting(&c, "home", &v).unwrap_err().to_string();
        assert!(err(json!({"mode": "mortgage"})).contains("mortgage"));
        assert!(err(json!({"rent": {}})).contains("mode"));
        let loan = json!({"mode": "mortgage", "mortgage": {"principal": 500000.0, "annual_rate_pct": 3.5, "term_months": 300, "start": "2026-01-15"}});
        put_setting(&c, "home", &loan).unwrap();
        assert_eq!(setting(&c, "home").unwrap(), Some(loan.clone()));
        assert!(err(json!({"mode": "mortgage", "mortgage": {"principal": 500000.0, "annual_rate_pct": 3.5, "term_months": 601, "start": "2026-01-15"}})).contains("term"));
        assert!(err(json!({"mode": "rent", "rent": {"lease": {"end": "soon"}}})).contains("lease end"));
        assert!(err(json!({"mode": "rent", "oneoffs": [{"kind": "tip", "amount": 1}]})).contains("kind"));
        assert_eq!(setting(&c, "home").unwrap(), Some(loan), "a refused write changes nothing");
    }

    #[test]
    fn avatar_fields_validate_and_survive_old_clients() {
        let c = crate::db::memory().unwrap();
        let base = Profile { base_currency: "HKD".into(), name: Some("Jane Doe".into()), ..Default::default() };
        put(&c, &base).unwrap();
        let with = |k: &str, i: &str, col: &str| Profile {
            avatar_kind: Some(k.into()), avatar_initials: Some(i.into()), avatar_color: Some(col.into()), ..base.clone()
        };
        assert!(put(&c, &with("selfie", "JD", "chart-1")).is_err(), "unknown kind");
        assert!(put(&c, &with("initials", "JDX", "chart-1")).is_err(), "three letters");
        assert!(put(&c, &with("initials", "J.", "chart-1")).is_err(), "not a letter");
        assert!(put(&c, &with("initials", "jd", "chart-9")).is_err(), "no such chart colour");
        assert!(put(&c, &with("initials", "jd", "#12345")).is_err(), "short hex");
        put(&c, &with("initials", "jd", "#AABBCC")).unwrap();
        let p = get(&c).unwrap().unwrap();
        assert_eq!((p.avatar_initials.as_deref(), p.avatar_color.as_deref()), (Some("JD"), Some("#aabbcc")));
        // a client that knows nothing of avatars keeps them
        put(&c, &Profile { home_city: Some("Hong Kong".into()), ..base.clone() }).unwrap();
        assert_eq!(get(&c).unwrap().unwrap().avatar_initials.as_deref(), Some("JD"));
        picture_changed(&c, "gravatar", Some("jane@example.com")).unwrap();
        let p = get(&c).unwrap().unwrap();
        assert_eq!((p.avatar_kind.as_deref(), p.avatar_email.as_deref()), (Some("gravatar"), Some("jane@example.com")));
        assert!(p.avatar_updated.is_some());
        assert!(valid_avatar_color("chart-5") && !valid_avatar_color("red"));
    }
}
