// idle-cpu.spec.mjs — Lane 3 E2E (ADR-0020 §3, bug #4).
//
// BUG #4 — Idle CPU: mote consumed ~9.5% CPU (and drove the GPU) when idle —
// the event loop rendered every ~4ms tick (ControlFlow::Poll) with no damage or
// visibility gate, so an unfocused/occluded window still composited at ~250Hz
// while CEF's 60Hz OSR paint loop kept feeding texture uploads.
//
// FIX (landed): two render gates in `crates/mote-shell/src/lib.rs`, keeping
// ControlFlow::Poll so CEF's external message pump stays serviced:
//   1. Damage/dirty gate — `request_redraw` only when the composited output
//      changed (`should_request_redraw(dirty, hidden)`); a clean, visible, idle
//      window does no render.
//   2. Visibility gate — on `Occluded(true)` call `Page::was_hidden(true)` on
//      every live page (CEF stops OSR painting) and skip render while hidden.
//
// PRIMARY REGRESSION GUARD is the pure-function unit test
// (`should_request_redraw`, mote-shell `tests::render_gate_*`): hidden ⇒ no
// redraw; dirty+visible ⇒ redraw; clean+visible ⇒ no redraw; dirty clears after
// a redraw. The live CPU drop is not reliably measurable in THIS spec (see
// below), so the unit test — not this E2E — is the gate.
//
// This spec stays QUARANTINED via test.fixme. The /proc sampling mechanic IS
// exercisable headlessly, and the dirty gate's effect is real and verified
// out-of-band (with a render-call counter on a static page: 0 renders over 5s
// idle vs the old ~250/s; whole-tree idle CPU measured ~1.4% on Xvfb/llvmpipe).
// It is still fixme because:
//   - Bare Xvfb has no compositor, so the OCCLUSION path (the user's exact
//     repro: window on another workspace → `Occluded(true)`) CANNOT be driven
//     headlessly — `WindowEvent::Occluded` never fires. That path needs a
//     real-display check.
//   - The chrome carries an ambient animation (the AI status dot) that can
//     legitimately keep the surface dirty, so a /proc threshold here is noisy
//     and must be calibrated against a real measurement in the target CI env.
//   - The debug build under software rendering (llvmpipe/swiftshader) idles
//     differently from a release build on a real GPU.
//
// When un-quarantining: run without fixme, measure the actual idle CPU% in the
// CI env on a genuinely static page (or assert a render-call count stays flat),
// set the threshold accordingly, and document the measurement context.

import { test, expect } from "./fixtures.mjs";
import { readFileSync } from "node:fs";

const SAMPLE_INTERVAL_MS = 3_000;
// Threshold: 5% idle CPU. Calibrate before un-quarantining.
const IDLE_CPU_MAX_PERCENT = 5;

/** Read utime+stime (ticks) for a pid from /proc/<pid>/stat. */
function readProcStatTicks(pid) {
  const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
  // Field 14 (utime) and 15 (stime) are 0-indexed at positions 13 and 14.
  // Format: pid (comm) state ppid ... utime stime ...
  // Split by whitespace after stripping the comm field (may contain spaces).
  const stripped = stat.replace(/\(.*?\)/, "()");
  const fields = stripped.split(/\s+/);
  const utime = parseInt(fields[13], 10);
  const stime = parseInt(fields[14], 10);
  return utime + stime;
}

function clockTicksPerSec() {
  // On Linux, typically 100. Read from sysconf(_SC_CLK_TCK) equivalent.
  // /proc/self/stat uses the same clock; we assume 100 Hz (standard).
  return 100;
}

test.fixme(
  "mote idle CPU is below threshold when no user input is pending (bug #4)",
  async ({ mote }) => {
    // Get the mote process PID via /json/version (the browser object doesn't
    // expose it directly; use the CDP client's endpoint).
    // We know the pid from the fixture's spawn — but the fixture doesn't
    // expose it. Sample /proc looking for the mote process.
    const { execSync } = await import("node:child_process");
    const motePid = parseInt(
      execSync("pgrep -x mote || pgrep -f 'target/debug/mote'", { encoding: "utf8" })
        .trim()
        .split("\n")[0],
      10
    );

    if (isNaN(motePid)) {
      throw new Error("Could not find mote PID for /proc sampling");
    }

    // Let mote settle into idle state.
    await new Promise((r) => setTimeout(r, 1_000));

    // Sample 1.
    const t0 = Date.now();
    const ticks0 = readProcStatTicks(motePid);

    // Wait for the sample interval.
    await new Promise((r) => setTimeout(r, SAMPLE_INTERVAL_MS));

    // Sample 2.
    const t1 = Date.now();
    const ticks1 = readProcStatTicks(motePid);

    const deltaWallMs = t1 - t0;
    const deltaTicks = ticks1 - ticks0;
    const cpuPercent =
      (deltaTicks / clockTicksPerSec()) / (deltaWallMs / 1000) * 100;

    console.log(
      `[idle-cpu] pid=${motePid} Δticks=${deltaTicks} Δwall=${deltaWallMs}ms ` +
        `cpu=${cpuPercent.toFixed(2)}%`
    );

    // The assertion (QUARANTINED — calibrate threshold before un-fixing):
    expect(cpuPercent).toBeLessThan(IDLE_CPU_MAX_PERCENT);
  }
);
