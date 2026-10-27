//! The README's source tables are generated from the catalog: this fails
//! when the catalog changes and the docs don't. `FINANCE_BLESS_DOCS=1`
//! rewrites the blocks between the markers.

use std::path::PathBuf;

use finance_engine::catalog;

fn root(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn check(file: &str, name: &str, body: &str) {
    let path = root(file);
    let text = std::fs::read_to_string(&path).unwrap();
    let (begin, end) = (format!("<!-- {name}:begin -->\n"), format!("<!-- {name}:end -->"));
    let a = text.find(&begin).unwrap_or_else(|| panic!("{file}: missing {begin:?}")) + begin.len();
    let b = a + text[a..].find(&end).unwrap_or_else(|| panic!("{file}: missing {end:?}"));
    if std::env::var_os("FINANCE_BLESS_DOCS").is_some() {
        std::fs::write(&path, format!("{}{}{}", &text[..a], body, &text[b..])).unwrap();
    } else {
        assert!(&text[a..b] == body, "{file}: the {name} block is out of date; run with FINANCE_BLESS_DOCS=1 to rewrite it");
    }
}

/// The demo's copy of the catalog (core/src/catalog.json).
#[test]
fn the_demos_catalog_matches() {
    let body = serde_json::to_string_pretty(&catalog::catalog()).unwrap() + "\n";
    let path = root("desktop/core/src/catalog.json");
    if std::env::var_os("FINANCE_BLESS_DOCS").is_some() {
        std::fs::write(&path, &body).unwrap();
    } else {
        assert!(std::fs::read_to_string(&path).unwrap() == body, "core/src/catalog.json is out of date; run with FINANCE_BLESS_DOCS=1 to rewrite it");
    }
}

#[test]
fn the_readmes_sources_blocks_match_the_catalog() {
    check("README.md", "sources", &catalog::readme_table());
    check("desktop/README.md", "sources-howto", &catalog::readme_howto());
}
