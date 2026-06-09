// fixtures.mjs — Lane 3 launch fixture (ADR-0020 §3, ADR-0021).
//
// launchMote({ size? }):
//   1. Allocates a unique Xvfb display number + CDP port.
//   2. Starts Xvfb for a virtual display (no window on the real compositor).
//   3. Launches mote headless with WAYLAND_DISPLAY scrubbed and a fresh
//      temp working dir (isolates .mote-cef-cache per run).
//   4. Waits for the CDP endpoint to respond.
//   5. Connects Playwright over CDP to get the initial pages (chrome + newtab).
//   6. Returns helpers: cdp, chromePage(), contentPages(), findPage(),
//      waitForPage(), cdpEval(), stderr(), setWindowSize(), windowScreenshot(), close().
//
// Playwright limitation with connectOverCDP + CEF:
//   When CEF creates new pages internally (via the shell's open_tab op),
//   Playwright's BrowserContext.pages() does NOT update — it only sees pages
//   that existed at connection time. waitForEvent("page") also does not fire.
//   Root cause: CEF page creation does not go through Playwright's Target tracking
//   mechanism. Workaround: poll the raw /json CDP endpoint for new targets and
//   use raw CDP WebSocket calls to evaluate JS on those pages.
//   The waitForPage() helper + cdpEval() implement this pattern.
//
// Exported as a Playwright test fixture via `test` (wraps base `test`).
//
// CRITICAL — WAYLAND_DISPLAY must NEVER be set in the mote subprocess env.
// See docs/running-mote-headless.md §"The WAYLAND_DISPLAY gotcha".

import { chromium, test as base } from "@playwright/test";
import { spawn, execSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readFileSync } from "node:fs";
// WebSocket is a global in Node >= 21 (no import needed).

// Repo root — fixtures.mjs is at crates/mote-ui/e2e/fixtures.mjs.
// Go up three levels: e2e/ → mote-ui/ → crates/ → repo root.
const REPO_ROOT = new URL("../../../", import.meta.url).pathname.replace(/\/$/, "");
const MOTE_BIN = join(REPO_ROOT, "target", "debug", "mote");
const CEF_LIB_DIR = join(REPO_ROOT, "target", "debug");

// Shared counter for allocating unique display+port pairs across parallel
// invocations (workers=1 per playwright.config.mjs, but defensive).
let _counter = 0;

// Wait up to `ms` for `fn()` to resolve without throwing, polling every
// `interval` ms.
async function pollUntil(fn, { ms = 15_000, interval = 200 } = {}) {
  const deadline = Date.now() + ms;
  let lastErr;
  while (Date.now() < deadline) {
    try {
      return await fn();
    } catch (e) {
      lastErr = e;
      await new Promise((r) => setTimeout(r, interval));
    }
  }
  throw new Error(`pollUntil timed out after ${ms}ms: ${lastErr}`);
}

/**
 * Execute a CDP Runtime.evaluate call over a raw WebSocket to a page target.
 * Returns the evaluated value (returnByValue semantics).
 */
async function rawCDPEval(wsUrl, expression, { timeout = 10_000 } = {}) {
  return new Promise((resolve, reject) => {
    const ws = new globalThis.WebSocket(wsUrl);
    let id = 1;
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error(`rawCDPEval timed out (${timeout}ms): ${expression.slice(0, 80)}`));
    }, timeout);

    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          id,
          method: "Runtime.evaluate",
          params: { expression, returnByValue: true, awaitPromise: true },
        })
      );
    };

    ws.onmessage = (e) => {
      const msg = JSON.parse(typeof e.data === "string" ? e.data : e.data.toString());
      if (msg.id === id) {
        clearTimeout(timer);
        ws.close();
        if (msg.result?.exceptionDetails) {
          reject(
            new Error(
              `CDP eval threw: ${msg.result.exceptionDetails.exception?.description ?? JSON.stringify(msg.result.exceptionDetails)}`
            )
          );
        } else {
          resolve(msg.result?.result?.value);
        }
      }
    };

    ws.onerror = (e) => {
      clearTimeout(timer);
      reject(new Error(`CDP WebSocket error: ${e.message ?? e}`));
    };
  });
}

/**
 * Launch mote under Xvfb headlessly and connect Playwright over CDP.
 *
 * @param {{ size?: { width: number, height: number } }} opts
 * @returns {Promise<{
 *   cdp: import("@playwright/test").Browser,
 *   chromePage: () => import("@playwright/test").Page,
 *   contentPages: () => import("@playwright/test").Page[],
 *   waitForPage: (urlSubstring: string, opts?: { timeout?: number }) => Promise<{ url: string, wsUrl: string, evaluate: (expr: string) => Promise<any>, locator: (selector: string) => { count: () => Promise<number>, evaluateAll: (fn: Function) => Promise<any> } }>,
 *   cdpEval: (wsUrl: string, expression: string) => Promise<any>,
 *   stderr: () => string,
 *   setWindowSize: (w: number, h: number) => void,
 *   windowScreenshot: () => Promise<Buffer>,
 *   close: () => Promise<void>
 * }>}
 */
export async function launchMote({ size = { width: 1280, height: 800 } } = {}) {
  const id = ++_counter;
  // Use high display numbers / ports to avoid conflicts with anything running.
  const displayNum = 90 + id;
  const cdpPort = 19200 + id;
  const display = `:${displayNum}`;
  const cdpEndpoint = `http://127.0.0.1:${cdpPort}`;

  // Fresh temp dir → fresh .mote-cef-cache per run (no session restore pollution).
  const workDir = mkdtempSync(join(tmpdir(), "mote-e2e-"));

  // ── 1. Start Xvfb ────────────────────────────────────────────────────────
  const xvfbArgs = [
    display,
    "-screen", "0", `${size.width}x${size.height}x24`,
    "-nolisten", "tcp",
  ];
  const xvfb = spawn("Xvfb", xvfbArgs, {
    stdio: "ignore",
    detached: false,
  });

  // Give Xvfb a moment to bind the socket.
  await new Promise((r) => setTimeout(r, 300));

  if (xvfb.exitCode !== null) {
    throw new Error(`Xvfb exited immediately with code ${xvfb.exitCode}`);
  }

  // ── 2. Build mote subprocess env (WAYLAND_DISPLAY scrubbed) ──────────────
  const moteEnv = { ...process.env };
  delete moteEnv.WAYLAND_DISPLAY; // CRITICAL — see doc gotcha
  // Also scrub XDG_STATE_HOME — mote prefers it over HOME for state_dir()
  // (see mote-shell/src/lib.rs state_dir()). Without scrubbing, the real user
  // session is restored even with HOME overridden.
  delete moteEnv.XDG_STATE_HOME;
  moteEnv.WINIT_UNIX_BACKEND = "x11";
  moteEnv.DISPLAY = display;
  moteEnv.MOTE_REMOTE_DEBUG_PORT = String(cdpPort);
  moteEnv.LD_LIBRARY_PATH = CEF_LIB_DIR;
  // Isolate session state + CEF profile/cache to the fresh temp dir.
  moteEnv.HOME = workDir;

  // ── 3. Launch mote ───────────────────────────────────────────────────────
  let stderrBuf = "";
  const mote = spawn(MOTE_BIN, ["--ozone-platform=x11"], {
    cwd: workDir,
    env: moteEnv,
    stdio: ["ignore", "ignore", "pipe"],
    detached: false,
  });

  mote.stderr.on("data", (chunk) => {
    stderrBuf += chunk.toString();
  });

  let moteExited = false;
  mote.on("exit", () => { moteExited = true; });

  // ── 3a. Verify env: DISPLAY set, WAYLAND_DISPLAY absent ──────────────────
  await new Promise((r) => setTimeout(r, 500));

  if (!moteExited) {
    try {
      const procEnv = readFileSync(`/proc/${mote.pid}/environ`, "utf8");
      const envPairs = procEnv.split("\0");
      const hasWayland = envPairs.some((e) => e.startsWith("WAYLAND_DISPLAY="));
      const hasDisplay = envPairs.some((e) => e === `DISPLAY=${display}`);
      if (hasWayland) {
        throw new Error(
          "SAFETY: mote subprocess has WAYLAND_DISPLAY set — it will attach to the real compositor!"
        );
      }
      if (!hasDisplay) {
        throw new Error(
          `mote subprocess does not have DISPLAY=${display} set — boot may attach to wrong display`
        );
      }
    } catch (e) {
      if (e.message.startsWith("SAFETY:") || e.message.startsWith("mote subprocess")) {
        throw e;
      }
      // /proc read can race; non-fatal if the process is still starting.
    }
  }

  // ── 4. Wait for CDP endpoint ─────────────────────────────────────────────
  await pollUntil(
    async () => {
      if (moteExited) throw new Error(`mote exited early; stderr:\n${stderrBuf}`);
      const res = await fetch(`${cdpEndpoint}/json/version`);
      if (!res.ok) throw new Error(`/json/version returned ${res.status}`);
      const j = await res.json();
      if (!j["webSocketDebuggerUrl"]) throw new Error("no webSocketDebuggerUrl yet");
      return j;
    },
    { ms: 30_000, interval: 300 }
  );

  // ── 5. Connect Playwright over CDP ───────────────────────────────────────
  // chromium.connectOverCDP attaches to the existing CEF process for the
  // initial pages only. New pages created via the shell's new_tab op are NOT
  // tracked by Playwright (CEF creates them internally); use waitForPage() to
  // detect them via raw /json polling.
  const browser = await chromium.connectOverCDP(cdpEndpoint);

  // ── Helpers ──────────────────────────────────────────────────────────────

  function getAllPages() {
    return browser.contexts().flatMap((c) => c.pages());
  }

  // The root chrome UI page: mote://chrome/index.html.
  function chromePage() {
    const p =
      getAllPages().find((p) => p.url().startsWith("mote://chrome/index")) ??
      getAllPages().find((p) => p.url().startsWith("mote://chrome/"));
    if (!p) throw new Error("chrome root page (mote://chrome/index.html) not found in CDP targets");
    return p;
  }

  // Pages tracked by Playwright that are NOT the root chrome page.
  function contentPages() {
    return getAllPages().filter((p) => !p.url().startsWith("mote://chrome/index"));
  }

  /**
   * Wait for a new CDP target whose URL contains `urlSubstring` to appear.
   * Uses raw /json polling since Playwright does not track pages created by
   * CEF's internal browser (see fixture header comment).
   *
   * Returns a thin CDP handle with:
   *   - url: the full target URL
   *   - wsUrl: the CDP WebSocket URL for the target
   *   - evaluate(expr): run JS in the target's renderer via raw CDP
   *   - locator(selector): minimal locator-like helpers
   *
   * @param {string} urlSubstring
   * @param {{ timeout?: number }} opts
   */
  async function waitForPage(urlSubstring, { timeout = 20_000 } = {}) {
    const target = await pollUntil(
      async () => {
        const targets = await fetch(`${cdpEndpoint}/json`).then((r) => r.json());
        const found = targets.find((t) => t.url.includes(urlSubstring));
        if (!found) throw new Error(`target with "${urlSubstring}" not found`);
        return found;
      },
      { ms: timeout, interval: 300 }
    );

    const wsUrl = target.webSocketDebuggerUrl;

    /**
     * Evaluate a JS expression in the target page via raw CDP.
     */
    async function evaluate(expression) {
      return rawCDPEval(wsUrl, expression);
    }

    /**
     * Minimal locator-like helper for counting and evaluating elements.
     */
    function locator(selector) {
      return {
        /**
         * Poll until the element count matches `expected`.
         */
        async toHaveCount(expected, { timeout: locTimeout = 15_000 } = {}) {
          const count = await pollUntil(
            async () => {
              const n = await rawCDPEval(
                wsUrl,
                `document.querySelectorAll(${JSON.stringify(selector)}).length`
              );
              if (n !== expected) {
                throw new Error(`expected ${expected} elements, got ${n}`);
              }
              return n;
            },
            { ms: locTimeout, interval: 300 }
          );
          return count;
        },
        /**
         * Evaluate a function over all matching elements — matches Playwright's
         * locator.evaluateAll(fn) semantics: fn receives the full array of
         * elements as its first argument (NOT mapped over each element).
         */
        async evaluateAll(fn) {
          const fnSrc = fn.toString();
          // Call fn with the full elements array, matching Playwright semantics.
          const expr = `(${fnSrc})(Array.from(document.querySelectorAll(${JSON.stringify(selector)})))`;
          return rawCDPEval(wsUrl, expr);
        },
      };
    }

    return { url: target.url, wsUrl, evaluate, locator };
  }

  /**
   * Evaluate a JS expression on a raw CDP WebSocket URL.
   * Use when you have a wsUrl from waitForPage() or /json.
   */
  async function cdpEval(wsUrl, expression) {
    return rawCDPEval(wsUrl, expression);
  }

  function stderr() {
    return stderrBuf;
  }

  /**
   * Resize the mote window via xdotool (if available).
   * xdotool is NOT installed on this box (checked during fixture build).
   * The mechanic is documented here for when it becomes available.
   */
  function setWindowSize(w, h) {
    // xdotool is not available on this machine — documented blocker.
    // When installed: xdotool search --name "mote" windowsize <w> <h>
    throw new Error(
      `setWindowSize(${w}, ${h}): xdotool is not installed on this machine. ` +
        "Install xdotool to enable headless window resize. " +
        "See the resize.spec.mjs blocker comment."
    );
  }

  /**
   * Capture a screenshot of the chrome page via Playwright.
   */
  async function windowScreenshot() {
    const cp = chromePage();
    return cp.screenshot({ fullPage: false });
  }

  // ── 6. close() ───────────────────────────────────────────────────────────
  async function close() {
    // Disconnect Playwright from the CDP endpoint first (graceful).
    try { await browser.close(); } catch (_) { /* ignore */ }

    // Kill mote: SIGTERM first, then SIGKILL if children linger.
    if (!moteExited) {
      try { process.kill(mote.pid, "SIGTERM"); } catch (_) { /* already gone */ }
      await new Promise((r) => setTimeout(r, 1_000));
      if (!moteExited) {
        try { process.kill(mote.pid, "SIGKILL"); } catch (_) { /* already gone */ }
      }
    }
    // pkill -9 any stray CEF child processes.
    try { execSync("pkill -9 -x mote", { stdio: "ignore" }); } catch (_) { /* none */ }

    // Kill Xvfb.
    try { xvfb.kill("SIGTERM"); } catch (_) { /* already gone */ }
    try { execSync(`pkill -9 -f 'Xvfb ${display}'`, { stdio: "ignore" }); } catch (_) { /* none */ }

    // Clean up temp dir (best-effort; CEF locks some files on exit).
    try { execSync(`rm -rf ${workDir}`, { stdio: "ignore" }); } catch (_) { /* ignore */ }
  }

  return {
    cdp: browser,
    chromePage,
    contentPages,
    waitForPage,
    cdpEval,
    stderr,
    setWindowSize,
    windowScreenshot,
    close,
  };
}

// ── Playwright fixture wrapper ────────────────────────────────────────────────
//
// Specs import `test` from this file instead of "@playwright/test" so they get
// a `mote` fixture that is launched before each test and torn down after.
//
// Usage in specs:
//   import { test, expect } from "./fixtures.mjs";
//   test("my test", async ({ mote }) => { ... });

export const test = base.extend({
  mote: [
    async ({}, use) => {
      const m = await launchMote();
      try {
        await use(m);
      } finally {
        await m.close();
      }
    },
    { scope: "test" },
  ],
});

export { expect } from "@playwright/test";
