//! OSR smoke test: the integration check CEF can't be hosted by a plain
//! `#[test]` (it needs the real process/subprocess split).
//!
//! This drives the entire `mote-cef` public API end-to-end:
//!   1. `bootstrap()` — the `execute_process` re-exec split (subprocess exits here).
//!   2. `Engine::init` — bring up the CEF runtime (OSR, CPU `on_paint`).
//!   3. `Page::new` on a `data:` URL — off-screen render a known page.
//!   4. pump until the first frame paints.
//!   5. write that frame to a PNG and assert it is non-blank.
//!
//! Run (libcef.so resolves via the crate's `$ORIGIN` rpath; under X11 force the
//! ozone platform for predictability — the spike's dev target):
//!
//! ```sh
//! DISPLAY=:1 mise exec -- cargo run -p mote-cef --example osr_smoke -- \
//!     --ozone-platform=x11 /tmp/osr_smoke.png
//! ```
//!
//! Exit code 0 + a non-blank PNG at the given path is the evidence.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use mote_cef::{Engine, EngineConfig, Page, PageOptions, ProcessRole};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 400;

// A self-contained page: a solid red background with white text. Solid color
// makes "non-blank" trivial to assert without font/AA assumptions.
const DATA_URL: &str = "data:text/html,\
<html><body style='margin:0;background:%23d62828;color:white;\
font:48px sans-serif;display:flex;align-items:center;justify-content:center'>\
mote-cef OSR</body></html>";

fn main() -> ExitCode {
    // STEP 1: process split. In a CEF subprocess this returns Subprocess and we
    // must exit immediately, doing nothing else.
    match mote_cef::bootstrap() {
        ProcessRole::Subprocess { exit_code } => {
            // Subprocess exit codes are 0 on the normal path; clamp into u8.
            return ExitCode::from(u8::try_from(exit_code.clamp(0, 255)).unwrap_or(0));
        }
        ProcessRole::Browser => {}
    }

    // Optional output path argument (skip CEF's own --type=/--ozone-platform args).
    let out_path = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_else(|| "osr_smoke.png".to_string());

    // STEP 2: engine.
    // no_sandbox must be set explicitly here: the default is `false` (sandbox
    // ON) per the DESIGN security model. This example/smoke test runs in a
    // headless/dev environment where the sandbox is intentionally disabled.
    let config = EngineConfig {
        no_sandbox: true,
        ..EngineConfig::default()
    };
    let engine = match Engine::init(&config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: engine init: {e}");
            return ExitCode::FAILURE;
        }
    };

    // STEP 3: off-screen page on the data: URL.
    let page = match Page::new(
        DATA_URL,
        &PageOptions {
            width: WIDTH,
            height: HEIGHT,
            frame_rate: 60,
        },
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: page create: {e}");
            return ExitCode::FAILURE;
        }
    };

    // STEP 4: pump until first paint (+ a short settle window for layout).
    let start = Instant::now();
    let mut first_paint: Option<Instant> = None;
    loop {
        engine.pump();
        std::thread::sleep(Duration::from_millis(4));
        if page.paint_count() >= 1 {
            let t = first_paint.get_or_insert_with(Instant::now);
            if t.elapsed().as_millis() > 200 {
                break;
            }
        }
        if start.elapsed().as_secs() > 15 {
            eprintln!("FAIL: timed out waiting for first paint");
            return ExitCode::FAILURE;
        }
    }

    let Some(frame) = page.latest_frame() else {
        eprintln!("FAIL: no frame after paint signalled");
        return ExitCode::FAILURE;
    };
    eprintln!(
        "painted {}x{} ({} paints) in {}ms",
        frame.width,
        frame.height,
        page.paint_count(),
        start.elapsed().as_millis()
    );

    // STEP 5: write PNG (convert BGRA -> RGBA for the encoder) and assert non-blank.
    let rgba = frame.to_rgba8();
    if let Err(e) = image::save_buffer(
        &out_path,
        &rgba.pixels,
        rgba.width,
        rgba.height,
        image::ColorType::Rgba8,
    ) {
        eprintln!("FAIL: save png: {e}");
        return ExitCode::FAILURE;
    }

    // Non-blank check: the page is mostly red (#d62828). Count pixels whose red
    // channel dominates; a blank/transparent frame fails this.
    let red_pixels = rgba
        .pixels
        .chunks_exact(4)
        .filter(|p| p[0] > 150 && p[1] < 120 && p[2] < 120)
        .count();
    let total = (rgba.width * rgba.height) as usize;
    let pct = if total == 0 {
        0.0
    } else {
        // total <= 640*400, well within f64's exact-integer range.
        let red = f64::from(u32::try_from(red_pixels).unwrap_or(u32::MAX));
        let all = f64::from(u32::try_from(total).unwrap_or(u32::MAX));
        100.0 * red / all
    };
    eprintln!("non-blank check: {red_pixels}/{total} red-dominant pixels ({pct:.0}%)");

    // Tidy up CEF before shutdown.
    page.close();
    for _ in 0..25 {
        engine.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    engine.shutdown();

    if red_pixels * 2 < total {
        eprintln!("FAIL: frame appears blank (too few red pixels)");
        return ExitCode::FAILURE;
    }

    println!("OK: wrote non-blank frame to {out_path}");
    ExitCode::SUCCESS
}
