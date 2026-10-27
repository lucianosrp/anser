//! Startup trace: `FINANCE_DESKTOP_TIMING=1` prints each milestone's time
//! since process start to stderr (what to look at when startup feels slow).
//!
//! Render trace: `FINANCE_DESKTOP_RENDER_TIMING=1` prints, every 60 frames,
//! how long building each page's element tree took (mean and worst): the
//! work a scroll frame repeats. `=all` prints every frame; `=spin` also
//! builds the page 60 more times on each frame with data, so an idle page
//! is measured as if it were scrolling. The web app takes
//! `?timing=1|all|spin` instead and prints to the console (`set_render`).

use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();

/// Call first thing in main.
pub fn start() {
    START.get_or_init(Instant::now);
}

pub fn mark(what: &str) {
    if std::env::var_os("FINANCE_DESKTOP_TIMING").is_some() {
        let ms = START.get().map_or(0., |s| s.elapsed().as_secs_f64() * 1e3);
        eprintln!("[timing] {ms:8.1} ms  {what}");
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Render {
    Off,
    Every60,
    All,
    Spin,
}

impl Render {
    pub fn parse(v: &str) -> Render {
        match v {
            "" | "0" | "off" => Render::Off,
            "all" => Render::All,
            "spin" => Render::Spin,
            _ => Render::Every60,
        }
    }
}

/// 0 = not decided yet (read the env on first use), else Render as 1 + index
static MODE: AtomicU8 = AtomicU8::new(0);
static SINK: OnceLock<fn(&str)> = OnceLock::new();
static RENDER: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());

/// The web app's switch (no env there): the mode, and where lines go.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub fn set_render(mode: Render, sink: fn(&str)) {
    MODE.store(1 + mode as u8, Ordering::Relaxed);
    let _ = SINK.set(sink);
}

pub fn render_mode() -> Render {
    const ALL: [Render; 4] = [Render::Off, Render::Every60, Render::All, Render::Spin];
    match MODE.load(Ordering::Relaxed) {
        0 => {
            let m = Render::parse(&std::env::var("FINANCE_DESKTOP_RENDER_TIMING").unwrap_or_default());
            MODE.store(1 + m as u8, Ordering::Relaxed);
            m
        }
        n => ALL[(n - 1) as usize],
    }
}

/// One frame of `page` took `took` to build (see the module docs).
pub fn render_sample(page: &str, took: Duration) {
    let mode = render_mode();
    if mode == Render::Off {
        return;
    }
    let Ok(mut v) = RENDER.lock() else { return };
    v.push((page.to_string(), took));
    if v.len() < 60 && mode != Render::All {
        return;
    }
    let mut by: Vec<(String, Vec<f64>)> = Vec::new();
    for (p, d) in v.drain(..) {
        let ms = d.as_secs_f64() * 1e3;
        match by.iter_mut().find(|(k, _)| *k == p) {
            Some((_, xs)) => xs.push(ms),
            None => by.push((p, vec![ms])),
        }
    }
    for (p, xs) in by {
        let mean = xs.iter().sum::<f64>() / xs.len() as f64;
        let max = xs.iter().cloned().fold(0., f64::max);
        let line = format!("[render] {p:<14} {:>3} frames  mean {mean:6.2} ms  max {max:6.2} ms", xs.len());
        match SINK.get() {
            Some(sink) => sink(&line),
            None => eprintln!("{line}"),
        }
    }
}
