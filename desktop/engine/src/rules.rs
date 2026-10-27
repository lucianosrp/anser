//! Managing the categorization rules (Phase 3): the list in effective
//! order, create / edit / delete, a dry run of a new rule (preview), "which
//! rule wins this row and why" (test), hand categorization by row key, and
//! the starter pack. Every write goes through [`write`]: one transaction
//! that also adds any new category to the table, recategorizes, and bumps
//! data_rev, and hands back the rows that moved.

use std::collections::HashMap;

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::categories::{self, Changed};
use crate::categorize::{self, order_by, pattern_hits, Rule, DEFAULT_RULES, UNCATEGORIZED};

/// One rule as the Rules manager shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleRow {
    pub id: i64,
    pub priority: i64,
    pub pattern: String,
    pub category: String,
    pub is_transfer: bool,
    pub direction: Option<String>,
    pub share: Option<f64>,
    pub origin: String,
    pub enabled: bool,
    pub created_at: Option<i64>,
    pub note: Option<String>,
    /// position in the effective order among the enabled rules (0 = first)
    pub rank: Option<usize>,
    /// bank rows this rule decides now (no override, first match)
    pub hits: i64,
}

/// A bank row as the previews read it.
struct Row {
    source: String,
    raw_hash: String,
    date: String,
    description: String,
    amount: f64,
    currency: String,
    category: String,
    share: Option<f64>,
}

fn bank_rows(conn: &Connection) -> Result<Vec<Row>> {
    let mut st = conn.prepare(
        "SELECT source, raw_hash, txn_date, COALESCE(description,''), COALESCE(amount,0), currency, COALESCE(category,'Other'), share
         FROM bank_transactions ORDER BY txn_date DESC, id DESC",
    )?;
    let v = st
        .query_map([], |r| {
            Ok(Row {
                source: r.get(0)?,
                raw_hash: r.get(1)?,
                date: r.get(2)?,
                description: r.get(3)?,
                amount: r.get(4)?,
                currency: r.get(5)?,
                category: r.get(6)?,
                share: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Every rule in the effective order (disabled ones where they would sit).
pub fn list(conn: &Connection) -> Result<Vec<RuleRow>> {
    categorize::ensure_rules(conn)?;
    let migrated = categories::rules_migrated(conn);
    let cols = if migrated { "origin, enabled, created_at, note" } else { "'user', 1, NULL, NULL" };
    let mut st = conn.prepare(&format!(
        "SELECT id, priority, pattern, category, is_transfer, direction, share, {cols} FROM category_rules {}",
        order_by(migrated)
    ))?;
    let mut rows: Vec<RuleRow> = st
        .query_map([], |r| {
            Ok(RuleRow {
                id: r.get(0)?,
                priority: r.get(1)?,
                pattern: r.get(2)?,
                category: r.get(3)?,
                is_transfer: r.get::<_, i64>(4)? != 0,
                direction: r.get(5)?,
                share: r.get(6)?,
                origin: r.get(7)?,
                enabled: r.get::<_, i64>(8)? != 0,
                created_at: r.get(9)?,
                note: r.get(10)?,
                rank: None,
                hits: 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut rank = 0;
    for r in rows.iter_mut().filter(|r| r.enabled) {
        r.rank = Some(rank);
        rank += 1;
    }
    // what each enabled rule decides now
    let rules = categorize::load_rules(conn)?;
    let overrides = categorize::load_overrides(conn)?;
    let mut hits: HashMap<i64, i64> = HashMap::new();
    for b in bank_rows(conn)? {
        if overrides.contains_key(&(b.source.clone(), b.raw_hash.clone())) {
            continue;
        }
        if let Some(i) = categorize::match_index(&b.description, b.amount, &rules) {
            *hits.entry(rules[i].id).or_default() += 1;
        }
    }
    for r in rows.iter_mut() {
        r.hits = hits.get(&r.id).copied().unwrap_or(0);
    }
    Ok(rows)
}

/// A rule as a client sends it (POST: pattern and category required).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Input {
    pub pattern: Option<String>,
    /// "in", "out", or "any" / null for both
    #[serde(default, deserialize_with = "some_or_null")]
    pub direction: Option<Option<String>>,
    pub category: Option<String>,
    #[serde(default, deserialize_with = "some_or_null_f64")]
    pub share: Option<Option<f64>>,
    pub priority: Option<i64>,
    #[serde(default, deserialize_with = "some_or_null")]
    pub note: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub origin: Option<String>,
}

fn some_or_null<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

fn some_or_null_f64<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Option<f64>>, D::Error> {
    Ok(Some(Option::<f64>::deserialize(d)?))
}

/// A pattern as the matcher wants it: lowercase; "=text" (whole
/// description) keeps its text trimmed. Never empty, except the bare "="
/// (an empty description).
pub fn clean_pattern(p: &str) -> Result<String> {
    let p = p.to_lowercase();
    let p = match p.strip_prefix('=') {
        Some(rest) => format!("={}", rest.trim()),
        None => p.trim_start().to_string(), // a trailing space is meaningful ("mtr ")
    };
    if p.trim().is_empty() {
        bail!("a rule needs a pattern");
    }
    if p.chars().count() > 120 {
        bail!("a pattern is 120 characters at most");
    }
    Ok(p)
}

pub fn clean_direction(d: Option<&str>) -> Result<Option<String>> {
    match d.map(|s| s.trim().to_lowercase()).as_deref() {
        None | Some("") | Some("any") | Some("both") => Ok(None),
        Some("in") => Ok(Some("in".into())),
        Some("out") => Ok(Some("out".into())),
        Some(x) => bail!("direction is in, out or any, not {x}"),
    }
}

fn clean_share(s: Option<f64>) -> Result<Option<f64>> {
    match s {
        None => Ok(None),
        Some(v) if v > 0.0 && v <= 1.0 => Ok(if v == 1.0 { None } else { Some(v) }),
        Some(_) => bail!("a share is a fraction between 0 and 1"),
    }
}

fn clean_priority(p: i64) -> Result<i64> {
    if !(0..=1000).contains(&p) {
        bail!("priority is 0 to 1000");
    }
    Ok(p)
}

/// The category a rule or override names: an existing one, or a new one
/// with its default flags. Returns whether it moves money (is_transfer).
fn category_for(conn: &Connection, name: &str) -> Result<(String, bool)> {
    let name = categories::clean_name(name)?;
    if name == UNCATEGORIZED {
        return Ok((name, false));
    }
    match categories::get(conn, &name)? {
        Some(c) => Ok((name, c.moves_money())),
        None => {
            let c = categories::create(conn, &categories::Patch { name: Some(name.clone()), ..Default::default() })?;
            Ok((name, c.moves_money()))
        }
    }
}

/// A write's result: what it made, the rows that moved, the new data_rev.
pub struct Written<T> {
    pub value: T,
    pub changed: Vec<Changed>,
    pub data_rev: i64,
}

/// Run a write in one transaction: the Phase 3 tables first, then `f`,
/// then new categories into the table, a recategorize and a data_rev bump.
pub fn write<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<Written<T>> {
    categories::ensure(conn)?;
    let tx = conn.unchecked_transaction()?;
    let value = f(&tx)?;
    categories::sync_categories(&tx)?;
    let changed = categorize::recategorize(&tx)?;
    let data_rev = categories::bump(&tx)?;
    tx.commit()?;
    Ok(Written { value, changed, data_rev })
}

/// The reply every write sends: the rows that moved, the new data_rev, and
/// the categories and rules as they are now (a client patches its copy).
pub fn reply<T: Serialize>(conn: &Connection, w: &Written<T>) -> Value {
    json!({
        "ok": true,
        "result": w.value,
        "changed": w.changed,
        "data_rev": w.data_rev,
        "categories": categories::list(conn).unwrap_or_default(),
        "rules": list(conn).unwrap_or_default(),
    })
}

/// A new rule (inside [`write`]). Returns its id.
pub fn create(conn: &Connection, input: &Input) -> Result<i64> {
    let pattern = clean_pattern(input.pattern.as_deref().unwrap_or(""))?;
    let direction = clean_direction(input.direction.clone().flatten().as_deref())?;
    let (category, transfer) = category_for(conn, input.category.as_deref().unwrap_or(""))?;
    let share = clean_share(input.share.flatten())?;
    let priority = clean_priority(input.priority.unwrap_or(50))?;
    let origin = input.origin.clone().unwrap_or_else(|| "user".into());
    if !categories::ORIGINS.contains(&origin.as_str()) {
        bail!("origin is seed, user or suggested");
    }
    let note = input.note.clone().flatten().map(|n| n.trim().chars().take(200).collect::<String>()).filter(|n| !n.is_empty());
    conn.execute(
        "INSERT INTO category_rules (priority, pattern, category, is_transfer, direction, share, origin, enabled, created_at, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![priority, pattern, category, transfer as i64, direction, share, origin, input.enabled.unwrap_or(true) as i64, crate::db::now_ms(), note],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Edit rule `id` (inside [`write`]): only what the input says.
pub fn update(conn: &Connection, id: i64, input: &Input) -> Result<()> {
    let exists = conn.query_row("SELECT 1 FROM category_rules WHERE id = ?", [id], |_| Ok(())).optional()?.is_some();
    if !exists {
        bail!("no rule {id}");
    }
    if let Some(p) = &input.pattern {
        conn.execute("UPDATE category_rules SET pattern = ?2 WHERE id = ?1", params![id, clean_pattern(p)?])?;
    }
    if let Some(d) = &input.direction {
        conn.execute("UPDATE category_rules SET direction = ?2 WHERE id = ?1", params![id, clean_direction(d.as_deref())?])?;
    }
    if let Some(c) = &input.category {
        let (category, transfer) = category_for(conn, c)?;
        conn.execute("UPDATE category_rules SET category = ?2, is_transfer = ?3 WHERE id = ?1", params![id, category, transfer as i64])?;
    }
    if let Some(s) = input.share {
        conn.execute("UPDATE category_rules SET share = ?2 WHERE id = ?1", params![id, clean_share(s)?])?;
    }
    if let Some(p) = input.priority {
        conn.execute("UPDATE category_rules SET priority = ?2 WHERE id = ?1", params![id, clean_priority(p)?])?;
    }
    if let Some(e) = input.enabled {
        conn.execute("UPDATE category_rules SET enabled = ?2 WHERE id = ?1", params![id, e as i64])?;
    }
    if let Some(n) = &input.note {
        let n = n.as_deref().map(|n| n.trim().chars().take(200).collect::<String>()).filter(|n| !n.is_empty());
        conn.execute("UPDATE category_rules SET note = ?2 WHERE id = ?1", params![id, n])?;
    }
    if let Some(o) = &input.origin {
        if !categories::ORIGINS.contains(&o.as_str()) {
            bail!("origin is seed, user or suggested");
        }
        conn.execute("UPDATE category_rules SET origin = ?2 WHERE id = ?1", params![id, o])?;
    }
    Ok(())
}

/// Delete rule `id` (inside [`write`]).
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    if conn.execute("DELETE FROM category_rules WHERE id = ?", [id])? == 0 {
        bail!("no rule {id}");
    }
    Ok(())
}

/// Pin one row (by its natural key) to a category, or, with None, hand it
/// back to the rules (inside [`write`]). share None = the whole row.
pub fn set_override(conn: &Connection, source: &str, raw_hash: &str, category: Option<&str>, share: Option<f64>, note: Option<&str>) -> Result<()> {
    let exists = conn
        .query_row("SELECT 1 FROM bank_transactions WHERE source = ? AND raw_hash = ?", params![source, raw_hash], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        bail!("no bank row {source}/{raw_hash}");
    }
    match category {
        None => {
            conn.execute("DELETE FROM txn_overrides WHERE source = ? AND raw_hash = ?", params![source, raw_hash])?;
        }
        Some(c) => {
            let (cat, _) = category_for(conn, c)?;
            let share = clean_share(share)?;
            let note = note.map(|n| n.trim().chars().take(200).collect::<String>()).filter(|n| !n.is_empty());
            conn.execute(
                "INSERT OR REPLACE INTO txn_overrides (source, raw_hash, category, share, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![source, raw_hash, cat, share, note, crate::db::now_ms() / 1000],
            )?;
        }
    }
    Ok(())
}

/// The candidate's place among `rules` (enabled, effective order): user
/// rules by (priority, id), the new rule's id after every existing one.
fn insert_at(rules: &[Rule], priority: i64) -> usize {
    rules.iter().position(|r| r.origin == "seed" || r.priority > priority).unwrap_or(rules.len())
}

fn rule_json(r: &Rule) -> Value {
    json!({"id": r.id, "pattern": r.pattern, "category": r.category, "priority": r.priority, "origin": r.origin,
           "direction": r.direction, "share": r.share})
}

/// POST /api/rules/preview: what a new user rule (pattern, direction,
/// category, and optionally priority and share) would do. Without a
/// priority it gets the highest one that still wins every row it matches
/// (at most 50). Writes nothing.
pub fn preview(conn: &Connection, input: &Input) -> Result<Value> {
    let pattern = clean_pattern(input.pattern.as_deref().unwrap_or(""))?;
    let direction = clean_direction(input.direction.clone().flatten().as_deref())?;
    let category = categories::clean_name(input.category.as_deref().unwrap_or(""))?;
    let share = clean_share(input.share.flatten())?;
    let rules = categorize::load_rules(conn)?;
    let overrides = categorize::load_overrides(conn)?;
    let rows = bank_rows(conn)?;
    let rates = crate::views::fx::Rates::load(conn);
    let lower: Vec<String> = rows.iter().map(|r| r.description.to_lowercase()).collect();
    let matched: Vec<usize> = (0..rows.len()).filter(|&i| pattern_hits(&pattern, direction.as_deref(), &lower[i], rows[i].amount)).collect();
    // the priority: given, else the highest that beats every user rule
    // winning a matched row now (seed rules never beat a user rule)
    let winners: Vec<Option<usize>> = matched.iter().map(|&i| categorize::match_index(&rows[i].description, rows[i].amount, &rules)).collect();
    let priority = match input.priority {
        Some(p) => clean_priority(p)?,
        None => {
            let beat = matched
                .iter()
                .zip(&winners)
                .filter(|(i, _)| !overrides.contains_key(&(rows[**i].source.clone(), rows[**i].raw_hash.clone())))
                .filter_map(|(_, w)| w.map(|w| &rules[w]))
                .filter(|r| r.origin != "seed")
                .map(|r| r.priority - 1)
                .min()
                .unwrap_or(50);
            beat.clamp(0, 50)
        }
    };
    let id = conn.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM category_rules", [], |r| r.get::<_, i64>(0))?;
    let candidate = Rule {
        id,
        priority,
        pattern: pattern.clone(),
        category: category.clone(),
        is_transfer: 0,
        direction: direction.clone(),
        share,
        origin: "user".into(),
    };
    let at = insert_at(&rules, priority);
    let mut with = rules.clone();
    with.insert(at, candidate);
    let (mut total, mut change, mut pinned, mut shadowed) = (0.0, 0, 0, 0);
    let mut sample = Vec::new();
    for (&i, now) in matched.iter().zip(&winners) {
        let r = &rows[i];
        let hkd = r.amount * rates.rate(&r.currency, r.date.get(..7).unwrap_or(""));
        total += hkd;
        let pin = overrides.get(&(r.source.clone(), r.raw_hash.clone()));
        let after = categorize::match_index(&r.description, r.amount, &with);
        let wins = after == Some(at);
        let new_cat = match pin {
            Some((c, _)) => c.clone(),
            None => after.map(|a| with[a].category.clone()).unwrap_or_else(|| UNCATEGORIZED.into()),
        };
        let moves = new_cat != r.category;
        if pin.is_some() {
            pinned += 1;
        } else if !wins {
            shadowed += 1;
        }
        if moves {
            change += 1;
        }
        if sample.len() < 20 {
            sample.push(json!({
                "source": r.source, "raw_hash": r.raw_hash, "date": r.date.get(..10).unwrap_or(&r.date), "description": r.description,
                "amount": r.amount, "currency": r.currency, "hkd": (hkd * 100.0).round() / 100.0, "category": r.category, "share": r.share,
                "pinned": pin.is_some(), "rule_now": now.map(|w| rule_json(&rules[w])), "wins": wins && pin.is_none(),
                "category_after": new_cat,
            }));
        }
    }
    Ok(json!({
        "pattern": pattern, "direction": direction, "category": category, "share": share, "priority": priority,
        "matches": matched.len(), "total_hkd": (total * 100.0).round() / 100.0, "would_change": change,
        "pinned": pinned, "shadowed": shadowed, "sample": sample,
        "category_exists": category == UNCATEGORIZED || categories::exists(conn, &category).unwrap_or(false),
    }))
}

/// POST /api/rules/test: which rule wins a (description, amount) and why,
/// with every other enabled rule that matches (in order) and the disabled
/// ones that would have.
pub fn test(conn: &Connection, description: &str, amount: f64) -> Result<Value> {
    let rules = categorize::load_rules(conn)?;
    let lower = description.to_lowercase();
    let hits: Vec<&Rule> = rules.iter().filter(|r| categorize::rule_hits(r, &lower, amount)).collect();
    let disabled: Vec<Value> = list(conn)?
        .into_iter()
        .filter(|r| !r.enabled && pattern_hits(&r.pattern, r.direction.as_deref(), &lower, amount))
        .map(|r| json!({"id": r.id, "pattern": r.pattern, "category": r.category, "priority": r.priority, "origin": r.origin}))
        .collect();
    let why = match hits.first() {
        None => format!("no enabled rule matches, so it is {UNCATEGORIZED}"),
        Some(w) => {
            let how = if w.pattern.starts_with('=') { "the whole description is" } else { "the description contains" };
            let dir = match w.direction.as_deref() {
                Some("in") => ", money in",
                Some("out") => ", money out",
                _ => "",
            };
            let mut s = format!("{how} \u{201c}{}\u{201d}{dir}: {} rule #{} (priority {}) is the first match", w.pattern.trim_start_matches('='), w.origin, w.id, w.priority);
            if hits.len() > 1 {
                s += &format!("; {} more rule{} match{} but come later (user rules before seed rules, then by priority)",
                    hits.len() - 1, if hits.len() > 2 { "s" } else { "" }, if hits.len() > 2 { "" } else { "es" });
            }
            s
        }
    };
    Ok(json!({
        "category": hits.first().map(|w| w.category.clone()).unwrap_or_else(|| UNCATEGORIZED.into()),
        "share": hits.first().and_then(|w| w.share),
        "winner": hits.first().map(|w| rule_json(w)),
        "why": why,
        "also": hits.iter().skip(1).map(|r| rule_json(r)).collect::<Vec<_>>(),
        "disabled": disabled,
    }))
}

/// DEFAULT_RULES patterns too regional for a starter pack: one bank's
/// unit-trust debit line and the UK's student loans. (DEFAULT_RULES holds
/// no one's personal rules: those live in their own database.)
const NOT_STARTER: [&str; 3] = ["sec     p", "loans company", "slc "];

/// The starter pack: DEFAULT_RULES without the regional patterns and the
/// shared-bill rules (priority, pattern, category, is_transfer, direction,
/// share).
pub fn starter_pack() -> Vec<(i64, &'static str, &'static str, i64, Option<&'static str>, Option<f64>)> {
    DEFAULT_RULES.iter().copied().filter(|r| !NOT_STARTER.contains(&r.1) && r.5.is_none()).collect()
}

/// Install the starter pack as seed rules (inside [`write`]): every rule
/// whose (pattern, direction) isn't there yet. Returns how many went in.
pub fn install_starter(conn: &Connection) -> Result<usize> {
    let have: Vec<(String, Option<String>)> = {
        let mut st = conn.prepare("SELECT pattern, direction FROM category_rules")?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    let now = crate::db::now_ms();
    let mut n = 0;
    for (pr, pat, cat, tr, dir, share) in starter_pack() {
        if have.iter().any(|(p, d)| p == pat && d.as_deref() == dir) {
            continue;
        }
        conn.execute(
            "INSERT INTO category_rules (priority, pattern, category, is_transfer, direction, share, origin, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'seed', 1, ?7)",
            params![pr, pat, cat, tr, dir, share, now],
        )?;
        n += 1;
    }
    Ok(n)
}

/// How many rules are enabled (the "starting from zero" offer wants few).
pub fn enabled_count(conn: &Connection) -> i64 {
    let sql = if categories::rules_migrated(conn) { "SELECT COUNT(*) FROM category_rules WHERE enabled = 1" } else { "SELECT COUNT(*) FROM category_rules" };
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = crate::db::memory().unwrap();
        categorize::ensure_rules(&c).unwrap();
        c.execute_batch(
            "INSERT INTO bank_transactions(source, account, currency, txn_date, amount, description, raw_hash, source_file, ingested_at) VALUES
               ('hsbc_card', '', 'HKD', '2026-09-01', -120, 'TAVERNA ZORBA MOS', 'h1', 'f', 0),
               ('hsbc_card', '', 'HKD', '2026-09-08', -80, 'TAVERNA ZORBA MK', 'h2', 'f', 0),
               ('hsbc_card', '', 'HKD', '2026-09-09', -50, 'Sushi Bar Toro', 'h3', 'f', 0),
               ('hsbc', '', 'HKD', '2026-09-10', 300, 'TAVERNA ZORBA REFUND', 'h4', 'f', 0),
               ('n26', '', 'EUR', '2026-09-11', -9.99, 'Spotify', 'h5', 'f', 0);",
        )
        .unwrap();
        categorize::categorize_all(&c).unwrap();
        categories::ensure(&c).unwrap();
        c
    }

    fn cat(c: &Connection, h: &str) -> String {
        c.query_row("SELECT category FROM bank_transactions WHERE raw_hash = ?", [h], |r| r.get(0)).unwrap()
    }

    #[test]
    fn user_rules_come_before_seed_rules_whatever_the_priority() {
        let c = crate::db::memory().unwrap();
        categories::ensure(&c).unwrap(); // a fresh install: the defaults go in as seed rules
        categorize::ensure_rules(&c).unwrap();
        let seeds: i64 = c.query_row("SELECT COUNT(*) FROM category_rules WHERE origin = 'seed'", [], |r| r.get(0)).unwrap();
        assert_eq!(seeds as usize, DEFAULT_RULES.len());
        assert!(categories::get(&c, "Dining").unwrap().is_some(), "seeded categories join the table");
        let w = write(&c, |t| create(t, &Input { pattern: Some("Starbucks".into()), category: Some("Coffee".into()), priority: Some(999), ..Default::default() })).unwrap();
        let rules = categorize::load_rules(&c).unwrap();
        assert_eq!(rules[0].id, w.value, "a user rule at priority 999 still comes first");
        assert_eq!(rules[0].pattern, "starbucks", "patterns are stored lowercase");
        assert_eq!(categorize::match_rule("STARBUCKS #12", -40.0, &rules).0, "Coffee");
        assert!(categories::get(&c, "Coffee").unwrap().is_some(), "a new category joins the table");
        // disabled rules don't match; the seed rule takes over again
        write(&c, |t| update(t, w.value, &Input { enabled: Some(false), ..Default::default() })).unwrap();
        assert_eq!(categorize::match_rule("STARBUCKS #12", -40.0, &categorize::load_rules(&c).unwrap()).0, "Dining");
    }

    #[test]
    fn overrides_beat_rules_and_writes_report_the_rows_that_moved() {
        let c = db();
        assert_eq!(cat(&c, "h1"), "Other");
        assert_eq!(cat(&c, "h3"), "Dining", "'sushi' is a default rule");
        let w = write(&c, |t| set_override(t, "hsbc_card", "h1", Some("Treats"), None, Some("birthday"))).unwrap();
        assert_eq!(w.changed, vec![Changed { source: "hsbc_card".into(), raw_hash: "h1".into(), category: "Treats".into(), share: None }]);
        assert_eq!(w.data_rev, 1);
        // a rule for the same rows: the pinned one stays
        let w = write(&c, |t| create(t, &Input { pattern: Some("zorba".into()), category: Some("Dining".into()), direction: Some(Some("out".into())), ..Default::default() })).unwrap();
        assert_eq!(w.changed.len(), 1, "h2 moves; h1 is pinned, h4 is money in");
        assert_eq!((cat(&c, "h1"), cat(&c, "h2"), cat(&c, "h4")), ("Treats".into(), "Dining".into(), "Other".into()));
        assert_eq!(w.data_rev, 2);
        // the override removed: the rule decides
        let w = write(&c, |t| set_override(t, "hsbc_card", "h1", None, None, None)).unwrap();
        assert_eq!(w.changed[0].category, "Dining");
        assert!(write(&c, |t| set_override(t, "hsbc_card", "nope", Some("Dining"), None, None)).is_err());
        assert_eq!(categories::data_rev(&c), 3, "a failed write bumps nothing");
    }

    #[test]
    fn preview_counts_and_the_winner_per_row() {
        let c = db();
        write(&c, |t| set_override(t, "hsbc_card", "h2", Some("Treats"), None, None)).unwrap();
        let p = preview(&c, &Input { pattern: Some("R".into()), category: Some("Japanese".into()), direction: Some(Some("out".into())), ..Default::default() }).unwrap();
        assert_eq!(p["matches"], 3, "money out only: not the refund");
        assert_eq!(p["pinned"], 1);
        assert_eq!(p["would_change"], 2, "h1 (Other) and h3 (Dining) become Japanese; h2 is pinned");
        assert_eq!(p["total_hkd"], -250.0);
        assert_eq!(p["category_exists"], false);
        let s = p["sample"].as_array().unwrap();
        let h3 = s.iter().find(|r| r["raw_hash"] == "h3").unwrap();
        assert_eq!(h3["rule_now"]["pattern"], "sushi");
        assert_eq!(h3["wins"], true, "a user rule beats the rule that wins now");
        assert_eq!(p["priority"], 50);
        // nothing was written
        assert_eq!(cat(&c, "h3"), "Dining");
        assert!(categories::get(&c, "Japanese").unwrap().is_none());

        // shadowed by a user rule: the suggested priority beats it
        write(&c, |t| create(t, &Input { pattern: Some("toro".into()), category: Some("Dining".into()), priority: Some(20), ..Default::default() })).unwrap();
        let p = preview(&c, &Input { pattern: Some("r".into()), category: Some("Japanese".into()), direction: Some(Some("out".into())), ..Default::default() }).unwrap();
        assert_eq!(p["priority"], 19);
        assert_eq!(p["shadowed"], 0);
        let p = preview(&c, &Input { pattern: Some("r".into()), category: Some("Japanese".into()), direction: Some(Some("out".into())), priority: Some(40), ..Default::default() }).unwrap();
        assert_eq!(p["shadowed"], 1);
    }

    #[test]
    fn test_explains_the_winner() {
        let c = db();
        let t = test(&c, "Sushi Bar Toro", -50.0).unwrap();
        assert_eq!(t["category"], "Dining");
        assert_eq!(t["winner"]["pattern"], "sushi");
        assert!(t["why"].as_str().unwrap().contains("first match"), "{}", t["why"]);
        assert!(!t["also"].as_array().unwrap().is_empty(), "'bar ' matches too");
        let t = test(&c, "zzz unknown", -5.0).unwrap();
        assert_eq!((t["category"].as_str(), t["winner"].is_null()), (Some("Other"), true));
    }

    #[test]
    fn the_starter_pack_skips_what_is_there_and_regional_rules() {
        let c = crate::db::memory().unwrap();
        categories::ensure(&c).unwrap();
        c.execute("INSERT INTO category_rules (priority, pattern, category) VALUES (50, 'tesco', 'Food')", []).unwrap();
        let w = write(&c, install_starter).unwrap();
        let pack = starter_pack();
        assert_eq!(w.value, pack.len() - 1);
        assert!(pack.iter().all(|r| !NOT_STARTER.contains(&r.1) && r.5.is_none()));
        assert!(enabled_count(&c) > 290);
    }

    #[test]
    fn list_is_in_effective_order_with_hits() {
        let c = db();
        let w = write(&c, |t| create(t, &Input { pattern: Some("zorba".into()), category: Some("Dining".into()), priority: Some(90), ..Default::default() })).unwrap();
        let l = list(&c).unwrap();
        // every existing rule became a user rule: the new one sits by priority
        let mine = l.iter().find(|r| r.id == w.value).unwrap();
        assert_eq!(mine.hits, 3);
        assert!(l.windows(2).all(|p| (p[0].origin == "seed") <= (p[1].origin == "seed")));
        assert_eq!(l.iter().filter(|r| r.rank.is_some()).count(), l.len());
    }
}
