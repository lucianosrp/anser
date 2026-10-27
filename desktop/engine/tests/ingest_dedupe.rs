//! Dedupe guarantees on a made-up transactions tree that has every hard
//! case: overlapping exports, balance drift from a reordered same-day group,
//! a renamed merchant with a timezone-shifted date, genuine same-day repeats
//! that differ only by balance, a newest-first export adding a second
//! identical fare, N26 double charges, identical card coffees, and duplicate
//! crypto/SMS/legacy files.
//!
//! Every scenario must add exactly 0 rows once the tree is in, and the
//! database the tree builds is pinned row for row (a digest per table).

mod ingest_common;

use std::path::{Path, PathBuf};

use finance_engine::{db, ingest};
use ingest_common::*;

/// HSBC current-account export: rows given chronologically with the
/// balance each export computed; written newest first, like HSBC does.
fn hsbc_csv(rows: &[(&str, &str, f64, f64)]) -> String {
    let mut s = String::from("Date,Description,Billing amount,Billing currency,Balance,Balance currency\r\n");
    for (d, desc, amt, bal) in rows.iter().rev() {
        s += &format!("{d},{desc},\"{}\"\t,HKD,\"{bal:.2}\"\t,HKD\r\n", fmt_money(*amt));
    }
    s
}

fn fmt_money(x: f64) -> String {
    finance_engine::sources::textfmt::format_thousands(x, 2)
}

/// Chronological rows with a running balance from `open`.
fn chain<'a>(open: f64, rows: &[(&'a str, &'a str, f64)]) -> Vec<(&'a str, &'a str, f64, f64)> {
    let mut b = open;
    rows.iter()
        .map(|(d, desc, a)| {
            b = ((b + a) * 100.0).round() / 100.0;
            (*d, *desc, *a, b)
        })
        .collect()
}

const N26H: &str = "\"Booking Date\",\"Value Date\",\"Partner Name\",\"Partner Iban\",Type,\"Payment Reference\",\"Account Name\",\"Amount (EUR)\",\"Original Amount\",\"Original Currency\",\"Exchange Rate\"\n";
const REVH: &str = "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\n";
const CARDH: &str = "Transaction date,Post date,Description,Billing amount,Billing currency,Transaction status,Merchant name,Credit / Debit\r\n";
const GBPH: &str = "Completed Date , Description , Paid Out (GBP) , Paid In (GBP) , Exchange Out, Exchange In, Balance (GBP), Category, Notes\n";
const BTCH: &str = "Completed Date , Description , Paid Out (BTC) , Paid In (BTC) , Exchange Out, Exchange In, Balance (BTC), Category, Notes\n";
const TREZH: &str = "Timestamp,Date,Time,Type,Transaction ID,Fee,Fee unit,Address,Label,Amount,Amount unit,Fiat (USD),Other\n";

/// Expected rows per source once the whole tree is in.
const EXPECTED: [(&str, i64); 5] = [("hsbc", 10), ("revolut_new", 6), ("n26", 5), ("hsbc_card", 4), ("revolut_old_gbp", 2)];

fn make_tree(tx: &Path) {
    // --- HSBC: A covers 01-05 with day 5's first fare; B (later) covers
    // 03-06, has day 5's second fare, and orders day 4 cafe-before-rebate,
    // drifting both day-4 balances
    let d3 = [("03/03/2026", "OVPAY FARE", -3.0); 3];
    let mut a = vec![
        ("01/03/2026", "SALARY DEPOSIT", 1000.0),
        ("02/03/2026", "SHOP A", -50.25),
    ];
    a.extend(d3);
    a.extend([("04/03/2026", "CASH REBATE", 0.09), ("04/03/2026", "CAFE", -20.0), ("05/03/2026", "OVPAY FARE", -3.0)]);
    let ca = chain(100.0, &a);
    write(tx, "TransactionHistory-A.csv", &hsbc_csv(&ca));
    let open_b = ca[1].3; // balance before day 3
    let mut b = d3.to_vec();
    b.extend([
        ("04/03/2026", "CAFE", -20.0),
        ("04/03/2026", "CASH REBATE", 0.09),
        ("05/03/2026", "OVPAY FARE", -3.0),
        ("05/03/2026", "OVPAY FARE", -3.0),
        ("06/03/2026", "BIG PURCHASE", -500.0),
    ]);
    write(tx, "TransactionHistory-B.csv", &hsbc_csv(&chain(open_b, &b)));

    // --- Revolut: R1 stores LA MAREA late on the 10th; R2 renames it and
    // (exported in another timezone) books it on the 11th, same balance; R2
    // also has three identical fares on the 12th
    write(
        tx,
        "account-statement_r1.csv",
        &format!(
            "{REVH}Card Payment,Current,2026-04-09 10:00:00,2026-04-09 10:00:00,Top up,100.00,0.00,EUR,COMPLETED,100.00\n\
             Card Payment,Current,2026-04-10 23:30:00,2026-04-10 23:30:00,LA MAREA,-30.00,0.00,EUR,COMPLETED,70.00\n"
        ),
    );
    write(
        tx,
        "account-statement_r2.csv",
        &format!(
            "{REVH}Card Payment,Current,2026-04-09 10:00:00,2026-04-09 10:00:00,Top up,100.00,0.00,EUR,COMPLETED,100.00\n\
             Card Payment,Current,2026-04-11 00:30:00,2026-04-11 00:30:00,Harbor Fish Bar,-30.00,0.00,EUR,COMPLETED,70.00\n\
             Card Payment,Current,2026-04-12 08:00:00,2026-04-12 08:00:00,Metro,-2.10,0.00,EUR,COMPLETED,67.90\n\
             Card Payment,Current,2026-04-12 09:00:00,2026-04-12 09:00:00,Metro,-2.10,0.00,EUR,COMPLETED,65.80\n\
             Card Payment,Current,2026-04-12 18:00:00,2026-04-12 18:00:00,Metro,-2.10,0.00,EUR,COMPLETED,63.70\n\
             Card Payment,Current,2026-04-13 18:00:00,,Pending,-1.00,0.00,EUR,PENDING,\n\
             Card Payment,Current,2026-04-13 18:00:00,2026-04-13 18:00:00,Shop,-10.00,0.00,GBP,COMPLETED,40.00\n"
        ),
    );

    // --- N26: a subscription charged twice (two references), overlapping
    // exports, an interest row with no partner
    let n1 = "2026-05-01,2026-05-01,\"Streaming Co\",,Presentment,\"Sub 1\",\"Main Account\",-9.99,,,\n\
              2026-05-01,2026-05-01,\"Streaming Co\",,Presentment,\"Sub 2\",\"Main Account\",-9.99,,,\n\
              2026-05-02,2026-05-02,\"Main Account\",,\"Credit Transfer\",,House,700.00,,,\n";
    let n2 = "2026-05-03,2026-05-02,,,Interest,,\"Instant Savings\",1.5,,,\n\
              2026-05-04,2026-05-04,House,,\"Debit Transfer\",,\"Main Account\",-700,,,\n";
    write(tx, "n26-may-a.csv", &format!("{N26H}{n1}"));
    write(tx, "n26-may-b.csv", &format!("{N26H}{n1}{n2}"));

    // --- card: two identical coffees; the second export repeats them (with
    // the channel prefix spelled differently) and adds one row
    let c1 = "01/06/2026,02/06/2026,SALES:   COFFEE PLACE   HONG KONG HK,\"-38.00\"\t,HKD,POSTED,X,DEBIT\r\n\
              01/06/2026,02/06/2026,SALES:   COFFEE PLACE   HONG KONG HK,\"-38.00\"\t,HKD,POSTED,X,DEBIT\r\n\
              03/06/2026,03/06/2026,PAID BY AUTOPAY - THANK YOU,\"1,000.00\"\t,HKD,POSTED,X,CREDIT\r\n";
    let c2 = "01/06/2026,02/06/2026,GPAY COFFEE PLACE HONG KONG HK,-38.00,HKD,POSTED,X,DEBIT\r\n\
              01/06/2026,02/06/2026,GPAY COFFEE PLACE HONG KONG HK,-38.00,HKD,POSTED,X,DEBIT\r\n\
              04/06/2026,05/06/2026,BOOK SHOP HONG KONG HK,-120.00,HKD,POSTED,X,DEBIT\r\n\
              05/06/2026,06/06/2026,PENDING THING,-1.00,HKD,PENDING,X,DEBIT\r\n";
    write(tx, "TransactionHistory(7).csv", &format!("{CARDH}{c1}"));
    write(tx, "TransactionHistory(8).csv", &format!("{CARDH}{c2}"));

    // --- legacy Revolut GBP, BTC; Trezor; SMS: each also as an exact copy
    let gbp = format!(
        "{GBPH}22 Jun 2021 , Corner Shop, Fee: 0.10  , 11.50 ,  ,  ,  , 88.50, Shopping, \n\
         1 Jun 2021 , Top-Up by *1234 ,  , 100.00 ,  ,  , 100.00, General, \n"
    );
    write(tx, "Revolut-GBP-Statement-1.csv", &gbp);
    write(tx, "Revolut-GBP-Statement-1 (2).csv", &gbp);
    let btc = format!(
        "{BTCH}25 Apr 2021 , Bought BTC with USD ,  , 0.00150000 ,  USD 50.00 ,  BTC 0.0015 , 0.0015, General, \n"
    );
    write(tx, "Revolut-BTC-Statement-1.csv", &btc);
    write(tx, "Revolut-BTC-Statement-1 (2).csv", &btc);
    let trez = format!("{TREZH}1770000000,x,x,RECV,abcdefabcdefabcdef,,,addr,,0.001,BTC,100.50,\n");
    write(tx, "Bitcoin_1.csv", &trez);
    write(tx, "Bitcoin_2.csv", &trez);
    let sms = "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\r\n<smses count=\"1\">\r\n\
               <sms date=\"1780000000000\" date_sent=\"0\" body=\"12.345 UNITS U50004 SUBSCRIBED AT HKD 20.1234 PER UNIT. P111111\" />\r\n\
               </smses>\r\n";
    write(tx, "sms-1.xml", sms);
    write(tx, "sms-2.xml", sms);
}

fn bank_count(dbp: &Path) -> i64 {
    count(dbp, "SELECT COUNT(*) FROM bank_transactions")
}

fn assert_expected(dbp: &Path) {
    for (src, n) in EXPECTED {
        let got = count(dbp, &format!("SELECT COUNT(*) FROM bank_transactions WHERE source='{src}'"));
        assert_eq!(got, n, "{src}");
    }
    assert_eq!(count(dbp, "SELECT COUNT(*) FROM crypto_events"), 2);
    assert_eq!(count(dbp, "SELECT COUNT(*) FROM fund_subscriptions"), 1);
}

fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = tmp_dir(name);
    let tx = root.join("tx");
    make_tree(&tx);
    (root.clone(), tx, root.join("finance.db"))
}

#[test]
fn full_tree_has_exactly_the_real_rows() {
    let (_root, tx, dbp) = setup("dd-full");
    ingest(&dbp, &tx);
    assert_expected(&dbp);
    // the three day-3 fares and both day-5 fares survive
    assert_eq!(count(&dbp, "SELECT COUNT(*) FROM bank_transactions WHERE description='OVPAY FARE'"), 5);
    assert_eq!(count(&dbp, "SELECT COUNT(*) FROM bank_transactions WHERE description='Metro'"), 3);
    // categories assigned after ingest
    assert_eq!(count(&dbp, "SELECT COUNT(*) FROM bank_transactions WHERE category IS NULL"), 0);
}

#[test]
fn rerun_and_touch_add_nothing() {
    let (_root, tx, dbp) = setup("dd-rerun");
    ingest(&dbp, &tx);
    let before = rows(&dbp, BANK);
    assert_eq!(ingest(&dbp, &tx), 0);
    assert_eq!(ingest(&dbp, &tx), 0);
    // touched mtime: re-hashed (same content) but nothing new, and the
    // fresh stat is recorded so the next run skips it unread
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    for f in std::fs::read_dir(&tx).unwrap() {
        let f = std::fs::File::open(f.unwrap().path()).unwrap();
        f.set_modified(later).unwrap();
    }
    let conn = db::connect(&dbp).unwrap();
    let s = ingest::run_ingest_dir(&conn, &tx).unwrap();
    assert!(s.iter().all(|(_, s)| s.files_new == 0 && s.rows_added == 0), "{s:?}");
    assert!(ingest::run_ingest_dir(&conn, &tx).unwrap().is_empty(), "stat fast path");
    assert_eq!(rows(&dbp, BANK), before);
}

#[test]
fn copies_under_new_names_add_nothing() {
    let (_root, tx, dbp) = setup("dd-copy");
    ingest(&dbp, &tx);
    let before = (rows(&dbp, BANK), rows(&dbp, CRYPTO), rows(&dbp, FUNDS));
    for f in std::fs::read_dir(&tx).unwrap().collect::<Vec<_>>() {
        let p = f.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let (stem, ext) = name.rsplit_once('.').unwrap();
        std::fs::copy(&p, tx.join(format!("{stem} copy.{ext}"))).unwrap();
    }
    assert_eq!(ingest(&dbp, &tx), 0);
    assert_eq!((rows(&dbp, BANK), rows(&dbp, CRYPTO), rows(&dbp, FUNDS)), before);
}

#[test]
fn every_file_order_gives_the_same_rows() {
    // one file per run, forward and reversed: same row count per source,
    // and the full tree on top adds nothing
    let (root, tx, dbp) = setup("dd-order");
    ingest(&dbp, &tx);
    let mut names: Vec<String> =
        std::fs::read_dir(&tx).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names.reverse();
    let one = root.join("one");
    let rdb = root.join("rev.db");
    for n in &names {
        std::fs::create_dir_all(&one).unwrap();
        std::fs::copy(tx.join(n), one.join(n)).unwrap();
        ingest(&rdb, &one);
    }
    assert_expected(&rdb);
    assert_eq!(bank_count(&rdb), bank_count(&dbp));
    assert_eq!(ingest(&rdb, &tx), 0);
}

#[test]
fn newest_first_export_keeps_a_second_identical_fare() {
    // the fixed hole: export B lists the new fare
    // (lower balance) before the re-exported one
    let root = tmp_dir("dd-hole");
    let tx = root.join("tx");
    let dbp = root.join("finance.db");
    let first = chain(100.0, &[("03/03/2026", "OVPAY FARE", -3.0)]);
    write(&tx, "TransactionHistory-a.csv", &hsbc_csv(&first));
    ingest(&dbp, &tx);
    let both = chain(100.0, &[("03/03/2026", "OVPAY FARE", -3.0), ("03/03/2026", "OVPAY FARE", -3.0)]);
    write(&tx, "TransactionHistory-b.csv", &hsbc_csv(&both));
    assert_eq!(ingest(&dbp, &tx), 1);
    assert_eq!(bank_count(&dbp), 2);
}

#[test]
fn overlapping_slices_add_nothing() {
    // every bank export cut into overlapping halves under new names
    let (_root, tx, dbp) = setup("dd-slices");
    ingest(&dbp, &tx);
    let before = rows(&dbp, BANK);
    for f in std::fs::read_dir(&tx).unwrap().collect::<Vec<_>>() {
        let p = f.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".csv") || name.contains("BTC") || name.starts_with("Bitcoin") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
        let header = lines.remove(0);
        let mid = lines.len() / 2;
        let (stem, _) = name.rsplit_once('.').unwrap();
        write(&tx, &format!("{stem} head.csv"), &format!("{header}{}", lines[..=mid.min(lines.len().saturating_sub(1))].concat()));
        write(&tx, &format!("{stem} tail.csv"), &format!("{header}{}", lines[mid.saturating_sub(1)..].concat()));
    }
    assert_eq!(ingest(&dbp, &tx), 0);
    assert_eq!(rows(&dbp, BANK), before);
}

/// FNV-1a 64 digests of the stored rows (bank, crypto, funds,
/// ingested_files), recorded when this tree's database was verified row for
/// row against an independent implementation of the same ingest. A change
/// means stored rows or their keys moved: existing databases would re-insert
/// or orphan rows, so never just re-record them.
/// (funds and ingested_files re-recorded 2026-10-07 when the tree's SMS
/// fixture swapped real units and price for made-up ones: inputs, not code;
/// ingested_files again 2026-10-20 when a real merchant in a fixture got a made-up name;
/// bank and ingested_files 2026-10-27 when the Revolut fixture's real merchant did too)
const BUILT_DB: [u64; 4] = [0xe4fec5e761cec568, 0x29aa86df0ef5b5eb, 0xf05c3ad256e1fbf5, 0x54f0040f5277a9a5];

#[test]
fn the_built_db_is_pinned() {
    let (_root, tx, dbp) = setup("dd-pinned");
    ingest(&dbp, &tx);
    let files = "SELECT replace(path, rtrim(path, replace(path, '/', '')), ''), source, file_hash, row_count, \
                 rows_added, size FROM ingested_files";
    let got = [BANK, CRYPTO, FUNDS, files].map(|q| digest(&rows(&dbp, q)));
    assert_eq!(got, BUILT_DB, "stored rows changed: got {got:#018x?}");
}

/// Revolut's current export skips the rows the legacy GBP statement holds:
/// same currency and cent amount, a day apart at most, one to one, and only
/// up to the legacy statement's last day.
#[test]
fn revolut_new_skips_legacy_twins() {
    use finance_engine::sources::{revolut::RevolutNewSource, FileSource};
    let d = tmp_dir("d-legacy");
    let tx = d.join("tx");
    write(
        &tx,
        "revolut-gbp-old.csv",
        "Completed Date , Description , Paid Out (GBP) , Paid In (GBP) , Exchange Out, Exchange In, Balance (GBP), Category, Notes\n\
         1 Mar 2021,Cafe,5.00,,,,95.00,,\n\
         2 Mar 2021,Cafe,5.00,,,,90.00,,\n\
         9 Mar 2021,Salary,,100.00,,,190.00,,\n",
    );
    let new_file = write(
        &d,
        "account-statement_new.csv",
        "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\n\
         Card Payment,Current,2021-03-02 08:00:00,2021-03-02 08:00:00,Cafe,-5.00,0,GBP,COMPLETED,95.00\n\
         Card Payment,Current,2021-03-02 09:00:00,2021-03-02 09:00:00,Cafe,-5.00,0,GBP,COMPLETED,90.00\n\
         Card Payment,Current,2021-03-03 09:00:00,2021-03-03 09:00:00,Cafe,-5.00,0,GBP,COMPLETED,85.00\n\
         Topup,Current,2021-03-10 09:00:00,2021-03-10 09:00:00,Salary,100.00,0,GBP,COMPLETED,185.00\n\
         Topup,Current,2021-03-01 09:00:00,2021-03-01 09:00:00,Other,7.00,0,GBP,COMPLETED,97.00\n\
         Card Payment,Current,2021-03-02 09:00:00,2021-03-02 09:00:00,Abroad,-5.00,0,EUR,COMPLETED,10.00\n",
    );
    let dbp = d.join("t.db");
    assert_eq!(ingest(&dbp, &tx), 3);
    let conn = db::connect(&dbp).unwrap();
    let spec = RevolutNewSource.spec().clone();
    let parsed = RevolutNewSource.parse(&[new_file]).unwrap();
    let load = ingest::load_parsed(&conn, &spec, parsed, "n.csv", 0).unwrap();
    // two cafes pair with the two legacy cafes (the 2nd and 3rd Mar rows are
    // a day from the 1st/2nd); the third is one too many
    assert_eq!((load.rows, load.legacy_twins), (6, 2));
    assert_eq!(load.added, 4, "the surplus cafe, the after-legacy salary, the unmatched amount, the EUR row");
}
