//! `--video`: a short tour (Overview, then a few pages picked in the
//! sidebar with the pointer) recorded with CDP's screencast, resampled to
//! 30 fps JPEG frames and, when ffmpeg is on PATH, encoded to MP4 (H.264)
//! and WebM (VP9).
//!
//! (Ctrl+N keys sent with Input.dispatchKeyEvent don't reach gpui's canvas
//! in headless Chrome, so the tour clicks.)

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::Engine as _;
use serde_json::json;

use crate::capture::Tab;
use crate::cdp::Cdp;
use crate::spec::Size;

const FPS: f64 = 30.;
/// The tour: (sidebar row's y in CSS px at 1440x900, seconds on that
/// page): Overview, Spending, Travel, Investments, Broker. The rows sit
/// ~42 px apart from Overview at 106 (WEALTH's heading adds a gap).
const TOUR: [(f64, f64); 5] = [(106., 2.0), (141., 2.4), (209., 2.6), (416., 2.4), (518., 2.4)];
/// the sidebar labels' x
const ROW_X: f64 = 90.;

fn mouse(cdp: &mut Cdp, s: &str, kind: &str, x: f64, y: f64) -> Result<()> {
    cdp.call(Some(s), "Input.dispatchMouseEvent", json!({
        "type": kind, "x": x, "y": y, "button": if kind == "mouseMoved" { "none" } else { "left" }, "clickCount": 1,
    }))?;
    Ok(())
}

/// Record the tour from `url` into `out_dir/tour.{mp4,webm}`; the frames
/// go to `frames` (emptied first). Returns the files written.
pub fn record(cdp: &mut Cdp, url: &str, base_ms: i64, frames: &Path, out_dir: &Path) -> Result<Vec<PathBuf>> {
    let size = Size { width: 1440, height: 900, scale: 1., mobile: false };
    let tab = Tab::open(cdp, size, base_ms)?;
    let r = (|| -> Result<Vec<(f64, Vec<u8>)>> {
        tab.load(cdp, url, Duration::from_secs(90))?;
        tab.settled(cdp, 30)?;
        let s = tab.session.clone();
        cdp.keep_events = true;
        cdp.call(Some(&s), "Page.startScreencast", json!({ "format": "jpeg", "quality": 90, "everyNthFrame": 1 }))?;
        let mut got: Vec<(f64, Vec<u8>)> = Vec::new();
        let mut take = |cdp: &mut Cdp, secs: f64| -> Result<()> {
            let until = Instant::now() + Duration::from_secs_f64(secs);
            while Instant::now() < until {
                for ev in cdp.events(Duration::from_millis(30))? {
                    if ev["method"] != "Page.screencastFrame" {
                        continue;
                    }
                    let p = &ev["params"];
                    let t = p["metadata"]["timestamp"].as_f64().unwrap_or(0.);
                    let jpg = base64::engine::general_purpose::STANDARD.decode(p["data"].as_str().unwrap_or_default())?;
                    got.push((t, jpg));
                    cdp.call(Some(&s), "Page.screencastFrameAck", json!({ "sessionId": p["sessionId"] }))?;
                }
            }
            Ok(())
        };
        let mut at = (720., 450.);
        for (i, (y, secs)) in TOUR.iter().enumerate() {
            if i > 0 {
                // glide to the row, then click it
                for k in 1..=12 {
                    let f = k as f64 / 12.;
                    mouse(cdp, &s, "mouseMoved", at.0 + (ROW_X - at.0) * f, at.1 + (y - at.1) * f)?;
                    take(cdp, 0.02)?;
                }
                at = (ROW_X, *y);
                mouse(cdp, &s, "mousePressed", at.0, at.1)?;
                mouse(cdp, &s, "mouseReleased", at.0, at.1)?;
            }
            take(cdp, *secs)?;
        }
        let _ = cdp.call(Some(&s), "Page.stopScreencast", json!({}));
        cdp.keep_events = false;
        Ok(got)
    })();
    cdp.keep_events = false;
    tab.close(cdp);
    let got = r?;
    anyhow::ensure!(!got.is_empty(), "the screencast sent no frames");

    // constant 30 fps: each output frame is the newest capture at its time;
    // the last one is held to the tour's end
    let _ = std::fs::remove_dir_all(frames);
    std::fs::create_dir_all(frames)?;
    let t0 = got[0].0;
    // to the last capture, plus the last page's dwell
    let total = got[got.len() - 1].0 - t0 + TOUR[TOUR.len() - 1].1;
    let n = (total * FPS).round() as usize;
    let mut j = 0;
    for i in 0..n {
        let t = t0 + i as f64 / FPS;
        while j + 1 < got.len() && got[j + 1].0 <= t {
            j += 1;
        }
        std::fs::write(frames.join(format!("{i:05}.jpg")), &got[j].1)?;
    }
    println!("video: {} captured frames, {n} at {FPS} fps in {}", got.len(), frames.display());

    let mut written = Vec::new();
    let input = frames.join("%05d.jpg");
    let encodes: [(&str, &[&str]); 2] = [
        ("tour.mp4", &["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", "-movflags", "+faststart"]),
        ("tour.webm", &["-c:v", "libvpx-vp9", "-pix_fmt", "yuv420p", "-b:v", "0", "-crf", "34"]),
    ];
    for (name, args) in encodes {
        let out = out_dir.join(name);
        let st = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-framerate", "30", "-i"])
            .arg(&input)
            .args(args)
            .arg(&out)
            .status();
        match st {
            Ok(s) if s.success() => written.push(out),
            Ok(s) => eprintln!("video: ffmpeg {name} failed ({s}); the frames are in {}", frames.display()),
            Err(e) => {
                eprintln!("video: no ffmpeg ({e}); the frames are in {}", frames.display());
                break;
            }
        }
    }
    Ok(written)
}
