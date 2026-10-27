//! Categories and rules on the client: what each category means (the
//! server's `categories` table, carried by the replica and by
//! `/api/ledger.json`), the rules in effective order, and the calls that
//! change them (Phase 3's write APIs).
//!
//! The pages ask [`not_spending`], [`fixed_always`], [`reimbursable`],
//! [`investment`], [`recurring_hint`], [`placeless`] and [`obligation`]
//! instead of the hard-coded lists they had. The table is
//! process-wide (set when a ledger loads, and by a write's reply): one
//! ledger is on screen at a time. A category the table doesn't know (an
//! older replica has no table at all) reads as before, from the lists.
//!
//! A write replies with the rows that moved, the new data_rev, and the
//! categories and rules as they are now: [`crate::Ledger::apply_write`]
//! patches the in-memory ledger at once, and the app syncs in the
//! background for the computed views.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client::{self, BoxFuture, Request};

pub const KINDS: [&str; 8] = ["spending", "income", "transfer", "investment", "card_bill", "fx", "partner", "deposit"];
pub const FIXED: [&str; 3] = ["auto", "always", "never"];
pub const OTHER: &str = "Other";

/// Category fragments that move money between the owner's own pockets (or
/// settle with the partner), so they are not spending. Matched lowercase,
/// as substrings ("transfer" covers "Internal transfer" and "Transfer: …").
/// The fallback for a category the table doesn't know.
const NOT_SPENDING: [&str; 6] = ["transfer", "investments", "credit-card bill", "fx exchange", "partner", "deposit"];
/// Categories fixed by nature (as the server's `FIXED_ALWAYS`): the fallback.
pub const FIXED_CATEGORIES: [&str; 7] = ["Rent", "Subscriptions", "Utilities / Telecom", "Student loan", "Insurance", "Fees", "Gym"];
/// Categories never suggested as a budget: the money comes back (fallback).
const REIMBURSED: &str = "reimbursed";
/// Recurring by nature (as the server's `RECURRING_STRONG`): the fallback.
const RECURRING_STRONG: [&str; 9] = ["subscription", "utilit", "telecom", "gym", "rent", "loan", "insurance", "salary", "partner"];
/// Everyday spending (as the server's `RECURRING_STRICT`): the fallback.
const RECURRING_STRICT: [&str; 2] = ["dining", "groceries"];
/// Paid wherever the owner happens to be (as the server's `PLACELESS`).
const PLACELESS: [&str; 9] = ["subscriptions", "utilities", "student loan", "rent", "gym", "office", "family", "interest", "fees"];
/// Obligations, never "maybe forgotten" (as the server's `OBLIGATION`).
const OBLIGATION: [&str; 5] = ["loan", "rent", "office", "insurance", "tax"];
pub const RECURRING: [&str; 3] = ["strong", "neutral", "strict"];

/// One category, as the server's table has it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Category {
    pub name: String,
    /// a Lucide icon name ("Utensils"); None = picked from the name
    pub icon: Option<String>,
    pub kind: String,
    pub fixed: String,
    pub reimbursable: bool,
    pub hidden: bool,
    pub sort: i64,
    /// strong, neutral or strict: how much evidence a recurring series
    /// needs (Phase 4; None from an older server: the name decides)
    pub recurring: Option<String>,
    /// paid from anywhere: places nobody on a trip (Phase 4)
    pub placeless: Option<bool>,
    /// an obligation: never "maybe forgotten" (Phase 4)
    pub obligation: Option<bool>,
    /// rules that file into it
    pub rules: i64,
    /// bank rows in it
    pub rows: i64,
}

impl Default for Category {
    fn default() -> Self {
        Category {
            name: String::new(), icon: None, kind: "spending".into(), fixed: "auto".into(), reimbursable: false, hidden: false, sort: 0,
            recurring: None, placeless: None, obligation: None, rules: 0, rows: 0,
        }
    }
}

impl Category {
    /// Not spending and not income: money between own pockets, invested,
    /// a card bill, an FX move, settling with the partner, a deposit.
    pub fn moves_money(&self) -> bool {
        !matches!(self.kind.as_str(), "spending" | "income")
    }
}

/// A kind for people.
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "spending" => "Spending",
        "income" => "Income",
        "transfer" => "Transfer",
        "investment" => "Investment",
        "card_bill" => "Card bill",
        "fx" => "FX",
        "partner" => "Partner",
        "deposit" => "Deposit",
        _ => "Spending",
    }
}

/// One rule, as `GET /api/rules` and the write replies list them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Rule {
    pub id: i64,
    pub priority: i64,
    pub pattern: String,
    pub category: String,
    pub is_transfer: bool,
    pub direction: Option<String>,
    pub share: Option<f64>,
    /// seed, user or suggested
    pub origin: String,
    pub enabled: bool,
    pub created_at: Option<i64>,
    pub note: Option<String>,
    /// position among the enabled rules (0 = first)
    pub rank: Option<usize>,
    /// bank rows it decides now
    pub hits: i64,
}

/// A row a write moved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Changed {
    pub source: String,
    pub raw_hash: String,
    pub category: String,
    pub share: Option<f64>,
}

// ---------- the process-wide table ----------

static TABLE: RwLock<Option<Arc<HashMap<String, Category>>>> = RwLock::new(None);

/// Replace the table (an empty list: no table, the lists answer).
pub fn set(list: &[Category]) {
    let t = (!list.is_empty()).then(|| Arc::new(list.iter().map(|c| (c.name.clone(), c.clone())).collect()));
    *TABLE.write().unwrap_or_else(|e| e.into_inner()) = t;
}

/// The category's row, when the table has it.
pub fn get(name: &str) -> Option<Category> {
    TABLE.read().ok()?.as_ref()?.get(name).cloned()
}

/// Money that isn't spending (or income): the table's kind, else the
/// fragments the drill-downs always used.
pub fn not_spending(cat: &str) -> bool {
    match get(cat) {
        Some(c) => c.moves_money(),
        None => {
            let l = cat.to_lowercase();
            NOT_SPENDING.iter().any(|x| l.contains(x))
        }
    }
}

/// Money invested (kind investment; else the name says "investments").
pub fn investment(cat: &str) -> bool {
    match get(cat) {
        Some(c) => c.kind == "investment",
        None => cat.to_lowercase().contains("investments"),
    }
}

/// Fixed by nature (fixed = always; else FIXED_CATEGORIES).
pub fn fixed_always(cat: &str) -> bool {
    match get(cat) {
        Some(c) => c.fixed == "always",
        None => FIXED_CATEGORIES.contains(&cat),
    }
}

/// The money comes back (never a budget suggestion).
pub fn reimbursable(cat: &str) -> bool {
    match get(cat) {
        Some(c) => c.reimbursable,
        None => cat.to_lowercase().contains(REIMBURSED),
    }
}

/// How much evidence a recurring series of this category needs: "strong"
/// (recurring by nature), "strict" (everyday spending) or "neutral". The
/// table's flag, else the fragments the recurring finder always used.
pub fn recurring_hint(cat: &str) -> &'static str {
    let from_name = || {
        let l = cat.to_lowercase();
        if RECURRING_STRONG.iter().any(|x| l.contains(x)) {
            "strong"
        } else if RECURRING_STRICT.iter().any(|x| l.contains(x)) {
            "strict"
        } else {
            "neutral"
        }
    };
    match get(cat).and_then(|c| c.recurring) {
        Some(r) => RECURRING.iter().copied().find(|k| *k == r).unwrap_or("neutral"),
        None => from_name(),
    }
}

/// Paid from wherever the owner is (a direct debit, a subscription): such a
/// row places nobody on a trip.
pub fn placeless(cat: &str) -> bool {
    match get(cat).and_then(|c| c.placeless) {
        Some(v) => v,
        None => {
            let l = cat.to_lowercase();
            PLACELESS.iter().any(|x| l.contains(x))
        }
    }
}

/// An obligation (a loan, the rent, tax): never flagged "maybe forgotten".
pub fn obligation(cat: &str) -> bool {
    match get(cat).and_then(|c| c.obligation) {
        Some(v) => v,
        None => {
            let l = cat.to_lowercase();
            OBLIGATION.iter().any(|x| l.contains(x))
        }
    }
}

/// Kept out of the pickers.
pub fn hidden(cat: &str) -> bool {
    get(cat).is_some_and(|c| c.hidden)
}

/// The icon the owner picked for a category, if any.
pub fn icon(cat: &str) -> Option<String> {
    get(cat).and_then(|c| c.icon)
}

// ---------- the calls ----------

/// "Utilities / Telecom" -> "Utilities%20%2F%20Telecom": one path segment.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn call(method: &str, path: String, body: Option<Value>) -> BoxFuture {
    let mut r = Request::new(method, &path);
    if let Some(b) = body {
        r = r.json(&b);
    }
    client::send(r)
}

/// PUT /api/txn/{source}/{raw_hash}/category: pin one row (None: back to the rules).
pub fn set_row(source: &str, raw_hash: &str, category: Option<&str>, share: Option<f64>, note: Option<&str>) -> BoxFuture {
    call("PUT", format!("/api/txn/{}/{}/category", encode(source), encode(raw_hash)), Some(json!({"category": category, "share": share, "note": note})))
}

/// What a new rule says (preview, create).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleDraft {
    pub pattern: String,
    /// "in", "out", or None for both
    pub direction: Option<String>,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn preview(d: &RuleDraft) -> BoxFuture {
    call("POST", "/api/rules/preview".into(), Some(serde_json::to_value(d).unwrap_or_default()))
}

pub fn create_rule(d: &RuleDraft) -> BoxFuture {
    call("POST", "/api/rules".into(), Some(serde_json::to_value(d).unwrap_or_default()))
}

/// PATCH /api/rules/{id} with only the fields to change.
pub fn patch_rule(id: i64, fields: Value) -> BoxFuture {
    call("PATCH", format!("/api/rules/{id}"), Some(fields))
}

pub fn delete_rule(id: i64) -> BoxFuture {
    call("DELETE", format!("/api/rules/{id}"), None)
}

pub fn test_rule(description: &str, amount: f64) -> BoxFuture {
    call("POST", "/api/rules/test".into(), Some(json!({"description": description, "amount": amount})))
}

pub fn install_starter() -> BoxFuture {
    call("POST", "/api/rules/starter".into(), None)
}

/// POST /api/categories: a new category (its flags from the name unless given).
pub fn create_category(fields: Value) -> BoxFuture {
    call("POST", "/api/categories".into(), Some(fields))
}

/// PATCH /api/categories/{name}: flags, or "name" to rename.
pub fn patch_category(name: &str, fields: Value) -> BoxFuture {
    call("PATCH", format!("/api/categories/{}", encode(name)), Some(fields))
}

/// PATCH /api/categories {order: [names]}: the manager's order.
pub fn reorder(names: &[String]) -> BoxFuture {
    call("PATCH", "/api/categories".into(), Some(json!({"order": names})))
}

pub fn merge(name: &str, into: &str) -> BoxFuture {
    call("POST", format!("/api/categories/{}/merge", encode(name)), Some(json!({"into": into})))
}

/// DELETE /api/categories/{name}?into=...: its rows go `into` (Other: back
/// to uncategorized, its rules and overrides deleted).
pub fn delete_category(name: &str, into: &str) -> BoxFuture {
    call("DELETE", format!("/api/categories/{}?into={}", encode(name), encode(into)), None)
}

pub fn suggestions() -> BoxFuture {
    call("GET", "/api/categorize/suggest".into(), None)
}

// ---------- replies ----------

/// POST /api/rules/preview's answer.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Preview {
    pub pattern: String,
    pub direction: Option<String>,
    pub category: String,
    pub priority: i64,
    pub matches: usize,
    pub total_hkd: f64,
    pub would_change: usize,
    pub pinned: usize,
    pub shadowed: usize,
    pub category_exists: bool,
    pub sample: Vec<PreviewRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PreviewRow {
    pub source: String,
    pub raw_hash: String,
    pub date: String,
    pub description: String,
    pub amount: f64,
    pub currency: String,
    pub hkd: f64,
    pub category: String,
    pub pinned: bool,
    /// the rule that decides the row now
    pub rule_now: Option<RuleRef>,
    /// the new rule would decide it
    pub wins: bool,
    pub category_after: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RuleRef {
    pub id: i64,
    pub pattern: String,
    pub category: String,
    pub priority: i64,
    pub origin: String,
}

/// POST /api/rules/test's answer.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TestResult {
    pub category: String,
    pub winner: Option<RuleRef>,
    pub why: String,
    pub also: Vec<RuleRef>,
    pub disabled: Vec<RuleRef>,
}

/// One cluster of uncategorized rows (GET /api/categorize/suggest).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Suggestion {
    pub key: String,
    pub label: String,
    pub pattern: String,
    pub direction: Option<String>,
    pub count: usize,
    pub total_hkd: f64,
    pub score: f64,
    pub first: String,
    pub last: String,
    pub samples: Vec<String>,
    /// bank rows the pattern would match in all (not only these)
    pub matches: usize,
    pub priority: i64,
    pub proposal: Option<Proposal>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Proposal {
    pub category: String,
    pub reason: String,
    /// high, medium, low
    pub confidence: String,
    /// self, partner, deposit, merchant, recurring, keyword
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Suggestions {
    pub clusters: Vec<Suggestion>,
    pub other_rows: usize,
    pub other_hkd: f64,
    pub enabled_rules: i64,
    /// few rules: offer the starter pack
    pub starter: bool,
    pub starter_rules: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_answers_and_the_lists_stand_in() {
        // names no other test uses: the table is process-wide
        set(&[
            Category { name: "Zz settle".into(), kind: "partner".into(), ..Default::default() },
            Category { name: "Zz rent".into(), fixed: "always".into(), ..Default::default() },
            Category { name: "Zz office".into(), reimbursable: true, hidden: true, ..Default::default() },
        ]);
        assert!(not_spending("Zz settle"));
        assert!(fixed_always("Zz rent"));
        assert!(reimbursable("Zz office") && hidden("Zz office"));
        // unknown: the old lists
        assert!(not_spending("Internal transfer") && !not_spending("Dining"));
        assert!(fixed_always("Rent") && reimbursable("Office (reimbursed)") && investment("Investments"));
        set(&[]);
        assert!(!not_spending("Zz settle"));
        assert_eq!(encode("Utilities / Telecom"), "Utilities%20%2F%20Telecom");
    }
}
