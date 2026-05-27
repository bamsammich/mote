/*
 * host.js — the privileged chrome bootstrap (ADR-0005).
 *
 * Wraps CEF's `window.cefQuery` into a structured `window.mote.invoke(op,
 * params)` promise API. The transport carries DATA, never markup: a request is
 * `{op, params}` and a response is parsed JSON the bootstrap constructs DOM
 * from — there is no eval path. The binding only exists when the document's
 * origin is the privileged `mote://chrome` (the renderer origin gate installs
 * `window.cefQuery` for nothing else), so untrusted web content can never reach
 * the ops below.
 *
 * Phase-2 interactive slice: this wires the omnibox `navigate` op and reports
 * focus changes. Richer tab-state push / nav-state rendering are stubbed (the
 * `applyOp` switch is the seam Wave C fills in).
 */
(function () {
  "use strict";

  function installInvoke() {
    if (typeof window.cefQuery !== "function") {
      // No privileged transport — either not the chrome origin or the router
      // is not attached. Fail closed: no window.mote.
      return false;
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
    return true;
  }

  // Parse the user's omnibox text into a navigable URL. A bare host or a term
  // is given a scheme; the canonical normalization + provider routing is a
  // Wave-C concern (the shell's `navigate` op accepts whatever we send).
  function toUrl(text) {
    var t = (text || "").trim();
    if (t === "") return null;
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(t) || /^(data|mote|about):/i.test(t)) {
      return t;
    }
    // Looks like a domain (has a dot, no spaces) → https. Otherwise leave as-is
    // and let the shell decide (Wave C adds a search provider).
    if (/^[^\s]+\.[^\s]+$/.test(t)) {
      return "https://" + t;
    }
    return "https://" + t;
  }

  function wireOmnibox() {
    var form = document.querySelector(".omnibar-row");
    var input = document.getElementById("omnibox-input");
    var omni = document.querySelector(".omni");
    if (!form || !input) return;

    form.addEventListener("submit", function (ev) {
      ev.preventDefault();
      var url = toUrl(input.value);
      if (!url) return;
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("navigate", { url: url }).catch(function () {});
      }
    });

    // Report focus ownership so the shell can route keyboard input to the
    // chrome (omnibox) vs the focused page (plan §1.3).
    input.addEventListener("focus", function () {
      if (omni) omni.classList.add("is-focused");
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("focus_changed", { owner: "chrome" }).catch(function () {});
      }
    });
    input.addEventListener("blur", function () {
      if (omni) omni.classList.remove("is-focused");
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("focus_changed", { owner: "page" }).catch(function () {});
      }
    });
  }

  // Rebuild the tab strip from the runtime tab list (Rust → chrome push). Each
  // tab is built with structured DOM construction + text nodes — NEVER innerHTML
  // of page-derived strings (bridge.rs §"Discipline the caller must uphold":
  // titles/URLs are injection vectors into the privileged document). Clicking a
  // tab selects it; the ✕ closes it — both ride the `select_tab`/`close_tab` ops.
  function renderTabs(tabs) {
    var strip = document.getElementById("tabstrip");
    if (!strip || !Array.isArray(tabs)) return;
    // Clear existing rows (textContent="" drops children without parsing markup).
    strip.textContent = "";
    tabs.forEach(function (tab) {
      var row = document.createElement("div");
      row.className = tab.active ? "tab is-active" : "tab";
      row.setAttribute("role", "tab");
      row.setAttribute("aria-selected", tab.active ? "true" : "false");

      var favicon = document.createElement("span");
      favicon.className = "favicon";
      row.appendChild(favicon);

      var title = document.createElement("span");
      title.className = "title";
      // Text node: the title/URL is page-derived and must not be parsed as HTML.
      title.textContent = tab.title || tab.url || "new tab";
      row.appendChild(title);

      var close = document.createElement("button");
      close.className = "tab-close";
      close.setAttribute("aria-label", "close tab");
      close.textContent = "✕";
      close.addEventListener("click", function (ev) {
        ev.stopPropagation();
        if (window.mote && window.mote.invoke) {
          window.mote.invoke("close_tab", { id: tab.id }).catch(function () {});
        }
      });
      row.appendChild(close);

      row.addEventListener("click", function () {
        if (window.mote && window.mote.invoke) {
          window.mote.invoke("select_tab", { id: tab.id }).catch(function () {});
        }
      });
      strip.appendChild(row);
    });
  }

  function wireNewTab() {
    // The omnibox "+" affordance lives in the tabs panel header meta region; if a
    // new-tab control is present, wire it. Falls back to nothing if absent.
    var btn = document.querySelector("[data-action='new-tab']");
    if (!btn) return;
    btn.addEventListener("click", function () {
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("new_tab", {}).catch(function () {});
      }
    });
  }

  // The structured-op application seam (Rust → chrome): the shell pushes live
  // tab-list and current-URL state here so the tab strip + omnibox reflect
  // reality. A payload is parsed DATA, never markup.
  window.mote = window.mote || {};
  window.mote.applyOp = function (op, payload) {
    switch (op) {
      case "set_url":
        var input = document.getElementById("omnibox-input");
        if (input && payload && typeof payload.url === "string") {
          input.value = payload.url;
        }
        break;
      case "set_tabs":
        if (payload && Array.isArray(payload.tabs)) {
          renderTabs(payload.tabs);
          var meta = document.querySelector(".sidepanel-meta");
          if (meta) {
            meta.textContent = payload.tabs.length + " open";
          }
        }
        break;
      default:
        // Unknown ops are ignored (forward-compatible).
        break;
    }
  };

  function boot() {
    installInvoke();
    wireOmnibox();
    wireNewTab();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
