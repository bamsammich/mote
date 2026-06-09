// playwright.component.mjs — Lane 1 (ADR-0020 §1): component tests.
//
// Renders chrome JS against the golden fixture in a real headless Chromium.
// A static webServer serves crates/mote-ui/chrome/ over HTTP so the harness
// HTML can load the real CSS and JS with proper relative paths.
//
// Runs pre-commit (headless, no display needed, a few seconds).
// Gate command (lefthook):
//   mise exec -- npx playwright test --config playwright.component.mjs

import { defineConfig } from "@playwright/test";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

// __dirname equivalent for ESM.
const __dirname = fileURLToPath(new URL(".", import.meta.url));
// The chrome/ directory to serve.
const CHROME_DIR = join(__dirname, "chrome");

// Fixed port for the static server (avoids dynamic-port indirection).
const STATIC_PORT = 6175;

export default defineConfig({
  testDir: "chrome/__tests__",

  // One worker: tests share a static server, no contention.
  workers: 1,

  // No retries — flaky tests must be fixed, not swallowed.
  retries: 0,

  timeout: 30_000,
  expect: {
    timeout: 10_000,
    // Pixel-perfect threshold for toHaveScreenshot.  If screenshots prove
    // machine-sensitive (font AA, subpixel), increase maxDiffPixelRatio and
    // document the reason here.
    toHaveScreenshot: { maxDiffPixelRatio: 0 },
  },

  use: {
    headless: true,
    baseURL: `http://127.0.0.1:${STATIC_PORT}`,
  },

  // Static file server: serves chrome/ over HTTP.
  // The server script is at chrome/__tests__/static-server.cjs to avoid
  // embedding shell-escaped paths in an inline -e string.
  webServer: {
    command: `node ${join(__dirname, "chrome/__tests__/static-server.cjs")} ${STATIC_PORT}`,
    url: `http://127.0.0.1:${STATIC_PORT}/__tests__/harness/integrity.html`,
    reuseExistingServer: false,
  },

  reporter: [["list"], ["html", { open: "never", outputFolder: "playwright-component-report" }]],

  // Visual snapshot baselines live alongside the test file.
  snapshotDir: "chrome/__tests__/__snapshots__",
  // Update baselines with:
  //   mise exec -- npx playwright test --config playwright.component.mjs --update-snapshots
});
