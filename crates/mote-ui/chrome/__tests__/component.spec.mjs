/**
 * component.spec.mjs — Lane 1 component tests (ADR-0020 §1).
 *
 * Real headless Chromium renders the chrome JS against the golden fixture
 * (Lane-2 contract) and structural DOM assertions verify correctness in both
 * themes (dusk / vellum).  Per-theme visual snapshots add CSS/theme coverage
 * on top; if a snapshot proves machine-sensitive it is documented as a
 * follow-up below.
 *
 * Test inventory:
 *   H14-dusk / H14-vellum  — integrity list from the golden fixture
 *   H16-dusk / H16-vellum  — mismatch drill-down detail panel
 *   H4-dusk  / H4-vellum   — plugin card timeline (panels.js builder)
 *   pure-logic              — opDecisionClass, roving step/keyToDir (node-
 *                             evaluated, migrated from panels.test.js /
 *                             roving.test.js; no browser needed)
 *
 * Golden fixture: chrome/__fixtures__/integrity_list.json (Lane-2 output).
 * Lane 1 consumes it directly so a Rust payload-shape change that forces a
 * fixture regeneration automatically propagates here.
 */

import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { createContext, runInContext } from "node:vm";

const __dirname = dirname(fileURLToPath(import.meta.url));
const CHROME_DIR = join(__dirname, "..");
const FIXTURES_DIR = join(CHROME_DIR, "__fixtures__");
const STATIC_PORT = 6175;

// ── Golden fixture (the Lane-2 contract) ─────────────────────────────────────
// Parsed once; tests push it into the harness page via page.evaluate().
const FIXTURE = JSON.parse(
  readFileSync(join(FIXTURES_DIR, "integrity_list.json"), "utf8")
);

// ── Harness URL helper ────────────────────────────────────────────────────────
function harnessUrl(theme) {
  return `http://127.0.0.1:${STATIC_PORT}/__tests__/harness/integrity.html?theme=${theme}`;
}

// ── Shared setup: navigate to harness + clear invoke log ─────────────────────
async function gotoHarness(page, theme) {
  await page.goto(harnessUrl(theme));
  // Wait for settings.js's DOMContentLoaded boot to finish.
  await page.waitForFunction(() => typeof window.__moteSettingsTest === "object");
  // Clear any invocations that the boot triggered (e.g. integrity_list call).
  await page.evaluate(() => { window.__invokeLog = []; });
}

// ── H14: integrity list from golden fixture ───────────────────────────────────

for (const theme of ["dusk", "vellum"]) {
  test(`H14-${theme}: integrity table renders fixture rows`, async ({ page }) => {
    await gotoHarness(page, theme);

    // Push the golden fixture into the page.
    await page.evaluate((payload) => {
      window.__moteIntegrityList(payload);
    }, FIXTURE);

    const tbody = page.locator(".integrity-table tbody");

    // Row count matches the fixture.
    const rows = tbody.locator("tr[data-plugin]");
    await expect(rows).toHaveCount(FIXTURE.plugins.length);

    // Verify each plugin's name and badge class from the fixture.
    for (const plugin of FIXTURE.plugins) {
      const row = tbody.locator(`tr[data-plugin="${plugin.name}"]`);
      await expect(row).toBeVisible();

      // data-status attribute matches the fixture integrity field.
      const expectedStatus = statusDataAttrFor(plugin.integrity);
      await expect(row).toHaveAttribute("data-status", expectedStatus);

      // Badge variant class is correct.
      const badge = row.locator(".badge");
      const expectedClass = badgeVariantFor(plugin.integrity);
      if (expectedClass) {
        await expect(badge).toHaveClass(new RegExp(`\\b${expectedClass}\\b`));
      }
    }

    // Structural DOM assertion: table is present and themed correctly.
    await expect(page.locator(".integrity-table")).toBeVisible();

    // Visual snapshot — per-theme baseline.
    // If this proves flaky across runs, set { maxDiffPixelRatio: 0.01 } in
    // playwright.component.mjs and document the reason.
    await expect(page.locator(".integrity-table")).toHaveScreenshot(
      `integrity-table-${theme}.png`
    );
  });
}

// ── H16: mismatch drill-down detail panel ────────────────────────────────────

// Synthetic mismatch payload: bundled plugins never produce real mismatches,
// so we inject a hand-built one to exercise the danger-banner path.
const MISMATCH_DETAIL = {
  name: "evil-plugin",
  integrity: "Mismatch",
  checksum: "sha256:aabbccdd00112233",
  actual_checksum: "sha256:deadbeef99887766",
  lock_source: "plugins.lock",
  pinned_commit: "abc123def456",
};

for (const theme of ["dusk", "vellum"]) {
  test(`H16-${theme}: mismatch drill-down shows danger banner + both checksums`, async ({ page }) => {
    await gotoHarness(page, theme);

    // First populate the table so the row-click wiring is in place.
    await page.evaluate((payload) => {
      window.__moteIntegrityList(payload);
    }, FIXTURE);

    // Push the synthetic mismatch detail directly (no row click needed — the
    // shell would push this after receiving the integrity_plugin_detail op).
    await page.evaluate((detail) => {
      window.__moteIntegrityDetail(detail);
    }, MISMATCH_DETAIL);

    // Danger banner must be visible.
    const banner = page.locator(".detail-mismatch-banner");
    await expect(banner).toBeVisible();

    // Both checksum values must appear.
    const checksumValues = page.locator(".detail-checksum-value");
    await expect(checksumValues).toHaveCount(2);
    await expect(checksumValues.nth(0)).toContainText(MISMATCH_DETAIL.checksum);
    await expect(checksumValues.nth(1)).toContainText(MISMATCH_DETAIL.actual_checksum);

    // Detail panel is visible.
    const panel = page.locator("#integrity-detail-panel");
    await expect(panel).toBeVisible();

    // Visual snapshot — detail panel in each theme.
    await expect(panel).toHaveScreenshot(`integrity-detail-mismatch-${theme}.png`);
  });
}

// ── H4: plugin card timeline (panels.js builder) ─────────────────────────────

// H4 exercises panels.js's buildRecentOpsTimeline via buildPanelDom.
// We render an IntegrityPanel payload with one plugin that has recent_ops
// including a "deny" — this exercises the op-decision-class danger treatment.

const PANEL_FIXTURE = {
  plugins: [
    {
      name: "test-plugin",
      version: "1.0.0",
      integrity: "Verified",
      kind: "Bundled",
      fulfills: [],
      consumes: [],
      permissions: [],
      secrets: [],
      recent_ops: [
        { operation: "read_bookmark", decision: "allow", latency: "2ms", when: "1s ago" },
        { operation: "fetch_history", decision: "deny",  latency: "1ms", when: "5s ago" },
        { operation: "write_tab",     decision: "defer", latency: "3ms", when: "10s ago" },
      ],
      actions: [],
    },
  ],
  network_audit: [],
  storage: [],
  denials: [],
};

for (const theme of ["dusk", "vellum"]) {
  test(`H4-${theme}: plugin card timeline rows + deny danger class`, async ({ page }) => {
    await gotoHarness(page, theme);

    // Render the integrity panel using the __MOTE_TEST__ guarded builder.
    // panels.js is NOT loaded by the harness, so we load it in the page
    // context and call buildPanelDom directly.
    await page.addScriptTag({ url: `http://127.0.0.1:${STATIC_PORT}/panels.js` });
    // Wait for panels.js to install its exports.
    await page.waitForFunction(() => window.__MOTE_TEST__ && typeof window.__motePanelsTest === "object");

    const container = await page.evaluate((panel) => {
      var dom = window.__motePanelsTest.buildPanelDom(panel);
      var root = document.getElementById("mote-integrity-root") || document.createElement("div");
      root.id = "mote-integrity-root";
      root.textContent = "";
      root.appendChild(dom);
      if (!document.getElementById("mote-integrity-root")) {
        document.body.appendChild(root);
      }
      root.hidden = false;
      return root.id;
    }, PANEL_FIXTURE);

    expect(container).toBe("mote-integrity-root");

    // Recent-ops section is present.
    const recentOps = page.locator(".recent-ops");
    await expect(recentOps).toBeVisible();

    // Three timeline rows.
    const opRows = page.locator(".op-row");
    await expect(opRows).toHaveCount(3);

    // The "deny" row carries the danger modifier class.
    const denyRow = page.locator(".op-row.op-decision-deny");
    await expect(denyRow).toHaveCount(1);
    await expect(denyRow).toContainText("fetch_history");

    // Visual snapshot — plugin card in each theme.
    const card = page.locator(".plugin-card").first();
    await expect(card).toBeVisible();
    await expect(card).toHaveScreenshot(`plugin-card-timeline-${theme}.png`);
  });
}

// ── Pure-logic tests (node-side, no browser) ─────────────────────────────────
//
// Migrated from roving.test.js + panels.test.js.  These run as Playwright
// "tests" but never open a browser page — they use node:vm to evaluate the
// pure-math helpers.  Keeping them here avoids two separate test runners while
// ensuring the coverage survives the removal of the old .js files.

test.describe("pure-logic: roving nav-math", () => {
  let roving;

  test.beforeAll(() => {
    const src = readFileSync(join(CHROME_DIR, "roving.js"), "utf8");
    const sandbox = { window: {} };
    createContext(sandbox);
    runInContext(src, sandbox, { filename: "roving.js" });
    roving = sandbox.window.mote.roving;
  });

  test("step: down wrap + up wrap contract", () => {
    const { step } = roving;
    // Down
    expect(step(4, 5, 1)).toBe(0);          // last → wraps to 0
    expect(step(2, 5, 1)).toBe(3);           // middle advances
    expect(step(-1, 5, 1)).toBe(0);          // from -1 → first
    // Up
    expect(step(0, 5, -1)).toBe(4);          // 0 → wraps to last
    expect(step(3, 5, -1)).toBe(2);          // middle retreats
    expect(step(-1, 5, -1)).toBe(4);         // from -1 → last
  });

  test("step: empty list always returns -1", () => {
    const { step } = roving;
    expect(step(-1, 0, 1)).toBe(-1);
    expect(step(-1, 0, -1)).toBe(-1);
    expect(step(3, 0, 1)).toBe(-1);
  });

  test("step: single-item list stays at 0", () => {
    const { step } = roving;
    expect(step(0, 1, 1)).toBe(0);
    expect(step(0, 1, -1)).toBe(0);
    expect(step(-1, 1, 1)).toBe(0);
    expect(step(-1, 1, -1)).toBe(0);
  });

  test("step: first / last directives", () => {
    const { step } = roving;
    expect(step(3, 5, "first")).toBe(0);
    expect(step(0, 5, "last")).toBe(4);
    expect(step(-1, 0, "first")).toBe(-1);
    expect(step(-1, 0, "last")).toBe(-1);
  });

  test("step: no-wrap mode clamps", () => {
    const { step } = roving;
    expect(step(4, 5, 1, { wrap: false })).toBe(4);
    expect(step(0, 5, -1, { wrap: false })).toBe(0);
  });

  test("keyToDir: arrows always map", () => {
    const { keyToDir } = roving;
    expect(keyToDir("ArrowDown")).toBe(1);
    expect(keyToDir("ArrowUp")).toBe(-1);
    expect(keyToDir("Home")).toBe("first");
    expect(keyToDir("End")).toBe("last");
  });

  test("keyToDir: j/k behind jk flag", () => {
    const { keyToDir } = roving;
    expect(keyToDir("j", { jk: true })).toBe(1);
    expect(keyToDir("k", { jk: true })).toBe(-1);
    expect(keyToDir("j")).toBeNull();
    expect(keyToDir("k")).toBeNull();
    expect(keyToDir("j", { jk: false })).toBeNull();
  });

  test("keyToDir: unknown keys return null", () => {
    const { keyToDir } = roving;
    expect(keyToDir("Enter")).toBeNull();
    expect(keyToDir("Tab")).toBeNull();
    expect(keyToDir("")).toBeNull();
  });
});

test.describe("pure-logic: opDecisionClass", () => {
  let opDecisionClass;

  test.beforeAll(() => {
    const src = readFileSync(join(CHROME_DIR, "panels.js"), "utf8");
    const sandbox = {
      window: { __MOTE_TEST__: true },
      document: {
        createElement: () => ({
          className: "", setAttribute() {}, removeAttribute() {},
          appendChild() {}, addEventListener() {}, textContent: "", hidden: false,
        }),
        createTextNode: () => ({}),
        getElementById: () => null,
        addEventListener() {},
      },
      Node: function () {},
    };
    createContext(sandbox);
    runInContext(src, sandbox, { filename: "panels.js" });
    opDecisionClass = sandbox.window.__motePanelsTest.opDecisionClass;
  });

  test("known lowercase decisions", () => {
    expect(opDecisionClass("deny")).toBe("deny");
    expect(opDecisionClass("allow")).toBe("allow");
    expect(opDecisionClass("defer")).toBe("defer");
  });

  test("case-insensitive (defensive against Rust serializer changes)", () => {
    expect(opDecisionClass("Deny")).toBe("deny");
    expect(opDecisionClass("Allow")).toBe("allow");
    expect(opDecisionClass("Defer")).toBe("defer");
  });

  test("unknown / empty → empty string", () => {
    expect(opDecisionClass("unknown")).toBe("");
    expect(opDecisionClass("")).toBe("");
    expect(opDecisionClass(null)).toBe("");
    expect(opDecisionClass(undefined)).toBe("");
  });
});

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Mirror of settings.js's statusDataAttr() for fixture assertion. */
function statusDataAttrFor(status) {
  switch (status) {
    case "Verified": return "verified";
    case "Mismatch": return "mismatch";
    case "DevMode":  return "dev-mode";
    case "Bundled":  return "bundled";
    default: return "unknown";
  }
}

/** Mirror of settings.js's integrityBadgeVariant() for fixture assertion. */
function badgeVariantFor(status) {
  switch (status) {
    case "Verified": return "success";
    case "Mismatch": return "danger";
    case "DevMode":  return "accent";
    case "Bundled":  return "info";
    default: return "";
  }
}
