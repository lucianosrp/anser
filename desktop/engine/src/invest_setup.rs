//! What the investment views know about the owner's accounts and the
//! instruments in them, as data (Phase 4) instead of constants in the views:
//!
//! * `investment_accounts`: one row per account money is sent to. `matches`
//!   (a JSON array of lowercase fragments of the bank row's description)
//!   files an Investments bank row to it, first account by `sort` wins
//!   (none: "Other"). `valued_by` says how it is marked: `ibkr` (the IBKR
//!   Flex data: broker positions and NAV), `proxy` (an index `proxy`, USD
//!   closes, with `proxy_label` naming it; and, when `fund_code` has rows in
//!   fund_subscriptions, real units bridged by the proxy) or `cost`. The
//!   platform's costs: `fund_ocf_pct` (the fund's yearly charge),
//!   `platform_flat_month` up to `platform_cliff`, `platform_pct_above` of
//!   the whole balance above it, and `fee_match` (a description fragment of
//!   the platform's fee rows, to sum what was paid).
//! * `instruments`: per symbol, `ter_pct` (the yearly fund fee, % of value)
//!   and `kind` (`fund`, or `stock`: no holding fee). A symbol missing here
//!   shows as "fee unknown", never as 0.
//!
//! Both tables are Rust-only, created at startup (never by db::connect,
//! like the categories table), editable through the admin API (Settings →
//! Investments). Every tree starts empty ([`ensure`], from auth::bootstrap),
//! one that predates the tables too: the investment setup fills them. The
//! readers never create anything: a missing table reads as empty.
//!
//! The benchmark the Investments page compares against is the profile's
//! (`profile.benchmark`, default [`DEFAULT_BENCHMARK`]).

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The S&P 500 index (Yahoo symbol): the benchmark when the profile has none.
pub const DEFAULT_BENCHMARK: &str = "^GSPC";
/// What an Investments row no account matches files to.
pub const OTHER: &str = "Other";
pub const VALUED_BY: [&str; 3] = ["ibkr", "proxy", "cost"];
pub const INSTRUMENT_KINDS: [&str; 2] = ["fund", "stock"];

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS investment_accounts (
  name                TEXT PRIMARY KEY,
  matches             TEXT NOT NULL DEFAULT '[]',   -- JSON array of description fragments
  sort                INTEGER NOT NULL DEFAULT 0,
  valued_by           TEXT NOT NULL DEFAULT 'cost', -- ibkr, proxy, cost
  proxy               TEXT,                         -- an index proxy symbol (USD closes)
  proxy_label         TEXT,                         -- what it tracks ('MSCI World')
  fund_code           TEXT,                         -- fund_subscriptions.fund
  fund_ocf_pct        REAL,
  platform_flat_month REAL,
  platform_cliff      REAL,
  platform_pct_above  REAL,
  fee_match           TEXT,
  created_at          INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS instruments (
  symbol     TEXT PRIMARY KEY,
  name       TEXT,
  kind       TEXT NOT NULL DEFAULT 'fund',          -- fund, stock
  ter_pct    REAL,                                  -- NULL: unknown
  created_at INTEGER NOT NULL DEFAULT 0
);
";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub name: String,
    pub matches: Vec<String>,
    pub sort: i64,
    pub valued_by: String,
    pub proxy: Option<String>,
    pub proxy_label: Option<String>,
    pub fund_code: Option<String>,
    pub fund_ocf_pct: Option<f64>,
    pub platform_flat_month: Option<f64>,
    pub platform_cliff: Option<f64>,
    pub platform_pct_above: Option<f64>,
    pub fee_match: Option<String>,
}

impl Default for Account {
    fn default() -> Self {
        Account {
            name: String::new(),
            matches: Vec::new(),
            sort: 0,
            valued_by: "cost".into(),
            proxy: None,
            proxy_label: None,
            fund_code: None,
            fund_ocf_pct: None,
            platform_flat_month: None,
            platform_cliff: None,
            platform_pct_above: None,
            fee_match: None,
        }
    }
}

impl Account {
    /// Has a platform fee schedule (the Costs section's platform block).
    pub fn has_platform(&self) -> bool {
        self.platform_flat_month.is_some() || self.platform_pct_above.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Instrument {
    pub symbol: String,
    pub name: Option<String>,
    pub kind: String,
    pub ter_pct: Option<f64>,
}

impl Default for Instrument {
    fn default() -> Self {
        Instrument { symbol: String::new(), name: None, kind: "fund".into(), ter_pct: None }
    }
}

/// The accounts and instruments, as the views read them.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Setup {
    pub accounts: Vec<Account>,
    pub instruments: Vec<Instrument>,
}

impl Setup {
    /// The tables' content; nothing when they are missing. A pure read.
    pub fn load(conn: &Connection) -> Setup {
        if !has_table(conn, "investment_accounts") || !has_table(conn, "instruments") {
            return Setup::default();
        }
        Setup { accounts: list_accounts(conn).unwrap_or_default(), instruments: list_instruments(conn).unwrap_or_default() }
    }

    /// The account an Investments bank row goes to (assembly.investment_holding).
    pub fn holding_of(&self, description: &str) -> &str {
        let d = description.to_lowercase();
        self.accounts
            .iter()
            .find(|a| a.matches.iter().any(|m| !m.is_empty() && d.contains(m.as_str())))
            .map(|a| a.name.as_str())
            .unwrap_or(OTHER)
    }

    pub fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }

    /// HOLDING_PROXY: the index proxy a holding is marked to.
    pub fn proxy(&self, holding: &str) -> Option<&str> {
        self.account(holding).filter(|a| a.valued_by == "proxy").and_then(|a| a.proxy.as_deref())
    }

    /// HOLDING_FUND: the fund code whose real executions a holding has.
    pub fn fund(&self, holding: &str) -> Option<&str> {
        self.account(holding).filter(|a| a.valued_by == "proxy").and_then(|a| a.fund_code.as_deref())
    }

    /// What a holding's proxy tracks ("MSCI World"), else its symbol.
    pub fn proxy_label(&self, holding: &str) -> String {
        let a = self.account(holding);
        a.and_then(|a| a.proxy_label.clone()).or_else(|| a.and_then(|a| a.proxy.clone())).unwrap_or_default()
    }

    /// The account marked by the IBKR data (broker truth), if any.
    pub fn ibkr(&self) -> Option<&Account> {
        self.accounts.iter().find(|a| a.valued_by == "ibkr")
    }

    /// Its name, else "IBKR" (the broker data may exist with no account).
    pub fn ibkr_name(&self) -> String {
        self.ibkr().map(|a| a.name.clone()).unwrap_or_else(|| "IBKR".into())
    }

    pub fn is_ibkr(&self, holding: &str) -> bool {
        match self.ibkr() {
            Some(a) => a.name == holding,
            None => holding == "IBKR",
        }
    }

    /// The first proxy-marked account: the fund page, its proxy quote.
    pub fn fund_account(&self) -> Option<&Account> {
        self.accounts.iter().find(|a| a.valued_by == "proxy" && a.proxy.is_some())
    }

    /// The first account with a platform fee schedule (the Costs section).
    pub fn platform(&self) -> Option<&Account> {
        self.accounts.iter().find(|a| a.has_platform())
    }

    /// The display order of the combined Investments page: the marked
    /// accounts (broker, proxy) in their order, then everything else, the
    /// crypto class last.
    pub fn order(&self, holding: &str, crypto: &str) -> i64 {
        if holding == crypto {
            return 1_000_000;
        }
        let marked: Vec<&Account> = self.accounts.iter().filter(|a| a.valued_by != "cost").collect();
        match marked.iter().position(|a| a.name == holding) {
            Some(i) => i as i64,
            None => marked.len().max(5) as i64,
        }
    }

    /// A symbol's yearly holding fee in %: a stock 0, a fund its TER, None unknown.
    pub fn ter(&self, symbol: &str) -> Option<f64> {
        let i = self.instruments.iter().find(|i| i.symbol == symbol)?;
        if i.kind == "stock" {
            Some(0.0)
        } else {
            i.ter_pct
        }
    }
}

// ---------- schema ----------

pub fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", [name], |_| Ok(())).optional().ok().flatten().is_some()
}

/// Create the tables if missing, empty (a new tree, a household member's,
/// the demo's, an archive's target, or an older tree that predates them; the
/// investment setup fills them). A table that exists is left as it is.
/// Returns whether they were made now.
pub fn ensure(conn: &Connection) -> Result<bool> {
    if has_table(conn, "investment_accounts") && has_table(conn, "instruments") {
        return Ok(false);
    }
    conn.execute_batch(SCHEMA)?;
    Ok(true)
}

/// Fill the tables from `{"accounts": [...], "instruments": [...]}`, making
/// them if missing; a row that exists is replaced. For tests that pin an
/// owner's output against a setup kept outside the repo.
pub fn import_json(conn: &Connection, json: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct Doc {
        accounts: Vec<Account>,
        instruments: Vec<Instrument>,
    }
    let d: Doc = serde_json::from_str(json)?;
    ensure(conn)?;
    for a in &d.accounts {
        conn.execute("DELETE FROM investment_accounts WHERE name = ?", [&a.name])?;
        insert_account(conn, a)?;
    }
    for i in &d.instruments {
        upsert_instrument(conn, i)?;
    }
    Ok(())
}

// ---------- reading ----------

pub fn list_accounts(conn: &Connection) -> Result<Vec<Account>> {
    let mut st = conn.prepare(
        "SELECT name, matches, sort, valued_by, proxy, proxy_label, fund_code, fund_ocf_pct, platform_flat_month,
                platform_cliff, platform_pct_above, fee_match
         FROM investment_accounts ORDER BY sort, name",
    )?;
    let v = st
        .query_map([], |r| {
            let m: String = r.get(1)?;
            Ok(Account {
                name: r.get(0)?,
                matches: serde_json::from_str(&m).unwrap_or_default(),
                sort: r.get(2)?,
                valued_by: r.get(3)?,
                proxy: r.get(4)?,
                proxy_label: r.get(5)?,
                fund_code: r.get(6)?,
                fund_ocf_pct: r.get(7)?,
                platform_flat_month: r.get(8)?,
                platform_cliff: r.get(9)?,
                platform_pct_above: r.get(10)?,
                fee_match: r.get(11)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

pub fn list_instruments(conn: &Connection) -> Result<Vec<Instrument>> {
    let mut st = conn.prepare("SELECT symbol, name, kind, ter_pct FROM instruments ORDER BY symbol")?;
    let v = st
        .query_map([], |r| Ok(Instrument { symbol: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, ter_pct: r.get(3)? }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

// ---------- writing (the admin API) ----------

fn clean(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

fn check_account(a: &Account) -> Result<Account> {
    let name = a.name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 60 || name == OTHER {
        bail!("an account needs a name (60 characters at most, not {OTHER})");
    }
    if !VALUED_BY.contains(&a.valued_by.as_str()) {
        bail!("valued_by must be ibkr, proxy or cost");
    }
    let matches: Vec<String> = a.matches.iter().map(|m| m.to_lowercase()).filter(|m| !m.trim().is_empty()).take(20).collect();
    if matches.iter().any(|m| m.chars().count() > 60) {
        bail!("a match fragment is 60 characters at most");
    }
    for v in [a.fund_ocf_pct, a.platform_pct_above].into_iter().flatten() {
        if !(0.0..=10.0).contains(&v) {
            bail!("a fee in % a year is between 0 and 10");
        }
    }
    for v in [a.platform_flat_month, a.platform_cliff].into_iter().flatten() {
        if !(v >= 0.0 && v.is_finite()) {
            bail!("a platform fee or threshold can't be negative");
        }
    }
    let proxy = clean(&a.proxy).map(|p| p.to_uppercase());
    if proxy.as_deref().is_some_and(|p| p.len() > 20 || !p.chars().all(|c| c.is_ascii_alphanumeric() || "^.-=".contains(c))) {
        bail!("a proxy is a ticker symbol like URTH");
    }
    if a.valued_by == "proxy" && proxy.is_none() {
        bail!("an account valued by a proxy needs its proxy symbol");
    }
    Ok(Account {
        name,
        matches,
        proxy,
        proxy_label: clean(&a.proxy_label),
        fund_code: clean(&a.fund_code),
        fee_match: clean(&a.fee_match).map(|f| f.to_lowercase()),
        ..a.clone()
    })
}

fn insert_account(conn: &Connection, a: &Account) -> Result<()> {
    conn.execute(
        "INSERT INTO investment_accounts (name, matches, sort, valued_by, proxy, proxy_label, fund_code, fund_ocf_pct,
           platform_flat_month, platform_cliff, platform_pct_above, fee_match, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            a.name,
            serde_json::to_string(&a.matches)?,
            a.sort,
            a.valued_by,
            a.proxy,
            a.proxy_label,
            a.fund_code,
            a.fund_ocf_pct,
            a.platform_flat_month,
            a.platform_cliff,
            a.platform_pct_above,
            a.fee_match,
            crate::db::now_ms()
        ],
    )?;
    Ok(())
}

/// Add an account, or replace the one called `old` (a rename when the
/// names differ). Returns the account as stored.
pub fn put_account(conn: &Connection, old: Option<&str>, a: &Account) -> Result<Account> {
    ensure(conn)?;
    let a = check_account(a)?;
    let taken = list_accounts(conn)?.iter().any(|x| x.name == a.name && Some(x.name.as_str()) != old);
    if taken {
        bail!("there is already an account called {}", a.name);
    }
    // a rename deletes then inserts: both or neither (a savepoint, so it
    // also nests inside a caller's transaction)
    conn.execute_batch("SAVEPOINT put_account")?;
    let r = (|| -> Result<()> {
        if let Some(o) = old {
            let n = conn.execute("DELETE FROM investment_accounts WHERE name = ?1", [o])?;
            if n == 0 {
                bail!("no account called {o}");
            }
        }
        insert_account(conn, &a)
    })();
    if r.is_err() {
        let _ = conn.execute_batch("ROLLBACK TO put_account");
    }
    conn.execute_batch("RELEASE put_account")?;
    r?;
    Ok(a)
}

pub fn delete_account(conn: &Connection, name: &str) -> Result<()> {
    ensure(conn)?;
    if conn.execute("DELETE FROM investment_accounts WHERE name = ?1", [name])? == 0 {
        bail!("no account called {name}");
    }
    Ok(())
}

fn upsert_instrument(conn: &Connection, i: &Instrument) -> Result<()> {
    conn.execute(
        "INSERT INTO instruments (symbol, name, kind, ter_pct, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(symbol) DO UPDATE SET name = ?2, kind = ?3, ter_pct = ?4",
        params![i.symbol, i.name, i.kind, i.ter_pct, crate::db::now_ms()],
    )?;
    Ok(())
}

/// Add or change an instrument (by symbol). Returns it as stored.
pub fn put_instrument(conn: &Connection, i: &Instrument) -> Result<Instrument> {
    ensure(conn)?;
    let symbol = i.symbol.trim().to_uppercase();
    if symbol.is_empty() || symbol.len() > 20 || !symbol.chars().all(|c| c.is_ascii_alphanumeric() || "^.-=".contains(c)) {
        bail!("a symbol is a ticker like VWRA");
    }
    if !INSTRUMENT_KINDS.contains(&i.kind.as_str()) {
        bail!("kind must be fund or stock");
    }
    if i.ter_pct.is_some_and(|t| !(0.0..=10.0).contains(&t)) {
        bail!("a TER is between 0 and 10 % a year");
    }
    let i = Instrument { symbol, name: clean(&i.name), kind: i.kind.clone(), ter_pct: i.ter_pct };
    upsert_instrument(conn, &i)?;
    Ok(i)
}

pub fn delete_instrument(conn: &Connection, symbol: &str) -> Result<()> {
    ensure(conn)?;
    if conn.execute("DELETE FROM instruments WHERE symbol = ?1", [symbol])? == 0 {
        bail!("no instrument {symbol}");
    }
    Ok(())
}

/// The profile's benchmark symbol, else [`DEFAULT_BENCHMARK`].
pub fn benchmark(conn: &Connection) -> String {
    crate::profile::get(conn)
        .ok()
        .flatten()
        .and_then(|p| p.benchmark)
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| DEFAULT_BENCHMARK.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"accounts": [
        {"name": "Broker A", "matches": ["acme sec"], "sort": 10, "valued_by": "proxy", "proxy": "PRX", "proxy_label": "Index", "fund_code": "F1",
         "fund_ocf_pct": 0.4, "platform_flat_month": 20.0, "platform_cliff": 100000.0, "platform_pct_above": 0.5, "fee_match": "acme fee"},
        {"name": "Broker B", "matches": ["beta trading"], "sort": 20, "valued_by": "ibkr"},
        {"name": "Robo", "matches": ["robo"], "sort": 30, "valued_by": "cost"}],
        "instruments": [
        {"symbol": "FND", "kind": "fund", "ter_pct": 0.15}, {"symbol": "STK", "kind": "stock", "ter_pct": 0.0}]}"#;

    #[test]
    fn a_new_tree_starts_empty() {
        let c = crate::db::memory().unwrap();
        c.execute_batch("INSERT INTO bank_transactions (source, account, currency, txn_date, amount, description, raw_hash, source_file, ingested_at)
                         VALUES ('hsbc', 'a', 'HKD', '2026-01-02', -10, 'ACME SEC 123', 'h1', 'f', 0)").unwrap();
        assert!(ensure(&c).unwrap(), "made empty, like for every tree");
        assert!(!ensure(&c).unwrap());
        assert_eq!(Setup::load(&c), Setup::default());
        assert_eq!(Setup::load(&c).holding_of("ACME SEC 123"), OTHER);
        let dir = std::env::temp_dir().join(format!("finance-invest-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = crate::db::connect(&dir.join("finance.db")).unwrap();
        assert!(ensure(&f).unwrap());
        assert!(!ensure(&f).unwrap());
        assert_eq!(Setup::load(&f), Setup::default(), "a new file too");
        drop(f);
        let f = crate::db::connect(&dir.join("finance.db")).unwrap();
        assert!(crate::db::is_new_tree(&f), "still new when opened again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tree_before_the_tables_gets_them_empty_and_a_setup_is_left_alone() {
        let c = crate::db::memory().unwrap();
        // a tree from before the setup tables, with history
        c.execute_batch("DROP TABLE IF EXISTS investment_accounts; DROP TABLE IF EXISTS instruments; PRAGMA user_version = 0;
                         INSERT INTO bank_transactions (source, account, currency, txn_date, amount, description, raw_hash, source_file, ingested_at)
                         VALUES ('hsbc', 'a', 'HKD', '2026-01-02', -10, 'ACME SEC 123', 'h1', 'f', 0)").unwrap();
        assert!(!has_table(&c, "investment_accounts") && !has_table(&c, "instruments"));
        assert_eq!(Setup::load(&c), Setup::default(), "no tables yet: a reader reads empty");
        assert!(ensure(&c).unwrap(), "made now");
        assert_eq!(Setup::load(&c), Setup::default(), "and empty, nothing seeded");
        assert!(!ensure(&c).unwrap());

        import_json(&c, SAMPLE).unwrap();
        let before = Setup::load(&c);
        assert_eq!(before.accounts.len(), 3);
        assert!(!ensure(&c).unwrap());
        assert_eq!(Setup::load(&c), before, "an existing setup is left as it is");
    }

    #[test]
    fn the_tables_round_trip() {
        let c = crate::db::memory().unwrap();
        import_json(&c, SAMPLE).unwrap();
        let s = Setup::load(&c);
        assert_eq!(s.holding_of("ACME SEC 123"), "Broker A");
        assert_eq!(s.holding_of("To Beta Trading"), "Broker B");
        assert_eq!(s.holding_of("to robo"), "Robo");
        assert_eq!(s.holding_of("Something"), OTHER);
        assert_eq!((s.proxy("Broker A"), s.fund("Broker A"), s.proxy("Broker B")), (Some("PRX"), Some("F1"), None));
        assert_eq!((s.ter("FND"), s.ter("STK"), s.ter("XYZ")), (Some(0.15), Some(0.0), None));
        assert_eq!((s.order("Broker A", "Crypto"), s.order("Broker B", "Crypto"), s.order("Robo", "Crypto"), s.order("Other", "Crypto")), (0, 1, 5, 5));
        assert!(s.order("Crypto", "Crypto") > 5);
        assert_eq!(s.platform().map(|a| a.name.as_str()), Some("Broker A"));

        // edits
        let mut a = s.account("Robo").unwrap().clone();
        a.name = "Robo invest".into();
        a.matches = vec!["Investment Account".into(), "adviser".into()];
        put_account(&c, Some("Robo"), &a).unwrap();
        let s = Setup::load(&c);
        assert_eq!(s.holding_of("to ADVISER"), "Robo invest");
        assert!(s.account("Robo").is_none());
        assert!(put_account(&c, None, &Account { name: "Broker B".into(), ..Default::default() }).is_err(), "taken");
        assert!(put_account(&c, None, &Account { name: "X".into(), valued_by: "proxy".into(), ..Default::default() }).is_err(), "a proxy needs a symbol");
        put_instrument(&c, &Instrument { symbol: " cspx ".into(), ter_pct: Some(0.07), ..Default::default() }).unwrap();
        assert_eq!(Setup::load(&c).ter("CSPX"), Some(0.07));
        assert!(put_instrument(&c, &Instrument { symbol: "BAD SYMBOL".into(), ..Default::default() }).is_err());
        delete_instrument(&c, "CSPX").unwrap();
        assert_eq!(Setup::load(&c).ter("CSPX"), None);
        delete_account(&c, "Robo invest").unwrap();
        assert_eq!(Setup::load(&c).holding_of("to adviser"), OTHER);
        assert_eq!(benchmark(&c), DEFAULT_BENCHMARK);
    }
}
