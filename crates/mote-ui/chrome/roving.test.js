/*
 * roving.test.js — regression coverage for the pure nav-math in roving.js
 * (CL-KBNAV phase 1).
 *
 * Plain node script, no framework: it loads roving.js in a minimal sandbox
 * (there is no real DOM here — we exercise ONLY the side-effect-free math) and
 * asserts the pinned omnibox wrap contract. Exits non-zero on the first failure.
 *
 * Run:  node crates/mote-ui/chrome/roving.test.js
 * Gate: wired as a lefthook pre-commit command (see lefthook.yml).
 */
"use strict";

const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

// Load roving.js into a sandbox that exposes a `window` global (the file hangs
// its API off window.mote.roving, matching host.js's IIFE/global style). No DOM
// is provided — the nav-math we test must not touch the document.
const src = fs.readFileSync(path.join(__dirname, "roving.js"), "utf8");
const sandbox = { window: {} };
vm.createContext(sandbox);
vm.runInContext(src, sandbox, { filename: "roving.js" });

const roving = sandbox.window.mote && sandbox.window.mote.roving;
assert.ok(roving, "window.mote.roving must be defined after loading roving.js");
assert.strictEqual(typeof roving.step, "function", "roving.step must be a function");
assert.strictEqual(typeof roving.keyToDir, "function", "roving.keyToDir must be a function");

const { step, keyToDir } = roving;

let passed = 0;
function check(label, actual, expected) {
  assert.strictEqual(actual, expected, label + " — expected " + expected + ", got " + actual);
  passed++;
}

// ---- step(): DOWN wrap (dir = +1), default opts.wrap === true ----------------
// Contract (host.js:518): cur < count-1 ? cur+1 : 0  (so -1→0, last→0).
check("down from last wraps to 0", step(4, 5, 1), 0);
check("down from middle advances", step(2, 5, 1), 3);
check("down from -1 selects first", step(-1, 5, 1), 0);

// ---- step(): UP wrap (dir = -1) ---------------------------------------------
// Contract (host.js:526): cur > 0 ? cur-1 : count-1  (so 0→last, -1→last).
check("up from 0 wraps to last", step(0, 5, -1), 4);
check("up from middle retreats", step(3, 5, -1), 2);
check("up from -1 selects last", step(-1, 5, -1), 4);

// ---- step(): empty list (count === 0) → -1 both directions -------------------
check("down on empty list is -1", step(-1, 0, 1), -1);
check("up on empty list is -1", step(-1, 0, -1), -1);
check("down on empty list from stale idx is -1", step(3, 0, 1), -1);

// ---- step(): single-item list (count === 1) stays at 0 ----------------------
check("single-item down stays at 0", step(0, 1, 1), 0);
check("single-item up stays at 0", step(0, 1, -1), 0);
check("single-item down from -1 lands on 0", step(-1, 1, 1), 0);
check("single-item up from -1 lands on 0", step(-1, 1, -1), 0);

// ---- step(): first / last ----------------------------------------------------
check("first selects index 0", step(3, 5, "first"), 0);
check("last selects last index", step(0, 5, "last"), 4);
check("first on empty list is -1", step(-1, 0, "first"), -1);
check("last on empty list is -1", step(-1, 0, "last"), -1);

// ---- step(): wrap disabled clamps instead of wrapping ------------------------
check("no-wrap down from last clamps", step(4, 5, 1, { wrap: false }), 4);
check("no-wrap up from 0 clamps", step(0, 5, -1, { wrap: false }), 0);

// ---- keyToDir(): arrows always map ------------------------------------------
check("ArrowDown → +1", keyToDir("ArrowDown"), 1);
check("ArrowUp → -1", keyToDir("ArrowUp"), -1);
check("Home → first", keyToDir("Home"), "first");
check("End → last", keyToDir("End"), "last");

// ---- keyToDir(): j/k only behind opts.jk ------------------------------------
check("j maps to +1 when jk enabled", keyToDir("j", { jk: true }), 1);
check("k maps to -1 when jk enabled", keyToDir("k", { jk: true }), -1);
check("j is null when jk disabled", keyToDir("j"), null);
check("k is null when jk disabled", keyToDir("k"), null);
check("j is null when jk explicitly false", keyToDir("j", { jk: false }), null);

// ---- keyToDir(): unknown keys → null ----------------------------------------
check("unknown key → null", keyToDir("Enter"), null);
check("Tab → null", keyToDir("Tab"), null);
check("empty key → null", keyToDir(""), null);

console.log("roving.test.js: " + passed + " assertions passed");
