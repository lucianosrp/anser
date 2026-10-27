//! Per-parser unit tests on made-up exports (no real rows). Each one pins
//! the exact row shape the natural key is built from.

mod ingest_common;

use finance_engine::ingest::{natural_key, raw_hash};
use finance_engine::sources::{self, hsbc_card, mox, BankRow, CryptoRow, FileSource, FundRow, Parsed};
use ingest_common::{tmp_dir, write};

fn bank(p: Parsed) -> Vec<BankRow> {
    match p {
        Parsed::Bank(r) => r,
        other => panic!("expected bank rows, got {other:?}"),
    }
}

fn row(date: &str, amount: Option<f64>, balance: Option<f64>, desc: &str) -> BankRow {
    BankRow {
        account: String::new(),
        currency: "HKD".into(),
        txn_date: date.into(),
        amount,
        balance,
        description: desc.into(),
        dedup: String::new(),
    }
}

#[test]
fn hsbc_current_account() {
    let d = tmp_dir("p-hsbc");
    // CRLF, stray tabs after quoted money, thousands separators, padded
    // descriptions, an exact duplicate line and an empty Balance
    let f = write(
        &d,
        "TransactionHistory-test.csv",
        "Date,Description,Billing amount,Billing currency,Balance,Balance currency\r\n\
         02/07/2026,   ACME SHOP      HK   ,\"-1,250.50\"\t,HKD,\"10,000.00\"\t,HKD\r\n\
         02/07/2026,   ACME SHOP      HK   ,\"-1,250.50\"\t,HKD,\"10,000.00\"\t,HKD\r\n\
         01/07/2026,SALARY DEPOSIT,30000,HKD,11250.5,HKD\r\n\
         30/06/2026,PENDING THING,-5.00,HKD,,HKD\r\n",
    );
    let src = sources::hsbc::HsbcSource;
    assert!(src.matches(&f));
    let rows = bank(src.parse(&[f]).unwrap());
    assert_eq!(
        rows,
        vec![
            row("2026-07-02", Some(-1250.5), Some(10000.0), "ACME SHOP      HK"),
            row("2026-07-01", Some(30000.0), Some(11250.5), "SALARY DEPOSIT"),
            row("2026-06-30", Some(-5.0), None, "PENDING THING"),
        ]
    );
    // the key: shortest round-trip floats -- "-1250.5", "10000.0"
    assert_eq!(natural_key("hsbc", &rows[0]), "hsbc||HKD|2026-07-02|-1250.5|10000.0|ACME SHOP      HK");
    assert_eq!(natural_key("hsbc", &rows[2]), "hsbc||HKD|2026-06-30|-5.0||PENDING THING");
    // the card CSV shares the name but has no Balance column
    let card = write(&d, "TransactionHistory(1).csv", "Transaction date,Post date,Description,Billing amount\n");
    assert!(!src.matches(&card));
}

#[test]
fn hsbc_card_csv() {
    let d = tmp_dir("p-card");
    let f = write(
        &d,
        "TransactionHistory(9).csv",
        "Transaction date,Post date,Description,Billing amount,Billing currency,Transaction status,Merchant name,Credit / Debit\r\n\
         01/08/2026,02/08/2026,SALES:   COFFEE PLACE        HONG KONG    HK,\"-38.00\"\t,HKD,POSTED,X,DEBIT\r\n\
         01/08/2026,02/08/2026,GPAY COFFEE PLACE   HONG KONG HK,-38.00,HKD,POSTED,X,DEBIT\r\n\
         01/08/2026,02/08/2026,PENDING CAFE,-9.00,HKD,PENDING,X,DEBIT\r\n\
         28/07/2026,29/07/2026,PAID BY AUTOPAY - THANK YOU,\"1,234.50\",HKD,POSTED,X,CREDIT\r\n",
    );
    let src = hsbc_card::HsbcCardSource;
    assert!(src.matches(&f));
    let rows = bank(src.parse(&[f]).unwrap());
    let got: Vec<(&str, Option<f64>, &str, &str)> =
        rows.iter().map(|r| (r.txn_date.as_str(), r.amount, r.description.as_str(), r.dedup.as_str())).collect();
    assert_eq!(
        got,
        vec![
            // two identical coffees stay two rows: the occurrence number
            ("2026-08-01", Some(-38.0), "COFFEE PLACE HONG KONG HK", "2026-08-02|1"),
            ("2026-08-01", Some(-38.0), "COFFEE PLACE HONG KONG HK", "2026-08-02|2"),
            ("2026-07-28", Some(1234.5), "PAYMENT - THANK YOU", "2026-07-29|1"),
        ]
    );
    assert_ne!(raw_hash("hsbc_card", &rows[0]), raw_hash("hsbc_card", &rows[1]));
    assert_eq!(
        natural_key("hsbc_card", &rows[2]),
        "hsbc_card||HKD|2026-07-28|1234.5||PAYMENT - THANK YOU|2026-07-29|1"
    );
}

#[test]
fn hsbc_card_statement_text() {
    // pdftotext -layout output of a January statement: December lines belong
    // to the previous year; a form feed starts page 2
    let text = "Statement of HSBC Red Credit Card Account\n\
                Statement date      12 JAN 2027\n\
                \n\
                  28DEC       27DEC        COFFEE PLACE        HONG KONG    HK          38.00\n\
                  05JAN       03JAN        PAID BY AUTOPAY - THANK YOU        1,234.50CR\n\
                \x0c  06JAN       05JAN        BOOK SHOP           HONG KONG    HK         120.00\n\
                  06JAN       05JAN        BOOK SHOP           HONG KONG    HK         120.00\n\
                  07XXX       05JAN        NOT A ROW                                    1.00\n";
    let rows = hsbc_card::parse_statement_text(text).unwrap();
    let got: Vec<(&str, Option<f64>, &str, &str)> =
        rows.iter().map(|r| (r.txn_date.as_str(), r.amount, r.description.as_str(), r.dedup.as_str())).collect();
    assert_eq!(
        got,
        vec![
            ("2026-12-27", Some(-38.0), "COFFEE PLACE HONG KONG HK", "2026-12-28|1"),
            ("2027-01-03", Some(1234.5), "PAYMENT - THANK YOU", "2027-01-05|1"),
            ("2027-01-05", Some(-120.0), "BOOK SHOP HONG KONG HK", "2027-01-06|1"),
            ("2027-01-05", Some(-120.0), "BOOK SHOP HONG KONG HK", "2027-01-06|2"),
        ]
    );
    assert!(hsbc_card::parse_statement_text("no date here").is_err());
    assert_eq!(hsbc_card::canonical_description("  APAY  SOME  SHOP "), "SOME SHOP");
}

/// A one-line PDF whose `pdftotext -layout` contains `line`.
fn pdf_with(dir: &std::path::Path, name: &str, line: &str) -> std::path::PathBuf {
    let esc = line.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)");
    let content = format!("BT /F1 12 Tf 72 720 Td ({esc}) Tj ET\n");
    let stream = format!("<< /Length {} >>\nstream\n{}endstream\n", content.len(), content);
    let body = format!(
        "%PDF-1.4\n\
         1 0 obj<< /Type /Catalog /Pages 2 0 R >>endobj\n\
         2 0 obj<< /Type /Pages /Count 1 /Kids [3 0 R] >>endobj\n\
         3 0 obj<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
            /Resources << /Font << /F1 5 0 R >> >> >>endobj\n\
         4 0 obj{stream}\
         5 0 obj<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>endobj\n"
    );
    let xref_at = body.len();
    let pdf = format!(
        "{body}xref\n0 6\n0000000000 65535 f \n\
         trailer<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"
    );
    write(dir, name, &pdf)
}

#[test]
fn mox_statement_text() {
    // pdftotext -layout of a February statement: opening/closing are not
    // rows; a wrapped FX description joins; two identical coffees stay two
    // rows; a December line on a January statement belongs to last year.
    let feb = "\
Mox Bank statement
Statement period: 1 Feb 2026 - 28 Feb 2026
Statement date: 3 Mar 2026

HKD Mox Account HKD pocket
Activity    Settlement                                                               Corresponding
date        date          Description                                                                          Amount
01 Feb      01 Feb        Opening balance                                                                        0.00

17 Feb      17 Feb        JANE DOE                                                                       +10,000.00

17 Feb      17 Feb        HKD USD                                                     100.00 USD               -785.18
                          To USD Mox Invest
                          1 USD = 7.8518 HKD

18 Feb      18 Feb        COFFEE PLACE                                                                        -38.00
18 Feb      18 Feb        COFFEE PLACE                                                                        -38.00

28 Feb      28 Feb        Closing balance                                                                    9,138.82

USD Mox Account USD pocket
01 Feb      01 Feb        Opening balance                                                                        0.00
28 Feb      28 Feb        Closing balance                                                                        0.00

USD Mox Invest
01 Feb      01 Feb        Opening balance                                                                        0.00
17 Feb      17 Feb        From HKD Mox Account                                        785.18 HKD              +100.00
28 Feb      28 Feb        Closing balance                                                                      100.00
";
    let rows = mox::parse_statement_text(feb).unwrap();
    let got: Vec<(&str, &str, Option<f64>, Option<f64>, &str, &str)> = rows
        .iter()
        .map(|r| {
            (r.account.as_str(), r.txn_date.as_str(), r.amount, r.balance, r.description.as_str(), r.dedup.as_str())
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("HKD", "2026-02-17", Some(10000.0), Some(10000.0), "JANE DOE", "2026-02-17|1"),
            (
                "HKD",
                "2026-02-17",
                Some(-785.18),
                Some(9214.82),
                "HKD USD To USD Mox Invest 1 USD = 7.8518 HKD",
                "2026-02-17|1"
            ),
            ("HKD", "2026-02-18", Some(-38.0), Some(9176.82), "COFFEE PLACE", "2026-02-18|1"),
            ("HKD", "2026-02-18", Some(-38.0), Some(9138.82), "COFFEE PLACE", "2026-02-18|2"),
            ("USD Invest", "2026-02-17", Some(100.0), Some(100.0), "From HKD Mox Account", "2026-02-17|1"),
        ]
    );
    assert_eq!(rows[1].currency, "HKD");
    assert_eq!(rows[4].currency, "USD");
    assert_eq!(
        natural_key("mox", &rows[0]),
        "mox|HKD|HKD|2026-02-17|10000.0|10000.0|JANE DOE|2026-02-17|1"
    );
    assert_ne!(raw_hash("mox", &rows[2]), raw_hash("mox", &rows[3]));

    let jan = "\
Mox Bank statement
Statement period: 1 Jan 2027 - 31 Jan 2027
HKD Mox Account
31 Dec      31 Dec        YEAR END COFFEE                                                                    -12.00
05 Jan      05 Jan        PAYROLL                                                                        +1,000.00
";
    let rows = mox::parse_statement_text(jan).unwrap();
    assert_eq!(rows[0].txn_date, "2026-12-31");
    assert_eq!(rows[0].amount, Some(-12.0));
    assert_eq!(rows[0].balance, Some(-12.0));
    assert_eq!(rows[1].txn_date, "2027-01-05");
    assert_eq!(rows[1].balance, Some(988.0));

    assert!(mox::parse_statement_text("no period here").is_err());
}

#[test]
fn mox_statement_over_two_pages_and_its_closing_check() {
    // page 2 repeats the pocket heading and the column headings after a
    // form feed, with a page number at the foot of page 1: the balance runs
    // on, nothing is glued onto a description, and the closing figure agrees
    let two_pages = "\
Mox Bank statement
Statement period: 1 Mar 2026 - 31 Mar 2026
HKD Mox Account HKD pocket
Activity    Settlement                                                               Corresponding
date        date          Description                                                                          Amount
01 Mar      01 Mar        Opening balance                                                                      100.00
02 Mar      02 Mar        SALARY                                                                            +500.00
03 Mar      03 Mar        GROCER                                                                             -50.00
Page 1 of 2
\x0cMox Bank statement
HKD Mox Account HKD pocket
Activity    Settlement                                                               Corresponding
date        date          Description                                                                          Amount
04 Mar      04 Mar        TAXI                                                                               -20.00
31 Mar      31 Mar        Closing balance                                                                      530.00
";
    let rows = mox::parse_statement_text(two_pages).unwrap();
    let got: Vec<(&str, Option<f64>)> = rows.iter().map(|r| (r.description.as_str(), r.balance)).collect();
    assert_eq!(got, vec![("SALARY", Some(600.0)), ("GROCER", Some(550.0)), ("TAXI", Some(530.0))]);

    // a row read wrong (here a credit printed without its sign) no longer
    // adds up to the closing balance: the file is refused, not half-read
    let unsigned = two_pages.replace("+500.00", " 500.00");
    let err = mox::parse_statement_text(&unsigned).unwrap_err().to_string();
    assert!(err.contains("closing balance is 530.00"), "{err}");
}

#[test]
fn mox_pdf_matches() {
    if std::process::Command::new("pdftotext").arg("-v").output().is_err() {
        eprintln!("skipping: pdftotext not on PATH");
        return;
    }
    let d = tmp_dir("p-mox");
    let src = sources::mox::MoxSource;
    let yes = pdf_with(&d, "Pat_Feb2026_Mox_Bank_Statement.pdf", "Mox Bank statement");
    let no = pdf_with(&d, "2026-02-23_Statement.pdf", "Statement of HSBC Red Credit Card Account");
    let csv = write(&d, "mox.csv", "Date,Amount\n");
    assert!(src.matches(&yes));
    assert!(!src.matches(&no));
    assert!(!src.matches(&csv));
    assert!(!sources::hsbc_card::HsbcCardSource.matches(&yes));
}

#[test]
fn mox_real_pdf_if_pointed_at() {
    // local check: FINANCE_MOX_PDF=/path/to/statement.pdf. Never committed.
    let Some(p) = std::env::var_os("FINANCE_MOX_PDF").map(std::path::PathBuf::from) else {
        return;
    };
    if !p.exists() {
        panic!("FINANCE_MOX_PDF does not exist: {}", p.display());
    }
    let src = sources::mox::MoxSource;
    assert!(src.matches(&p), "should claim {}", p.display());
    let rows = bank(src.parse(std::slice::from_ref(&p)).unwrap());
    assert!(!rows.is_empty(), "expected activity rows");
    for r in &rows {
        assert!(!r.txn_date.is_empty());
        assert!(r.amount.is_some());
        assert!(r.balance.is_some());
        assert!(["HKD", "USD", "EUR", "GBP", "JPY", "AUD", "CAD", "CNY", "NZD", "SGD"]
            .iter()
            .any(|c| r.currency == *c || r.account.starts_with(c)));
    }
}

#[test]
fn n26() {
    let d = tmp_dir("p-n26");
    let h = "\"Booking Date\",\"Value Date\",\"Partner Name\",\"Partner Iban\",Type,\"Payment Reference\",\"Account Name\",\"Amount (EUR)\",\"Original Amount\",\"Original Currency\",\"Exchange Rate\"\n";
    let f = write(
        &d,
        "n26-test.csv",
        &format!(
            "{h}2026-05-01,2026-05-01,\"Some Shop\",,Presentment,,\"Main Account\",-9.5,9.5,EUR,1\n\
             2026-05-01,2026-05-01,\"Some Shop\",,Presentment,\"ref 2\",\"Main Account\",-9.5,9.5,EUR,1\n\
             2026-05-01,2026-05-01,\"Some Shop\",,Presentment,\"ref 2\",\"Main Account\",-9.5,9.5,EUR,1\n\
             2026-05-02,2026-05-01,,,Interest,,\"Instant Savings\",100,,,\n"
        ),
    );
    let src = sources::n26::N26Source;
    assert!(src.matches(&f));
    let rows = bank(src.parse(&[f]).unwrap());
    assert_eq!(rows.len(), 3, "the exact duplicate line collapses, distinct refs stay");
    assert_eq!(rows[0].dedup, "2026-05-01|");
    assert_eq!(rows[1].dedup, "2026-05-01|ref 2");
    assert_eq!(rows[2].amount, Some(100.0));
    assert_eq!(rows[2].description, "");
    // a dedup of just "|" is not appended (the historical key format);
    // "date|" is
    assert_eq!(natural_key("n26", &rows[0]), "n26|Main Account|EUR|2026-05-01|-9.5||Some Shop|2026-05-01|");
    assert_eq!(natural_key("n26", &rows[1]), "n26|Main Account|EUR|2026-05-01|-9.5||Some Shop|2026-05-01|ref 2");
    assert_eq!(natural_key("n26", &rows[2]), "n26|Instant Savings|EUR|2026-05-02|100.0|||2026-05-01|");

    // an all-numeric Payment Reference column is inferred as an integer
    let g = write(&d, "n26-numeric.csv", &format!("{h}2026-05-03,2026-05-03,X,,T,007,A,-1,,,\n"));
    let rows = bank(src.parse(&[g]).unwrap());
    assert_eq!(rows[0].dedup, "2026-05-03|7");
    assert_eq!(rows[0].amount, Some(-1.0));
}

#[test]
fn revolut_new() {
    let d = tmp_dir("p-revnew");
    let f = write(
        &d,
        "account-statement_test.csv",
        "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\n\
         Card Payment,Current,2026-07-01 23:50:00,2026-07-02 00:10:00,Some Cafe,-4.20,0.00,EUR,COMPLETED,95.80\n\
         Card Payment,Current,2026-07-01 23:50:00,,Reverted Cafe,-1.00,0.00,EUR,REVERTED,\n\
         Card Payment,Current,2026-07-03 10:00:00,2026-07-03 10:00:00,Train,-4620,0,JPY,COMPLETED,11977\n",
    );
    let src = sources::revolut::RevolutNewSource;
    assert!(src.matches(&f));
    let rows = bank(src.parse(&[f]).unwrap());
    assert_eq!(rows.len(), 2);
    assert_eq!(natural_key("revolut_new", &rows[0]), "revolut_new|EUR|EUR|2026-07-02|-4.2|95.8|Some Cafe");
    assert_eq!(natural_key("revolut_new", &rows[1]), "revolut_new|JPY|JPY|2026-07-03|-4620.0|11977.0|Train");
}

/// The Excel export reads as the CSV does: every row hashes the same, so the
/// two formats de-duplicate against each other.
#[test]
fn revolut_xlsx_hashes_like_the_csv() {
    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
    let d = tmp_dir("p-revxlsx");
    let csv = write(
        &d,
        "account-statement_csv.csv",
        "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\n\
         Card Payment,Current,2026-07-01 23:50:00,2026-07-02 00:10:00,Some Cafe,-4.20,0.00,GBP,COMPLETED,95.80\n\
         Transfer,Savings,2026-07-02 00:00:00,2026-07-02 00:00:00,To vault,10,0,GBP,COMPLETED,10\n\
         Card Payment,Current,2026-07-03 10:00:00,,Reverted,-1.00,0.00,GBP,REVERTED,\n\
         Topup,Current,2026-07-05 09:30:00,2026-07-05 09:30:15,Top-up by *1234,0.1,0.0,GBP,COMPLETED,95.9\n",
    );
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    for (c, h) in
        ["Type", "Product", "Started Date", "Completed Date", "Description", "Amount", "Fee", "Currency", "State", "Balance"].iter().enumerate()
    {
        ws.write_string(0, c as u16, *h).unwrap();
    }
    let dt = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let when = |s: &str| ExcelDateTime::parse_from_str(s).unwrap();
    let rows: [(&str, &str, &str, Option<&str>, &str, f64, f64, &str, Option<f64>); 4] = [
        ("Card Payment", "Current", "2026-07-01 23:50:00", Some("2026-07-02 00:10:00"), "Some Cafe", -4.2, 0.0, "COMPLETED", Some(95.8)),
        // a midnight completion is a bare date cell
        ("Transfer", "Savings", "2026-07-02 00:00:00", Some("2026-07-02 00:00:00"), "To vault", 10.0, 0.0, "COMPLETED", Some(10.0)),
        ("Card Payment", "Current", "2026-07-03 10:00:00", None, "Reverted", -1.0, 0.0, "REVERTED", None),
        ("Topup", "Current", "2026-07-05 09:30:00", Some("2026-07-05 09:30:15"), "Top-up by *1234", 0.1, 0.0, "COMPLETED", Some(95.9)),
    ];
    for (i, (ty, prod, st, done, desc, amt, fee, state, bal)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        ws.write_string(r, 0, *ty).unwrap();
        ws.write_string(r, 1, *prod).unwrap();
        ws.write_datetime_with_format(r, 2, when(st), &dt).unwrap();
        if let Some(done) = done {
            ws.write_datetime_with_format(r, 3, when(done), &dt).unwrap();
        }
        ws.write_string(r, 4, *desc).unwrap();
        ws.write_number(r, 5, *amt).unwrap();
        ws.write_number(r, 6, *fee).unwrap();
        ws.write_string(r, 7, "GBP").unwrap();
        ws.write_string(r, 8, *state).unwrap();
        if let Some(b) = bal {
            ws.write_number(r, 9, *b).unwrap();
        }
    }
    let xlsx = d.join("account-statement_x.xlsx");
    wb.save(&xlsx).unwrap();

    let src = sources::revolut::RevolutNewSource;
    assert!(src.matches(&xlsx));
    assert_eq!(sources::source_for(&xlsx).unwrap().spec().key, "revolut_new");
    let (from_csv, from_xlsx) = (bank(src.parse(&[csv]).unwrap()), bank(src.parse(&[xlsx]).unwrap()));
    assert_eq!(from_csv.len(), 3, "the reverted row is skipped");
    assert_eq!(from_csv, from_xlsx);
    assert_eq!(from_csv.iter().map(|r| r.account.as_str()).collect::<Vec<_>>(), ["GBP", "GBP Savings", "GBP"]);
    let hashes = |r: &[BankRow]| r.iter().map(|r| raw_hash("revolut_new", r)).collect::<Vec<_>>();
    assert_eq!(hashes(&from_csv), hashes(&from_xlsx));

    // a workbook that is not Revolut's is left alone, whatever its name
    let mut other = Workbook::new();
    other.add_worksheet().write_string(0, 0, "Date").unwrap();
    let other_path = d.join("account-statement_other.xlsx");
    other.save(&other_path).unwrap();
    assert!(!src.matches(&other_path));
    assert!(sources::source_for(&other_path).is_none());
}

/// A made-up `pdftotext -layout` Revolut statement: amounts left-aligned under
/// their column heading, continuation lines under a row, a vault table.
fn revolut_pdf_text(closing_account: &str) -> String {
    let at = |parts: &[(usize, &str)]| {
        let mut l = String::new();
        for (col, t) in parts {
            while l.chars().count() < *col {
                l.push(' ');
            }
            l += t;
        }
        l + "\n"
    };
    let head = at(&[(0, "Date"), (24, "Description"), (107, "Money out"), (137, "Money in"), (178, "Balance")]);
    let row = |d: &str, desc: &str, out: &str, inn: &str, bal: &str| {
        let mut p = vec![(0, d), (24, desc)];
        if !out.is_empty() {
            p.push((107, out));
        }
        if !inn.is_empty() {
            p.push((137, inn));
        }
        p.push((178, bal));
        at(&p) + "\n"
    };
    let mut t = String::from("Alex Example\n\nBalance summary\n\n");
    t += &at(&[(0, " Product"), (100, "Opening balance"), (140, "Money out"), (180, "Money in"), (220, "Closing balance")]);
    // out / in totals deliberately differ from the table (reversed pairs): only the net must tie
    t += &at(&[(1, "Account (E-Money)"), (100, "\u{a3}10.00"), (140, "\u{a3}99.00"), (180, "\u{a3}120.00"), (220, closing_account)]);
    t += &at(&[(1, "Personal and Group Vaults"), (100, "\u{a3}0.00"), (140, "\u{a3}0.00"), (180, "\u{a3}5.00"), (220, "\u{a3}5.00")]);
    t += "\nAccount transactions from 1 March 2021 to 30 April 2021\n\n";
    t += &head;
    t += &row("1 Mar 2021", "Top-Up by *1234", "", "\u{a3}100.00", "\u{a3}110.00");
    t += &row("2 Mar 2021", "Some Cafe", "\u{a3}4.20", "", "\u{a3}105.80");
    t += &at(&[(24, "Card: 123456******1234")]);
    t += &row("3 Mar 2021", "Exchanged to EUR", "\u{a3}10.00", "", "\u{a3}95.80");
    t += &at(&[(107, "\u{20ac}11.50")]);
    t += "\n\u{c}";
    t += &head; // the heading repeats on the next page
    t += &row("4 Mar 2021", "Refund", "", "\u{a3}20.00", "\u{a3}115.80");
    t += "\nPersonal and Group Vaults transactions from 1 March 2021 to 30 April 2021\n\n";
    t += &head;
    t += &row("5 Mar 2021", "To pocket", "", "\u{a3}5.00", "\u{a3}5.00");
    t
}

#[test]
fn revolut_pdf_text_rows_and_closing_check() {
    use sources::revolut_pdf::parse_statement_text;
    let rows = parse_statement_text(&revolut_pdf_text("\u{a3}115.80")).unwrap();
    let got: Vec<(&str, &str, &str, Option<f64>, Option<f64>)> = rows
        .iter()
        .map(|r| (r.account.as_str(), r.txn_date.as_str(), r.description.as_str(), r.amount, r.balance))
        .collect();
    assert_eq!(
        got,
        vec![
            ("GBP", "2021-03-01", "Top-Up by *1234", Some(100.0), Some(110.0)),
            ("GBP", "2021-03-02", "Some Cafe", Some(-4.2), Some(105.8)),
            ("GBP", "2021-03-03", "Exchanged to EUR", Some(-10.0), Some(95.8)),
            ("GBP", "2021-03-04", "Refund", Some(20.0), Some(115.8)),
            ("GBP Savings", "2021-03-05", "To pocket", Some(5.0), Some(5.0)),
        ]
    );
    // shaped like the CSV row: the same natural key
    assert_eq!(natural_key("revolut_new", &rows[1]), "revolut_new|GBP|GBP|2021-03-02|-4.2|105.8|Some Cafe");
    // a statement whose rows do not reach its closing balance is refused
    let err = parse_statement_text(&revolut_pdf_text("\u{a3}116.80")).unwrap_err().to_string();
    assert!(err.contains("Account") && err.contains("115.80"), "{err}");
    assert!(parse_statement_text("nothing here").is_err());
}

#[test]
fn revolut_legacy_gbp_and_btc() {
    let d = tmp_dir("p-revold");
    let h = "Completed Date , Description , Paid Out (GBP) , Paid In (GBP) , Exchange Out, Exchange In, Balance (GBP), Category, Notes\n";
    // an unquoted comma inside a description shifts a naive split
    let f = write(
        &d,
        "Revolut-GBP-Statement-test.csv",
        &format!(
            "{h}22 Jun 2021 , Corner Shop, Fee: 0.10  , 11.50 ,  ,  ,  , 701.17, Shopping, \n\
             1 Jun 2021 , \"\"Swan, Globe\"\" , , 20.00 ,  ,  , 712.67, General, \n\
             1 Jun 2021 , \"\"Swan, Globe\"\" , , 20.00 ,  ,  , 712.67, General, \n\
             \n"
        ),
    );
    let src = sources::revolut::RevolutOldGbpSource;
    assert!(src.matches(&f));
    let rows = bank(src.parse(&[f]).unwrap());
    assert_eq!(rows.len(), 2);
    assert_eq!(
        natural_key("revolut_old_gbp", &rows[0]),
        "revolut_old_gbp|GBP|GBP|2021-06-22|-11.5|701.17|Corner Shop, Fee: 0.10"
    );
    assert_eq!(rows[1].description, "Swan, Globe");
    assert_eq!(rows[1].amount, Some(20.0));

    let hb = "Completed Date , Description , Paid Out (BTC) , Paid In (BTC) , Exchange Out, Exchange In, Balance (BTC), Category, Notes\n";
    let b = write(
        &d,
        "Revolut-BTC-Statement-test.csv",
        &format!(
            "{hb}25 Apr 2021 , Bought BTC with GBP FX Rate 1 BTC = £30000.00, Fee: 0.00001 BTC ,  , 0.00150000 ,  GBP 50.00 ,  BTC 0.0015 , 0.0105, General, \n\
             20 Apr 2021 , Bought BTC with USD ,  , 0.001 ,  USD 100.00 ,  BTC 0.001 , 0.009, General, \n"
        ),
    );
    let src = sources::revolut::RevolutBtcSource;
    assert!(src.matches(&b));
    let Parsed::Crypto(ev) = src.parse(&[b]).unwrap() else { panic!() };
    assert_eq!(
        ev,
        vec![
            CryptoRow {
                ts: 1619308800000,
                asset: "BTC".into(),
                delta: 0.0015,
                usd: None,
                kind: "revolut-btc".into(),
                detail: "Bought BTC with GBP FX Rate 1 BTC = £30000.00, Fee: 0.00001 BTC | exchange_out=GBP 50.00".into(),
            },
            CryptoRow {
                ts: 1618876800000,
                asset: "BTC".into(),
                delta: 0.001,
                usd: Some(100.0),
                kind: "revolut-btc".into(),
                detail: "Bought BTC with USD | exchange_out=USD 100.00".into(),
            },
        ]
    );
}

#[test]
fn trezor() {
    let d = tmp_dir("p-trezor");
    let f = write(
        &d,
        "Bitcoin_test.csv",
        "Timestamp,Date,Time,Type,Transaction ID,Fee,Fee unit,Address,Label,Amount,Amount unit,Fiat (USD),Other\n\
         1770000000,x,x,RECV,aaaaaaaaaaaaaaaaaaaa,,,addr,,0.001,BTC,\"1,000.50\",\n\
         1770000000,x,x,RECV,aaaaaaaaaaaaaaaaaaaa,,,addr,,0.001,BTC,\"1,000.50\",\n\
         1770100000,x,x,SENT,bbbbbbbbbbbbbbbbbbbb,,,addr,cold move,0.0005,BTC,,\n",
    );
    let src = sources::trezor::TrezorSource;
    assert!(src.matches(&f));
    let Parsed::Crypto(ev) = src.parse(&[f]).unwrap() else { panic!() };
    assert_eq!(
        ev,
        vec![
            CryptoRow {
                ts: 1770000000000,
                asset: "BTC".into(),
                delta: 0.001,
                usd: Some(1000.5),
                kind: "trezor-tx".into(),
                detail: "RECV txid=aaaaaaaaaaaa".into(),
            },
            CryptoRow {
                ts: 1770100000000,
                asset: "BTC".into(),
                delta: -0.0005,
                usd: None,
                kind: "trezor-tx".into(),
                detail: "SENT: cold move txid=bbbbbbbbbbbb".into(),
            },
        ]
    );
}

#[test]
fn hsbc_sms() {
    let d = tmp_dir("p-sms");
    let f = write(
        &d,
        "sms-test.xml",
        "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\r\n<smses count=\"4\">\r\n\
         <sms date=\"1780000000000\" date_sent=\"0\" body=\"HSBC: 12.345 UNITS U50004 SUBSCRIBED AT HKD 20.1234 PER UNIT. P111111 &amp; more\" />\r\n\
         <sms date=\"1780000000000\" date_sent=\"1780090000000\" body=\"1,000.5 units u50004 redeemed at HKD 1,001.25 per unit P222222\" />\r\n\
         <sms date=\"1780000000000\" body=\"2 UNITS U60001 SWITCHED&#10;OUT AT HKD 10 PER UNIT. P333333\" />\r\n\
         <sms date=\"1780000000000\" body=\"unrelated text\" />\r\n</smses>\r\n",
    );
    let src = sources::hsbc_sms::HsbcSmsSource;
    assert!(src.matches(&f));
    let Parsed::FundUnits(rows) = src.parse(&[f]).unwrap() else { panic!() };
    let fr = |r: &str, d: &str, f: &str, u: f64, p: f64| FundRow {
        reference: r.into(),
        trade_date: d.into(),
        fund: f.into(),
        units: u,
        px: p,
    };
    assert_eq!(
        rows,
        vec![
            // date_sent '0' falls back to date
            fr("P111111", "2026-05-28", "U50004", 12.345, 20.1234),
            fr("P222222", "2026-05-29", "U50004", -1000.5, 1001.25),
            fr("P333333", "2026-05-28", "U60001", -2.0, 10.0),
        ]
    );
    // malformed XML: the whole file is skipped, like ElementTree's ParseError
    let bad = write(&d, "sms-bad.xml", "<smses><sms body=\"1 UNITS U1 SUBSCRIBED AT HKD 1 PER UNIT P1\"></smsx>");
    let Parsed::FundUnits(rows) = src.parse(&[bad]).unwrap() else { panic!() };
    assert!(rows.is_empty());
}

#[test]
fn hsbc_sms_pasted_text_round_trip() {
    use finance_core::paste;
    let d = tmp_dir("p-sms-paste");
    // two messages pasted together, one with characters that need escaping
    let text = "HSBC: 12.345 UNITS U50004 SUBSCRIBED AT HKD 20.1234 PER UNIT. P123456 \"Tom & Jerry\" <ok>\r\n\r\n\
                1,000.5 units u50004 redeemed at HKD 1,001.25 per unit P222222\nsee 'terms'";
    let msgs = paste::confirmations_in(text);
    assert_eq!(msgs.len(), 2);
    let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
    let xml = paste::sms_xml(&msgs, date);
    let f = write(&d, &paste::file_name(), &xml);
    let src = sources::hsbc_sms::HsbcSmsSource;
    assert!(src.matches(&f));
    let Parsed::FundUnits(rows) = src.parse(&[f]).unwrap() else { panic!() };
    let fr = |r: &str, f: &str, u: f64, p: f64| FundRow {
        reference: r.into(),
        trade_date: "2026-09-14".into(),
        fund: f.into(),
        units: u,
        px: p,
    };
    assert_eq!(rows, vec![fr("P123456", "U50004", 12.345, 20.1234), fr("P222222", "U50004", -1000.5, 1001.25)]);
}

#[test]
fn classify_order_and_precedence() {
    let d = tmp_dir("p-classify");
    let n26h = "\"Booking Date\",\"Value Date\",\"Partner Name\",\"Partner Iban\",Type,\"Payment Reference\",\"Account Name\",\"Amount (EUR)\"\n";
    let files = [
        // N26 wins by name even for a revolut-ish header
        write(&d, "n26-x.csv", "Type,Product,Started Date,Completed Date,Amount,Fee,Currency,State,Balance\n"),
        write(&d, "export.csv", n26h),
        write(&d, "TransactionHistory.csv", "Date,Description,Billing amount,Billing currency,Balance,Balance currency\n"),
        write(&d, "TransHist-Card.csv", "Transaction date,Post date,Description,Billing amount,Credit / Debit\n"),
        write(&d, "revolut-gbp-x.csv", "whatever\n"),
        write(&d, "revolut-btc-x.csv", "whatever\n"),
        write(&d, "account-statement_x.csv", "whatever\n"),
        write(&d, "bitcoin-x.csv", "whatever\n"),
        write(&d, "sms.xml", "<?xml version='1.0'?><smses></smses>"),
        write(&d, "random.csv", "a,b\n"),
        write(&d, "other.xml", "<foo/>"),
    ];
    let got: Vec<(String, Vec<String>)> = sources::classify(&files)
        .into_iter()
        .map(|(s, fs)| {
            (
                s.spec().key.to_string(),
                fs.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect(),
            )
        })
        .collect();
    let want: Vec<(String, Vec<String>)> = [
        ("hsbc_card", vec!["TransHist-Card.csv"]),
        ("hsbc", vec!["TransactionHistory.csv"]),
        ("revolut_new", vec!["account-statement_x.csv"]),
        ("trezor", vec!["bitcoin-x.csv"]),
        ("n26", vec!["export.csv", "n26-x.csv"]),
        ("revolut", vec!["revolut-btc-x.csv"]),
        ("revolut_old_gbp", vec!["revolut-gbp-x.csv"]),
        ("hsbc_sms", vec!["sms.xml"]),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.into_iter().map(String::from).collect()))
    .collect();
    assert_eq!(got, want);
}
