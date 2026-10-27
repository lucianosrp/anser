//! Categories as data (Phase 3): what each category means, kept in the
//! user's finance.db so every device and the web app share it.
//!
//! * `categories`: one row per category. `kind` says what the money is
//!   (spending, income, transfer, investment, card_bill, fx, partner,
//!   deposit: every kind but spending and income is money moving between
//!   the owner's own pockets or settling up, so it is not spending), `fixed`
//!   (auto, always, never) whether its outflows count as fixed costs,
//!   `reimbursable` money that comes back (never a budget suggestion; the
//!   Owed page's reimbursements), `hidden` keeps it out of the pickers,
//!   `icon` a Lucide icon name (NULL: the app picks one from the name),
//!   `sort` the order the Categories manager shows. Phase 4 adds the
//!   flags the client-side pages read: `recurring` (strong, neutral,
//!   strict: how much evidence a recurring series needs), `placeless` (paid
//!   from wherever the owner is: it places nobody on a trip) and
//!   `obligation` (never a "maybe forgotten" subscription). They are
//!   seeded per name from the fragments the pages used to hard-code, so a
//!   migrated db reads exactly as before.
//! * `category_rules` gains `origin` (seed, user, suggested), `enabled`,
//!   `created_at` and `note`. The effective order is: an override, then the
//!   enabled user (and suggested) rules by (priority, id), then the enabled
//!   seed rules by (priority, id). The columns default to user/enabled, so
//!   the migration makes every existing rule a user rule and nothing moves.
//! * `app_meta`: `data_rev`, a counter every write through the category
//!   APIs bumps (in capabilities and the bundle: a device that sees a new one
//!   knows its replica is behind).
//!
//! All of it is created by [`ensure`], which the server runs at startup
//! (after its pre-migration backup), never by `db::connect`: connect() and
//! ingest leave sqlite_master exactly as SCHEMA has it (the golden tests
//! compare it). Every reader treats a missing table or column as "as
//! before": [`Flags`] falls back to the hard-coded lists the views used, so
//! an old replica (or a db this server never migrated) computes the same
//! numbers.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const KINDS: [&str; 8] = ["spending", "income", "transfer", "investment", "card_bill", "fx", "partner", "deposit"];
pub const FIXED: [&str; 3] = ["auto", "always", "never"];
pub const ORIGINS: [&str; 3] = ["seed", "user", "suggested"];
/// What an uncategorized row reads as (categorize::UNCATEGORIZED).
pub const OTHER: &str = crate::categorize::UNCATEGORIZED;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS categories (
  name         TEXT PRIMARY KEY,
  icon         TEXT,                          -- a Lucide icon name; NULL = from the name
  kind         TEXT NOT NULL DEFAULT 'spending',
  fixed        TEXT NOT NULL DEFAULT 'auto',  -- auto, always, never
  reimbursable INTEGER NOT NULL DEFAULT 0,
  hidden       INTEGER NOT NULL DEFAULT 0,
  sort         INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS app_meta (
  key   TEXT PRIMARY KEY,                     -- data_rev
  value TEXT NOT NULL
);
";

/// Columns the categories table gains in Phase 4: (name, declaration).
/// Added by [`ensure`] and backfilled from [`default_flags`].
const CATEGORY_COLUMNS: [(&str, &str); 3] = [
    ("recurring", "TEXT NOT NULL DEFAULT 'neutral'"),
    ("placeless", "INTEGER NOT NULL DEFAULT 0"),
    ("obligation", "INTEGER NOT NULL DEFAULT 0"),
];
pub const RECURRING: [&str; 3] = ["strong", "neutral", "strict"];

/// Columns category_rules gains: (name, declaration).
const RULE_COLUMNS: [(&str, &str); 4] = [
    ("origin", "TEXT NOT NULL DEFAULT 'user'"),
    ("enabled", "INTEGER NOT NULL DEFAULT 1"),
    ("created_at", "INTEGER"),
    ("note", "TEXT"),
];

// ---------- the hard-coded semantics the table replaces ----------
// (the views' lists, kept as the fallback for a db without the table)

/// analytics FIXED_CATEGORIES (and core filter's copy): fixed by nature.
pub const FIXED_ALWAYS: [&str; 7] = ["Rent", "Subscriptions", "Utilities / Telecom", "Student loan", "Insurance", "Fees", "Gym"];
/// analytics FIXED_NEVER: never fixed however regular.
pub const FIXED_NEVER: [&str; 7] =
    ["Credit-card bill", "Cash / ATM", "Dining", "Groceries", "Transport", "Travel", "Office (reimbursed)"];
/// core drill NOT_SPENDING: category fragments that aren't spending, and the
/// kind each one becomes (lowercase substrings, first match wins).
pub const NOT_SPENDING: [(&str, &str); 6] = [
    ("transfer", "transfer"),
    ("investments", "investment"),
    ("credit-card bill", "card_bill"),
    ("fx exchange", "fx"),
    ("partner", "partner"),
    ("deposit", "deposit"),
];
/// analytics' `cat == "Salary"`: income that never counts as spending.
pub const INCOME: [&str; 1] = ["Salary"];
/// get_spending's `cat == "Investments"`: money invested.
pub const INVESTMENT: [&str; 1] = ["Investments"];
/// receivables' names.
pub const DEPOSIT: &str = "Deposit";
pub const PARTNER: &str = "Partner";
pub const REIMBURSED: &str = "Office (reimbursed)";
/// core budget NOT_SUGGESTED: a fragment that marks money coming back.
pub const REIMBURSED_FRAGMENT: &str = "reimbursed";
/// core recurring STRONG_CATEGORIES: recurring by nature (lowercase
/// substrings), so a steady cadence is enough.
pub const RECURRING_STRONG: [&str; 9] = ["subscription", "utilit", "telecom", "gym", "rent", "loan", "insurance", "salary", "partner"];
/// core recurring STRICT_CATEGORIES: everyday spending, only a near-perfect
/// pattern counts.
pub const RECURRING_STRICT: [&str; 2] = ["dining", "groceries"];
/// core travel PLACELESS: paid wherever the owner happens to be.
pub const PLACELESS: [&str; 9] = ["subscriptions", "utilities", "student loan", "rent", "gym", "office", "family", "interest", "fees"];
/// core recurring FORGOTTEN_EXEMPT: obligations, never "maybe forgotten".
pub const OBLIGATION: [&str; 5] = ["loan", "rent", "office", "insurance", "tax"];

/// The recurring hint a name had before the column (strong, neutral, strict).
pub fn default_recurring(name: &str) -> &'static str {
    let l = name.to_lowercase();
    if RECURRING_STRONG.iter().any(|x| l.contains(x)) {
        "strong"
    } else if RECURRING_STRICT.iter().any(|x| l.contains(x)) {
        "strict"
    } else {
        "neutral"
    }
}

/// One category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Category {
    pub name: String,
    pub icon: Option<String>,
    pub kind: String,
    pub fixed: String,
    pub reimbursable: bool,
    pub hidden: bool,
    pub sort: i64,
    /// strong, neutral or strict (how much evidence a recurring series needs)
    pub recurring: String,
    /// paid from anywhere: places nobody on a trip
    pub placeless: bool,
    /// an obligation: never a "maybe forgotten" charge
    pub obligation: bool,
    /// rules that file into it (list only)
    pub rules: i64,
    /// bank rows in it now (list only)
    pub rows: i64,
}

impl Default for Category {
    fn default() -> Self {
        Category {
            name: String::new(),
            icon: None,
            kind: "spending".into(),
            fixed: "auto".into(),
            reimbursable: false,
            hidden: false,
            sort: 0,
            recurring: "neutral".into(),
            placeless: false,
            obligation: false,
            rules: 0,
            rows: 0,
        }
    }
}

impl Category {
    /// Not spending (and not income either): money moving between the
    /// owner's own pockets, invested, or settled with someone.
    pub fn moves_money(&self) -> bool {
        !matches!(self.kind.as_str(), "spending" | "income")
    }
}

/// The flags a category had before the table existed: what the hard-coded
/// lists said about this name (and whether any of its rules was a transfer
/// rule). Seeds the table, and is the default for a new category.
pub fn default_flags(name: &str, rule_transfer: bool) -> Category {
    let l = name.to_lowercase();
    let kind = NOT_SPENDING
        .iter()
        .find(|(frag, _)| l.contains(frag))
        .map(|(_, k)| *k)
        .unwrap_or(if rule_transfer {
            "transfer"
        } else if INCOME.contains(&name) {
            "income"
        } else {
            "spending"
        });
    let fixed = if FIXED_ALWAYS.contains(&name) {
        "always"
    } else if FIXED_NEVER.contains(&name) {
        "never"
    } else {
        "auto"
    };
    Category {
        name: name.to_string(),
        kind: kind.into(),
        fixed: fixed.into(),
        reimbursable: l.contains(REIMBURSED_FRAGMENT),
        recurring: default_recurring(name).into(),
        placeless: PLACELESS.iter().any(|x| l.contains(x)),
        obligation: OBLIGATION.iter().any(|x| l.contains(x)),
        ..Default::default()
    }
}

// ---------- schema ----------

pub fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", [name], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
    conn.query_row(&format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"), [col], |r| r.get::<_, i64>(0))
        .unwrap_or(0)
        > 0
}

/// category_rules has the Phase 3 columns (origin, enabled, ...).
pub fn rules_migrated(conn: &Connection) -> bool {
    has_column(conn, "category_rules", "origin") && has_column(conn, "category_rules", "enabled")
}

/// Bring a finance.db up to Phase 3, idempotently: the rule columns (every
/// existing rule becomes a user rule), the categories table seeded from
/// what is in use, app_meta. Returns whether anything was created now.
pub fn ensure(conn: &Connection) -> Result<bool> {
    let had = has_table(conn, "categories") && rules_migrated(conn) && meta(conn, "schema").is_some();
    if had {
        ensure_flags(conn)?;
        return Ok(false);
    }
    let run = |c: &Connection| -> Result<()> {
        c.execute_batch(SCHEMA)?;
        for (col, decl) in RULE_COLUMNS {
            if !has_column(c, "category_rules", col) {
                c.execute_batch(&format!("ALTER TABLE category_rules ADD COLUMN {col} {decl}"))?;
            }
        }
        let rules: i64 = c.query_row("SELECT COUNT(*) FROM category_rules", [], |r| r.get(0))?;
        if rules > 0 {
            set_meta(c, RULES_SEEDED, "1")?;
        }
        set_meta(c, "schema", "3")?;
        add_flag_columns(c)?;
        sync_categories(c)?;
        Ok(())
    };
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        run(&tx)?;
        tx.commit()?;
    } else {
        run(conn)?;
    }
    Ok(true)
}

/// The table has the Phase 4 flag columns.
pub fn has_flag_columns(conn: &Connection) -> bool {
    CATEGORY_COLUMNS.iter().all(|(c, _)| has_column(conn, "categories", c))
}

/// Add the Phase 4 flag columns (recurring, placeless, obligation), each
/// row backfilled with what the hard-coded lists said about its name. A
/// no-op once they are there; data_rev doesn't move (nothing a server view
/// computes changes).
fn add_flag_columns(conn: &Connection) -> Result<()> {
    let mut added = false;
    for (col, decl) in CATEGORY_COLUMNS {
        if !has_column(conn, "categories", col) {
            conn.execute_batch(&format!("ALTER TABLE categories ADD COLUMN {col} {decl}"))?;
            added = true;
        }
    }
    if added {
        let names: Vec<String> = {
            let mut st = conn.prepare("SELECT name FROM categories")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            v
        };
        let mut st = conn.prepare("UPDATE categories SET recurring = ?2, placeless = ?3, obligation = ?4 WHERE name = ?1")?;
        for n in names {
            let d = default_flags(&n, false);
            st.execute(params![n, d.recurring, d.placeless as i64, d.obligation as i64])?;
        }
    }
    Ok(())
}

/// Phase 4 on a Phase 3 table, in its own transaction (or the caller's).
fn ensure_flags(conn: &Connection) -> Result<()> {
    if !has_table(conn, "categories") || has_flag_columns(conn) {
        return Ok(());
    }
    if conn.is_autocommit() {
        let tx = conn.unchecked_transaction()?;
        add_flag_columns(&tx)?;
        tx.commit()?;
    } else {
        add_flag_columns(conn)?;
    }
    Ok(())
}

/// Every category name in use (rules, overrides, bank rows, and "Other")
/// that the table lacks gets a row with its [`default_flags`]. No-op
/// without the table. Returns the rows added.
pub fn sync_categories(conn: &Connection) -> Result<usize> {
    if !has_table(conn, "categories") {
        return Ok(0);
    }
    let known: HashSet<String> = {
        let mut st = conn.prepare("SELECT name FROM categories")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    // name -> any rule of it is a transfer rule; first-seen order
    let mut names: Vec<(String, bool)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut add = |n: String, tr: bool| match seen.get(&n) {
        Some(&i) => names[i].1 |= tr,
        None => {
            seen.insert(n.clone(), names.len());
            names.push((n, tr));
        }
    };
    {
        let mut st = conn.prepare("SELECT category, MAX(is_transfer) FROM category_rules GROUP BY category ORDER BY MIN(priority), MIN(id)")?;
        for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (n, t) = r?;
            add(n, t != 0);
        }
        let mut st = conn.prepare("SELECT DISTINCT category FROM txn_overrides")?;
        for r in st.query_map([], |r| r.get::<_, String>(0))? {
            add(r?, false);
        }
        let mut st = conn.prepare("SELECT DISTINCT category FROM bank_transactions WHERE category IS NOT NULL")?;
        for r in st.query_map([], |r| r.get::<_, String>(0))? {
            add(r?, false);
        }
        add(OTHER.to_string(), false);
    }
    let missing: Vec<Category> =
        names.into_iter().filter(|(n, _)| !known.contains(n) && !n.trim().is_empty()).map(|(n, t)| default_flags(&n, t)).collect();
    if missing.is_empty() {
        return Ok(0);
    }
    // new rows sort after what is there, grouped by kind then by name
    let base: i64 = conn.query_row("SELECT COALESCE(MAX(sort), 0) FROM categories", [], |r| r.get(0))?;
    let mut order = missing;
    order.sort_by(|a, b| kind_rank(&a.kind).cmp(&kind_rank(&b.kind)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let now = crate::db::now_ms();
    let mut st = conn.prepare(
        "INSERT OR IGNORE INTO categories (name, icon, kind, fixed, reimbursable, hidden, sort, created_at) VALUES (?1, NULL, ?2, ?3, ?4, 0, ?5, ?6)",
    )?;
    let flags = has_flag_columns(conn);
    let mut n = 0;
    for (i, c) in order.iter().enumerate() {
        let added = st.execute(params![c.name, c.kind, c.fixed, c.reimbursable as i64, base + 10 * (i as i64 + 1), now])?;
        if added > 0 && flags {
            conn.execute(
                "UPDATE categories SET recurring = ?2, placeless = ?3, obligation = ?4 WHERE name = ?1",
                params![c.name, c.recurring, c.placeless as i64, c.obligation as i64],
            )?;
        }
        n += added;
    }
    Ok(n)
}

fn kind_rank(kind: &str) -> usize {
    KINDS.iter().position(|k| *k == kind).unwrap_or(KINDS.len())
}

// ---------- app_meta: data_rev, the seed marker ----------

/// app_meta key: the default rules went in once (an emptied rule table is
/// then left empty, not re-seeded).
pub const RULES_SEEDED: &str = "rules_seeded";

pub fn meta(conn: &Connection, key: &str) -> Option<String> {
    if !has_table(conn, "app_meta") {
        return None;
    }
    conn.query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |r| r.get::<_, String>(0)).optional().ok().flatten()
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    conn.execute("INSERT INTO app_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", [key, value])?;
    Ok(())
}

/// Forget that the default rules went in, so `categorize::ensure_rules`
/// seeds an emptied table again (`categorize --reset`).
pub fn clear_rules_seeded(conn: &Connection) -> Result<()> {
    if has_table(conn, "app_meta") {
        conn.execute("DELETE FROM app_meta WHERE key = ?1", [RULES_SEEDED])?;
    }
    Ok(())
}

/// The data revision: 0 before the first write through these APIs.
pub fn data_rev(conn: &Connection) -> i64 {
    meta(conn, "data_rev").and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Move data_rev on (inside the caller's transaction). Returns the new one.
pub fn bump(conn: &Connection) -> Result<i64> {
    let next = data_rev(conn) + 1;
    set_meta(conn, "data_rev", &next.to_string())?;
    Ok(next)
}

// ---------- reading ----------

/// Every category, in the manager's order (sort, name), with how many rules
/// and bank rows each has. Empty without the table.
pub fn list(conn: &Connection) -> Result<Vec<Category>> {
    if !has_table(conn, "categories") {
        return Ok(Vec::new());
    }
    // a Phase 3 table (no flag columns yet) reads its flags from the names
    let flags = if has_flag_columns(conn) { "c.recurring, c.placeless, c.obligation" } else { "NULL, NULL, NULL" };
    let mut st = conn.prepare(&format!(
        "SELECT c.name, c.icon, c.kind, c.fixed, c.reimbursable, c.hidden, c.sort,
                (SELECT COUNT(*) FROM category_rules r WHERE r.category = c.name),
                (SELECT COUNT(*) FROM bank_transactions b WHERE b.category = c.name),
                {flags}
         FROM categories c ORDER BY c.sort, c.name"
    ))?;
    let v = st
        .query_map([], |r| {
            let name: String = r.get(0)?;
            let d = default_flags(&name, false);
            Ok(Category {
                icon: r.get(1)?,
                kind: r.get(2)?,
                fixed: r.get(3)?,
                reimbursable: r.get::<_, i64>(4)? != 0,
                hidden: r.get::<_, i64>(5)? != 0,
                sort: r.get(6)?,
                rules: r.get(7)?,
                rows: r.get(8)?,
                recurring: r.get::<_, Option<String>>(9)?.unwrap_or(d.recurring),
                placeless: r.get::<_, Option<i64>>(10)?.map(|v| v != 0).unwrap_or(d.placeless),
                obligation: r.get::<_, Option<i64>>(11)?.map(|v| v != 0).unwrap_or(d.obligation),
                name,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

pub fn get(conn: &Connection, name: &str) -> Result<Option<Category>> {
    Ok(list(conn)?.into_iter().find(|c| c.name == name))
}

/// What the views need to know about categories: the table's flags where
/// it has the category, else the hard-coded lists (so a db without the
/// table, or a category the table doesn't know yet, reads as before).
#[derive(Debug, Clone, Default)]
pub struct Flags {
    table: HashMap<String, Category>,
    /// categories with an is_transfer rule (the pre-table transfer test)
    rule_transfers: HashSet<String>,
}

impl Flags {
    /// The flags in `conn` (the rule transfers seed an empty rule table
    /// first, as categorize::transfer_categories always did).
    pub fn load(conn: &Connection) -> Self {
        let rule_transfers = crate::categorize::rule_transfer_categories(conn).unwrap_or_default();
        let table = list(conn).unwrap_or_default().into_iter().map(|c| (c.name.clone(), c)).collect();
        Flags { table, rule_transfers }
    }

    /// Flags from a list (tests, and a client's copy).
    pub fn from_list(list: &[Category], rule_transfers: HashSet<String>) -> Self {
        Flags { table: list.iter().map(|c| (c.name.clone(), c.clone())).collect(), rule_transfers }
    }

    pub fn has_table(&self) -> bool {
        !self.table.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Category> {
        self.table.get(name)
    }

    /// Money moving between own accounts (analytics' transfer categories):
    /// excluded from income and spending.
    pub fn is_transfer(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.moves_money(),
            None => self.rule_transfers.contains(cat),
        }
    }

    /// Income that is never spending, whatever its sign (Salary).
    pub fn is_income(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.kind == "income",
            None => INCOME.contains(&cat),
        }
    }

    pub fn is_investment(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.kind == "investment",
            None => INVESTMENT.contains(&cat),
        }
    }

    pub fn is_deposit(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.kind == "deposit",
            None => cat == DEPOSIT,
        }
    }

    pub fn is_partner(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.kind == "partner",
            None => cat == PARTNER,
        }
    }

    pub fn is_reimbursable(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.reimbursable,
            None => cat == REIMBURSED,
        }
    }

    pub fn fixed_always(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.fixed == "always",
            None => FIXED_ALWAYS.contains(&cat),
        }
    }

    pub fn fixed_never(&self, cat: &str) -> bool {
        match self.table.get(cat) {
            Some(c) => c.fixed == "never",
            None => FIXED_NEVER.contains(&cat),
        }
    }

    /// The name the reimbursements block reports (the first reimbursable
    /// category, else the old constant).
    pub fn reimbursed_name(&self) -> String {
        let mut v: Vec<&Category> = self.table.values().filter(|c| c.reimbursable).collect();
        v.sort_by(|a, b| (a.sort, &a.name).cmp(&(b.sort, &b.name)));
        v.first().map(|c| c.name.clone()).unwrap_or_else(|| REIMBURSED.to_string())
    }

    /// The transfer categories as a set (every name the table or the rules know).
    pub fn transfer_set(&self) -> HashSet<String> {
        let mut out: HashSet<String> = self.table.values().filter(|c| c.moves_money()).map(|c| c.name.clone()).collect();
        out.extend(self.rule_transfers.iter().filter(|n| !self.table.contains_key(*n)).cloned());
        out
    }
}

// ---------- writing ----------

/// What a client may set on a category (POST: name required; PATCH: any).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Patch {
    pub name: Option<String>,
    /// Some(None) clears it (JSON null); absent leaves it
    #[serde(default, deserialize_with = "some_or_null")]
    pub icon: Option<Option<String>>,
    pub kind: Option<String>,
    pub fixed: Option<String>,
    pub reimbursable: Option<bool>,
    pub hidden: Option<bool>,
    pub sort: Option<i64>,
    /// strong, neutral or strict
    pub recurring: Option<String>,
    pub placeless: Option<bool>,
    pub obligation: Option<bool>,
}

fn some_or_null<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

/// A user-facing name: trimmed, 1..=60 characters, no control characters.
pub fn clean_name(name: &str) -> Result<String> {
    let n = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() {
        bail!("a category needs a name");
    }
    if n.chars().count() > 60 {
        bail!("a category name is 60 characters at most");
    }
    if n.chars().any(char::is_control) {
        bail!("that name has a control character");
    }
    Ok(n)
}

fn check_patch(p: &Patch) -> Result<()> {
    if let Some(k) = &p.kind {
        if !KINDS.contains(&k.as_str()) {
            bail!("kind must be one of {}", KINDS.join(", "));
        }
    }
    if let Some(f) = &p.fixed {
        if !FIXED.contains(&f.as_str()) {
            bail!("fixed must be auto, always or never");
        }
    }
    if let Some(r) = &p.recurring {
        if !RECURRING.contains(&r.as_str()) {
            bail!("recurring must be strong, neutral or strict");
        }
    }
    if let Some(Some(i)) = &p.icon {
        if i.len() > 40 || !i.chars().all(|c| c.is_ascii_alphanumeric()) {
            bail!("an icon is a Lucide icon name like Utensils");
        }
    }
    Ok(())
}

fn require_table(conn: &Connection) -> Result<()> {
    if !has_table(conn, "categories") {
        ensure(conn)?;
    }
    Ok(())
}

/// A new category: the name's default flags, then what the patch says.
pub fn create(conn: &Connection, p: &Patch) -> Result<Category> {
    check_patch(p)?;
    require_table(conn)?;
    let name = clean_name(p.name.as_deref().unwrap_or(""))?;
    if exists(conn, &name)? {
        bail!("there is already a category called {name}");
    }
    let mut c = default_flags(&name, false);
    apply(&mut c, p);
    if p.sort.is_none() {
        c.sort = conn.query_row("SELECT COALESCE(MAX(sort), 0) + 10 FROM categories", [], |r| r.get(0))?;
    }
    conn.execute(
        "INSERT INTO categories (name, icon, kind, fixed, reimbursable, hidden, sort, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![c.name, c.icon, c.kind, c.fixed, c.reimbursable as i64, c.hidden as i64, c.sort, crate::db::now_ms()],
    )?;
    write_flags(conn, &c)?;
    Ok(c)
}

fn apply(c: &mut Category, p: &Patch) {
    if let Some(i) = &p.icon {
        c.icon = i.clone().filter(|s| !s.is_empty());
    }
    if let Some(k) = &p.kind {
        c.kind = k.clone();
    }
    if let Some(f) = &p.fixed {
        c.fixed = f.clone();
    }
    if let Some(r) = p.reimbursable {
        c.reimbursable = r;
    }
    if let Some(h) = p.hidden {
        c.hidden = h;
    }
    if let Some(s) = p.sort {
        c.sort = s;
    }
    if let Some(r) = &p.recurring {
        c.recurring = r.clone();
    }
    if let Some(v) = p.placeless {
        c.placeless = v;
    }
    if let Some(v) = p.obligation {
        c.obligation = v;
    }
}

/// The Phase 4 flags of `c` (the columns are added first when missing).
fn write_flags(conn: &Connection, c: &Category) -> Result<()> {
    ensure_flags(conn)?;
    conn.execute(
        "UPDATE categories SET recurring = ?2, placeless = ?3, obligation = ?4 WHERE name = ?1",
        params![c.name, c.recurring, c.placeless as i64, c.obligation as i64],
    )?;
    Ok(())
}

pub fn exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM categories WHERE name = ?", [name], |_| Ok(())).optional()?.is_some())
}

/// Change a category's flags (and its name: a rename). A kind change keeps
/// the rules' is_transfer in step (old replicas read transfers from the
/// rules). Returns the category as it is now.
pub fn update(conn: &Connection, name: &str, p: &Patch) -> Result<Category> {
    check_patch(p)?;
    require_table(conn)?;
    let Some(mut c) = get(conn, name)? else { bail!("no category called {name}") };
    let mut current = name.to_string();
    if let Some(new) = p.name.as_deref() {
        let new = clean_name(new)?;
        if new != name {
            rename(conn, name, &new)?;
            current = new.clone();
            c.name = new;
        }
    }
    apply(&mut c, p);
    conn.execute(
        "UPDATE categories SET icon = ?2, kind = ?3, fixed = ?4, reimbursable = ?5, hidden = ?6, sort = ?7 WHERE name = ?1",
        params![current, c.icon, c.kind, c.fixed, c.reimbursable as i64, c.hidden as i64, c.sort],
    )?;
    write_flags(conn, &c)?;
    if p.kind.is_some() {
        conn.execute("UPDATE category_rules SET is_transfer = ?2 WHERE category = ?1", params![current, c.moves_money() as i64])?;
    }
    Ok(c)
}

/// The manager's order: `names` first, in that order (sort 10, 20, ...),
/// then whatever was left out, keeping its relative order.
pub fn reorder(conn: &Connection, names: &[String]) -> Result<()> {
    require_table(conn)?;
    let all: Vec<String> = list(conn)?.into_iter().map(|c| c.name).collect();
    let mut order: Vec<&String> = names.iter().filter(|n| all.contains(n)).collect();
    order.dedup();
    for n in &all {
        if !order.contains(&n) {
            order.push(n);
        }
    }
    let mut st = conn.prepare("UPDATE categories SET sort = ?2 WHERE name = ?1")?;
    for (i, n) in order.iter().enumerate() {
        st.execute(params![n, 10 * (i as i64 + 1)])?;
    }
    Ok(())
}

/// Rename `old` to `new` everywhere a category name lives: the table, the
/// rules, the overrides, the budgets. The rows follow on recategorize.
pub fn rename(conn: &Connection, old: &str, new: &str) -> Result<()> {
    let new = clean_name(new)?;
    if old == new {
        return Ok(());
    }
    require_table(conn)?;
    if !exists(conn, old)? {
        bail!("no category called {old}");
    }
    if exists(conn, &new)? {
        bail!("there is already a category called {new}: merge into it instead");
    }
    conn.execute("UPDATE categories SET name = ?2 WHERE name = ?1", params![old, new])?;
    conn.execute("UPDATE category_rules SET category = ?2 WHERE category = ?1", params![old, new])?;
    conn.execute("UPDATE txn_overrides SET category = ?2 WHERE category = ?1", params![old, new])?;
    rewrite_budgets(conn, old, Some(&new))?;
    Ok(())
}

/// Fold `from` into `into` (which must exist): rules and overrides move,
/// budgets add up, `from` leaves the table.
pub fn merge(conn: &Connection, from: &str, into: &str) -> Result<()> {
    require_table(conn)?;
    if from == into {
        bail!("a category can't merge into itself");
    }
    if !exists(conn, from)? {
        bail!("no category called {from}");
    }
    if !exists(conn, into)? {
        bail!("no category called {into}");
    }
    let tr = get(conn, into)?.map(|c| c.moves_money()).unwrap_or(false) as i64;
    conn.execute("UPDATE category_rules SET category = ?2, is_transfer = ?3 WHERE category = ?1", params![from, into, tr])?;
    conn.execute("UPDATE txn_overrides SET category = ?2 WHERE category = ?1", params![from, into])?;
    conn.execute("DELETE FROM categories WHERE name = ?1", [from])?;
    rewrite_budgets(conn, from, Some(into))?;
    Ok(())
}

/// Delete `name`. Its rows go `into` another category (a merge), or, into
/// "Other", back to uncategorized: its rules and overrides are deleted.
pub fn delete(conn: &Connection, name: &str, into: &str) -> Result<()> {
    require_table(conn)?;
    if name == OTHER {
        bail!("Other is where uncategorized rows go: it can't be deleted");
    }
    if into != OTHER {
        return merge(conn, name, into);
    }
    if !exists(conn, name)? {
        bail!("no category called {name}");
    }
    conn.execute("DELETE FROM category_rules WHERE category = ?1", [name])?;
    conn.execute("DELETE FROM txn_overrides WHERE category = ?1", [name])?;
    conn.execute("DELETE FROM categories WHERE name = ?1", [name])?;
    rewrite_budgets(conn, name, None)?;
    Ok(())
}

/// user_settings.budgets {"monthly": {category: amount}}: `from` renamed to
/// `into` (amounts add up when both have one), or dropped (None).
pub fn rewrite_budgets(conn: &Connection, from: &str, into: Option<&str>) -> Result<bool> {
    let Some(mut v) = crate::profile::setting(conn, "budgets")? else { return Ok(false) };
    let Some(m) = v.get_mut("monthly").and_then(Value::as_object_mut) else { return Ok(false) };
    let Some(amount) = m.remove(from) else { return Ok(false) };
    if let Some(to) = into {
        let add = amount.as_f64().unwrap_or(0.0);
        let sum = m.get(to).and_then(Value::as_f64).unwrap_or(0.0) + add;
        m.insert(to.to_string(), json!(sum));
    }
    crate::profile::put_setting(conn, "budgets", &v)?;
    Ok(true)
}

/// The rows a recategorize moved: what a write replies so a client can
/// patch its copy at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Changed {
    pub source: String,
    pub raw_hash: String,
    pub category: String,
    pub share: Option<f64>,
}

/// Seeded categories for a fresh table (tests): the default rules' names.
pub fn seed_names() -> Vec<String> {
    let mut seen = BTreeMap::new();
    for (i, r) in crate::categorize::DEFAULT_RULES.iter().enumerate() {
        seen.entry(r.2.to_string()).or_insert(i);
    }
    let mut v: Vec<(usize, String)> = seen.into_iter().map(|(n, i)| (i, n)).collect();
    v.sort();
    v.into_iter().map(|(_, n)| n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = crate::db::memory().unwrap();
        crate::categorize::ensure_rules(&c).unwrap();
        c
    }

    #[test]
    fn a_reset_reseeds_an_emptied_table() {
        let c = db();
        ensure(&c).unwrap();
        c.execute("DELETE FROM category_rules", []).unwrap();
        crate::categorize::ensure_rules(&c).unwrap();
        let n = |c: &Connection| c.query_row("SELECT COUNT(*) FROM category_rules", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n(&c), 0, "an emptied table stays empty");
        clear_rules_seeded(&c).unwrap();
        crate::categorize::ensure_rules(&c).unwrap();
        assert!(n(&c) > 0, "the defaults went back in");
        assert!(meta(&c, RULES_SEEDED).is_some());
    }

    #[test]
    fn the_seed_reproduces_the_hard_coded_lists() {
        let c = db();
        let before = Flags::load(&c);
        assert!(!before.has_table());
        ensure(&c).unwrap();
        let after = Flags::load(&c);
        assert!(after.has_table());
        let mut names: Vec<String> = seed_names();
        names.extend(["Other", "Insurance", "Taxes", "Something new"].map(String::from));
        for n in &names {
            let n = n.as_str();
            assert_eq!(before.is_transfer(n), after.is_transfer(n), "transfer {n}");
            assert_eq!(before.is_income(n), after.is_income(n), "income {n}");
            assert_eq!(before.is_investment(n), after.is_investment(n), "investment {n}");
            assert_eq!(before.is_deposit(n), after.is_deposit(n), "deposit {n}");
            assert_eq!(before.is_partner(n), after.is_partner(n), "partner {n}");
            assert_eq!(before.is_reimbursable(n), after.is_reimbursable(n), "reimbursable {n}");
            assert_eq!(before.fixed_always(n), after.fixed_always(n), "fixed {n}");
            assert_eq!(before.fixed_never(n), after.fixed_never(n), "never {n}");
        }
        assert_eq!(before.transfer_set(), after.transfer_set());
        assert_eq!(after.reimbursed_name(), REIMBURSED);
        // every rule became a user rule, enabled
        let (n, users): (i64, i64) = c
            .query_row("SELECT COUNT(*), SUM(origin = 'user' AND enabled = 1) FROM category_rules", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(n, users);
        assert!(!ensure(&c).unwrap(), "idempotent");
        let k = |n: &str| get(&c, n).unwrap().unwrap().kind;
        assert_eq!((k("Salary"), k("Internal transfer"), k("Credit-card bill"), k("Dining")),
                   ("income".into(), "transfer".into(), "card_bill".into(), "spending".into()));
    }

    #[test]
    fn a_phase_3_table_gains_the_flags_from_the_names() {
        let c = db();
        ensure(&c).unwrap();
        // a Phase 3 table: no flag columns
        for (col, _) in CATEGORY_COLUMNS {
            c.execute_batch(&format!("ALTER TABLE categories DROP COLUMN {col}")).unwrap();
        }
        assert!(!has_flag_columns(&c));
        let rev = data_rev(&c);
        let rent = get(&c, "Rent").unwrap().unwrap();
        assert_eq!((rent.recurring.as_str(), rent.placeless, rent.obligation), ("strong", true, true), "read from the name meanwhile");
        assert!(!ensure(&c).unwrap());
        assert!(has_flag_columns(&c));
        let f = |n: &str| {
            let x = get(&c, n).unwrap().unwrap();
            (x.recurring, x.placeless, x.obligation)
        };
        assert_eq!(f("Rent"), ("strong".into(), true, true));
        assert_eq!(f("Dining"), ("strict".into(), false, false));
        assert_eq!(f("Subscriptions"), ("strong".into(), true, false));
        assert_eq!(f("Shopping"), ("neutral".into(), false, false));
        assert_eq!(f("Taxes"), ("neutral".into(), false, true));
        assert_eq!(data_rev(&c), rev, "a migration isn't a write");
        update(&c, "Shopping", &Patch { recurring: Some("strict".into()), placeless: Some(true), ..Default::default() }).unwrap();
        assert_eq!(f("Shopping"), ("strict".into(), true, false));
        assert!(update(&c, "Shopping", &Patch { recurring: Some("sometimes".into()), ..Default::default() }).is_err());
    }

    #[test]
    fn rename_merge_and_delete_rewrite_everything() {
        let c = db();
        ensure(&c).unwrap();
        crate::profile::put_setting(&c, "budgets", &json!({"monthly": {"Dining": 2000.0, "Groceries": 1500.0, "Transport": 300.0}})).unwrap();
        c.execute("INSERT INTO txn_overrides (source, raw_hash, category, share, note, created_at) VALUES ('hsbc', 'h1', 'Dining', NULL, NULL, 0)", []).unwrap();
        let rules = |cat: &str| -> i64 { c.query_row("SELECT COUNT(*) FROM category_rules WHERE category = ?", [cat], |r| r.get(0)).unwrap() };
        let dining = rules("Dining");
        assert!(dining > 10);

        rename(&c, "Dining", "Eating out").unwrap();
        assert_eq!((rules("Dining"), rules("Eating out")), (0, dining));
        let ov: String = c.query_row("SELECT category FROM txn_overrides", [], |r| r.get(0)).unwrap();
        assert_eq!(ov, "Eating out");
        let b = crate::profile::setting(&c, "budgets").unwrap().unwrap();
        assert_eq!(b["monthly"]["Eating out"], json!(2000.0));
        assert!(b["monthly"].get("Dining").is_none());
        assert!(rename(&c, "Eating out", "Groceries").is_err(), "a rename onto a name is a merge");

        let groceries = rules("Groceries");
        merge(&c, "Eating out", "Groceries").unwrap();
        assert_eq!(rules("Groceries"), groceries + dining);
        assert!(get(&c, "Eating out").unwrap().is_none());
        let b = crate::profile::setting(&c, "budgets").unwrap().unwrap();
        assert_eq!(b["monthly"]["Groceries"], json!(3500.0), "budgets add up");

        let transport = rules("Transport");
        assert!(transport > 0);
        delete(&c, "Transport", OTHER).unwrap();
        assert_eq!(rules("Transport"), 0);
        assert!(get(&c, "Transport").unwrap().is_none());
        let b = crate::profile::setting(&c, "budgets").unwrap().unwrap();
        assert!(b["monthly"].get("Transport").is_none());
        assert!(delete(&c, OTHER, "Dining").is_err());
    }

    #[test]
    fn a_kind_change_keeps_the_rules_transfer_flag() {
        let c = db();
        // the owner's own rule for a partner (the seed names no one)
        c.execute("INSERT INTO category_rules (priority, pattern, category, is_transfer) VALUES (28, 'sam partnerly', 'Partner', 1)", []).unwrap();
        ensure(&c).unwrap();
        assert_eq!(get(&c, "Partner").unwrap().unwrap().kind, "partner");
        update(&c, "Partner", &Patch { kind: Some("spending".into()), ..Default::default() }).unwrap();
        let tr: i64 = c.query_row("SELECT MAX(is_transfer) FROM category_rules WHERE category = 'Partner'", [], |r| r.get(0)).unwrap();
        assert_eq!(tr, 0);
        assert!(!Flags::load(&c).is_transfer("Partner"));
        assert!(update(&c, "Partner", &Patch { kind: Some("nonsense".into()), ..Default::default() }).is_err());
        let made = create(&c, &Patch { name: Some("  Pets  ".into()), icon: Some(Some("PawPrint".into())), ..Default::default() }).unwrap();
        assert_eq!((made.name.as_str(), made.icon.as_deref(), made.kind.as_str()), ("Pets", Some("PawPrint"), "spending"));
        assert!(create(&c, &Patch { name: Some("Pets".into()), ..Default::default() }).is_err());
        reorder(&c, &["Pets".to_string()]).unwrap();
        assert_eq!(list(&c).unwrap()[0].name, "Pets");
        assert_eq!(data_rev(&c), 0);
        assert_eq!(bump(&c).unwrap(), 1);
        assert_eq!(bump(&c).unwrap(), 2);
    }
}
