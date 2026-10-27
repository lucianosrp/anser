//! "The same experience", client side: what the app computes from the
//! owner's replica (the ledger's base-currency amounts, trips and their
//! routes, recurring series and their flags, budget suggestions) must not
//! move across the de-personalization (Phase 4). Needs personal data, so it
//! reads $FINANCE_PARITY_DIR (prod.db) and skips when that is unset or absent.
//!
//! On a COPY of prod.db with the server's startup migrations and the owner's
//! setup (invest_setup.json, owner_profile.json), the computed text must equal
//! `$FINANCE_PARITY_DIR/client-hkd.txt`, written by the code before Phase 4
//! (FINANCE_BLESS_CLIENT=1 writes it; bless only from a known-good commit).

use std::fmt::Write as _;
use std::path::PathBuf;

use finance_core::ledger::Ledger;

/// The owner's own setup, which lives with the parity data and not in the
/// repo: the investment accounts and instruments (invest_setup.json) and the
/// profile's home country (owner_profile.json), on top of what a new user
/// gets (profile::seed_owner).
fn apply_owner_setup(conn: &rusqlite::Connection, dir: &std::path::Path) {
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_else(|_| panic!("{f} missing from the parity dir"));
    finance_engine::invest_setup::ensure(conn).unwrap();
    finance_engine::invest_setup::import_json(conn, &read("invest_setup.json")).unwrap();
    finance_engine::profile::seed_owner(conn).unwrap();
    let mut p = finance_engine::profile::get(conn).unwrap().unwrap();
    p.home_country = serde_json::from_str::<serde_json::Value>(&read("owner_profile.json")).unwrap()["home_country"].as_str().map(String::from);
    finance_engine::profile::put(conn, &p).unwrap();
}

fn snapshot(ledger: &Ledger) -> String {
    let mut out = String::new();
    writeln!(out, "# ledger ({} entries)", ledger.entries.len()).unwrap();
    for e in &ledger.entries {
        writeln!(out, "{} {:?} {} {:?} {} {} {:?} {}", e.date, e.kind, e.source, e.category, e.amount, e.currency, e.hkd, e.share).unwrap();
    }
    let trips = finance_core::travel::discover(ledger);
    writeln!(out, "# trips ({})", trips.len()).unwrap();
    for t in &trips {
        writeln!(out, "{t:?}").unwrap();
    }
    let atlas = finance_core::travel::atlas(ledger, &trips);
    writeln!(out, "# routes").unwrap();
    for r in &atlas.routes {
        writeln!(out, "{r:?}").unwrap();
    }
    let series = finance_core::recurring::detect(ledger);
    let newest = ledger.entries.first().map(|e| e.date.clone()).unwrap_or_default();
    let flags = finance_core::recurring::flags(&series, &newest);
    writeln!(out, "# recurring ({}), home {}", series.len(), finance_core::recurring::home_currency(&series)).unwrap();
    for (s, f) in series.iter().zip(&flags) {
        writeln!(out, "{s:?} {f:?}").unwrap();
    }
    let by = finance_core::budget::by_month(ledger);
    for m in ["2026-08", "2026-09", "2026-10"] {
        writeln!(out, "# budget suggest {m}: {:?}", finance_core::budget::suggest(&by, m)).unwrap();
    }
    out
}

#[test]
fn owner_client_views_are_identical() {
    let Some(dir) = std::env::var_os("FINANCE_PARITY_DIR").map(PathBuf::from) else {
        eprintln!("skipping: FINANCE_PARITY_DIR unset");
        return;
    };
    let db = dir.join("prod.db");
    if !db.exists() {
        eprintln!("skipping: no parity data at {}", dir.display());
        return;
    }
    let golden = dir.join("client-hkd.txt");
    let bless = std::env::var_os("FINANCE_BLESS_CLIENT").is_some();
    if !golden.exists() && !bless {
        eprintln!("skipping: no {} (bless it from a known-good commit)", golden.display());
        return;
    }
    let tmp = std::env::temp_dir().join(format!("finance-client-identity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let copy = tmp.join("finance.db");
    std::fs::copy(&db, &copy).unwrap();
    {
        let conn = finance_engine::db::connect(&copy).unwrap();
        finance_engine::profile::ensure(&conn).unwrap();
        finance_engine::categories::ensure(&conn).unwrap();
        apply_owner_setup(&conn, &dir);
        // what Bundle::load does with the bundle's profile and user_settings
        finance_core::base::set(finance_core::base::Home::from_sections(
            &finance_engine::profile::bundle_profile(&conn),
            &finance_engine::profile::bundle_settings(&conn),
        ));
    }
    let ledger = Ledger::load(&copy).unwrap();
    let got = snapshot(&ledger);
    if bless {
        std::fs::write(&golden, &got).unwrap();
        eprintln!("blessed {} ({} bytes)", golden.display(), got.len());
    }
    let want = std::fs::read_to_string(&golden).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    if got != want {
        let (g, w): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
        let at = g.iter().zip(&w).position(|(a, b)| a != b).unwrap_or(g.len().min(w.len()));
        panic!(
            "the owner's client views changed at line {} (lines {} want {}):\n got: {}\nwant: {}",
            at + 1,
            g.len(),
            w.len(),
            g.get(at).unwrap_or(&""),
            w.get(at).unwrap_or(&"")
        );
    }
}
