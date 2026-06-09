/*
 * mote-bridge.js — canonical bridge bootstrap (ADR-0005).
 *
 * Shared by chrome.html and all privileged mote://chrome pages (settings,
 * overlays). Wraps CEF's origin-gated `window.cefQuery` into the structured
 * `window.mote.invoke(op, params)` promise API.
 *
 * Transport discipline:
 *   - Data only: request is `{op, params}` JSON; response is JSON.parse'd.
 *   - Fail-closed: if `window.cefQuery` is not a function (non-privileged
 *     origin or the router is not attached), this IIFE does nothing — no
 *     `window.mote` is installed, and callers that guard on
 *     `window.mote?.invoke` degrade gracefully.
 *   - No eval path: the bootstrap never eval's a string or assigns innerHTML.
 *
 * Load order: this file MUST be loaded BEFORE host.js on chrome.html, and
 * BEFORE settings.js on every settings section page. host.js delegates to
 * `window.mote.invoke` (installed here) instead of calling cefQuery directly.
 */
(function () {
  "use strict";

  if (typeof window.cefQuery !== "function") {
    // No privileged transport — either not the mote://chrome origin or the
    // router is not attached yet. Fail closed: do not install window.mote.
    return;
  }

  window.mote = window.mote || {};
  window.mote.invoke = function (op, params) {
    return new Promise(function (resolve, reject) {
      window.cefQuery({
        request: JSON.stringify({ op: op, params: params || {} }),
        onSuccess: function (response) {
          try {
            resolve(JSON.parse(response));
          } catch (e) {
            resolve(response);
          }
        },
        onFailure: function (code, message) {
          reject({ code: code, message: message });
        },
      });
    });
  };
})();
