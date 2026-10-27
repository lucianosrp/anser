//! Every computed view against the view goldens frozen at the 2026-10-03
//! production cutover: one JSON file per bundle section plus three
//! month_details. Needs personal data, so it reads $FINANCE_PARITY_DIR
//! (prod.db + the goldens in GOLDENS/, e.g. <repo>/data/parity) and skips
//! when that is unset or absent. Nothing from there is ever committed.
//!
//! Runs the views on a COPY of prod.db with FINANCE_TODAY=2026-10-03 and
//! FINANCE_OFFLINE=1, then deep-compares section by section: keys and
//! strings exact, money (a golden already rounded to cents) within 0.01,
//! every other number within 1e-9 relative. Wall-clock fields are ignored.
//! bundle_identity.rs pins the bundle byte for byte; this one adds the
//! month details, the db before the categories migration, and readable
//! per-field diffs.
//!
//! The goldens were frozen from the code at the production cutover.
//! FINANCE_BLESS_GOLDENS=1 rewrites ONLY the golden files whose sections differ
//! (same layout as they are stored: indent 1, non-ASCII as \uXXXX, no trailing
//! newline) and prints which; only ever bless from a commit whose output is
//! known good, and the owner does it. The migrated pass is not blessed: it
//! must then match the files just written.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The goldens' folder in the parity dir (its name on disk predates the
/// cutover and is kept so the frozen data never moves).
const GOLDENS: &str = "goldens";

fn parity_dir() -> Option<PathBuf> {
    std::env::var_os("FINANCE_PARITY_DIR").map(PathBuf::from)
}

/// Pure wall-clock fields (now()), never comparable.
fn ignored(path: &str) -> bool {
    path == "$.generated_at" || path.ends_with("net_worth.as_of")
}

fn num_ok(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let cents = (b * 100.0 - (b * 100.0).round()).abs() < 1e-6;
    if cents && (a - b).abs() <= 0.01 + 1e-9 {
        return true;
    }
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-300) || (a - b).abs() < 1e-12
}

fn diff(path: &str, got: &Value, want: &Value, out: &mut Vec<String>) {
    if ignored(path) {
        return;
    }
    match (got, want) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            if !num_ok(a, b) {
                out.push(format!("{path}: got {a} want {b} (diff {:e})", a - b));
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            let ak: Vec<&String> = a.keys().collect();
            let bk: Vec<&String> = b.keys().collect();
            for k in &bk {
                match a.get(*k) {
                    Some(v) => diff(&format!("{path}.{k}"), v, &b[*k], out),
                    None => out.push(format!("{path}.{k}: missing (want {})", short(&b[*k]))),
                }
            }
            for k in &ak {
                if !b.contains_key(*k) {
                    out.push(format!("{path}.{k}: unexpected {}", short(&a[*k])));
                }
            }
            let common_a: Vec<&&String> = ak.iter().filter(|k| b.contains_key(**k)).collect();
            let common_b: Vec<&&String> = bk.iter().filter(|k| a.contains_key(**k)).collect();
            if common_a != common_b {
                out.push(format!("{path}: key order {:?} want {:?}", common_a, common_b));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!("{path}: len {} want {}", a.len(), b.len()));
            }
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}[{i}]"), x, y, out);
            }
        }
        _ => {
            if got != want {
                out.push(format!("{path}: got {} want {}", short(got), short(want)));
            }
        }
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 120 { format!("{}...", s.chars().take(120).collect::<String>()) } else { s }
}

fn load(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn views_match_the_frozen_goldens() {
    let Some(dir) = parity_dir() else {
        eprintln!("skipping: FINANCE_PARITY_DIR unset");
        return;
    };
    let (db, goldens) = (dir.join("prod.db"), dir.join(GOLDENS));
    if !db.exists() || !goldens.is_dir() {
        eprintln!("skipping: no parity data at {}", dir.display());
        return;
    }
    let tmp = std::env::temp_dir().join(format!("finance-parity-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let copy = tmp.join("parity.db");
    std::fs::copy(&db, &copy).unwrap();
    // one test fn, set before any view runs: env is process-global
    std::env::set_var("FINANCE_DB", &copy);
    std::env::set_var("FINANCE_TODAY", "2026-10-03");
    std::env::set_var("FINANCE_OFFLINE", "1");

    let conn = finance_engine::db::open().unwrap();
    // the owner's investment setup (kept with the parity data) is what the
    // goldens were frozen with; a tree without one reads empty
    finance_engine::invest_setup::import_json(&conn, &std::fs::read_to_string(dir.join("invest_setup.json")).expect("invest_setup.json missing from the parity dir")).unwrap();
    let bless = std::env::var_os("FINANCE_BLESS_GOLDENS").is_some();
    let mut failed = compare(&conn, &goldens, "as is", bless);
    // the same db after the Phase 3 migration (the categories table and its
    // flags in place of the hard-coded lists): the numbers must not move
    let migrated = tmp.join("migrated.db");
    std::fs::copy(&db, &migrated).unwrap();
    let conn2 = finance_engine::db::connect(&migrated).unwrap();
    finance_engine::invest_setup::import_json(&conn2, &std::fs::read_to_string(dir.join("invest_setup.json")).unwrap()).unwrap();
    assert!(finance_engine::categories::ensure(&conn2).unwrap());
    assert!(finance_engine::categorize::recategorize(&conn2).unwrap().is_empty());
    assert!(finance_engine::categories::Flags::load(&conn2).has_table());
    failed.extend(compare(&conn2, &goldens, "with the categories table", false).into_iter().map(|f| format!("{f} (migrated)")));
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(failed.is_empty(), "sections differ from the goldens: {failed:?}");
}

/// Every bundle section and the month details on `conn` against the goldens.
fn compare(conn: &rusqlite::Connection, goldens: &Path, label: &str, bless: bool) -> Vec<String> {
    eprintln!("{label}:");
    // a fresh Ctx per run: its memos are request-scoped
    let mut times = Vec::new();
    let mut bundle = serde_json::Value::Null;
    for _ in 0..5 {
        let ctx = finance_engine::views::Ctx::offline(conn);
        let t = std::time::Instant::now();
        bundle = finance_engine::export::bundle(&ctx, None);
        times.push(t.elapsed());
    }
    eprintln!("bundle: first {:?}, then {:?}", times[0], &times[1..]);
    let ctx = finance_engine::views::Ctx::offline(conn);

    let mut failed = Vec::new();
    for k in finance_engine::export::ADDED_SECTIONS {
        assert!(bundle.get(k).is_some_and(|v| v.get("error").is_none()), "{k}: missing or failed");
    }
    for (k, v) in bundle.as_object().unwrap() {
        if finance_engine::export::ADDED_SECTIONS.contains(&k.as_str()) {
            continue;
        }
        let golden = goldens.join(format!("{k}.json"));
        if !golden.exists() {
            failed.push(format!("{k}: no golden"));
            continue;
        }
        let mut out = Vec::new();
        diff(&format!("$.{k}"), v, &load(&golden), &mut out);
        let path_root = out;
        if path_root.is_empty() {
            eprintln!("  {k}: OK");
        } else {
            eprintln!("  {k}: {} diffs", path_root.len());
            for d in path_root.iter().take(25) {
                eprintln!("      {d}");
            }
            if bless {
                write_golden(&golden, v);
            } else {
                failed.push(k.clone());
            }
        }
    }
    for m in ["2026-08", "2026-09", "2026-10"] {
        let golden = goldens.join(format!("month_detail-{m}.json"));
        if !golden.exists() {
            continue;
        }
        let got = finance_engine::views::api::get_month_detail(&ctx, m).unwrap();
        let mut out = Vec::new();
        diff(&format!("$.month_detail-{m}"), &got, &load(&golden), &mut out);
        if out.is_empty() {
            eprintln!("  month_detail-{m}: OK");
        } else {
            eprintln!("  month_detail-{m}: {} diffs", out.len());
            for d in out.iter().take(25) {
                eprintln!("      {d}");
            }
            if bless {
                write_golden(&golden, &got);
            } else {
                failed.push(format!("month_detail-{m}"));
            }
        }
    }
    failed
}

/// serde_json's pretty printer with indent 1 and non-ASCII escaped as
/// \uXXXX (what the goldens were written with).
struct GoldenFormat(serde_json::ser::PrettyFormatter<'static>);

impl serde_json::ser::Formatter for GoldenFormat {
    fn begin_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_array(w)
    }
    fn end_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array(w)
    }
    fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array_value(w)
    }
    fn begin_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object(w)
    }
    fn end_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object(w)
    }
    fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_object_key(w, first)
    }
    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object_value(w)
    }
    fn write_string_fragment<W: ?Sized + std::io::Write>(&mut self, w: &mut W, fragment: &str) -> std::io::Result<()> {
        let mut buf = [0u16; 2];
        for ch in fragment.chars() {
            if ch.is_ascii() {
                w.write_all(&[ch as u8])?;
            } else {
                for u in ch.encode_utf16(&mut buf) {
                    write!(w, "\\u{:04x}", u)?;
                }
            }
        }
        Ok(())
    }
}

fn golden_bytes(v: &Value) -> Vec<u8> {
    use serde::Serialize;
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, GoldenFormat(serde_json::ser::PrettyFormatter::with_indent(b" ")));
    v.serialize(&mut ser).unwrap();
    out
}

fn write_golden(path: &Path, v: &Value) {
    std::fs::write(path, golden_bytes(v)).unwrap();
    eprintln!("  blessed {}", path.file_name().unwrap().to_string_lossy());
}
