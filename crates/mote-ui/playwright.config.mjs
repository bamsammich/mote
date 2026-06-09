// playwright.config.mjs — Lane 3 (full-app E2E via CDP into CEF).
//
// ADR-0020 §3: drives the real running mote app over the Chrome DevTools
// Protocol (CDPfortest). See docs/running-mote-headless.md for the launch
// incantation and the critical WAYLAND_DISPLAY gotcha.
//
// Lane 3 runs pre-push; it is NOT wired into lefthook yet (pending Lane 1+2
// completion). Run manually: cd crates/mote-ui && npx playwright test

import { defineConfig } from "@playwright/test";

export default defineConfig({
  // All E2E specs live here.
  testDir: "e2e",

  // CEF is process-global: only one mote instance at a time.
  workers: 1,

  // No retries — intermittent failures mask real regressions.
  retries: 0,

  // Generous timeouts: CEF + Xvfb boot takes ~1-2 s; individual ops can take
  // several seconds on a debug build.
  timeout: 60_000,
  expect: { timeout: 15_000 },

  reporter: [["list"], ["html", { open: "never", outputFolder: "playwright-report" }]],
});
