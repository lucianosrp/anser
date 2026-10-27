//! `GET /api/categorize/suggest`: the uncategorized ("Other") bank and card
//! rows, clustered by merchant key (core drill::merchant_key), ranked by
//! spend times count, each with a rule pattern and, when something points
//! somewhere, a proposed category and why:
//!
//! 1. self: the profile's name or an alias (money between own accounts);
//! 2. partner: a word of a partner rule's pattern;
//! 3. deposit: the description says deposit;
//! 4. merchant: other rows of the same merchant already have a category;
//! 5. recurring: a steady monthly/yearly series (core recurring): a
//!    subscription when money goes out, income when it comes in;
//! 6. keyword: a word the cluster shares with the existing rules' patterns.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

use finance_core::{drill, recurring, Kind, Ledger};
use finance_engine::categories::{self, Category};
use finance_engine::{categorize, config, profile, rules};

/// Clusters returned at most.
const MAX_CLUSTERS: usize = 40;
/// Fewer enabled rules than this: offer the starter pack.
const FEW_RULES: i64 = 20;
/// The priority an accepted suggestion's rule gets: after the owner's
/// merchant rules (it only has to beat nothing: its rows are uncategorized).
pub const PRIORITY: i64 = 100;

struct Cluster {
    /// drill::merchant_key (what recurring keys a series by)
    drill_key: String,
    key: String,
    rows: Vec<usize>,
}

fn title(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first category of `kind` (by the manager's order), preferring one
/// whose name contains `hint`.
fn of_kind(cats: &[Category], kind: &str, hint: &str) -> Option<String> {
    let mut v: Vec<&Category> = cats.iter().filter(|c| c.kind == kind && !c.hidden).collect();
    v.sort_by_key(|c| (!c.name.to_lowercase().contains(hint), c.sort));
    v.first().map(|c| c.name.clone())
}

fn named(cats: &[Category], fragment: &str) -> Option<String> {
    cats.iter().find(|c| c.name.to_lowercase().contains(fragment) && !c.hidden).map(|c| c.name.clone())
}

/// Leading words that name a kind of payment, not who it is with.
const GENERIC: [&str; 14] =
    ["transfer", "payment", "from", "to", "by", "direct", "debit", "credit", "card", "purchase", "online", "standing", "order", "trf"];

/// The cluster key: drill's merchant key, unless that is a payment word
/// ("TRANSFER FROM JANE DOE" would lump every transfer together): then the
/// first three merchant words that aren't.
fn cluster_key(desc: &str) -> String {
    let key = drill::merchant_key(desc);
    if !key.split(' ').all(|w| GENERIC.contains(&w)) {
        return key;
    }
    let text = drill::merchant_text(desc);
    let rest: Vec<&str> = text.split(' ').filter(|w| !w.is_empty() && !GENERIC.contains(w)).take(3).collect();
    if rest.is_empty() { key } else { rest.join(" ") }
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// A proposal: (category, reason, confidence, source).
type Prop = (String, String, &'static str, &'static str);

pub fn suggest(conn: &Connection) -> Result<Value> {
    let ledger = Ledger::load(&config::db_path())?;
    let cats = categories::list(conn)?;
    let rules_now = categorize::load_rules(conn)?;
    let enabled = rules::enabled_count(conn);

    // the raw descriptions (the ledger's are whitespace-normalized; the rules
    // match the raw text)
    let raw: HashMap<(String, String), String> = {
        let mut st = conn.prepare("SELECT source, raw_hash, COALESCE(description,'') FROM bank_transactions")?;
        let v = st.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    let raw_lower: Vec<String> = raw.values().map(|d| d.to_lowercase()).collect();
    let raw_of = |i: usize| -> String {
        let e = &ledger.entries[i];
        raw.get(&(e.source.clone(), e.raw_hash.clone())).cloned().unwrap_or_else(|| e.description.clone()).to_lowercase()
    };

    // the uncategorized rows by merchant key
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut slot: HashMap<String, usize> = HashMap::new();
    let mut other_rows = 0;
    let mut other_hkd = 0.0;
    // merchant key -> category -> rows, for the categorized rows
    let mut known: HashMap<String, BTreeMap<String, usize>> = HashMap::new();
    for (i, e) in ledger.entries.iter().enumerate() {
        if !matches!(e.kind, Kind::Bank | Kind::Card) {
            continue;
        }
        let key = cluster_key(&e.description);
        if e.category != categorize::UNCATEGORIZED {
            if !key.is_empty() {
                *known.entry(key).or_default().entry(e.category.clone()).or_default() += 1;
            }
            continue;
        }
        other_rows += 1;
        other_hkd += drill::my_hkd(e).unwrap_or(0.0);
        if key.is_empty() {
            continue;
        }
        let at = *slot.entry(key.clone()).or_insert_with(|| {
            clusters.push(Cluster { key: key.clone(), drill_key: drill::merchant_key(&e.description), rows: Vec::new() });
            clusters.len() - 1
        });
        clusters[at].rows.push(i);
    }

    // what points somewhere
    let me: Vec<Vec<String>> = profile::get(conn)
        .ok()
        .flatten()
        .map(|p| p.name.into_iter().chain(p.aliases).map(|n| words(&n)).filter(|w| w.len() >= 2).collect())
        .unwrap_or_default();
    let partner_cats: Vec<&Category> = cats.iter().filter(|c| c.kind == "partner").collect();
    let partner_words: Vec<(String, String, String)> = rules_now
        .iter()
        .filter(|r| partner_cats.iter().any(|c| c.name == r.category))
        .flat_map(|r| words(r.pattern.trim_start_matches('=')).into_iter().filter(|w| w.len() >= 2).map(move |w| (w, r.pattern.clone(), r.category.clone())))
        .collect();
    let series = recurring::detect(&ledger);
    // how many rows each word appears in: a common word ("payment", "hong")
    // says nothing about a merchant
    let mut df: HashMap<String, usize> = HashMap::new();
    for d in &raw_lower {
        let mut ws = words(d);
        ws.sort();
        ws.dedup();
        for w in ws {
            *df.entry(w).or_default() += 1;
        }
    }
    let common = (raw_lower.len() / 100).max(20);
    // rule pattern words (4+ letters, not common, one category only) -> (category, pattern)
    let mut keyword: HashMap<String, (String, String)> = HashMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for r in &rules_now {
        if r.pattern.starts_with('=') {
            continue;
        }
        for w in words(&r.pattern).into_iter().filter(|w| w.chars().count() >= 4 && w.chars().all(char::is_alphabetic)) {
            if GENERIC.contains(&w.as_str()) || df.get(&w).copied().unwrap_or(0) > common {
                continue;
            }
            match keyword.get(&w) {
                Some((c, _)) if *c != r.category => ambiguous.push(w),
                Some(_) => {}
                None => {
                    keyword.insert(w, (r.category.clone(), r.pattern.clone()));
                }
            }
        }
    }
    for w in ambiguous {
        keyword.remove(&w);
    }

    let propose = |c: &Cluster, text: &str, out: bool| -> Option<Prop> {
        let tw = words(text);
        let has = |ws: &[String]| ws.iter().all(|w| tw.contains(w));
        // 1. the owner's own name, either word order
        if me.iter().any(|n| has(n)) {
            let cat = of_kind(&cats, "transfer", "self").unwrap_or_else(|| "Transfer: self/exchange".into());
            return Some((cat, "your own name: money between your accounts".into(), "high", "self"));
        }
        // 2. the partner
        if let Some((w, pat, cat)) = partner_words.iter().find(|(w, _, _)| tw.contains(w)) {
            return Some((cat.clone(), format!("\u{201c}{w}\u{201d} is in your partner rule \u{201c}{pat}\u{201d}"), "medium", "partner"));
        }
        // 3. a deposit
        if ["deposit", "kaution", "caution"].iter().any(|d| tw.iter().any(|w| w == d)) && !tw.iter().any(|w| w == "cash" || w == "atm") {
            let cat = of_kind(&cats, "deposit", "deposit").unwrap_or_else(|| "Deposit".into());
            return Some((cat, "the description says deposit".into(), "medium", "deposit"));
        }
        // 4. the same merchant elsewhere
        if let Some(m) = known.get(&c.key) {
            if let Some((cat, n)) = m.iter().max_by_key(|(_, n)| **n) {
                return Some((cat.clone(), format!("{n} other row{} from this merchant {} {cat}", if *n == 1 { "" } else { "s" }, if *n == 1 { "is" } else { "are" }), "high", "merchant"));
            }
        }
        // 5. a recurring series
        if let Some(s) = series.iter().find(|s| s.key == c.drill_key && s.active && s.regularity >= 0.6) {
            let cad = s.cadence.label();
            if out {
                if let Some(cat) = named(&cats, "subscription").or_else(|| cats.iter().find(|c| c.fixed == "always" && !c.hidden).map(|c| c.name.clone())) {
                    return Some((cat, format!("{cad}, about {:.0} {} each time", s.typical, s.currency), "medium", "recurring"));
                }
            } else if let Some(cat) = of_kind(&cats, "income", "salary") {
                return Some((cat, format!("comes in {cad}, about {:.0} {}", s.typical, s.currency), "medium", "recurring"));
            }
        }
        // 6. a word an existing rule uses
        if let Some((w, (cat, pat))) = tw.iter().filter(|w| w.chars().count() >= 4).find_map(|w| keyword.get(w).map(|k| (w, k))) {
            return Some((cat.clone(), format!("\u{201c}{w}\u{201d} is in the rule \u{201c}{pat}\u{201d}"), "low", "keyword"));
        }
        None
    };

    let mut out: Vec<(f64, Value)> = Vec::new();
    for c in &clusters {
        let es: Vec<&finance_core::Entry> = c.rows.iter().map(|&i| &ledger.entries[i]).collect();
        let total: f64 = es.iter().map(|e| drill::my_hkd(e).unwrap_or(0.0)).sum();
        let abs: f64 = es.iter().map(|e| drill::my_hkd(e).unwrap_or(0.0).abs()).sum();
        let outs = es.iter().filter(|e| e.amount < 0.0).count();
        let direction = if outs == es.len() { Some("out") } else if outs == 0 { Some("in") } else { None };
        // the pattern: the merchant key when every row's raw text has it,
        // else its first word, else the key anyway (the preview shows the fit)
        let texts: Vec<String> = c.rows.iter().map(|&i| raw_of(i)).collect();
        let first_word = c.key.split(' ').next().unwrap_or(&c.key).to_string();
        let pattern = [c.key.clone(), first_word]
            .into_iter()
            .find(|p| p.chars().count() >= 3 && texts.iter().all(|t| t.contains(p.as_str())))
            .unwrap_or_else(|| c.key.clone());
        let matches = raw_lower.iter().filter(|d| d.contains(pattern.as_str())).count();
        let text = drill::merchant_text(&es[0].description);
        let prop = propose(c, &text, direction != Some("in"));
        let score = abs * es.len() as f64;
        let mut samples: Vec<String> = es.iter().map(|e| e.description.clone()).collect();
        samples.dedup();
        samples.truncate(3);
        out.push((score, json!({
            "key": c.key, "label": title(&c.key), "pattern": pattern, "direction": direction,
            "count": es.len(), "total_hkd": (total * 100.0).round() / 100.0, "score": (score * 100.0).round() / 100.0,
            "first": es.iter().map(|e| e.date.as_str()).min(), "last": es.iter().map(|e| e.date.as_str()).max(),
            "samples": samples, "matches": matches, "priority": PRIORITY,
            "proposal": prop.map(|(category, reason, confidence, source)| json!({
                "category": category, "reason": reason, "confidence": confidence, "source": source})),
        })));
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.truncate(MAX_CLUSTERS);
    Ok(json!({
        "clusters": out.into_iter().map(|x| x.1).collect::<Vec<_>>(),
        "other_rows": other_rows,
        "other_hkd": (other_hkd * 100.0).round() / 100.0,
        "enabled_rules": enabled,
        "starter": enabled < FEW_RULES,
        "starter_rules": rules::starter_pack().len(),
    }))
}
