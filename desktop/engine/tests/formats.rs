//! Format definitions end to end: a synthetic bank's CSV described in TOML
//! ingests, hashes exactly like a code source's rows, re-ingests to 0, and
//! an overlapping export adds only its new rows. Excel too, and the
//! built-in examples against their synthetic samples.
//!
//! The formats directory is process-global (`format::set_dirs`), so every
//! test here holds one lock.

mod ingest_common;

use std::path::Path;
use std::sync::Mutex;

use finance_engine::ingest::{natural_key, raw_hash};
use finance_engine::preview;
use finance_engine::sources::format::{self, FormatDef};
use finance_engine::sources::{self, BalanceMethod, BankRow, Parsed};
use ingest_common::*;

static LOCK: Mutex<()> = Mutex::new(());

const ACME: &str = r#"
id = "acme_bank"
label = "Acme Bank"
institution = "Acme Bank plc"
country = "NL"
kind = "bank"
currency = "EUR"

[detect]
columns = ["Boekdatum", "Omschrijving", "Bedrag", "Saldo"]

[columns]
date = "Boekdatum"
amount = "Bedrag"
description = ["Omschrijving", "Tegenrekening"]
balance = "Saldo"

[parse]
date_format = "%d-%m-%Y"
decimal = ","
"#;

const HEAD: &str = "Acme Bank export\nRekening;NL00ACME0123456789\n\nBoekdatum;Omschrijving;Tegenrekening;Bedrag;Saldo\n";
const R1: &str = "01-03-2026;Albert Heijn 1234;;-23,45;1.976,55\n";
const R2: &str = "02-03-2026;Salaris maart;NL99EXMP0000000001;2.500,00;4.476,55\n";
const R3: &str = "03-03-2026;Huur;NL99EXMP0000000002;-1.200,00;3.276,55\n";
const R4: &str = "05-03-2026;NS Reizigers;;-4,10;3.272,45\n";

fn with_formats(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("acme_bank.toml"), ACME).unwrap();
    format::set_dirs(vec![dir.to_path_buf()]);
}

fn bank(p: Parsed) -> Vec<BankRow> {
    match p {
        Parsed::Bank(r) => r,
        other => panic!("not bank rows: {other:?}"),
    }
}

#[test]
fn a_definition_ingests_dedupes_and_overlaps() {
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let d = tmp_dir("fmt-ingest");
    with_formats(&d.join("formats"));
    let tx = d.join("transactions");
    let dbp = d.join("finance.db");

    // the definition claims the file by its header (below a preamble)
    write(&tx, "export-march.csv", &format!("{HEAD}{R1}{R2}{R3}"));
    let buckets = sources::classify(&[tx.join("export-march.csv")]);
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].0.spec().key, "acme_bank");
    assert_eq!(buckets[0].0.spec().balance_method, BalanceMethod::Reported);

    // rows hash like a code source's: source key = the definition's id
    let rows = bank(buckets[0].0.parse(&buckets[0].1).unwrap());
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].txn_date, "2026-03-01");
    assert_eq!(rows[0].amount, Some(-23.45));
    assert_eq!(rows[0].balance, Some(1976.55));
    assert_eq!(rows[0].description, "Albert Heijn 1234");
    assert_eq!(rows[1].description, "Salaris maart NL99EXMP0000000001");
    assert_eq!(rows[1].amount, Some(2500.0));
    assert_eq!(natural_key("acme_bank", &rows[0]), "acme_bank||EUR|2026-03-01|-23.45|1976.55|Albert Heijn 1234");
    assert_eq!(raw_hash("acme_bank", &rows[0]), md5_of("acme_bank||EUR|2026-03-01|-23.45|1976.55|Albert Heijn 1234"));

    assert_eq!(ingest(&dbp, &tx), 3);
    let stored = rows_of(&dbp, "SELECT raw_hash FROM bank_transactions WHERE source='acme_bank'");
    let mut want: Vec<String> = rows.iter().map(|r| raw_hash("acme_bank", r)).collect();
    want.sort();
    assert_eq!(stored, want, "stored hashes are the natural-key hashes");

    // the same file again, and an identical copy under another name: 0
    assert_eq!(ingest(&dbp, &tx), 0);
    write(&tx, "export-march-copy.csv", &format!("{HEAD}{R1}{R2}{R3}"));
    assert_eq!(ingest(&dbp, &tx), 0);

    // an overlapping export (R2, R3 again + R4 new): only R4 is added
    write(&tx, "export-later.csv", &format!("{HEAD}{R2}{R3}{R4}"));
    assert_eq!(ingest(&dbp, &tx), 1);
    assert_eq!(count(&dbp, "SELECT COUNT(*) FROM bank_transactions WHERE source='acme_bank'"), 4);

    // the dry run (the Upload page) agrees: nothing new in any of them
    let replica = preview::replica_copy(&dbp).unwrap();
    for f in ["export-march.csv", "export-later.csv"] {
        let pf = preview::parse_file(&tx.join(f)).unwrap().unwrap();
        assert_eq!(pf.source.key, "acme_bank");
        assert_eq!(preview::dedupe(&replica, &pf, f).unwrap().added, 0, "{f}");
    }
    // and the views know the source by its definition
    assert_eq!(finance_engine::views::spec("acme_bank").unwrap().label, "Acme Bank");
    format::set_dirs(vec![]);
}

fn md5_of(s: &str) -> String {
    use md5::Digest as _;
    hex::encode(md5::Md5::digest(s.as_bytes()))
}

fn rows_of(dbp: &Path, sql: &str) -> Vec<String> {
    let c = rusqlite::Connection::open(dbp).unwrap();
    let mut st = c.prepare(sql).unwrap();
    let mut v: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    v.sort();
    v
}

#[test]
fn code_sources_win_and_unknown_files_stay_unknown() {
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let d = tmp_dir("fmt-precedence");
    // a definition whose signature an N26 export also has
    let greedy = ACME.replace(r#"columns = ["Boekdatum", "Omschrijving", "Bedrag", "Saldo"]"#, r#"columns = ["Booking Date"]"#);
    std::fs::create_dir_all(d.join("formats")).unwrap();
    std::fs::write(d.join("formats/acme_bank.toml"), greedy).unwrap();
    format::set_dirs(vec![d.join("formats")]);
    let n26 = write(&d, "export.csv", "\"Booking Date\",\"Value Date\",\"Partner Name\",\"Partner Iban\",Type,\"Payment Reference\",\"Account Name\",\"Amount (EUR)\"\n");
    assert_eq!(sources::source_for(&n26).unwrap().spec().key, "n26");
    let other = write(&d, "other.csv", "what,is,this\n1,2,3\n");
    assert!(sources::source_for(&other).is_none());
    assert_eq!(preview::detect(&other).err(), Some(preview::Unsupported::Unknown));
    assert!(preview::mappable(&other));
    // a broken user file is reported, not fatal; the built-ins still load
    std::fs::write(d.join("formats/broken.toml"), "id = \"broken\"\n").unwrap();
    format::invalidate();
    assert!(format::load_errors().iter().any(|e| e.contains("broken.toml")));
    assert!(format::get("monzo").is_some());
    format::set_dirs(vec![]);
}

#[test]
fn excel_with_debit_and_credit_columns() {
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let d = tmp_dir("fmt-xlsx");
    let def = r#"
id = "example_xlsx"
label = "Example Card"
kind = "card"
currency = "USD"

[detect]
columns = ["Posted", "Details", "Debit", "Credit"]

[columns]
date = "Posted"
debit = "Debit"
credit = "Credit"
description = "Details"

[parse]
date_format = "%m/%d/%Y"
sheet = "Activity"
"#;
    std::fs::create_dir_all(d.join("formats")).unwrap();
    std::fs::write(d.join("formats/example_xlsx.toml"), def).unwrap();
    format::set_dirs(vec![d.join("formats")]);

    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
    let mut wb = Workbook::new();
    wb.add_worksheet().set_name("Summary").unwrap().write_string(0, 0, "nothing here").unwrap();
    let ws = wb.add_worksheet().set_name("Activity").unwrap();
    ws.write_string(0, 0, "Statement").unwrap();
    for (c, h) in ["Posted", "Details", "Debit", "Credit"].iter().enumerate() {
        ws.write_string(2, c as u16, *h).unwrap();
    }
    let datef = Format::new().set_num_format("mm/dd/yyyy");
    // a real date cell, a text date, a credit
    ws.write_datetime_with_format(3, 0, ExcelDateTime::from_ymd(2026, 3, 1).unwrap(), &datef).unwrap();
    ws.write_string(3, 1, "COFFEE SHOP").unwrap();
    ws.write_number(3, 2, 4.5).unwrap();
    ws.write_string(4, 0, "03/02/2026").unwrap();
    ws.write_string(4, 1, "BOOKSTORE").unwrap();
    ws.write_number(4, 2, 1234.5).unwrap();
    ws.write_datetime_with_format(5, 0, ExcelDateTime::from_ymd(2026, 3, 5).unwrap(), &datef).unwrap();
    ws.write_string(5, 1, "PAYMENT THANK YOU").unwrap();
    ws.write_number(5, 3, 500).unwrap();
    ws.write_string(7, 0, "Total").unwrap();
    let path = d.join("activity.xlsx");
    wb.save(&path).unwrap();

    let src = sources::source_for(&path).expect("the definition claims the workbook");
    assert_eq!(src.spec().key, "example_xlsx");
    assert_eq!(src.spec().balance_method, BalanceMethod::None, "a card");
    let rows = bank(src.parse(&[path.clone()]).unwrap());
    let got: Vec<(&str, Option<f64>, &str)> = rows.iter().map(|r| (r.txn_date.as_str(), r.amount, r.description.as_str())).collect();
    assert_eq!(
        got,
        vec![
            ("2026-03-01", Some(-4.5), "COFFEE SHOP"),
            ("2026-03-02", Some(-1234.5), "BOOKSTORE"),
            ("2026-03-05", Some(500.0), "PAYMENT THANK YOU"),
        ]
    );
    // ingest reads .xlsx from the transactions dir
    let tx = d.join("transactions");
    std::fs::create_dir_all(&tx).unwrap();
    std::fs::copy(&path, tx.join("activity.xlsx")).unwrap();
    let dbp = d.join("finance.db");
    assert_eq!(ingest(&dbp, &tx), 3);
    assert_eq!(ingest(&dbp, &tx), 0);

    // the mapper's guess on the same workbook's sheet
    let draft = format::guess(&path, &sources::grid::ReadOpts { sheet: Some("Activity".into()), ..Default::default() }).unwrap();
    assert_eq!(draft.header, 2);
    assert_eq!(draft.def.columns.date, "Posted");
    assert_eq!(draft.def.columns.debit.as_deref(), Some("Debit"));
    assert_eq!(draft.def.columns.credit.as_deref(), Some("Credit"));
    assert_eq!(draft.def.columns.description.0, vec!["Details".to_string()]);
    assert_eq!(draft.sheets, vec!["Summary".to_string(), "Activity".into()]);
    format::set_dirs(vec![]);
}

#[test]
fn the_mapper_guesses_a_csv_and_its_definition_round_trips() {
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let d = tmp_dir("fmt-guess");
    format::set_dirs(vec![d.join("formats")]);
    let path = write(&d, "unknown-bank.csv", &format!("{HEAD}{R1}{R2}{R3}"));
    assert!(sources::source_for(&path).is_none());
    let draft = format::guess(&path, &Default::default()).unwrap();
    assert_eq!(draft.header, 3);
    assert_eq!(draft.columns, vec!["Boekdatum", "Omschrijving", "Tegenrekening", "Bedrag", "Saldo"]);
    assert_eq!(draft.def.parse.decimal, ",");
    assert_eq!(draft.date_formats, vec!["%d-%m-%Y"]);
    // Dutch names are in the guess lists; the user still checks them
    assert_eq!(draft.def.columns.date, "Boekdatum");
    assert_eq!(draft.def.columns.amount.as_deref(), Some("Bedrag"));
    // one bad date doesn't hide the pattern
    let mut bad = draft.rows.clone();
    bad.push(vec![sources::grid::Val::Text("31-13-2026".into())]);
    let col: Vec<sources::grid::Val> = bad[4..].iter().map(|r| r[0].clone()).collect();
    assert_eq!(format::detect_date_formats(&col), vec!["%d-%m-%Y"]);
    let mut def = draft.def.clone();
    def.id = format::slug("Acme Bank (NL)");
    def.label = "Acme Bank".into();
    def.currency = Some("EUR".into());
    def.columns.date = "Boekdatum".into();
    def.columns.amount = Some("Bedrag".into());
    (def.columns.debit, def.columns.credit) = (None, None);
    def.columns.balance = Some("Saldo".into());
    def.columns.description = format::Names(vec!["Omschrijving".into()]);
    def.detect.columns = vec!["Boekdatum".into(), "Omschrijving".into(), "Bedrag".into(), "Saldo".into()];
    let preview = def.rows(&draft.rows).unwrap();
    assert_eq!(preview.len(), 3);
    assert!(preview.iter().all(|r| matches!(r.row, Ok(Some(_)))));
    // a bad date override shows on every row, without failing the preview
    let mut wrong = def.clone();
    wrong.parse.date_format = format::Names(vec!["%m/%d/%Y".into()]);
    assert!(wrong.rows(&draft.rows).unwrap().iter().all(|r| r.row.is_err()));
    // saved, it takes effect at once and claims the file
    let saved = format::save(&d.join("formats"), &def).unwrap();
    assert!(saved.ends_with("acme_bank_nl.toml"));
    assert_eq!(FormatDef::from_toml(&std::fs::read_to_string(&saved).unwrap()).unwrap(), def);
    assert_eq!(sources::source_for(&path).unwrap().spec().key, "acme_bank_nl");
    // save_text keeps the text and checks the id
    assert!(format::save_text(&d.join("formats"), "other_id", &def.to_toml().unwrap()).is_err());
    format::set_dirs(vec![]);
}

#[test]
fn builtin_examples_read_their_samples() {
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let d = tmp_dir("fmt-builtin");
    format::set_dirs(vec![d.join("none")]);
    let samples = repo_root().join("desktop/docs/formats/samples");
    let monzo = samples.join("monzo-sample.csv");
    let src = sources::source_for(&monzo).unwrap();
    assert_eq!(src.spec().key, "monzo");
    let rows = bank(src.parse(&[monzo.clone()]).unwrap());
    assert_eq!(rows.len(), 6, "the two identical coffees stay two rows (dedup = Transaction ID)");
    assert_ne!(raw_hash("monzo", &rows[0]), raw_hash("monzo", &rows[1]));
    assert_eq!(rows[2].amount, Some(2450.0));
    let barclays = samples.join("barclays_uk-sample.csv");
    let src = sources::source_for(&barclays).unwrap();
    assert_eq!(src.spec().key, "barclays_uk");
    let rows = bank(src.parse(&[barclays]).unwrap());
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].account, "20-00-00 12345678");
    assert_eq!(rows[1].amount, Some(1850.0));
    format::set_dirs(vec![]);
}
