//! Receivables: money the owner paid out but gets back. Deposits held (per counterparty + currency,
//! refunds close the oldest open one), the partner's balance (their part of
//! shared bills less what they paid back), and reimbursable office spend
//! (information only); the rules are spelled out in desktop/server/README.md.

use std::sync::OnceLock;

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde_json::{json, Value};

use super::fx::{current_fx_rates, get_rate, Rates};
use super::{next_month, num, title_case, r2, take_chars, ym_of, Ctx};

pub use super::store::BankRow as Row;

impl Row {
    fn date10(&self) -> &str {
        take_chars(&self.txn_date, 10)
    }
}

/// counterparty_key: an account number wins; else the first two words that
/// aren't a transfer verb, a reference or a legal suffix.
pub fn counterparty_key(desc: &str) -> String {
    static ACCOUNT_NO: OnceLock<Regex> = OnceLock::new();
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let d = desc.to_lowercase();
    let acc = ACCOUNT_NO.get_or_init(|| Regex::new(r"\b\d{2,4}-\d{3,8}-\d{2,4}\b|\b\d{9,}\b").unwrap());
    if let Some(m) = acc.find(&d) {
        return m.as_str().to_string();
    }
    let lead = LEAD.get_or_init(|| {
        Regex::new(r"^(?:(?:cr|dr|fps|payment|transfer|trf|to|from|by)\b[\s:/-]*)+").unwrap()
    });
    let d = lead.replace(d.trim(), "").into_owned();
    let word = WORD.get_or_init(|| Regex::new(r"[a-z][a-z&'.]*").unwrap());
    const STOP: [&str; 7] = ["the", "and", "ltd", "limited", "gmbh", "co", "inc"];
    let words: Vec<&str> = word.find_iter(&d).map(|m| m.as_str()).filter(|w| !STOP.contains(w)).take(2).collect();
    if words.is_empty() { "unknown".into() } else { words.join(" ") }
}

fn label(key: &str) -> String {
    if key.chars().any(|c| c.is_ascii_digit()) { key.to_string() } else { title_case(key) }
}

/// One open deposit after the replay.
#[derive(Clone, Debug)]
pub struct Deposit {
    pub key: String,
    pub currency: String,
    pub native: f64,
    pub since: String,
    pub note: Option<String>,
    pub payments: Vec<(String, f64)>,
    pub refunded: f64,
}

/// deposits_held: replay deposit rows chronologically up to `until`
/// (inclusive); outflows open/top up, inflows refund (a differently
/// described payer closes the oldest open deposit in that currency; with
/// nothing open, the refund predates the data and is ignored).
pub fn deposits_held(rows: &[Row], until: Option<&str>) -> Vec<Deposit> {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| (a.txn_date.as_str(), a.id).cmp(&(b.txn_date.as_str(), b.id)));
    let mut open: Vec<Deposit> = Vec::new(); // insertion order: first insertion of a (key, ccy)
    for r in sorted {
        if until.is_some_and(|u| r.date10() > u) {
            break;
        }
        let amt = r.amount.unwrap_or(0.0);
        let key = counterparty_key(&r.description);
        let pos = open.iter().position(|s| s.key == key && s.currency == r.currency);
        if amt < 0.0 {
            let fresh = Deposit {
                key: key.clone(),
                currency: r.currency.clone(),
                native: 0.0,
                since: r.date10().to_string(),
                note: None,
                payments: vec![],
                refunded: 0.0,
            };
            let i = match pos {
                Some(i) if open[i].native > 1e-9 => i,
                Some(i) => {
                    open[i] = fresh; // re-assignment keeps the slot
                    i
                }
                None => {
                    open.push(fresh);
                    open.len() - 1
                }
            };
            let st = &mut open[i];
            st.native += -amt;
            st.payments.push((r.date10().to_string(), r2(-amt)));
            if st.note.as_deref().is_none_or(str::is_empty) {
                st.note = r.note.clone();
            }
        } else if amt > 0.0 {
            let mut target = pos.filter(|&i| open[i].native > 1e-9);
            if target.is_none() {
                let d = r.date10();
                let mut best: Option<usize> = None;
                for (i, s) in open.iter().enumerate() {
                    if s.currency == r.currency && s.native > 1e-9 && s.since.as_str() <= d
                        && best.is_none_or(|b| s.since < open[b].since)
                    {
                        best = Some(i);
                    }
                }
                target = best;
            }
            let Some(t) = target else { continue };
            let back = amt.min(open[t].native);
            open[t].native -= back;
            open[t].refunded += back;
        }
    }
    open.into_iter().filter(|s| s.native > 0.005).collect()
}

/// A deposit the home setup says came back: (currency, the one-off's date,
/// returned on).
pub type Return = (String, String, String);

/// The home setting's deposit one-offs that have a `returned_on`. Nothing
/// without a setting, so the receivables are then what the rows say.
pub fn returns(ctx: &Ctx) -> Vec<Return> {
    let Ok(Some(v)) = crate::profile::setting(ctx.conn, finance_core::home::SETTING_KEY) else { return vec![] };
    let Some(home) = finance_core::home::HomeSetup::parse(&v) else { return vec![] };
    let base = super::fx::base_currency(ctx.conn);
    let ccy = home.currency.as_deref().map(str::trim).filter(|c| !c.is_empty()).unwrap_or(&base).to_uppercase();
    home.oneoffs
        .iter()
        .filter(|o| o.kind == "deposit")
        .filter_map(|o| Some((ccy.clone(), o.date.clone().filter(|d| !d.trim().is_empty())?, o.returned_on.clone().filter(|d| !d.trim().is_empty())?)))
        .collect()
}

/// Close the held deposits the setup says were returned on or before
/// `as_of`: the one in that currency opened within a month of the one-off's
/// date. Each return closes one deposit.
pub fn without_returned(held: Vec<Deposit>, returned: &[Return], as_of: &str) -> Vec<Deposit> {
    let mut held = held;
    for (ccy, date, _) in returned.iter().filter(|r| r.2.as_str() <= as_of) {
        if let Some(i) = held.iter().position(|d| d.currency.eq_ignore_ascii_case(ccy) && finance_core::home::near(&d.since, date)) {
            held.remove(i);
        }
    }
    held
}

/// partner_balance.
pub fn partner_balance(rows: &[Row], rates: &Rates, until: Option<&str>, is_partner: &dyn Fn(&str) -> bool) -> Value {
    let mut rows: Vec<&Row> = rows.iter().filter(|r| until.is_none_or(|u| r.date10() <= u)).collect();
    rows.sort_by(|a, b| a.txn_date.cmp(&b.txn_date));
    let shared: Vec<&Row> = rows
        .iter()
        .copied()
        .filter(|r| r.amount.unwrap_or(0.0) < 0.0 && r.share.is_some_and(|s| s < 1.0))
        .collect();
    if shared.is_empty() {
        return json!({"amount_hkd": 0.0, "since": null, "shared_hkd": 0.0, "repaid_hkd": 0.0,
                      "paid_to_partner_hkd": 0.0, "shared_rows": 0, "last_repayment": null,
                      "note": "starts with the first shared bill"});
    }
    let since = shared[0].date10().to_string();
    let hkd = |r: &Row| r.amount.unwrap_or(0.0).abs() * rates.rate(&r.currency, r.ym());
    let their_part = shared.iter().fold(0.0, |a, r| a + (1.0 - r.share.unwrap_or(1.0).max(0.0)) * hkd(r));
    let partner: Vec<&Row> = rows
        .iter()
        .copied()
        .filter(|r| r.category.as_deref().is_some_and(is_partner) && r.date10() >= since.as_str())
        .collect();
    let amt = |r: &Row| r.amount.unwrap_or(0.0);
    let repaid = partner.iter().filter(|r| amt(r) > 0.0).fold(0.0, |a, r| a + hkd(r));
    let paid_out = partner.iter().filter(|r| amt(r) < 0.0).fold(0.0, |a, r| a + hkd(r));
    let last_in = partner.iter().filter(|r| amt(r) > 0.0).map(|r| r.date10()).max();
    let owes = their_part - repaid + paid_out;
    let n = shared.len();
    json!({"amount_hkd": num(r2(owes)), "since": since, "shared_hkd": num(r2(their_part)),
           "repaid_hkd": num(r2(repaid)), "paid_to_partner_hkd": num(r2(paid_out)),
           "shared_rows": n, "last_repayment": last_in,
           "note": format!("their part of {n} shared bill{} since {since}, less what they paid back",
                           if n != 1 { "s" } else { "" })})
}

/// reimbursable_spend: net office spend YTD and trailing 12 months.
pub fn reimbursable_spend(rows: &[Row], rates: &Rates, today: NaiveDate, category: &str) -> Value {
    let ytd_from = format!("{:04}-01-01", today.year());
    let t12_from = (today - chrono::Duration::days(365)).to_string();
    let total = |frm: &str| {
        r2(rows
            .iter()
            .filter(|r| r.date10() >= frm)
            .fold(0.0, |a, r| a + -r.amount.unwrap_or(0.0) * rates.rate(&r.currency, r.ym())))
    };
    let (ytd, t12) = (total(&ytd_from), total(&t12_from));
    // average over the months actually billed in the window, not a flat 12
    let span: Vec<&str> = rows.iter().filter(|r| r.date10() >= t12_from.as_str()).map(|r| r.ym()).collect();
    let n = match (span.iter().min(), span.iter().max()) {
        (Some(first), Some(last)) => {
            let p = |s: &str| s[..4].parse::<i64>().unwrap_or(0) * 12 + s[5..7].parse::<i64>().unwrap_or(0);
            p(last) - p(first) + 1
        }
        _ => 0,
    };
    let last_charge = rows.iter().filter(|r| r.amount.unwrap_or(0.0) < 0.0).map(|r| r.date10()).max();
    json!({"category": category, "ytd_hkd": num(ytd), "t12m_hkd": num(t12),
           "monthly_avg_hkd": num(if n != 0 { r2(t12 / n as f64) } else { 0.0 }), "months": n,
           "last_charge": last_charge,
           "note": "repaid inside salary, so what is still outstanding can't be read from the data; \
                    shown for information, not counted in net worth"})
}

struct Loaded {
    deposit: Vec<Row>,
    partner: Vec<Row>,
    office: Vec<Row>,
    /// the categories that are the partner settling up (kind partner)
    partner_cats: std::collections::HashSet<String>,
    reimbursed_name: String,
}

/// Any row a receivable comes from (deposits held, a partner's share, spend
/// to be reimbursed): the Owed page and the receivables tiles need one.
pub fn has_rows(ctx: &Ctx) -> bool {
    load(ctx).is_ok_and(|l| !(l.deposit.is_empty() && l.partner.is_empty() && l.office.is_empty()))
}

fn load(ctx: &Ctx) -> anyhow::Result<Loaded> {
    let bank = ctx.bank();
    let flags = ctx.flags();
    let pick = |f: &dyn Fn(&Row) -> bool| bank.iter().filter(|b| f(b)).cloned().collect::<Vec<Row>>();
    Ok(Loaded {
        deposit: pick(&|b| b.category.as_deref().is_some_and(|c| flags.is_deposit(c))),
        partner: pick(&|b| {
            b.category.as_deref().is_some_and(|c| flags.is_partner(c)) || (b.share.is_some_and(|s| s < 1.0) && b.amount.is_some_and(|a| a < 0.0))
        }),
        office: pick(&|b| b.category.as_deref().is_some_and(|c| flags.is_reimbursable(c))),
        partner_cats: bank.iter().filter_map(|b| b.category.as_deref()).filter(|c| flags.is_partner(c)).map(String::from).collect(),
        reimbursed_name: flags.reimbursed_name(),
    })
}

/// Receivables now. `fx_now` values the open deposits
/// (default: the latest cached rates, network-free).
pub fn compute(ctx: &Ctx, fx_now: Option<&[(String, f64)]>) -> anyhow::Result<Value> {
    let today = ctx.today;
    let rows = load(ctx)?;
    let rates = ctx.rates();
    let owned;
    let fx_now = match fx_now {
        Some(f) => f,
        None => {
            owned = current_fx_rates(ctx, false);
            &owned
        }
    };
    let base = rates.base().to_string();
    let now_rate = |ccy: &str| if ccy == base { Some(1.0) } else { get_rate(fx_now, ccy) };
    let held = without_returned(deposits_held(&rows.deposit, None), &returns(ctx), &today.to_string());
    let mut deposits: Vec<(String, Value, f64)> = held
        .into_iter()
        .map(|s| {
            let r = now_rate(&s.currency);
            let amount_hkd = match r {
                Some(r) if r != 0.0 => r2(s.native * r),
                _ => 0.0,
            };
            let v = json!({
                "key": format!("deposit_{}", s.key).replace(' ', "_"), "label": label(&s.key),
                "note": s.note, "currency": s.currency, "native_amount": num(r2(s.native)),
                "amount_hkd": num(amount_hkd), "no_fx": r.is_none(), "since": s.since,
                "payments": s.payments.iter().map(|(d, n)| json!({"date": d, "native": num(*n)})).collect::<Vec<_>>(),
                "refunded_native": num(r2(s.refunded))});
            (s.since.clone(), v, amount_hkd)
        })
        .collect();
    deposits.sort_by(|a, b| a.0.cmp(&b.0));
    let partner = partner_balance(&rows.partner, &rates, None, &|c| rows.partner_cats.contains(c));
    let deposits_hkd = r2(deposits.iter().fold(0.0, |a, d| a + d.2));
    let p_amt = partner.get("amount_hkd").and_then(Value::as_f64).unwrap_or(0.0);
    Ok(json!({
        "as_of": today.to_string(),
        "total_hkd": num(r2(deposits_hkd + p_amt)),
        "deposits_hkd": num(deposits_hkd),
        "deposits": deposits.into_iter().map(|d| d.1).collect::<Vec<_>>(),
        "partner": partner,
        "reimbursements": reimbursable_spend(&rows.office, &rates, today, &rows.reimbursed_name),
    }))
}

/// (key, label, currency, native, hkd) net-worth rows.
pub fn by_source(rec: &Value, base: &str) -> Vec<(String, String, String, f64, f64)> {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let mut out: Vec<_> = rec
        .get("deposits")
        .and_then(Value::as_array)
        .map(|ds| {
            ds.iter()
                .map(|d| (s(d, "key"), format!("Deposit · {}", s(d, "label")), s(d, "currency"),
                          f(d, "native_amount"), f(d, "amount_hkd")))
                .collect()
        })
        .unwrap_or_default();
    let p = f(&rec["partner"], "amount_hkd");
    if p.abs() > 0.005 {
        out.push(("partner_balance".into(), "Partner balance".into(), base.to_string(), p, p));
    }
    out
}

/// Receivables at each month's end (deposits at
/// that month's rate, the partner balance at each row's own rate).
pub fn monthly_hkd(ctx: &Ctx, months: &[NaiveDate]) -> anyhow::Result<Vec<f64>> {
    let rows = load(ctx)?;
    if rows.deposit.is_empty() && rows.partner.is_empty() {
        return Ok(vec![0.0; months.len()]);
    }
    let rates = ctx.rates();
    let returned = returns(ctx);
    Ok(months
        .iter()
        .map(|m| {
            let end = (next_month(*m) - chrono::Duration::days(1)).to_string();
            let ym = ym_of(*m);
            let dep = without_returned(deposits_held(&rows.deposit, Some(&end)), &returned, &end)
                .iter()
                .fold(0.0, |a, s| a + s.native * rates.rate(&s.currency, &ym));
            let p = partner_balance(&rows.partner, &rates, Some(&end), &|c| rows.partner_cats.contains(c))["amount_hkd"].as_f64().unwrap_or(0.0);
            r2(dep + p)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, NewBankTxn};

    fn deposit(since: &str) -> Deposit {
        Deposit { key: "landlord".into(), currency: "HKD".into(), native: 2400.0, since: since.into(), note: None, payments: vec![], refunded: 0.0 }
    }

    #[test]
    fn a_setups_return_closes_the_matching_deposit_only() {
        let back = vec![("HKD".to_string(), "2026-01-02".to_string(), "2026-06-30".to_string())];
        let held = vec![deposit("2026-01-03"), deposit("2025-05-01")];
        let left = without_returned(held.clone(), &back, "2026-07-01");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].since, "2025-05-01", "the one within a month of the date goes");
        assert_eq!(without_returned(held.clone(), &back, "2026-06-29").len(), 2, "not yet returned that day");
        assert_eq!(without_returned(held.clone(), &[("EUR".into(), "2026-01-02".into(), "2026-02-01".into())], "2026-07-01").len(), 2, "another currency");
        assert_eq!(without_returned(held.clone(), &[], "2026-07-01").len(), 2, "no setting: untouched");
        let once = vec![back[0].clone(), back[0].clone()];
        assert_eq!(without_returned(vec![deposit("2026-01-03"), deposit("2026-01-05")], &once, "2026-07-01").len(), 0, "each return closes one");
        assert_eq!(without_returned(vec![deposit("2026-01-03")], &once, "2026-07-01").len(), 0);
    }

    fn conn_with_deposit() -> rusqlite::Connection {
        let c = db::memory().unwrap();
        db::insert_bank_transactions(&c, &[NewBankTxn {
            source: "n26".into(), account: "main".into(), currency: "HKD".into(), txn_date: "2026-01-02".into(), amount: Some(-2400.0), balance: None,
            description: "RENTAL DEPOSIT LINDEN".into(), raw_hash: "dep1".into(), source_file: "f.csv".into(), ingested_at: 1,
        }]).unwrap();
        c.execute("UPDATE bank_transactions SET category = 'Deposit'", []).unwrap();
        c
    }

    fn held(c: &rusqlite::Connection, today: &str) -> (f64, usize) {
        let mut ctx = Ctx::offline(c);
        ctx.today = today.parse().unwrap();
        let v = compute(&ctx, Some(&[])).unwrap();
        (v["deposits_hkd"].as_f64().unwrap(), v["deposits"].as_array().unwrap().len())
    }

    #[test]
    fn receivables_with_and_without_returned_on() {
        let c = conn_with_deposit();
        assert_eq!(held(&c, "2026-09-01"), (2400.0, 1), "no setting: the rows alone");
        let home = |back: serde_json::Value| serde_json::json!({"mode": "rent", "oneoffs": [{"kind": "deposit", "amount": 2400.0, "date": "2026-01-02", "returned_on": back}]});
        crate::profile::put_setting(&c, "home", &home(serde_json::Value::Null)).unwrap();
        assert_eq!(held(&c, "2026-09-01"), (2400.0, 1), "a setting without returned_on changes nothing");
        crate::profile::put_setting(&c, "home", &home(serde_json::json!("2026-08-15"))).unwrap();
        assert_eq!(held(&c, "2026-09-01"), (0.0, 0), "returned: no longer owed");
        assert_eq!(held(&c, "2026-08-01"), (2400.0, 1), "still held before the day it came back");
        crate::profile::put_setting(&c, "home", &serde_json::json!({"mode": "rent", "oneoffs": [{"kind": "deposit", "amount": 1.0, "date": "2025-01-02", "returned_on": "2025-06-01"}]})).unwrap();
        assert_eq!(held(&c, "2026-09-01"), (2400.0, 1), "a one-off for another deposit leaves this one");
        // the month-end series follows the same rule
        crate::profile::put_setting(&c, "home", &home(serde_json::json!("2026-08-15"))).unwrap();
        let ctx = Ctx::offline(&c);
        let months: Vec<chrono::NaiveDate> = ["2026-07-01", "2026-08-01", "2026-09-01"].iter().map(|d| d.parse().unwrap()).collect();
        let m = monthly_hkd(&ctx, &months).unwrap();
        assert_eq!((m[0] > 0.0, m[1], m[2]), (true, 0.0, 0.0), "held at July's end, closed by August's");
    }
}
