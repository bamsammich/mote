/*
 * roving.js — shared roving-focus / list-navigation helper (CL-KBNAV).
 *
 * One namespace, hung off `window.mote.roving`, matching host.js's IIFE/global
 * style. It has NO dependency on the cefQuery bridge — it only touches the DOM
 * nodes the consumer hands it. Two layers:
 *
 *   1. Pure, side-effect-free nav-math (`step`, `keyToDir`) — the unit-tested
 *      core. These never touch the document and are loadable in a bare node VM.
 *   2. A DOM `attach(opts)` factory returning a controller. Two modes:
 *      - "activedescendant": the combobox case (omnibox). The focused element
 *        stays the input; selection is a marker class + aria-selected on rows +
 *        aria-activedescendant on the input, with a real "no selection" (-1)
 *        state. Reproduces host.js's setSelection() DOM effects exactly.
 *      - "roving": real DOM focus moves to each item (tabindex roving). Built
 *        now because the design must support it; exercised in CL-KBNAV phase 2.
 *
 * Phase 1 consumes only the activedescendant mode (omnibox). The j/k bindings
 * and the roving mode are implemented but dormant until phase 2 wires them.
 */
(function () {
  "use strict";

  // ---- Pure nav-math ---------------------------------------------------------

  // Resolve the next index given the current index, item count, and a direction.
  //   dir:  +1 (down) | -1 (up) | "first" | "last"
  //   opts.wrap (default true): down past the end wraps to 0; up past the start
  //     wraps to count-1; from current === -1, down → 0 and up → count-1. This
  //     is exactly the omnibox contract (host.js:518 / :526). With wrap=false,
  //     movement clamps at the ends instead of wrapping.
  // count === 0 always yields -1 (nothing selectable). "first"/"last" ignore the
  // current index and the wrap flag.
  function step(current, count, dir, opts) {
    if (!(count > 0)) return -1;
    var wrap = !opts || opts.wrap !== false;

    if (dir === "first") return 0;
    if (dir === "last") return count - 1;

    if (dir === 1) {
      // Down. From a real position, advance; at/over the end, wrap to 0 (or
      // clamp). From -1 (no selection), land on the first row.
      if (current < 0) return 0;
      if (current < count - 1) return current + 1;
      return wrap ? 0 : count - 1;
    }

    if (dir === -1) {
      // Up. From a real position, retreat; at/under the start, wrap to last (or
      // clamp). From -1 (no selection), land on the last row.
      if (current < 0) return wrap ? count - 1 : 0;
      if (current > 0) return current - 1;
      return wrap ? count - 1 : 0;
    }

    return current;
  }

  // Map a KeyboardEvent.key to a step() direction, or null when the key is not a
  // navigation key. ArrowDown/ArrowUp and Home/End always map. "j"/"k" map only
  // when opts.jk is true (phase-2 vim-style navigation; dormant for the omnibox).
  function keyToDir(key, opts) {
    var jk = !!(opts && opts.jk);
    switch (key) {
      case "ArrowDown":
        return 1;
      case "ArrowUp":
        return -1;
      case "Home":
        return "first";
      case "End":
        return "last";
      case "j":
        return jk ? 1 : null;
      case "k":
        return jk ? -1 : null;
      default:
        return null;
    }
  }

  // ---- DOM attach factory ----------------------------------------------------

  // attach(opts) → controller. Shared config (both modes):
  //   container   : the element scoping the items (only used as a default root).
  //   getItems()  : returns the CURRENT live items (NodeList or array). Called
  //                 fresh on every key so re-rendered lists stay correct.
  //   mode        : "activedescendant" (default) | "roving".
  //   jk, wrap    : passed through to keyToDir/step (wrap default true).
  //   onActivate(item, index) : invoked when an item is activated (Enter/click
  //                 wiring is the consumer's; phase 2's roving mode calls it).
  //   onEscape(currentIndex)  : invoked on Escape so the consumer owns the
  //                 two-stage-vs-single-stage decision. Return value is ignored;
  //                 the consumer mutates via clear()/setIndex() as it sees fit.
  //
  // activedescendant-mode config:
  //   focusEl     : the input that keeps DOM focus (aria-activedescendant target).
  //   markerClass : selection marker class on the active row (default "is-sel").
  //   selectedAttr: aria attribute toggled per row (default "aria-selected").
  //
  // Controller API:
  //   getIndex()      : current selected index (-1 when none).
  //   setIndex(i)     : select row i (-1 clears). Applies the mode's DOM effects.
  //   clear()         : setIndex(-1).
  //   handleKey(ev)   : if ev.key is a nav key (per keyToDir), move selection and
  //                     return true (caller should preventDefault). Otherwise
  //                     return false and leave ev untouched, so the caller can
  //                     run its own Enter/Escape/Tab/copy logic. handleKey never
  //                     calls preventDefault itself — the caller owns the event.
  function attach(opts) {
    opts = opts || {};
    var mode = opts.mode === "roving" ? "roving" : "activedescendant";
    var markerClass = opts.markerClass || "is-sel";
    var selectedAttr = opts.selectedAttr || "aria-selected";
    var wrap = opts.wrap !== false;
    var jk = !!opts.jk;

    function items() {
      var got = typeof opts.getItems === "function" ? opts.getItems() : null;
      if (!got) return [];
      // Normalise NodeList → array so .length / indexing are stable.
      return Array.prototype.slice.call(got);
    }

    function currentIndex() {
      var list = items();
      for (var i = 0; i < list.length; i++) {
        if (list[i].classList && list[i].classList.contains(markerClass)) {
          return i;
        }
      }
      return -1;
    }

    // Apply selection idx across the live items. Mirrors host.js setSelection():
    // clear marker + selectedAttr=false on all rows; on a valid idx set marker +
    // selectedAttr=true and (activedescendant) point focusEl's
    // aria-activedescendant at the row id + scrollIntoView({block:"nearest"});
    // on -1 remove aria-activedescendant. In roving mode, real focus moves and
    // tabindex rolls (the active item gets tabindex 0, the rest -1).
    function applySelection(idx) {
      var list = items();
      for (var i = 0; i < list.length; i++) {
        var row = list[i];
        row.classList.remove(markerClass);
        row.setAttribute(selectedAttr, "false");
        if (mode === "roving") row.setAttribute("tabindex", "-1");
      }
      var valid = idx >= 0 && idx < list.length;
      if (valid) {
        var sel = list[idx];
        sel.classList.add(markerClass);
        sel.setAttribute(selectedAttr, "true");
        if (mode === "activedescendant") {
          if (opts.focusEl) opts.focusEl.setAttribute("aria-activedescendant", sel.id);
        } else {
          sel.setAttribute("tabindex", "0");
          if (typeof sel.focus === "function") sel.focus();
        }
        if (typeof sel.scrollIntoView === "function") {
          sel.scrollIntoView({ block: "nearest" });
        }
      } else if (mode === "activedescendant") {
        if (opts.focusEl) opts.focusEl.removeAttribute("aria-activedescendant");
      }
    }

    function setIndex(i) {
      applySelection(i);
    }

    function clear() {
      applySelection(-1);
    }

    function handleKey(ev) {
      var dir = keyToDir(ev.key, { jk: jk });
      if (dir === null) return false;
      var list = items();
      var count = list.length;
      var next = step(currentIndex(), count, dir, { wrap: wrap });
      setIndex(next);
      return true;
    }

    return {
      mode: mode,
      getIndex: currentIndex,
      setIndex: setIndex,
      clear: clear,
      handleKey: handleKey,
      // Exposed so phase-2 roving consumers can run activate/escape through the
      // same controller without re-reading the item list themselves.
      activate: function (index) {
        var list = items();
        var i = typeof index === "number" ? index : currentIndex();
        if (i >= 0 && i < list.length && typeof opts.onActivate === "function") {
          opts.onActivate(list[i], i);
        }
      },
      escape: function () {
        if (typeof opts.onEscape === "function") opts.onEscape(currentIndex());
      },
    };
  }

  window.mote = window.mote || {};
  window.mote.roving = {
    step: step,
    keyToDir: keyToDir,
    attach: attach,
  };
})();
