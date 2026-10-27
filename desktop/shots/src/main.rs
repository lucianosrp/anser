//! finance-shots: README and website screenshots of the web app, on the
//! made-up demo household only.
//!
//!   cargo run -p finance-shots --release -- [options]
//!
//! 1. builds the web app (scripts/build-web.sh into target/shots/webapp),
//!    or takes --webapp DIR;
//! 2. starts anser-server on a free 127.0.0.1 port with a throwaway
//!    FINANCE_HOME, FINANCE_OFFLINE=1 and the demo's today pinned
//!    (FINANCE_DEMO_TODAY), serving that build;
//! 3. drives headless Chrome over CDP: `/app/?demo=1&page=...` per shot in
//!    shots.toml, waits for the app's ready flag and a settled frame;
//! 4. frames each capture (frame.rs) and writes PNGs to docs/screenshots/.
//!
//! The raw captures go to target/shots/raw/: with the same commit and
//! --today they are the same bytes run after run.

mod capture;
mod cdp;
mod frame;
mod procs;
mod spec;
mod video;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use spec::{Frame, Shot};

const USAGE: &str = "\
finance-shots: screenshots of the web app on the demo data

usage: cargo run -p finance-shots --release -- [options]

  --out DIR         where the framed images go (default desktop/docs/screenshots)
  --list FILE       the shot list (default desktop/shots/shots.toml)
  --only A,B        just these shots (by name)
  --webapp DIR      use this web build instead of building one
  --server-bin PATH use this anser-server instead of building it
  --chrome PATH     the Chrome to drive (default: $CHROME, else google-chrome-stable on PATH)
  --today DATE      the demo's today, YYYY-MM-DD (default 2026-09-30)
  --webp            also write lossless WebP next to each PNG
  --video           also record a short tour (tour.mp4/.webm with ffmpeg)
  --timeout SECS    how long a page may take to be ready (default 90)
";

struct Opts {
    out: PathBuf,
    list: PathBuf,
    only: Vec<String>,
    webapp: Option<PathBuf>,
    server_bin: Option<PathBuf>,
    chrome: Option<PathBuf>,
    today: String,
    webp: bool,
    video: bool,
    timeout: Duration,
}

/// desktop/ (this crate's parent).
fn desktop_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("desktop/").to_path_buf()
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| desktop_dir().join("target"))
}

fn parse_args() -> Result<Opts> {
    let mut o = Opts {
        out: desktop_dir().join("docs/screenshots"),
        list: desktop_dir().join("shots/shots.toml"),
        only: vec![],
        webapp: None,
        server_bin: None,
        chrome: None,
        today: "2026-09-30".into(),
        webp: false,
        video: false,
        timeout: Duration::from_secs(90),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--out" => o.out = val()?.into(),
            "--list" => o.list = val()?.into(),
            "--only" => o.only = val()?.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "--webapp" => o.webapp = Some(val()?.into()),
            "--server-bin" => o.server_bin = Some(val()?.into()),
            "--chrome" => o.chrome = Some(val()?.into()),
            "--today" => o.today = val()?,
            "--timeout" => o.timeout = Duration::from_secs(val()?.parse().context("--timeout")?),
            "--webp" => o.webp = true,
            "--video" => o.video = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => bail!("unknown option {a}\n\n{USAGE}"),
        }
    }
    if capture::noon_ms(&o.today).is_none() {
        bail!("--today {:?}: YYYY-MM-DD", o.today);
    }
    Ok(o)
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let st = cmd.status().with_context(|| format!("running {what}"))?;
    if !st.success() {
        bail!("{what} failed ({st})");
    }
    Ok(())
}

/// The web build to serve: --webapp, else build-web.sh into target/shots.
fn webapp(o: &Opts) -> Result<PathBuf> {
    if let Some(d) = &o.webapp {
        if !d.join("index.html").is_file() || !d.join("finance_web_bg.wasm").is_file() {
            bail!("{}: not a web build (no index.html / finance_web_bg.wasm)", d.display());
        }
        return Ok(d.clone());
    }
    let out = target_dir().join("shots/webapp");
    println!("building the web app into {}", out.display());
    run(Command::new(desktop_dir().join("scripts/build-web.sh")).arg(&out), "scripts/build-web.sh")?;
    Ok(out)
}

/// anser-server: --server-bin, else built here (release).
fn server_bin(o: &Opts) -> Result<PathBuf> {
    if let Some(p) = &o.server_bin {
        // it runs in the scratch folder: an absolute path
        return p.canonicalize().with_context(|| format!("--server-bin {}", p.display()));
    }
    println!("building anser-server");
    run(
        Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["build", "--release", "-q", "-p", "finance-server", "--bin", "anser-server"])
            .current_dir(desktop_dir()),
        "cargo build -p finance-server",
    )?;
    Ok(target_dir().join("release/anser-server"))
}

fn write_png(img: &image::RgbaImage, path: &Path) -> Result<()> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder as _;
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    // the frames are opaque: RGB is a quarter smaller
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    PngEncoder::new_with_quality(f, CompressionType::Best, FilterType::Adaptive)
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .with_context(|| format!("writing {}", path.display()))
}

fn write_webp(img: &image::RgbaImage, path: &Path) -> Result<()> {
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    image::codecs::webp::WebPEncoder::new_lossless(f)
        .encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .with_context(|| format!("writing {}", path.display()))
}

/// Capture one view of `shot` in `mode`: the raw PNG bytes.
fn capture(cdp: &mut cdp::Cdp, origin: &str, shot: &Shot, mode: &str, base_ms: i64, timeout: Duration) -> Result<Vec<u8>> {
    let url = shot.url(origin, mode);
    let tab = capture::Tab::open(cdp, shot.size(), base_ms)?;
    let r = (|| -> Result<Vec<u8>> {
        tab.load(cdp, &url, timeout)?;
        if std::env::var_os("FINANCE_SHOTS_PROBE").is_some() {
            let v = cdp.eval(&tab.session, "(() => { const c = document.querySelector('canvas'); return JSON.stringify({dpr: devicePixelRatio, iw: innerWidth, ih: innerHeight, cw: c && c.width, ch: c && c.height, csw: c && c.getBoundingClientRect().width, vv: visualViewport && visualViewport.scale, coarse: matchMedia('(pointer: coarse)').matches}); })()")?;
            eprintln!("  probe {}: {v}", shot.name);
        }
        let (png, settled) = tab.settled(cdp, 40)?;
        if !settled {
            eprintln!("  {} ({mode}): the frame kept changing; took the last one", shot.name);
        }
        Ok(png)
    })();
    tab.close(cdp);
    r.with_context(|| format!("shot {} ({mode})", shot.name))
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("finance-shots: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let started = Instant::now();
    let o = parse_args()?;
    let text = std::fs::read_to_string(&o.list).with_context(|| format!("reading {}", o.list.display()))?;
    let mut shots = spec::parse(&text)?;
    if !o.only.is_empty() {
        for n in &o.only {
            if !shots.iter().any(|s| &s.name == n) {
                bail!("--only: no shot named {n}");
            }
        }
        shots.retain(|s| o.only.contains(&s.name));
    }
    let chrome_bin = procs::find_chrome(o.chrome.as_deref())?;
    let web = webapp(&o)?;
    let server = server_bin(&o)?;
    let raw_dir = target_dir().join("shots/raw");
    std::fs::create_dir_all(&raw_dir)?;
    std::fs::create_dir_all(&o.out)?;
    let base_ms = capture::noon_ms(&o.today).expect("checked");

    // dropped in reverse: Chrome, then the server, then the scratch folder
    let scratch = procs::Scratch::new()?;
    let port = procs::free_port()?;
    let t = Instant::now();
    let srv = procs::server(&server, &scratch.0, &web, port, &o.today)?;
    println!("anser-server {} on 127.0.0.1:{port} (demo today {}), ready in {:.1}s", srv.pid(), o.today, t.elapsed().as_secs_f32());
    let (chrome, dt_port, dt_path) = procs::chrome(&chrome_bin, &scratch.0)?;
    println!("chrome {} headless, DevTools on {dt_port}", chrome.pid());
    let mut cdp = cdp::Cdp::connect(dt_port, &dt_path)?;
    let origin = format!("http://127.0.0.1:{port}");

    let mut written = Vec::new();
    for shot in &shots {
        let t = Instant::now();
        let mode = if shot.dark() { "dark" } else { "light" };
        let png = capture(&mut cdp, &origin, shot, mode, base_ms, o.timeout)?;
        std::fs::write(raw_dir.join(format!("{}.png", shot.name)), &png)?;
        let mut pm = frame::from_png(&png)?;
        if shot.pair {
            let other = if shot.dark() { "light" } else { "dark" };
            let png2 = capture(&mut cdp, &origin, shot, other, base_ms, o.timeout)?;
            std::fs::write(raw_dir.join(format!("{}-{other}.png", shot.name)), &png2)?;
            let pm2 = frame::from_png(&png2)?;
            pm = if shot.dark() { frame::split(&pm, &pm2) } else { frame::split(&pm2, &pm) };
        }
        let s = shot.size().scale as f32;
        let b = frame::backdrop(shot.backdrop()).expect("checked");
        let framed = match shot.frame() {
            Frame::Window => frame::window(&pm, s, &b, shot.dark() || shot.pair),
            Frame::Phone => frame::phone(&pm, s, &b),
            Frame::None => pm,
        };
        let img = frame::rgba(&framed);
        let path = o.out.join(format!("{}.png", shot.name));
        write_png(&img, &path)?;
        if o.webp {
            write_webp(&img, &o.out.join(format!("{}.webp", shot.name)))?;
        }
        let kb = std::fs::metadata(&path).map(|m| m.len() / 1024).unwrap_or(0);
        println!("  {:<22} {}x{} {kb:>5} KB  {:.1}s", shot.name, img.width(), img.height(), t.elapsed().as_secs_f32());
        written.push(path);
    }

    if o.video {
        let url = format!("{origin}/app/?demo=1&gl=webgl&page=overview");
        match video::record(&mut cdp, &url, base_ms, &target_dir().join("shots/video-frames"), &o.out) {
            Ok(files) => written.extend(files),
            Err(e) => eprintln!("video: {e:#}"),
        }
    }

    // a clean exit for Chrome; the guards make sure of it either way
    let _ = cdp.call(None, "Browser.close", serde_json::json!({}));
    drop(cdp);
    drop(chrome);
    drop(srv);
    drop(scratch);
    println!("{} files in {} ({:.0}s); raw captures in {}", written.len(), o.out.display(), started.elapsed().as_secs_f32(), raw_dir.display());
    Ok(())
}
