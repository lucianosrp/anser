//! One capture: a fresh tab at a viewport, the clock pinned, the web app
//! loaded until it says it has drawn (`window.__financeReady`, set by
//! web/src/lib.rs) and its pixels stop changing.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use base64::Engine as _;
use serde_json::json;

use crate::cdp::Cdp;
use crate::spec::Size;

/// The page's clock starts at `base_ms` (and runs from there): `Date`,
/// `Date.now()` and so chrono's `Local::now()` in the wasm all read it.
pub fn clock_shim(base_ms: i64) -> String {
    format!(
        r#"(() => {{
  const R = Date, base = {base_ms}, t0 = performance.now();
  const now = () => base + Math.floor(performance.now() - t0);
  class D extends R {{
    constructor(...a) {{ if (a.length === 0) super(now()); else super(...a); }}
    static now() {{ return now(); }}
  }}
  globalThis.Date = new Proxy(D, {{ apply() {{ return new R(now()).toString(); }} }});
  // under Emulation.setDeviceMetricsOverride Chrome reports the canvas's
  // device-pixel box in CSS pixels, so gpui would lay out at 1/scale of the
  // width: without it gpui takes the CSS box x devicePixelRatio (its
  // Safari path, also what the web app's pixel cap uses)
  if (globalThis.ResizeObserverEntry) delete ResizeObserverEntry.prototype.devicePixelContentBoxSize;
}})();"#
    )
}

/// Milliseconds since the epoch at noon UTC on `YYYY-MM-DD`.
pub fn noon_ms(date: &str) -> Option<i64> {
    let mut it = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // days from civil (Howard Hinnant's algorithm)
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 + 12 * 3_600_000)
}

/// An open tab on its own session; closed on drop is not possible without
/// the connection, so call `close`.
pub struct Tab {
    pub target: String,
    pub session: String,
}

impl Tab {
    /// A new tab at `size`, the clock at `base_ms`, UTC, en-US.
    pub fn open(cdp: &mut Cdp, size: Size, base_ms: i64) -> Result<Tab> {
        let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?["targetId"]
            .as_str().unwrap_or_default().to_string();
        let session = cdp.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true }))?["sessionId"]
            .as_str().unwrap_or_default().to_string();
        let tab = Tab { target, session };
        let s = Some(tab.session.as_str());
        let setup = (|| -> Result<()> {
            cdp.call(s, "Page.enable", json!({}))?;
            cdp.call(s, "Emulation.setDeviceMetricsOverride", json!({
                "width": size.width, "height": size.height, "deviceScaleFactor": size.scale, "mobile": size.mobile,
            }))?;
            if size.mobile {
                cdp.call(s, "Emulation.setTouchEmulationEnabled", json!({ "enabled": true, "maxTouchPoints": 5 }))?;
            }
            cdp.call(s, "Emulation.setTimezoneOverride", json!({ "timezoneId": "UTC" }))?;
            cdp.call(s, "Emulation.setFocusEmulationEnabled", json!({ "enabled": true }))?;
            cdp.call(s, "Page.addScriptToEvaluateOnNewDocument", json!({ "source": clock_shim(base_ms) }))?;
            Ok(())
        })();
        if let Err(e) = setup {
            tab.close(cdp);
            return Err(e);
        }
        Ok(tab)
    }

    pub fn close(&self, cdp: &mut Cdp) {
        let _ = cdp.call(None, "Target.closeTarget", json!({ "targetId": self.target }));
    }

    /// Load `url` and wait for the app's ready flag.
    pub fn load(&self, cdp: &mut Cdp, url: &str, timeout: Duration) -> Result<()> {
        cdp.call(Some(&self.session), "Page.navigate", json!({ "url": url }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let v = cdp.eval(&self.session, "[window.__financeReady === true, (document.getElementById('boot') || {}).textContent || '']")
                .unwrap_or_default();
            if v[0] == true {
                return Ok(());
            }
            let boot = v[1].as_str().unwrap_or_default();
            if boot.starts_with("Could not start") || boot.contains("graphics device was lost") {
                bail!("{url}: {boot}");
            }
            if Instant::now() > deadline {
                bail!("{url}: not ready in {}s (boot line: {boot:?})", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A PNG of the viewport.
    pub fn screenshot(&self, cdp: &mut Cdp) -> Result<Vec<u8>> {
        let r = cdp.call(Some(&self.session), "Page.captureScreenshot", json!({ "format": "png", "fromSurface": true }))?;
        Ok(base64::engine::general_purpose::STANDARD.decode(r["data"].as_str().unwrap_or_default())?)
    }

    /// Screenshots until three in a row are the same (the frame has
    /// settled), at most `tries`. The last one, and whether it settled.
    pub fn settled(&self, cdp: &mut Cdp, tries: usize) -> Result<(Vec<u8>, bool)> {
        let mut last = self.screenshot(cdp)?;
        let mut same = 0;
        for _ in 0..tries {
            std::thread::sleep(Duration::from_millis(120));
            let png = self.screenshot(cdp)?;
            if png == last {
                same += 1;
                if same >= 2 {
                    return Ok((png, true));
                }
            } else {
                same = 0;
                last = png;
            }
        }
        Ok((last, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noon_on_a_day() {
        assert_eq!(noon_ms("1970-01-01"), Some(12 * 3_600_000));
        assert_eq!(noon_ms("2026-09-30"), Some(1_790_769_600_000));
        assert_eq!(noon_ms("2024-02-29"), Some(1_709_208_000_000));
        assert_eq!(noon_ms("2026-13-01"), None);
        assert_eq!(noon_ms("today"), None);
    }
}
