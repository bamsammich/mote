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

  // ---- Completion popup helpers ------------------------------------------

  // Return the completion dropdown element and the current selected index (-1
  // when nothing is selected). These are the only two pieces of mutable state
  // the completion handlers need.
  function getCompletions() {
    return document.getElementById("omnibox-completions");
  }

  function selectedIndex(dropdown) {
    var rows = dropdown ? dropdown.querySelectorAll(".omni-completion-row") : [];
    for (var i = 0; i < rows.length; i++) {
      if (rows[i].classList.contains("is-sel")) return i;
    }
    return -1;
  }

  // Clear the .is-sel marker and ARIA attributes from all rows, then
  // optionally select the row at `idx`.
  function setSelection(dropdown, input, idx) {
    var rows = dropdown ? dropdown.querySelectorAll(".omni-completion-row") : [];
    for (var i = 0; i < rows.length; i++) {
      rows[i].classList.remove("is-sel");
      rows[i].setAttribute("aria-selected", "false");
    }
    if (idx >= 0 && idx < rows.length) {
      rows[idx].classList.add("is-sel");
      rows[idx].setAttribute("aria-selected", "true");
      if (input) input.setAttribute("aria-activedescendant", rows[idx].id);
      rows[idx].scrollIntoView({ block: "nearest" });
    } else {
      if (input) input.removeAttribute("aria-activedescendant");
    }
  }

  function closeCompletions(dropdown, input, omni) {
    if (!dropdown) return;
    dropdown.classList.remove("is-open");
    if (input) input.setAttribute("aria-expanded", "false");
    if (omni) omni.classList.remove("has-completions");
    setSelection(dropdown, input, -1);
  }

  // Build a single span node with class and text content (no innerHTML).
  function makeSpan(cls, text) {
    var s = document.createElement("span");
    s.className = cls;
    s.textContent = text;
    return s;
  }

  // Build the .url cell with matched-substring highlighting.  The matching
  // text is wrapped in <b> elements constructed via createElement + textContent
  // — never innerHTML of payload content (ADR-0005).
  function buildUrlCell(url, matchText) {
    var cell = document.createElement("span");
    cell.className = "url";

    if (!matchText || matchText.length === 0) {
      cell.textContent = url;
      return cell;
    }

    // Case-insensitive search for the first occurrence of matchText in url.
    var lowerUrl = url.toLowerCase();
    var lowerMatch = matchText.toLowerCase();
    var pos = lowerUrl.indexOf(lowerMatch);

    if (pos === -1) {
      cell.textContent = url;
      return cell;
    }

    // Build three text segments: before, match, after.
    if (pos > 0) {
      cell.appendChild(document.createTextNode(url.slice(0, pos)));
    }
    var highlight = document.createElement("b");
    highlight.textContent = url.slice(pos, pos + matchText.length);
    cell.appendChild(highlight);
    var after = url.slice(pos + matchText.length);
    if (after.length > 0) {
      cell.appendChild(document.createTextNode(after));
    }
    return cell;
  }

  // Build the [source] lockup: both brackets and name use .br/.name but all
  // dim (--fg-2) per the design — it's metadata, not a mode indicator.
  function buildSourceCell(sourceName) {
    var cell = document.createElement("span");
    cell.className = "source";
    cell.appendChild(makeSpan("br", "["));
    cell.appendChild(makeSpan("name", sourceName));
    cell.appendChild(makeSpan("br", "]"));
    return cell;
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

    // Blur: delay close so a row click can register before the dropdown hides
    // (standard combobox pattern — 150ms is enough for a click to fire).
    input.addEventListener("blur", function () {
      if (omni) omni.classList.remove("is-focused");
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("focus_changed", { owner: "page" }).catch(function () {});
      }
      setTimeout(function () {
        closeCompletions(getCompletions(), input, omni);
      }, 150);
    });

    // Input: push urlbar_query to the shell on every keystroke.  Empty value
    // closes the dropdown locally without a round-trip.
    input.addEventListener("input", function () {
      var dropdown = getCompletions();
      if (!input.value) {
        closeCompletions(dropdown, input, omni);
        return;
      }
      if (window.mote && window.mote.invoke) {
        window.mote
          .invoke("urlbar_query", { text: input.value })
          .catch(function () {});
      }
    });

    // Keyboard navigation inside the completion dropdown.
    input.addEventListener("keydown", function (ev) {
      var dropdown = getCompletions();
      var isOpen = dropdown && dropdown.classList.contains("is-open");
      var rows = dropdown
        ? dropdown.querySelectorAll(".omni-completion-row")
        : [];
      var count = rows.length;

      if (ev.key === "ArrowDown") {
        if (!isOpen || count === 0) return;
        ev.preventDefault();
        var curDown = selectedIndex(dropdown);
        setSelection(dropdown, input, curDown < count - 1 ? curDown + 1 : 0);
        return;
      }

      if (ev.key === "ArrowUp") {
        if (!isOpen || count === 0) return;
        ev.preventDefault();
        var curUp = selectedIndex(dropdown);
        setSelection(dropdown, input, curUp > 0 ? curUp - 1 : count - 1);
        return;
      }

      if (ev.key === "Enter") {
        var sel = selectedIndex(dropdown);
        if (isOpen && sel >= 0 && rows[sel]) {
          // Navigate to the selected row's URL; close dropdown + blur.
          ev.preventDefault();
          var url = rows[sel].getAttribute("data-url");
          closeCompletions(dropdown, input, omni);
          input.blur();
          if (url && window.mote && window.mote.invoke) {
            window.mote.invoke("navigate", { url: url }).catch(function () {});
          }
        }
        // No selected row: fall through to form submit (no preventDefault).
        return;
      }

      if (ev.key === "Escape") {
        if (isOpen && selectedIndex(dropdown) >= 0) {
          // Clear selection only; leave dropdown open and input focused.
          setSelection(dropdown, input, -1);
          ev.preventDefault();
        }
        // No selection (or dropdown closed): fall through to existing Esc
        // behavior (omnibox blur, handled by the browser/form).
        return;
      }

      if (ev.key === "Tab") {
        // Clear selection and fall through (do not preventDefault).
        if (isOpen) setSelection(dropdown, input, -1);
      }
    });

    // Click on a completion row: navigate to that URL.
    var dropdown = getCompletions();
    if (dropdown) {
      dropdown.addEventListener("mousedown", function (ev) {
        // Use mousedown (fires before blur) so the click registers even though
        // the blur handler runs after with a 150ms delay.
        var row = ev.target.closest(".omni-completion-row");
        if (!row) return;
        ev.preventDefault(); // prevent the input from losing focus early
        var url = row.getAttribute("data-url");
        closeCompletions(getCompletions(), input, omni);
        if (url && window.mote && window.mote.invoke) {
          window.mote.invoke("navigate", { url: url }).catch(function () {});
        }
      });
    }
  }

  // ---- applyOp handler for urlbar_suggestions ----------------------------
  // Chained via prevApplyOp so the existing set_url/set_tabs ops are preserved.
  // This is installed AFTER wireOmnibox() is called inside boot().
  function wireCompletionsOp() {
    var input = document.getElementById("omnibox-input");
    var omni = document.querySelector(".omni");
    var prevApplyOp =
      typeof window.mote.applyOp === "function" ? window.mote.applyOp : null;

    window.mote.applyOp = function (op, payload) {
      if (op !== "urlbar_suggestions") {
        if (prevApplyOp) prevApplyOp(op, payload);
        return;
      }

      var dropdown = getCompletions();
      if (!dropdown) return;

      // Empty array → close.
      if (!Array.isArray(payload) || payload.length === 0) {
        closeCompletions(dropdown, input, omni);
        return;
      }

      // Clear previous rows (textContent="" drops children without markup).
      dropdown.textContent = "";

      var matchText = input ? input.value : "";

      payload.forEach(function (record, idx) {
        var row = document.createElement("div");
        row.className = "omni-completion-row";
        row.setAttribute("role", "option");
        row.setAttribute("aria-selected", "false");
        row.setAttribute("id", "omnibox-completion-row-" + idx);
        // Store the URL as a data attribute for keyboard/click handlers to read.
        // Only strings from the runtime payload are stored; no markup is parsed.
        if (record && typeof record.url === "string") {
          row.setAttribute("data-url", record.url);
        }

        // url cell — matched-substring highlighting via DOM, never innerHTML.
        row.appendChild(
          buildUrlCell(
            typeof record.url === "string" ? record.url : "",
            matchText
          )
        );

        // title cell.
        var titleCell = document.createElement("span");
        titleCell.className = "title";
        titleCell.textContent =
          typeof record.title === "string" ? record.title : "";
        row.appendChild(titleCell);

        // [source] lockup cell.
        row.appendChild(
          buildSourceCell(
            typeof record.source === "string" ? record.source : ""
          )
        );

        dropdown.appendChild(row);
      });

      // Open the dropdown.
      dropdown.classList.add("is-open");
      if (input) input.setAttribute("aria-expanded", "true");
      if (omni) omni.classList.add("has-completions");
    };
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
    // Chain the urlbar_suggestions applyOp handler after the base handler is
    // installed above.  wireCompletionsOp() captures prevApplyOp at call-time.
    wireCompletionsOp();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
