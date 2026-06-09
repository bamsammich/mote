/*
 * panels.test.js — node tests for pure-logic helpers in panels.js (H4).
 *
 * Tests only the DOM-free, side-effect-free functions exported via
 * window.mote. No DOM is provided — DOM-rendering functions are not tested
 * here (they are verified via live inspection of the running app).
 *
 * Run:  node crates/mote-ui/chrome/panels.test.js
 * Gate: wired as a lefthook pre-commit command alongside roving.test.js.
 */
"use strict";

const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

// Load panels.js into a minimal sandbox.  panels.js is an IIFE that hangs its
// API off window.mote.*; it also touches document.* for DOM work — none of
// those calls execute in the paths we test here (the helpers under test don't
// reference the document).
const src = fs.readFileSync(path.join(__dirname, "panels.js"), "utf8");

// Minimal browser-environment stubs:
//   window  — needed by the IIFE to install window.mote
//   document.createElement / document.createTextNode / document.addEventListener
//             — referenced at module level for the applyOp wiring; providing
//               stubs prevents ReferenceError without executing any real DOM
const sandbox = {
  window: {},
  document: {
    createElement: function () {
      return {
        className: "",
        setAttribute: function () {},
        removeAttribute: function () {},
        appendChild: function () {},
        addEventListener: function () {},
        textContent: "",
        hidden: false,
      };
    },
    createTextNode: function () { return {}; },
    getElementById: function () { return null; },
    addEventListener: function () {},
  },
  Node: function () {},
};
vm.createContext(sandbox);
vm.runInContext(src, sandbox, { filename: "panels.js" });

const mote = sandbox.window.mote;
assert.ok(mote, "window.mote must be defined after loading panels.js");

// ---- opDecisionClass -------------------------------------------------------

const { opDecisionClass } = mote;
assert.ok(
  typeof opDecisionClass === "function",
  "window.mote.opDecisionClass must be a function"
);

let passed = 0;
function check(label, actual, expected) {
  assert.strictEqual(actual, expected, label + " — expected " + JSON.stringify(expected) + ", got " + JSON.stringify(actual));
  passed++;
}

// Known decision strings (exact case as produced by Rust serialization).
check("deny → 'deny' class",      opDecisionClass("deny"),  "deny");
check("allow → 'allow' class",    opDecisionClass("allow"), "allow");
check("defer → 'defer' class",    opDecisionClass("defer"), "defer");

// Case-insensitive (Rust uses lowercase; defensive against future changes).
check("Deny uppercase",  opDecisionClass("Deny"),  "deny");
check("Allow uppercase", opDecisionClass("Allow"), "allow");
check("Defer uppercase", opDecisionClass("Defer"), "defer");

// Unknown / empty → empty string (no CSS class added).
check("unknown string → ''",  opDecisionClass("unknown"), "");
check("empty string → ''",    opDecisionClass(""),         "");
check("null → ''",            opDecisionClass(null),       "");
check("undefined → ''",       opDecisionClass(undefined),  "");

// ---- applyOp presence ------------------------------------------------------
// panels.js wraps host.js's applyOp. The settings-page integrity ops (H14/H16)
// are NOT routed through applyOp — the shell pushes them straight to the
// settings page — so an unknown op must simply fall through without throwing.

assert.ok(
  typeof mote.applyOp === "function",
  "window.mote.applyOp must be a function after loading panels.js"
);
passed++;

assert.doesNotThrow(function () {
  mote.applyOp("some_unknown_op", {});
}, "applyOp(unknown op) must fall through without throwing");
passed++;

console.log("panels.test.js: " + passed + " assertions passed");
