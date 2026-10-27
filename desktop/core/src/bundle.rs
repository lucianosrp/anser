//! The server-computed views, read from the cached `export.json`. Kept as a
//! JSON value with small typed accessors: the server owns the shapes, and
//! a missing field degrades one tile instead of failing the whole load.

use std::path::Path;

#[cfg(feature = "native")]
use anyhow::Context;
use anyhow::Result;
use serde_json::Value;

pub const SUPPORTED_VERSION: u64 = 1;

#[derive(Debug, Clone, Default)]
pub struct Bundle(pub Value);

impl Bundle {
    pub fn load(path: &Path) -> Result<Self> {
        #[cfg(not(feature = "native"))]
        let b = {
            // web build: what the page fetched (crate::preload)
            let _ = path;
            crate::preload::bundle()?
        };
        #[cfg(feature = "native")]
        let b = {
            // plain, or the encrypted replica decrypted in memory
            let raw = crate::replica::read(path)?;
            Bundle(serde_json::from_slice(&raw).context("parsing export.json")?)
        };
        // whose money, where: the pages read it (crate::base)
        crate::base::set_from_bundle(&b.0);
        if crate::demo::active() {
            crate::demo::remember(&b.0);
        }
        Ok(b)
    }

    /// `bundle.at(&["net_worth", "total_hkd"])`
    pub fn at(&self, path: &[&str]) -> &Value {
        let mut v = &self.0;
        for key in path {
            v = v.get(*key).unwrap_or(&Value::Null);
        }
        v
    }

    pub fn f64(&self, path: &[&str]) -> Option<f64> {
        self.at(path).as_f64()
    }

    pub fn str(&self, path: &[&str]) -> Option<&str> {
        self.at(path).as_str()
    }

    pub fn arr(&self, path: &[&str]) -> &[Value] {
        self.at(path).as_array().map(|a| a.as_slice()).unwrap_or(&[])
    }

    /// When the server built this bundle (unix ms).
    pub fn generated_at(&self) -> Option<i64> {
        self.at(&["generated_at"]).as_i64()
    }

    /// Monthly spending trend: completed months only (the running month is
    /// partial and would read as a dip).
    pub fn completed_trend(&self) -> Vec<&Value> {
        let current = chrono::Utc::now().format("%Y-%m").to_string();
        self.arr(&["spending", "trend"])
            .iter()
            .filter(|t| t.get("month").and_then(|m| m.as_str()) != Some(current.as_str()))
            .collect()
    }
}
