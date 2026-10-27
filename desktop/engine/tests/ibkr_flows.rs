//! investment_flows: the IBKR holding's money in from the broker's own
//! deposits (ibkr_cash_flows) onward, the bank rows before them, and the other
//! holdings untouched. Synthetic data, all in HKD (no fx).

use finance_engine::db::{self, IbkrFlow, NewBankTxn};
use finance_engine::invest_setup::{self, Account};
use finance_engine::views::holdings::investment_flows;
use finance_engine::views::Ctx;

fn setup(bank: &[(&str, f64, &str)], broker: &[(&str, f64)]) -> rusqlite::Connection {
    let conn = db::memory().unwrap();
    let rows: Vec<NewBankTxn> = bank
        .iter()
        .enumerate()
        .map(|(i, (date, amount, desc))| NewBankTxn {
            source: "hsbc".into(),
            account: String::new(),
            currency: "HKD".into(),
            txn_date: (*date).into(),
            amount: Some(*amount),
            balance: None,
            description: (*desc).into(),
            raw_hash: format!("h{i}"),
            source_file: "t.csv".into(),
            ingested_at: 1_700_000_000_000 + i as i64,
        })
        .collect();
    db::insert_bank_transactions(&conn, &rows).unwrap();
    conn.execute("UPDATE bank_transactions SET category = 'Investments'", []).unwrap();
    let acct = |name: &str, m: &str, valued_by: &str| Account {
        name: name.into(),
        matches: vec![m.into()],
        valued_by: valued_by.into(),
        ..Account::default()
    };
    invest_setup::put_account(&conn, None, &acct("IBKR", "interactive brokers", "ibkr")).unwrap();
    invest_setup::put_account(&conn, None, &acct("Fund", "fundco", "cost")).unwrap();
    let flows: Vec<IbkrFlow> = broker
        .iter()
        .enumerate()
        .map(|(i, (date, amount))| IbkrFlow {
            flow_id: format!("f{i}"),
            date: (*date).into(),
            kind: if *amount >= 0.0 { "Deposits" } else { "Withdrawals" }.into(),
            currency: "HKD".into(),
            amount: *amount,
            amount_base: *amount,
            description: String::new(),
        })
        .collect();
    db::upsert_ibkr_cash_flows(&conn, &flows).unwrap();
    conn
}

/// (holding, date, hkd) by date.
fn flows(conn: &rusqlite::Connection) -> Vec<(String, String, f64)> {
    let ctx = Ctx::offline(conn);
    let rates = ctx.rates();
    investment_flows(&ctx, &rates, None, true).into_iter().map(|c| (c.holding, c.date, c.hkd)).collect()
}

fn ibkr_total(f: &[(String, String, f64)]) -> f64 {
    f.iter().filter(|r| r.0 == "IBKR").map(|r| r.2).sum()
}

#[test]
fn bank_before_broker_after() {
    let conn = setup(
        &[("2026-01-02", -100.0, "TO INTERACTIVE BROKERS"), ("2026-02-02", -200.0, "TO INTERACTIVE BROKERS")],
        &[("2026-02-03", 200.0)],
    );
    let f = flows(&conn);
    let ib: Vec<_> = f.iter().filter(|r| r.0 == "IBKR").collect();
    assert_eq!(ib.len(), 2);
    assert_eq!((ib[0].1.as_str(), ib[0].2), ("2026-01-02", 100.0));
    assert_eq!((ib[1].1.as_str(), ib[1].2), ("2026-02-03", 200.0));
}

#[test]
fn one_day_lag_across_a_month_boundary_is_not_double_counted() {
    // bank on 31 Jan, broker books it on 1 Feb (the first broker flow)
    let conn = setup(&[("2026-01-31", -500.0, "TO INTERACTIVE BROKERS")], &[("2026-02-01", 500.0)]);
    let f = flows(&conn);
    assert_eq!(ibkr_total(&f), 500.0);
    // the broker's flow is in February; January has none
    assert!(f.iter().all(|r| r.1 == "2026-02-01"));
}

#[test]
fn bank_rows_after_the_cutover_are_dropped_and_withdrawals_count() {
    let conn = setup(
        &[("2026-03-02", -300.0, "TO INTERACTIVE BROKERS"), ("2026-04-02", 50.0, "FROM INTERACTIVE BROKERS")],
        &[("2026-03-03", 300.0), ("2026-04-03", -50.0)],
    );
    let f = flows(&conn);
    assert_eq!(f.len(), 2);
    assert_eq!(ibkr_total(&f), 250.0);
    assert_eq!(f[1].2, -50.0);
}

#[test]
fn no_broker_flows_is_unchanged() {
    let conn = setup(&[("2026-01-02", -100.0, "TO INTERACTIVE BROKERS"), ("2026-01-05", -70.0, "FUNDCO")], &[]);
    let f = flows(&conn);
    assert_eq!(f.len(), 2);
    assert_eq!(ibkr_total(&f), 100.0);
}

#[test]
fn other_holdings_are_untouched() {
    let conn = setup(
        &[("2026-03-02", -300.0, "TO INTERACTIVE BROKERS"), ("2026-03-05", -70.0, "FUNDCO"), ("2025-12-05", -10.0, "FUNDCO")],
        &[("2026-03-03", 300.0)],
    );
    let f = flows(&conn);
    let fund: Vec<_> = f.iter().filter(|r| r.0 == "Fund").map(|r| r.2).collect();
    assert_eq!(fund, vec![10.0, 70.0]);
    assert_eq!(ibkr_total(&f), 300.0);
}
