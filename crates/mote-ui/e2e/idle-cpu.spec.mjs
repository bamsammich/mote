// idle-cpu.spec.mjs — Lane 3 E2E (ADR-0020 §3, bug #4).
//
// BUG #4 — Idle CPU: mote consumes unexpectedly high CPU when idle (no user
// input, no pending network, no animation). Suspected source: the wgpu
// compositor paint loop or a CEF renderer timer.
//
// Mechanic: sample /proc/<pid>/stat utime+stime twice with a known sleep
// interval; compute CPU% = Δticks / (Δwall_ms * clock_ticks_per_sec / 1000).
// Assert < a documented threshold (5% — aggressive; if the bug is real, idle
// CPU is measurably > 5% on a debug build with GPU painting spinning).
//
// This spec is QUARANTINED via test.fixme. The /proc sampling mechanic IS
// exercisable headlessly; the assertion is marked fixme because:
//   - A debug build under Xvfb (software rendering via llvmpipe / swiftshader)
//     may idle higher than 5% due to unrelated rendering overhead.
//   - The threshold must be calibrated against a real measurement before
//     this test is un-quarantined.
//   - The actual bug may only be detectable in a release build or on real GPU.
//
// When un-quarantining: run the test without fixme, measure the actual idle
// CPU%, set the threshold accordingly, and document the measurement context.

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
