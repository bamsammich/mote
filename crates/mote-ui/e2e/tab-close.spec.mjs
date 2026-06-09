// tab-close.spec.mjs — Lane 3 E2E (ADR-0020 §3, bug #2).
//
// BUG #2 — Stale content after tab close: after closing a tab, the viewport
// continues to show the closed tab's content instead of switching to the
// remaining tab. The compositor does not repaint after the tab-close op.
//
// This spec is QUARANTINED via test.fixme. The tab-close op sequence IS
// exercisable headlessly, but a tab-count assertion does NOT guard BUG #2.
//
// REGRESSION GUARD (the bug IS covered): BUG #2 is a COMPOSITOR REPAINT bug —
// after activating the surviving tab, the compositor keeps the closed tab's
// retained texture (the count-based upload_frames skip never re-uploads). It is
// guarded deterministically by the compositor unit test:
//   crates/mote-ui/tests/compositor_offscreen.rs
//     fn clear_page_drops_stale_texture_shows_clear_color
// which uploads a prior-tab page, calls clear_page() (the on_active_changed
// fix), and asserts the viewport center is the clear color, not the prior
// page's pixels.
//
// WHY THIS SPEC STAYS test.fixme (not a forced green):
//   - The exercisable part here (open 2 → close 1 → verify CDP target count) is
//     a TAB-LIFECYCLE assertion. It passes whether or not the compositor
//     repaints — so flipping it green would be a FALSE guard for BUG #2.
//   - import -window root CAN capture the wgpu surface under Xvfb (verified
//     during this fix: it reads back the composited frame, ~970 colors, chrome
//     dusk pixel intact). But a true visual guard needs two tabs with
//     deterministic, distinct on-screen colors and a known viewport rect to
//     diff before/after close; network pages (example.com / mozilla.org) are
//     not color-controlled, making the pixel assertion fragile. The compositor
//     unit test above is the deterministic guard instead.
//
// The shell's close_tab op requires a tab ID, read from the chrome DOM via the
// data-tab-id attribute (populated by set_tabs push).
//
// Implementation note: content tabs (http/https) opened via new_tab ARE tracked
// by Playwright's connectOverCDP (they appear in ctx.pages()); settings pages
// are not. See fixtures.mjs header comment.

import { test, expect } from "./fixtures.mjs";

test.fixme(
  "closing a tab removes it from CDP targets and updates the shell state (bug #2)",
  async ({ mote }) => {
    const chrome = mote.chromePage();

    // Step 1: open two content tabs (http URLs).
    // These ARE tracked by Playwright since they're opened in content contexts.
    await chrome.evaluate(() =>
      window.mote.invoke("new_tab", { url: "https://example.com" })
    );
    await chrome.evaluate(() =>
      window.mote.invoke("new_tab", { url: "https://mozilla.org" })
    );

    // Wait for both to appear as CDP content targets (Playwright tracks these).
    await expect
      .poll(
        () => mote.contentPages().length >= 2,
        { timeout: 15_000, intervals: [300], message: "expected 2 content tabs" }
      )
      .toBe(true);

    // Step 2: read tab IDs from the chrome DOM.
    // The shell pushes tab state via applyOp('set_tabs', {tabs:[{id,...}]})
    // and panels.js renders each tab as a DOM element with [data-tab-id].
    let tabs;
    await expect
      .poll(
        async () => {
          tabs = await chrome.evaluate(() => {
            const tabEls = document.querySelectorAll("[data-tab-id]");
            if (tabEls.length < 2) return null;
            return Array.from(tabEls).map((el) => ({
              id: Number(el.getAttribute("data-tab-id")),
              active: el.classList.contains("is-active"),
            }));
          });
          return tabs !== null && tabs.length >= 2;
        },
        { timeout: 10_000, intervals: [200], message: "tab strip did not show 2 tabs" }
      )
      .toBe(true);

    const tabToClose = tabs.find((t) => !t.active) ?? tabs[0];

    // Step 3: close the tab via the close_tab op.
    await chrome.evaluate(
      ({ id }) => window.mote.invoke("close_tab", { id }),
      { id: tabToClose.id }
    );

    // Brief wait for the shell to process the close.
    await new Promise((r) => setTimeout(r, 1_500));

    // Step 4 (exercisable): verify one fewer CDP content target.
    // Note: the raw /json endpoint also reflects this — both Playwright and
    // the raw CDP should show the reduced count.
    const remaining = mote.contentPages();
    expect(remaining.length).toBe(1);

    // Step 5 (NOT a BUG #2 guard — see header):
    // The compositor repaint after close is the actual BUG #2 behavior, and it
    // is NOT asserted here: the CDP target count above is a tab-lifecycle check
    // that passes regardless of whether the compositor repaints. The wgpu
    // surface CAN be captured under Xvfb (`import -window root`), but a robust
    // pixel assertion needs deterministic, distinctly-colored tab content and a
    // known viewport rect — fragile with network pages. The deterministic
    // BUG #2 guard is the compositor unit test named in the header.
    console.log(
      "[tab-close] CDP target lifecycle OK (1 content tab remaining). " +
        "BUG #2 repaint is guarded by the compositor unit test — see header."
    );
  }
);
