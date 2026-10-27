//! The tables the views scan over and over, read once per Ctx. Rather than a
//! dozen differently filtered SELECTs over bank_transactions per bundle,
//! the rows are loaded in one pass (rowid order, which is what those
//! unindexed scans return) and filtered in memory -- reading the rows was
//! costing more than all the math.

use std::collections::HashMap;

use rusqlite::Connection;

use super::{get_f64, get_str, take_chars};

/// One bank_transactions row.
#[derive(Clone, Debug)]
pub struct BankRow {
    pub id: i64,
    pub source: String,
    pub account: String,
    pub currency: String,
    pub txn_date: String,
    pub amount: Option<f64>,
    pub balance: Option<f64>,
    pub description: String,
    pub category: Option<String>,
    pub share: Option<f64>,
    /// txn_overrides.note of the row (hand categorization), if any
    pub note: Option<String>,
}

impl BankRow {
    /// substr(txn_date,1,7)
    pub fn ym(&self) -> &str {
        take_chars(&self.txn_date, 7)
    }

    /// COALESCE(category,'Other')
    pub fn cat(&self) -> &str {
        self.category.as_deref().unwrap_or("Other")
    }

    /// COALESCE(share,1)
    pub fn share1(&self) -> f64 {
        self.share.unwrap_or(1.0)
    }

    pub fn is(&self, category: &str) -> bool {
        self.category.as_deref() == Some(category)
    }
}

pub fn load_bank(conn: &Connection) -> Vec<BankRow> {
    // raw_hash -> [(source, note)]: looked up by the row's borrowed hash, so
    // the scan below allocates nothing for rows without a note
    let mut notes: HashMap<String, Vec<(String, String)>> = HashMap::new();
    if let Ok(mut s) = conn.prepare("SELECT source, raw_hash, note FROM txn_overrides WHERE note IS NOT NULL") {
        if let Ok(rows) = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))) {
            for (s, h, n) in rows.flatten() {
                notes.entry(h).or_default().push((s, n));
            }
        }
    }
    let sql = "SELECT id, source, account, currency, txn_date, amount, balance, description, category, share, raw_hash \
               FROM bank_transactions ORDER BY rowid";
    let Ok(mut stmt) = conn.prepare(sql) else { return vec![] };
    let rows = stmt.query_map([], |r| {
        let source = get_str(r, 1).unwrap_or_default();
        let note = if notes.is_empty() {
            None
        } else {
            let hash = match r.get_ref_unwrap(10) {
                rusqlite::types::ValueRef::Text(t) => std::str::from_utf8(t).ok(),
                _ => None,
            };
            hash.and_then(|h| notes.get(h)).and_then(|v| v.iter().find(|(s, _)| *s == source)).map(|(_, n)| n.clone())
        };
        Ok(BankRow {
            id: r.get::<_, Option<i64>>(0)?.unwrap_or(0),
            account: get_str(r, 2).unwrap_or_default(),
            currency: get_str(r, 3).unwrap_or_default(),
            txn_date: get_str(r, 4).unwrap_or_default(),
            amount: get_f64(r, 5),
            balance: get_f64(r, 6),
            description: get_str(r, 7).unwrap_or_default(),
            category: get_str(r, 8),
            share: get_f64(r, 9),
            note,
            source,
        })
    });
    let Ok(rows) = rows else { return vec![] };
    rows.flatten().collect()
}
