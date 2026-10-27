//! The web app's "Just try it": `GET /api/demo/ledger.json` and
//! `GET /api/demo/export.json`, the made-up household of
//! finance_engine::demo in the shapes `/api/ledger.json` and
//! `/api/export.json` have.
//!
//! Public (no sign-in: it is what a browser sees before it has an
//! account) and safe to be: it is built from nothing but the generator,
//! in a scratch folder of its own, as a throwaway tenant; no user's tree,
//! secret or setting is read. Built once a day (the data is relative to
//! today) and kept in memory, so a burst of visitors costs one build.
//!
//! `FINANCE_DEMO_TODAY=2026-09-30` pins the demo's today, and its clock
//! stamps (`generated_at`, the net worth's `as_of`) to noon that day: the
//! same commit then serves the same bytes (desktop/shots' screenshots).

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::NaiveDate;

/// (the day it was built for, ledger JSON, bundle JSON)
type Built = (NaiveDate, Arc<Vec<u8>>, Arc<Vec<u8>>);

static BUILT: Mutex<Option<Built>> = Mutex::new(None);

/// FINANCE_DEMO_TODAY, when set to a date.
fn pinned_today() -> Option<NaiveDate> {
    std::env::var("FINANCE_DEMO_TODAY").ok().and_then(|s| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok())
}

/// The bundle's clock stamps at noon on `day` instead of now.
fn restamp(bundle: &[u8], day: NaiveDate) -> Result<Vec<u8>> {
    let noon = day.and_hms_opt(12, 0, 0).context("noon")?.and_utc();
    let mut b: serde_json::Value = serde_json::from_slice(bundle)?;
    b["generated_at"] = noon.timestamp_millis().into();
    let as_of = finance_engine::views::networth::as_of(noon);
    for at in ["/net_worth/as_of", "/overview/net_worth/as_of"] {
        if let Some(v) = b.pointer_mut(at).filter(|v| v.is_string()) {
            *v = as_of.clone().into();
        }
    }
    Ok(serde_json::to_vec(&b)?)
}

/// Today's demo (ledger.json, export.json), built on first use each day.
pub fn files() -> Result<(Arc<Vec<u8>>, Arc<Vec<u8>>)> {
    let pinned = pinned_today();
    let today = pinned.unwrap_or_else(finance_engine::config::utc_today);
    // held while building: concurrent first visitors wait for one build
    let mut g = BUILT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((day, l, b)) = g.as_ref() {
        if *day == today {
            return Ok((l.clone(), b.clone()));
        }
    }
    let dir = std::env::temp_dir().join(format!("finance-demo-{}-{}", std::process::id(), today));
    let _ = std::fs::remove_dir_all(&dir);
    let made = (|| -> Result<(Vec<u8>, Vec<u8>)> {
        finance_engine::demo::write(&dir, today)?;
        let ledger = finance_core::Ledger::load(&dir.join("finance.db"))?.to_json()?;
        let bundle = std::fs::read(dir.join("export.json")).context("reading the demo bundle")?;
        let bundle = if pinned.is_some() { restamp(&bundle, today)? } else { bundle };
        Ok((ledger, bundle))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    let (l, b) = made?;
    let (l, b) = (Arc::new(l), Arc::new(b));
    *g = Some((today, l.clone(), b.clone()));
    Ok((l, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restamp_pins_the_clock_stamps() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        let src = br#"{"generated_at":1,"net_worth":{"as_of":"x","total":2},"overview":{"net_worth":{"as_of":"y"}},"other":"z"}"#;
        let b: serde_json::Value = serde_json::from_slice(&restamp(src, day).unwrap()).unwrap();
        assert_eq!(b["generated_at"], 1_790_769_600_000i64);
        assert_eq!(b["net_worth"]["as_of"], "2026-09-30T12:00:00.000000+00:00");
        assert_eq!(b["overview"]["net_worth"]["as_of"], "2026-09-30T12:00:00.000000+00:00");
        assert_eq!(b["net_worth"]["total"], 2);
        assert_eq!(b["other"], "z");
    }
}
