// tab-close.spec.mjs — Lane 3 E2E (ADR-0020 §3, bug #2).
//
// BUG #2 — Stale content after tab close: after closing a tab, the viewport
// continues to show the closed tab's content instead of switching to the
// remaining tab. The compositor does not repaint after the tab-close op.
//
// This spec is QUARANTINED via test.fixme. The tab-close op sequence IS
// exercisable headlessly (open 2 tabs, close one via close_tab op, check CDP
// targets), but the visual repaint assertion is blocked:
//
// PARTIAL IMPLEMENTATION BLOCKER — visual repaint verification:
// Playwright's screenshot() on the chrome page captures only the DOM renderer
// (the CEF chrome page). Verifying that the wgpu compositor surface repainted
// to show the remaining tab's content requires either an X11 composite capture
// (xwd, scrot) or a native screenshot op — neither is available over standard CDP.
//
// WHAT IS EXERCISABLE: the tab lifecycle (open 2 → close 1 → verify CDP
// targets) is fully testable. The shell's close_tab op requires a tab ID,
// which is read from the chrome DOM via the data-tab-id attribute (populated
// by set_tabs push). The visual repaint assertion is documented as TODO.
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

    // Step 5 (BLOCKED — visual repaint assertion):
    // After close, the compositor should repaint to show the remaining tab.
    // Asserting this requires capturing the wgpu compositor output, which is
    // not accessible via CDP Page.captureScreenshot.
    //
    // TODO: once xwd/scrot or a native screenshot op is available, compare:
    //   before-close screenshot vs. after-close screenshot and assert they differ.
    //
    // BLOCKER: wgpu compositor repaint verification requires native screenshot
    // or a compositor-side CDP event — not available over standard CDP.
    console.log(
      "[tab-close] CDP target lifecycle OK (1 content tab remaining). " +
        "Visual repaint assertion BLOCKED — see spec comment."
    );
  }
);
