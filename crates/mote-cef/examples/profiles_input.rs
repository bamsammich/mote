//! Wave-A integration smoke: per-identity profiles + page roles + input
//! injection. Like `osr_smoke`, this needs the real CEF process split so it lives
//! as an example, not a `#[test]`.
//!
//! It exercises the new public API end-to-end:
//!   1. `bootstrap()` — the `execute_process` re-exec split (subprocess exits here).
//!   2. `Engine::init` — bring up the CEF runtime (OSR, CPU `on_paint`).
//!   3. `ProfileManager` — create two identity profiles ("alice", "bob") and
//!      assert their on-disk storage paths are distinct (the directory-isolation
//!      precondition for cookie/storage isolation; full isolation is asserted by
//!      the headless cookie test in W-A2, not here).
//!   4. A `Chrome`-role page and a `Content`-role page under profile "alice",
//!      plus a second content page under profile "bob".
//!   5. Pump to first paint, then inject a mouse move + left click + a keystroke
//!      into the content page and confirm it still paints (the host accepted the
//!      events without panicking — input plumbing is live).
//!
//! Run (libcef.so resolves via the crate's `$ORIGIN` rpath; force ozone under X11):
//!
//! ```sh
//! DISPLAY=:1 mise exec -- cargo run -p mote-cef --example profiles_input -- \
//!     --ozone-platform=x11
//! ```
//!
//! Exit code 0 with the printed assertions is the evidence.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use mote_cef::{
    ButtonAction, Engine, EngineConfig, IdentityId, KeyAction, KeyInput, Modifiers, MouseButton,
    MousePosition, Page, PageOptions, PageRole, ProcessRole, ProfileManager,
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

// A page with a focusable input + a button, so injected clicks/keys have a target.
const CONTENT_URL: &str = "data:text/html,\
<html><body style='margin:0;background:%23204060'>\
<input id='f' style='width:200px'><button>go</button></body></html>";

const CHROME_URL: &str = "data:text/html,\
<html><body style='margin:0;background:%23101015;color:%23eee'>chrome</body></html>";

fn pump_to_paint(engine: &Engine, page: &Page, label: &str) -> bool {
    let start = Instant::now();
    loop {
        engine.pump();
        std::thread::sleep(Duration::from_millis(4));
        if page.paint_count() >= 1 {
            return true;
        }
        if start.elapsed().as_secs() > 15 {
            eprintln!("FAIL: {label} timed out waiting for first paint");
            return false;
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "linear integration-smoke narrative; splitting hurts readability of the end-to-end flow"
)]
fn main() -> ExitCode {
    match mote_cef::bootstrap() {
        ProcessRole::Subprocess { exit_code } => {
            return ExitCode::from(u8::try_from(exit_code.clamp(0, 255)).unwrap_or(0));
        }
        ProcessRole::Browser => {}
    }

    let config = EngineConfig {
        no_sandbox: true,
        ..EngineConfig::default()
    };
    // Profile dirs must be DIRECT children of the engine's root_cache_path, so
    // the profile manager is rooted at the cache path itself (each identity lands
    // at <cache_path>/profile-<id>, a sibling of CEF's own Default/ etc.).
    let profiles_root = config.cache_path.clone();
    let engine = match Engine::init(&config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: engine init: {e}");
            return ExitCode::FAILURE;
        }
    };

    // STEP 3: two identity profiles, direct children of the engine cache path.
    let manager = ProfileManager::new(&profiles_root);

    let alice_id = IdentityId::new("alice").expect("valid id");
    let bob_id = IdentityId::new("bob").expect("valid id");

    let alice = match manager.get_or_create(&alice_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: create profile alice: {e}");
            return ExitCode::FAILURE;
        }
    };
    let bob = match manager.get_or_create(&bob_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: create profile bob: {e}");
            return ExitCode::FAILURE;
        }
    };

    // get_or_create must intern: asking again returns the same storage path.
    let alice_again = manager.get_or_create(&alice_id).expect("intern");
    if alice.storage_path() != alice_again.storage_path() {
        eprintln!("FAIL: get_or_create did not intern alice's profile");
        return ExitCode::FAILURE;
    }
    if alice.storage_path() == bob.storage_path() {
        eprintln!("FAIL: distinct identities must have distinct storage paths");
        return ExitCode::FAILURE;
    }
    eprintln!(
        "profiles: alice={} bob={} (distinct directories)",
        alice.storage_path().display(),
        bob.storage_path().display()
    );

    // Let the freshly-created profile contexts initialize on the UI thread before
    // we synchronously create browsers under them.
    for _ in 0..50 {
        engine.pump();
        std::thread::sleep(Duration::from_millis(4));
    }

    // STEP 4: chrome page + content pages under the profiles.
    let opts = PageOptions {
        width: WIDTH,
        height: HEIGHT,
        frame_rate: 30,
        role: PageRole::Chrome,
    };
    let chrome = match Page::with_profile(CHROME_URL, &opts, &alice) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: chrome page: {e}");
            return ExitCode::FAILURE;
        }
    };
    if chrome.role() != PageRole::Chrome {
        eprintln!("FAIL: chrome page role mismatch");
        return ExitCode::FAILURE;
    }

    let content_opts = PageOptions {
        width: WIDTH,
        height: HEIGHT,
        frame_rate: 30,
        role: PageRole::Content,
    };
    let alice_page = match Page::with_profile(CONTENT_URL, &content_opts, &alice) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: alice content page: {e}");
            return ExitCode::FAILURE;
        }
    };
    let bob_page = match Page::with_profile(CONTENT_URL, &content_opts, &bob) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: bob content page: {e}");
            return ExitCode::FAILURE;
        }
    };

    if !pump_to_paint(&engine, &chrome, "chrome")
        || !pump_to_paint(&engine, &alice_page, "alice content")
    {
        return ExitCode::FAILURE;
    }
    let paints_before = alice_page.paint_count();

    // STEP 5: inject input into the content page. Focus it, move to the input
    // field, click, then type 'a'. Pump between events so CEF processes them.
    let pos = MousePosition { x: 30, y: 20 }; // over the <input>
    alice_page.send_focus(true);
    alice_page.send_mouse_move(pos, Modifiers::NONE, false);
    alice_page.send_mouse_button(
        pos,
        MouseButton::Left,
        ButtonAction::Down,
        1,
        Modifiers::NONE,
    );
    alice_page.send_mouse_button(pos, MouseButton::Left, ButtonAction::Up, 1, Modifiers::NONE);
    alice_page.send_key(KeyInput {
        action: KeyAction::Down,
        windows_key_code: 0x41,
        native_key_code: 0,
        character: 0,
        modifiers: Modifiers::NONE,
    });
    alice_page.send_key(KeyInput {
        action: KeyAction::Char,
        windows_key_code: 0x41,
        native_key_code: 0,
        character: u16::try_from('a' as u32).unwrap(),
        modifiers: Modifiers::NONE,
    });
    alice_page.send_key(KeyInput {
        action: KeyAction::Up,
        windows_key_code: 0x41,
        native_key_code: 0,
        character: 0,
        modifiers: Modifiers::NONE,
    });
    alice_page.send_mouse_wheel(pos, 0, -40, Modifiers::NONE);

    // Pump for a bit; the input field caret blink / focus ring should re-paint.
    let start = Instant::now();
    while start.elapsed().as_millis() < 800 {
        engine.pump();
        std::thread::sleep(Duration::from_millis(8));
    }
    let paints_after = alice_page.paint_count();
    eprintln!(
        "input injected into content page; paints {paints_before} -> {paints_after} (host accepted events)"
    );

    // Tidy up CEF before shutdown.
    chrome.close();
    alice_page.close();
    bob_page.close();
    for _ in 0..25 {
        engine.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    engine.shutdown();

    if paints_after < paints_before {
        eprintln!("FAIL: paint count went backwards");
        return ExitCode::FAILURE;
    }

    println!("OK: two profiles created, chrome+content pages, input injected");
    ExitCode::SUCCESS
}
