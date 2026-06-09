// integrity-settings.spec.mjs — Lane 3 E2E (ADR-0020 §3, ADR-0021).
//
// LIVE two-layer-transport proof (ADR-0005 amendment 2026-06-09).
//
// Verifies the in-settings integrity transport is LIVE — not just that static
// seed rows render. The settings page is now BRIDGE-BEARING: it is opened onto
// the privileged mote://chrome chrome-page path (PageRole::Chrome via
// ChromePageRequest), so its client carries the browser-side message router
// wired to the same OpRegistry as the chrome root. window.cefQuery messages from
// the settings page therefore reach the op registry and dispatch (previously
// they hit a router-less Overlay client and hung — bug #3 / the H14/H16 break).
//
// The proof (why static seed rows are NOT enough):
//   1. Open settings/integrity; confirm window.mote.invoke is installed.
//   2. CLEAR the integrity table tbody (remove the static seed rows).
//   3. RE-REQUEST integrity_list over the live transport.
//   4. Poll for the 3 rows to REAPPEAR.
// If Layer 2 were still dead, the cleared rows would never return — the invoke
// would hang and the table would stay empty. Their reappearance is positive proof
// the round-trip (settings page -> cefQuery -> browser-side router -> OpRegistry
// -> applyOp back into the settings DOM) is live.
//
// http-iframes-mote://chrome regression (audit case): an http page that iframes
// mote://chrome/settings/* is blocked because the subframe's query is delivered
// to the http browser's router-less client. Hosting a real http origin in this
// headless harness is impractical (no local server in the fixture); this is
// covered by the CEF-free type-routing assertions in mote-shell
// (adr0005_content_and_overlay_are_router_less) and is left as a follow-up live
// regression test here rather than faked.
//
// Implementation note: settings pages opened via new_tab appear as CDP targets
// with URL mote://chrome/settings/integrity. Playwright's connectOverCDP does
// NOT track pages created by CEF's internal browser (only initial pages are
// tracked). The fixture's waitForPage() polls raw /json and uses raw CDP
// WebSocket calls to interact with these pages — see fixtures.mjs header.

import { test, expect } from "./fixtures.mjs";

test("integrity_list transport is LIVE: cleared rows reappear via the bridge", async ({ mote }) => {
  const chrome = mote.chromePage();

  // Navigate to settings/integrity via the new_tab op (as the UI would).
  await chrome.evaluate(() =>
    window.mote.invoke("new_tab", { url: "mote://chrome/settings/integrity" })
  );

  // Wait for the settings/integrity page to appear as a CDP target.
  const integrityPage = await mote.waitForPage("settings/integrity", {
    timeout: 20_000,
  });

  // ── Bridge presence check ─────────────────────────────────────────────────
  // Poll: waitForPage returns when the target appears in /json, which can race
  // the page's scripts finishing (the binding install). The binding always
  // arrives; we just wait for it.
  await expect
    .poll(
      () =>
        integrityPage.evaluate(
          "typeof window.mote !== 'undefined' && typeof window.mote.invoke === 'function'"
        ),
      { timeout: 15_000 }
    )
    .toBe(true);

  // ── Baseline: the 3 bundled-plugin rows are present (static seed or live). ──
  await integrityPage.locator(".integrity-table tbody tr[data-plugin]").toHaveCount(3, {
    timeout: 15_000,
  });

  // ── LIVE PROOF — clear the tbody, re-request, assert rows reappear ──────────
  //
  // 1. Clear the table body. After this the static seed rows are GONE; only a
  //    live transport can repopulate them.
  await integrityPage.evaluate(
    "document.querySelector('.integrity-table tbody').innerHTML = ''; document.querySelectorAll('.integrity-table tbody tr[data-plugin]').length"
  );
  const afterClear = await integrityPage.evaluate(
    "document.querySelectorAll('.integrity-table tbody tr[data-plugin]').length"
  );
  expect(afterClear).toBe(0);

  // 2. Re-request integrity_list over the live transport. With Layer 2 fixed the
  //    settings page's cefQuery reaches the OpRegistry; the shell pushes an
  //    applyOp that rebuilds the rows. (If the transport were dead this invoke
  //    would hang and the rows would never return.)
  await integrityPage.evaluate("window.mote.invoke('integrity_list', {})");

  // 3. Poll for the 3 rows to REAPPEAR — the positive proof of a live round-trip.
  await integrityPage.locator(".integrity-table tbody tr[data-plugin]").toHaveCount(3, {
    timeout: 15_000,
  });

  // All 3 bundled plugins are present by name after the live re-request.
  const pluginNames = await integrityPage
    .locator(".integrity-table tbody tr[data-plugin]")
    .evaluateAll((rows) => rows.map((r) => r.getAttribute("data-plugin")));

  expect(pluginNames).toEqual(
    expect.arrayContaining(["bookmarks", "history", "workspace-manager"])
  );
});
