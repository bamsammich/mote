// resize.spec.mjs — Lane 3 E2E (ADR-0020 §3, bug #1).
//
// BUG #1 — Window resize proportion: after resizing the mote window, the
// content viewport does not fill the expected geometry. The expected viewport
// size after resize to 1600×1000 is:
//   width  = 1600 - VIEWPORT_LEFT (316)  = 1284
//   height = 1000 - VIEWPORT_TOP  (44)   = 956
//
// This spec is QUARANTINED via test.fixme. See BLOCKER note below.
//
// BLOCKER — headless window resize under Xvfb without a WM:
//   Winit creates a window on the Xvfb display, but Xvfb runs without a window
//   manager. Programmatic window resize requires `xdotool`, which is NOT
//   installed on this machine:
//     $ command -v xdotool → (not found)
//   When xdotool becomes available, the resize mechanic is:
//     xdotool search --name "mote" windowsize 1600 1000
//   Until then, this test cannot exercise the actual resize path. The fixture's
//   setWindowSize() throws explicitly with a descriptive error.
//
// Once xdotool is installed AND the resize bug is fixed, replace test.fixme
// with test() and implement the full assertion.

import { test, expect } from "./fixtures.mjs";

// Constants from DESIGN.md / layout spec.
const VIEWPORT_LEFT = 316;
const VIEWPORT_TOP = 44;

test.fixme(
  "content viewport fills expected geometry after window resize to 1600×1000 (bug #1)",
  async ({ mote }) => {
    // BLOCKER: xdotool is not installed — cannot resize window under headless Xvfb.
    // See spec header comment for the intended mechanic and unblock path.

    // Step 1: attempt resize (will throw with a descriptive blocker message).
    // When xdotool is available, the fixture will implement this properly.
    mote.setWindowSize(1600, 1000);

    // Step 2 (intended assertion — not yet reachable):
    // Open a content tab and verify viewport dimensions.
    const chrome = mote.chromePage();
    await chrome.evaluate(() =>
      window.mote.invoke("new_tab", { url: "https://example.com" })
    );

    let contentPage;
    await expect
      .poll(
        async () => {
          const pages = mote.contentPages();
          contentPage = pages.find((p) => p.url().includes("example.com"));
          return contentPage !== undefined;
        },
        { timeout: 10_000, intervals: [300] }
      )
      .toBe(true);

    const { innerWidth, innerHeight } = await contentPage.evaluate(() => ({
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
    }));

    expect(innerWidth).toBe(1600 - VIEWPORT_LEFT);
    expect(innerHeight).toBe(1000 - VIEWPORT_TOP);

    // Step 3 (intended assertion): screenshot bottom region of viewport is non-blank.
    // (Validates the compositor actually fills the enlarged viewport.)
    // TODO: implement once resize mechanic works.
  }
);
