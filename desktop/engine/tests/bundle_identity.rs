//! "The same experience": the owner's export bundle, byte for byte, across
//! the de-personalization (Phase 4). Needs personal data, so it reads
//! $FINANCE_PARITY_DIR (prod.db, e.g. <repo>/data/parity) and skips when that
//! is unset or absent. Nothing from there is ever committed.
//!
//! On a COPY of prod.db, with the migrations the server runs at startup and
//! the owner's setup applied (invest_setup.json and owner_profile.json in the parity dir), FINANCE_TODAY=2026-10-03 and
//! FINANCE_OFFLINE=1, the bundle as the server sends it
//! (export::bundle_bytes) must equal `$FINANCE_PARITY_DIR/bundle-hkd.json`
//! byte for byte, except the wall-clock fields (generated_at and the
//! now-stamped as_of strings), which are blanked on both sides.
//!
//! The golden was written by the code before Phase 4
//! (FINANCE_BLESS_BUNDLE=1 writes it; only ever bless from a commit whose
//! output is known good).

use std::path::PathBuf;

fn parity_dir() -> Option<PathBuf> {
    std::env::var_os("FINANCE_PARITY_DIR").map(PathBuf::from)
}

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

/// generated_at's number and every now-stamped as_of string
/// ("YYYY-MM-DDTHH:MM:SS.ffffff+00:00", what networth::as_of writes) blanked.
/// The bundle names the server's build (capabilities: finance_core::VERSION,
/// "v0.1.0-3-gabc1234-dirty" and the like): read it as the crate's version,
/// which is what the golden holds, so the pin doesn't move with every build.
fn unstamped(bytes: &[u8]) -> Vec<u8> {
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    let stamp = format!("\"version\":\"{}\"", finance_core::VERSION);
    s.replace(&stamp, &format!("\"version\":\"{}\"", env!("CARGO_PKG_VERSION"))).into_bytes()
}

fn untimed(bytes: &[u8]) -> Vec<u8> {
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    loop {
        let g = rest.find("\"generated_at\":");
        let a = rest.find("\"as_of\":\"");
        let next = match (g, a) {
            (None, None) => break,
            (Some(g), None) => (g, true),
            (None, Some(a)) => (a, false),
            (Some(g), Some(a)) => if g < a { (g, true) } else { (a, false) },
        };
        let (at, gen) = next;
        if gen {
            let head = at + "\"generated_at\":".len();
            out.push_str(&rest[..head]);
            let end = rest[head..].find([',', '}']).map(|e| head + e).unwrap_or(rest.len());
            out.push('0');
            rest = &rest[end..];
        } else {
            let head = at + "\"as_of\":\"".len();
            out.push_str(&rest[..head]);
            let end = rest[head..].find('"').map(|e| head + e).unwrap_or(rest.len());
            let v = &rest[head..end];
            let stamped = v.len() == 32 && v.as_bytes()[10] == b'T' && v.ends_with("+00:00");
            out.push_str(if stamped { "NOW" } else { v });
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out.into_bytes()
}

#[test]
fn owner_bundle_is_byte_identical() {
    let Some(dir) = parity_dir() else {
        eprintln!("skipping: FINANCE_PARITY_DIR unset");
        return;
    };
    let db = dir.join("prod.db");
    if !db.exists() {
        eprintln!("skipping: no parity data at {}", dir.display());
        return;
    }
    let golden = dir.join("bundle-hkd.json");
    let bless = std::env::var_os("FINANCE_BLESS_BUNDLE").is_some();
    if !golden.exists() && !bless {
        eprintln!("skipping: no {} (bless it from a known-good commit)", golden.display());
        return;
    }
    let tmp = std::env::temp_dir().join(format!("finance-bundle-identity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("data")).unwrap();
    let copy = tmp.join("data/finance.db");
    std::fs::copy(&db, &copy).unwrap();
    // hermetic: no .env, no secrets store, no caches from the real tree
    std::env::set_var("FINANCE_HOME", &tmp);
    std::env::set_var("FINANCE_DB", &copy);
    std::env::set_var("FINANCE_TODAY", "2026-10-03");
    std::env::set_var("FINANCE_OFFLINE", "1");

    let conn = finance_engine::db::open().unwrap();
    // what the server's startup does to an existing tree (auth::bootstrap)
    finance_engine::profile::ensure(&conn).unwrap();
    finance_engine::categories::ensure(&conn).unwrap();
    apply_owner_setup(&conn, &dir);

    let ctx = finance_engine::views::Ctx::offline(&conn);
    let nw = ctx.net_worth_offline().clone();
    let bytes = finance_engine::export::bundle_bytes(&ctx, &nw);
    let got = unstamped(&untimed(&bytes));
    if bless {
        std::fs::write(&golden, &got).unwrap();
        eprintln!("blessed {} ({} bytes)", golden.display(), got.len());
    }
    let want = std::fs::read(&golden).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    if got != want {
        let at = got.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
        let from = at.saturating_sub(200);
        panic!(
            "the owner's bundle changed at byte {at} (len {} want {}):\n got: {}\nwant: {}",
            got.len(),
            want.len(),
            String::from_utf8_lossy(&got[from..(at + 200).min(got.len())]),
            String::from_utf8_lossy(&want[from..(at + 200).min(want.len())])
        );
    }
}
