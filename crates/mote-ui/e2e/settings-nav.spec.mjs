// settings-nav.spec.mjs — Lane 3 E2E (ADR-0020 §3, bug #3).
//
// BUG #3 — Settings section-nav: clicking a settings tab (e.g. "integrity")
// from within a settings page did not navigate to the target section.
//
// ROOT CAUSE (RESOLVED 2026-06-09 — ADR-0005 amendment, this change set):
//
//   Layer 1 (renderer origin gate): mote-bridge.js installs window.mote.invoke
//   on all privileged mote://chrome pages by wrapping window.cefQuery.
//
//   Layer 2 (browser-side router) — THE FIX: the browser-side message router is
//   now attached to EVERY privileged mote://chrome page — the chrome root AND the
//   enumerated settings sections — gated by the ChromePageRequest / PageRole::Chrome
//   type marker. Settings pages are opened onto that bridge-bearing path
//   (create_tab_page -> HostBridge::open_chrome_page), so a settings page's
//   cefQuery reaches the op registry and the navigate op dispatches. Previously
//   the settings page was a router-less Overlay tab and the navigate Promise hung
//   forever.
//
//   After the fix:
//     window.cefQuery     = exists (renderer origin gate passes)
//     window.mote         = exists (mote-bridge.js installed it)
//     window.mote.invoke() = fires cefQuery AND dispatches (browser-side router present)
//
// Implementation note: settings pages are CEF-created targets not tracked by
// Playwright's connectOverCDP. The fixture's waitForPage() + cdpEval() are
// used instead of Playwright's standard page API.

import { test, expect } from "./fixtures.mjs";

test(
  "clicking the integrity settings tab navigates to settings/integrity (bug #3)",
  async ({ mote }) => {
    const chrome = mote.chromePage();

    // 1. Open settings/general via new_tab.
    await chrome.evaluate(() =>
      window.mote.invoke("new_tab", { url: "mote://chrome/settings/general" })
    );

    // Wait for settings/general to appear (CEF-created page, polled via /json).
    const generalPage = await mote.waitForPage("settings/general", {
      timeout: 15_000,
    });

    // 1a. Layer 1: mote-bridge.js installs window.mote.invoke. Poll — waitForPage
    //     returns as soon as the target appears in /json, which can be before the
    //     page's scripts have finished running (the binding install races the
    //     commit). The binding always arrives; we just wait for it.
    await expect
      .poll(
        () =>
          generalPage.evaluate(
            "typeof window.mote !== 'undefined' && typeof window.mote.invoke === 'function'"
          ),
        { timeout: 15_000 }
      )
      .toBe(true);

    // 2. Click the "integrity" settings tab.
    await generalPage.evaluate(`
      (function() {
        var tab = document.querySelector(".settings-tab[data-section='integrity']");
        if (!tab) throw new Error("integrity tab not found");
        tab.click();
      })()
    `);

    // 3. Layer 2 fixed: the navigate op dispatches (browser-side router is now on
    //    the settings page's client). The settings/integrity page appears.
    const integrityPageAfter = await mote.waitForPage("settings/integrity", {
      timeout: 15_000,
    });

    await integrityPageAfter.locator(".integrity-table").toHaveCount(1, {
      timeout: 10_000,
    });
  }
);
