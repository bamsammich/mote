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
      // Blur after committing — keyboard-first expectation: Enter returns focus
      // to the page so the next ⌘K (or click) re-opens the bar with select-all.
      input.blur();
    });

    // Report focus ownership so the shell can route keyboard input to the
    // chrome (omnibox) vs the focused page (plan §1.3).
    input.addEventListener("focus", function () {
      if (omni) omni.classList.add("is-focused");
      // Select-all on focus — standard browser address-bar behavior so a click
      // followed by typing replaces the URL rather than inserting into it.
      input.select();
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
          // First Esc with a selection: clear selection only; stays open + focused.
          setSelection(dropdown, input, -1);
          ev.preventDefault();
          return;
        }
        // No selection (or dropdown closed): blur the input per the omnibox
        // spec ("Esc blurs the omnibox without committing").  Text inputs don't
        // blur on Esc natively in browsers, so it's explicit here.  Two-stage
        // Esc: first clears selection, second blurs.
        closeCompletions(dropdown, input, omni);
        input.blur();
        ev.preventDefault();
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

  // True if a tab URL is the newtab page (ADR-0015). Used to apply the [·]
  // favicon glyph and for any other newtab-specific rendering.
  function isNewtabUrl(url) {
    return typeof url === "string" &&
      url.toLowerCase().indexOf("mote://chrome/newtab") === 0;
  }

  // Rebuild the tab strip from the runtime tab list (Rust → chrome push). Each
  // tab is built with structured DOM construction + text nodes — NEVER innerHTML
  // of page-derived strings (bridge.rs §"Discipline the caller must uphold":
  // titles/URLs are injection vectors into the privileged document). Clicking a
  // tab selects it; the .tab-close closes it — both ride `select_tab`/`close_tab`.
  //
  // P1 changes:
  //   - .favicon-placeholder dot-grid (not a checkbox-looking surface-3 square)
  //   - .tab-close renders via lucide sprite <use>, is hover-only via CSS
  //   - Active tab: surface-2 bg lift + 2px left-stripe (handled by CSS)
  //
  // P3 changes:
  //   - Newtab tabs get .favicon.is-newtab with the [·] glyph (tabs.css)
  //   - Middle-click (mousedown button=1) → close_tab op
  //   - Tooltip carries both title (primary) and URL (secondary) on two lines
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

      // P3: tooltip carries title (primary) + URL (secondary) on two lines.
      // The tooltip primitive (wireTooltip) reads data-tooltip for the caption
      // and data-tooltip-secondary for a second line below it.
      var displayTitle = tab.title || tab.url || "new tab";
      row.setAttribute("data-tooltip", displayTitle);
      if (tab.url && tab.url !== displayTitle) {
        row.setAttribute("data-tooltip-secondary", tab.url);
      }

      // P1/P3: favicon placeholder. Newtab tabs get the [·] glyph (is-newtab
      // CSS class); all others get the dot-grid placeholder.
      var favicon = document.createElement("span");
      if (isNewtabUrl(tab.url)) {
        favicon.className = "favicon favicon-placeholder is-newtab";
        favicon.textContent = "[·]"; // text node — not innerHTML
      } else {
        favicon.className = "favicon favicon-placeholder";
      }
      favicon.setAttribute("aria-hidden", "true");
      row.appendChild(favicon);

      var title = document.createElement("span");
      title.className = "title";
      // Text node: the title/URL is page-derived and must not be parsed as HTML.
      title.textContent = displayTitle;
      row.appendChild(title);

      // P1: close button uses the lucide sprite (not a unicode ✕ glyph).
      // The button is hidden by default; CSS .tab:hover .tab-close reveals it.
      var closeBtn = document.createElement("button");
      closeBtn.className = "tab-close";
      closeBtn.setAttribute("aria-label", "close tab");
      closeBtn.setAttribute("tabindex", "-1");
      // Build the <svg><use> without innerHTML.
      var svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      svg.setAttribute("class", "icon");
      svg.setAttribute("aria-hidden", "true");
      var use = document.createElementNS("http://www.w3.org/2000/svg", "use");
      use.setAttributeNS(
        "http://www.w3.org/1999/xlink",
        "xlink:href",
        "assets/lucide-sprite.svg#icon-x"
      );
      // Also set the non-namespaced href for modern browsers.
      use.setAttribute("href", "assets/lucide-sprite.svg#icon-x");
      svg.appendChild(use);
      closeBtn.appendChild(svg);
      closeBtn.addEventListener("click", function (ev) {
        ev.stopPropagation();
        if (window.mote && window.mote.invoke) {
          window.mote.invoke("close_tab", { id: tab.id }).catch(function () {});
        }
      });
      row.appendChild(closeBtn);

      row.addEventListener("click", function () {
        if (window.mote && window.mote.invoke) {
          window.mote.invoke("select_tab", { id: tab.id }).catch(function () {});
        }
      });

      // P3: middle-click closes the tab.  Use mousedown rather than click
      // because `click` does not fire for button=1 (middle) in all browsers.
      // stopPropagation prevents the select_tab click handler from also firing.
      row.addEventListener("mousedown", function (ev) {
        if (ev.button !== 1) return; // only middle button
        ev.preventDefault();         // prevent autoscroll on some platforms
        ev.stopPropagation();
        if (window.mote && window.mote.invoke) {
          window.mote.invoke("close_tab", { id: tab.id }).catch(function () {});
        }
      });

      strip.appendChild(row);
    });
  }

  function wireNewTab() {
    // P1: the [⊕] new-tab button moved from the sidebar header into the global
    // chrome header. The selector targets the first [data-action='new-tab']
    // element, which is the header button in the new HTML. Falls back gracefully.
    var btn = document.querySelector("[data-action='new-tab']");
    if (!btn) return;
    btn.addEventListener("click", function () {
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("new_tab", {}).catch(function () {});
      }
    });
  }

  function wireNewTabShortcut() {
    document.addEventListener("keydown", function (ev) {
      // ⌘T on macOS / Ctrl+T on Linux.
      var modifier = ev.metaKey || ev.ctrlKey;
      if (!modifier) return;
      if (ev.key !== "t" && ev.key !== "T") return;
      ev.preventDefault();
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("new_tab", {}).catch(function () {});
      }
    });
  }

  // ---- Activity-bar panel switching --------------------------------------
  //
  // Binds the three first-class panels (tabs, bookmarks, history) to the
  // activity-bar buttons identified by aria-label. On click:
  //   1. Update is-active + aria-pressed on the activity buttons.
  //   2. Switch the visible [data-panel] container via is-active-panel.
  //   3. Update the [tabs]/[bookmarks]/[history] lockup .name in the header.
  //   4. Invoke set_active_panel so the shell pushes fresh data for the panel.
  //
  // P1: plugin-placeholder slots (data-rail-slot="plugin-4/5") are wired
  // separately in wireRailPlaceholders() — they open the command palette.
  function wireActivityBar() {
    var panels = ["tabs", "bookmarks", "history"];
    var buttons = {};
    var containers = {};
    var nameEl = document.querySelector(".sidepanel-slot .name");
    // P1: no sidepanel-meta; we now have .tab-count-chip (tabs panel only).
    var metaEl = null; // retained for wireActivityBar return shape compat

    panels.forEach(function (name) {
      var btn = document.querySelector(".activity-btn[aria-label='" + name + "']");
      var container = document.querySelector("[data-panel='" + name + "']");
      if (btn) buttons[name] = btn;
      if (container) containers[name] = container;
    });

    function switchToPanel(name) {
      // Update activity button states.
      panels.forEach(function (p) {
        var btn = buttons[p];
        if (!btn) return;
        if (p === name) {
          btn.classList.add("is-active");
          btn.setAttribute("aria-pressed", "true");
        } else {
          btn.classList.remove("is-active");
          btn.setAttribute("aria-pressed", "false");
        }
      });

      // Switch visible panel container.
      panels.forEach(function (p) {
        var c = containers[p];
        if (!c) return;
        if (p === name) {
          c.classList.add("is-active-panel");
        } else {
          c.classList.remove("is-active-panel");
        }
      });

      // Update the lockup header name.
      if (nameEl) {
        nameEl.textContent = name;
      }

      // Tell the shell to push fresh data for the newly active panel.
      if (window.mote && window.mote.invoke) {
        window.mote
          .invoke("set_active_panel", { name: name })
          .catch(function () {});
      }
    }

    panels.forEach(function (name) {
      var btn = buttons[name];
      if (!btn) return;
      btn.addEventListener("click", function () {
        switchToPanel(name);
      });
    });

    // Expose for use in applyOp handlers that need to update meta selectively.
    return {
      activePanel: function () {
        for (var i = 0; i < panels.length; i++) {
          var p = panels[i];
          var btn = buttons[p];
          if (btn && btn.classList.contains("is-active")) return p;
        }
        return "tabs";
      },
      metaEl: metaEl,
    };
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
        // Update bookmark-toggle visual state when present.
        if (payload && typeof payload.bookmarked === "boolean") {
          var bm = document.querySelector(".bookmark-toggle");
          if (bm) {
            if (payload.bookmarked) {
              bm.classList.add("is-bookmarked");
              bm.setAttribute("aria-pressed", "true");
            } else {
              bm.classList.remove("is-bookmarked");
              bm.setAttribute("aria-pressed", "false");
            }
          }
        }
        break;
      case "set_tabs":
        if (payload && Array.isArray(payload.tabs)) {
          renderTabs(payload.tabs);
          // P1: count chip shows just the number (e.g. "3" not "3 open").
          var countChip = document.querySelector(".tab-count-chip");
          if (countChip) {
            countChip.textContent = String(payload.tabs.length);
          }
          // P1: also update the status-line tab count segment.
          var slTabSeg = document.querySelector(".sl .seg[data-sl-tabs]");
          if (slTabSeg) {
            slTabSeg.textContent = payload.tabs.length + " tabs";
          }
        }
        break;
      default:
        // Unknown ops are ignored (forward-compatible).
        break;
    }
  };

  // Format a Unix-millisecond timestamp as a human-readable relative string
  // using Intl.RelativeTimeFormat.  Returns "" for invalid input.
  function formatRelativeTime(timeMs) {
    if (typeof timeMs !== "number" || !isFinite(timeMs)) return "";
    var diffMs = Date.now() - timeMs;
    var rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });
    var sec = Math.round(diffMs / 1000);
    if (Math.abs(sec) < 60) return rtf.format(-sec, "second");
    var min = Math.round(sec / 60);
    if (Math.abs(min) < 60) return rtf.format(-min, "minute");
    var hr = Math.round(min / 60);
    if (Math.abs(hr) < 24) return rtf.format(-hr, "hour");
    var day = Math.round(hr / 24);
    if (Math.abs(day) < 7) return rtf.format(-day, "day");
    var wk = Math.round(day / 7);
    if (Math.abs(wk) < 5) return rtf.format(-wk, "week");
    var mon = Math.round(day / 30);
    if (Math.abs(mon) < 12) return rtf.format(-mon, "month");
    return rtf.format(-Math.round(day / 365), "year");
  }

  // Build one sidepanel row.  Title is the primary identifier (real-browser
  // convention); URL is the dim secondary context.  If `title` is missing/empty
  // or equal to the URL, the URL becomes the primary text and the secondary
  // cell is left empty so the grid columns stay aligned across rows.
  //
  // When `record.time_ms` is a finite number (history rows carry this; bookmark
  // rows do not), a `.row-time` span with a relative-time label is appended.
  function buildSidePanelRow(record, options) {
    var url = (typeof record.url === "string") ? record.url : "";
    var rawTitle = (typeof record.title === "string") ? record.title : "";

    var row = document.createElement("button");
    row.className = "sidepanel-row";
    row.setAttribute("data-url", url);

    // Primary: title if it adds info, else url as fallback.
    var titleSpan = document.createElement("span");
    titleSpan.className = "row-title";
    titleSpan.textContent = rawTitle || url;
    row.appendChild(titleSpan);

    // Secondary: url, only when distinct from the primary.
    var urlSpan = document.createElement("span");
    urlSpan.className = "row-url";
    if (rawTitle && rawTitle !== url) {
      urlSpan.textContent = url;
    }
    row.appendChild(urlSpan);

    // Relative timestamp (history rows only — bookmarks don't carry time_ms).
    if (typeof record.time_ms === "number" && isFinite(record.time_ms)) {
      var timeSpan = document.createElement("span");
      timeSpan.className = "row-time";
      timeSpan.textContent = formatRelativeTime(record.time_ms);
      row.appendChild(timeSpan);
    }

    if (options && options.withRemove) {
      var removeBtn = document.createElement("button");
      removeBtn.className = "row-remove";
      removeBtn.setAttribute(
        "aria-label",
        options.removeAriaLabel || "remove",
      );
      removeBtn.textContent = "×"; // ×
      removeBtn.addEventListener("click", function (ev) {
        ev.stopPropagation();
        if (window.mote && window.mote.invoke && options.removeOp) {
          window.mote
            .invoke(options.removeOp, { url: url })
            .catch(function () {});
        }
      });
      row.appendChild(removeBtn);
    }

    row.addEventListener("click", function () {
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("navigate", { url: url }).catch(function () {});
      }
    });

    return row;
  }

  // ---- applyOp handler for bookmark_list + history_list -----------------
  //
  // Chained via prevApplyOp so existing set_url/set_tabs/urlbar_suggestions ops
  // are preserved.  Payload contracts:
  //   bookmark_list: { rows: [{url, title}, ...], count: N }
  //   history_list:  { rows: [{url, title}, ...], count: N, truncated: bool }
  //
  // DOM-build discipline (ADR-0005): createElement + textContent + appendChild.
  // NEVER innerHTML on payload-derived content.
  function wireSidePanelOps(activityBar) {
    var prevApplyOp =
      typeof window.mote.applyOp === "function" ? window.mote.applyOp : null;

    window.mote.applyOp = function (op, payload) {
      if (op === "bookmark_list") {
        var container = document.querySelector("[data-panel='bookmarks']");
        if (!container) {
          if (prevApplyOp) prevApplyOp(op, payload);
          return;
        }

        // Clear previous content.
        container.textContent = "";

        var rows = (payload && Array.isArray(payload.rows)) ? payload.rows : [];
        var count = (payload && typeof payload.count === "number") ? payload.count : rows.length;

        var list = document.createElement("div");
        list.className = "sidepanel-list";

        rows.forEach(function (record) {
          var row = buildSidePanelRow(record, {
            withRemove: true,
            removeAriaLabel: "remove bookmark",
            removeOp: "bookmark_remove",
          });
          list.appendChild(row);
        });

        container.appendChild(list);

        // Update meta when bookmarks is the active panel.
        var metaEl = activityBar.metaEl;
        if (metaEl && activityBar.activePanel() === "bookmarks") {
          metaEl.textContent = count + " saved";
        }
        return;
      }

      if (op === "history_list") {
        var hContainer = document.querySelector("[data-panel='history']");
        if (!hContainer) {
          if (prevApplyOp) prevApplyOp(op, payload);
          return;
        }

        // Clear previous content.
        hContainer.textContent = "";

        var hRows = (payload && Array.isArray(payload.rows)) ? payload.rows : [];
        var hCount = (payload && typeof payload.count === "number") ? payload.count : hRows.length;
        var truncated = (payload && payload.truncated === true);

        var hList = document.createElement("div");
        hList.className = "sidepanel-list";

        hRows.forEach(function (record) {
          hList.appendChild(buildSidePanelRow(record, { withRemove: false }));
        });

        hContainer.appendChild(hList);

        if (truncated) {
          var footer = document.createElement("div");
          footer.className = "sidepanel-footer";
          footer.textContent = "showing 200 most recent";
          hContainer.appendChild(footer);
        }

        // Update meta when history is the active panel.
        var hMetaEl = activityBar.metaEl;
        if (hMetaEl && activityBar.activePanel() === "history") {
          hMetaEl.textContent = hCount + " visits";
        }
        return;
      }

      if (prevApplyOp) prevApplyOp(op, payload);
    };
  }

  // ---- Workspace chip + popover ----------------------------------------
  //
  // P1: the workspace strip moved from the left sidebar into the chrome header
  // as a .ws-chip keycap button. The ws-chip shows "[ws] <name> ›" and
  // clicking toggles the workspace popover dropdown.
  //
  // Accessibility: role="button" + aria-haspopup on the chip; role="listbox"
  // on the popover. Esc closes; click-outside closes; Tab leaves the chip.
  //
  // DOM-build discipline (ADR-0005): createElement + textContent only.
  // NEVER innerHTML on payload content.
  function wireWorkspaceStrip() {
    var strip = document.querySelector(".ws-chip");
    var popover = document.getElementById("workspace-popover");
    if (!strip || !popover) return;

    // Seed the popover with at least the current chip name as a placeholder
    // row so the first click renders SOMETHING visible even if the shell's
    // workspace_list push hasn't arrived (or got missed due to timing).  The
    // real list replaces this once the applyOp fires.
    function seedFallbackRow() {
      if (popover.childElementCount > 0) return;
      // P1: workspace name is in .ws-name inside the .ws-chip.
      var nameEl = strip.querySelector(".ws-name");
      var current = nameEl ? nameEl.textContent : "";
      if (!current) return;
      var row = document.createElement("div");
      row.className = "row is-current";
      row.setAttribute("role", "option");
      row.setAttribute("data-id", current);
      var check = document.createElement("span");
      check.className = "check";
      check.textContent = "✓"; // ✓
      row.appendChild(check);
      var name = document.createElement("span");
      name.className = "name";
      name.textContent = current;
      row.appendChild(name);
      popover.appendChild(row);
    }
    seedFallbackRow();

    function isPopoverOpen() {
      return !popover.hidden;
    }

    function openPopover() {
      popover.hidden = false;
      strip.setAttribute("aria-expanded", "true");
    }

    function closePopover() {
      popover.hidden = true;
      strip.setAttribute("aria-expanded", "false");
    }

    // Toggle on strip click.
    strip.addEventListener("click", function () {
      if (isPopoverOpen()) {
        closePopover();
      } else {
        openPopover();
      }
    });

    // Keyboard: Esc closes and returns focus to strip.
    strip.addEventListener("keydown", function (ev) {
      if (ev.key === "Escape" && isPopoverOpen()) {
        closePopover();
        strip.focus();
        ev.preventDefault();
      } else if (ev.key === "Enter" || ev.key === " ") {
        if (isPopoverOpen()) {
          closePopover();
        } else {
          openPopover();
        }
        ev.preventDefault();
      }
    });

    // Close on click-outside (strip or popover clicks are kept).
    document.addEventListener("click", function (ev) {
      if (!isPopoverOpen()) return;
      if (ev.target.closest(".workspace-strip, .workspace-popover")) return;
      closePopover();
    });

    // Popover row clicks: invoke set_active_workspace + close.
    popover.addEventListener("click", function (ev) {
      var row = ev.target.closest(".row[data-id]");
      if (!row) return;
      var id = row.getAttribute("data-id");
      if (id && window.mote && window.mote.invoke) {
        window.mote
          .invoke("set_active_workspace", { id: id })
          .catch(function () {});
      }
      closePopover();
      // Strip text updates when the next workspace_list push arrives — no
      // optimistic update here (the shell is the source of truth).
    });

    // workspace_list applyOp handler chained via prevApplyOp.
    // Payload: { rows: [{id, name, active}, ...] }
    var prevApplyOp =
      typeof window.mote.applyOp === "function" ? window.mote.applyOp : null;

    window.mote.applyOp = function (op, payload) {
      if (op !== "workspace_list") {
        if (prevApplyOp) prevApplyOp(op, payload);
        return;
      }

      var rows = (payload && Array.isArray(payload.rows)) ? payload.rows : [];

      // Build into a fragment first.  If rendering produces zero valid rows
      // (e.g., the shell pushed an empty list due to a transient invoke
      // failure), we preserve the existing popover content rather than clear
      // it — so the seeded fallback row remains visible.
      var frag = document.createDocumentFragment();
      var activeRow = null;
      var renderedCount = 0;
      for (var i = 0; i < rows.length; i++) {
        var record = rows[i];
        if (!record || typeof record.id !== "string") continue;
        if (record.active === true) activeRow = record;

        var rowEl = document.createElement("div");
        rowEl.className = "row" + (record.active === true ? " is-current" : "");
        rowEl.setAttribute("role", "option");
        rowEl.setAttribute("data-id", record.id);

        var checkEl = document.createElement("span");
        checkEl.className = "check";
        checkEl.textContent = record.active === true ? "✓" : " "; // ✓
        rowEl.appendChild(checkEl);

        var nameSpan = document.createElement("span");
        nameSpan.className = "name";
        nameSpan.textContent =
          typeof record.name === "string" ? record.name : record.id;
        rowEl.appendChild(nameSpan);

        frag.appendChild(rowEl);
        renderedCount++;
      }

      if (renderedCount > 0) {
        popover.textContent = "";
        popover.appendChild(frag);
        // P1: the workspace name is in .ws-name inside the .ws-chip button.
        var nameEl = strip.querySelector(".ws-name");
        if (nameEl && activeRow && typeof activeRow.name === "string") {
          nameEl.textContent = activeRow.name;
        }
        // Also update aria-label on the chip for accessibility.
        if (activeRow && typeof activeRow.name === "string") {
          strip.setAttribute("aria-label", "workspace: " + activeRow.name);
        }
      }
      // else: keep whatever was in the popover (typically the seeded fallback).
    };
  }

  function wireBookmarkToggle() {
    var bookmarkBtn = document.querySelector(".bookmark-toggle");
    if (bookmarkBtn) {
      bookmarkBtn.addEventListener("click", function () {
        if (window.mote && window.mote.invoke) {
          window.mote.invoke("bookmark_toggle", {}).catch(function () {});
        }
      });
    }
  }

  // R4: wire the close-window button in the top-right of the header.
  // Sends the `close_window` op to the shell, which sets `should_exit = true`
  // and exits the event loop on the next tick.
  function wireCloseWindowButton() {
    var btn = document.querySelector(".close-window-btn");
    if (!btn) return;
    btn.addEventListener("click", function () {
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("close_window", {}).catch(function () {});
      }
    });
  }

  // ---- P1: Tooltip primitive (Group B) ------------------------------------
  //
  // A single delegated event listener at the chrome root handles all tooltips.
  // No per-element JS wiring required — just add data-tooltip="..." to an
  // element. Optionally add data-tooltip-kbd="⌘T" for a keyboard shortcut.
  //
  // Spec (polish-phase-design §P1):
  //   200ms hover delay
  //   surface-2 bg, 1px var(--border), sharp corners (var(--radius-2))
  //   caption text + optional <kbd> chord on right
  //   positions below trigger; flips to above when clipped
  //
  // The tooltip element is injected into <body> on first use and reused.
  // It is removed from view (not from DOM) by opacity + pointer-events: none.
  function wireTooltip() {
    var tip = null;         // the .mote-tooltip element (created lazily)
    var hoverTimer = null;  // setTimeout handle for the 200ms delay
    var currentTarget = null; // the element currently being timed/shown

    function getOrCreateTip() {
      if (!tip) {
        tip = document.createElement("div");
        tip.className = "mote-tooltip";
        tip.setAttribute("role", "tooltip");
        document.body.appendChild(tip);
      }
      return tip;
    }

    function showTip(target) {
      var text = target.getAttribute("data-tooltip");
      if (!text) return;

      var el = getOrCreateTip();
      el.textContent = ""; // clear previous content (textContent, not innerHTML)

      // Caption span (primary line).
      var caption = document.createElement("span");
      caption.className = "tooltip-caption";
      caption.textContent = text;
      el.appendChild(caption);

      // P3: optional secondary line (tab URL). Rendered in a dimmer span below
      // the primary caption, before the optional <kbd> chord. This enables the
      // two-line tab tooltip: title on top, URL underneath.
      var secondary = target.getAttribute("data-tooltip-secondary");
      if (secondary) {
        var secSpan = document.createElement("span");
        secSpan.className = "tooltip-secondary";
        secSpan.textContent = secondary; // text node — not innerHTML
        el.appendChild(secSpan);
      }

      // Optional kbd chord.
      var kbd = target.getAttribute("data-tooltip-kbd");
      if (kbd) {
        // Build individual <kbd> elements for each key in the chord.
        // The chord string may be something like "⌘T" or "⌘⇧W".
        // We split on space to allow multi-key chords like "⌘ K".
        var parts = kbd.split(/\s+/);
        parts.forEach(function (part) {
          if (!part) return;
          var k = document.createElement("kbd");
          k.textContent = part;
          el.appendChild(k);
        });
      }

      // Position below the trigger; flip to above if clipped by viewport bottom.
      el.classList.remove("is-visible");
      el.style.left = "-9999px";
      el.style.top = "-9999px";

      // Force layout so we can measure.
      var rect = target.getBoundingClientRect();
      var tipW = el.offsetWidth || 120;
      var tipH = el.offsetHeight || 28;

      var left = rect.left;
      var top = rect.bottom + 4;

      // Clamp to viewport.
      var vpW = window.innerWidth || document.documentElement.clientWidth;
      var vpH = window.innerHeight || document.documentElement.clientHeight;

      if (left + tipW > vpW - 4) {
        left = Math.max(4, vpW - tipW - 4);
      }

      // Flip to above if below would clip.
      if (top + tipH > vpH - 4) {
        top = rect.top - tipH - 4;
      }

      el.style.left = left + "px";
      el.style.top = top + "px";
      el.classList.add("is-visible");
    }

    function hideTip() {
      if (hoverTimer) {
        clearTimeout(hoverTimer);
        hoverTimer = null;
      }
      currentTarget = null;
      if (tip) {
        tip.classList.remove("is-visible");
      }
    }

    document.addEventListener("mouseover", function (ev) {
      var target = ev.target.closest("[data-tooltip]");
      if (!target) {
        // Moved to a non-tooltip element: cancel pending timer.
        if (hoverTimer) {
          clearTimeout(hoverTimer);
          hoverTimer = null;
        }
        return;
      }
      if (target === currentTarget) return; // still on same target
      hideTip();
      currentTarget = target;
      hoverTimer = setTimeout(function () {
        hoverTimer = null;
        showTip(target);
      }, 200); // 200ms delay per spec
    });

    document.addEventListener("mouseout", function (ev) {
      // If leaving the document entirely or moving to a non-tooltip area, hide.
      var related = ev.relatedTarget;
      if (!related || !related.closest("[data-tooltip]")) {
        hideTip();
      }
    });

    // Hide on scroll or pointer down (tooltip is purely informational).
    document.addEventListener("pointerdown", hideTip, { passive: true });
    document.addEventListener("scroll", hideTip, { passive: true, capture: true });
    // Hide when keyboard focus moves away.
    document.addEventListener("focusin", function (ev) {
      if (!ev.target.closest("[data-tooltip]")) hideTip();
    });
  }

  // ---- P1: Rail plugin-placeholder click ----------------------------------
  //
  // Clicking slots 4 or 5 (rail-plugin-placeholder) opens the command palette.
  // The palette doesn't have a filter mechanism in P1, so we just open it.
  // Per ADR-0014 §v0.1 scope + the brief: "don't expand scope here."
  function wireRailPlaceholders() {
    var placeholders = document.querySelectorAll(".rail-plugin-placeholder");
    for (var i = 0; i < placeholders.length; i++) {
      (function (btn) {
        btn.addEventListener("click", function () {
          // Open the command palette — the nearest integration point for
          // plugin discovery. The palette toggle op opens the palette widget.
          if (window.mote && window.mote.invoke) {
            window.mote
              .invoke("open_palette", {})
              .catch(function () {});
          }
        });
      })(placeholders[i]);
    }
  }

  // P6: wire the settings rail button — opens the settings panel in a
  // new tab.
  //
  // The cog is slot 4 of the activity bar ([data-action='open-settings']).
  // Clicking it invokes `new_tab` with the settings URL; the shell routes
  // mote:// URLs through `create_content_page` (ADR-0015) which uses CEF's
  // global request context. Per-profile contexts cannot load mote:// URLs
  // — the S1 navigation guard cancels them — so navigating the active tab
  // (the original P6 wiring) silently failed. Opening in a new tab also
  // matches the Chrome `chrome://settings` convention.
  function wireSettingsButton() {
    var btn = document.querySelector("[data-action='open-settings']");
    if (!btn) return;
    btn.addEventListener("click", function () {
      if (window.mote && window.mote.invoke) {
        window.mote
          .invoke("new_tab", { url: "mote://chrome/settings/general" })
          .catch(function () {});
      }
    });
  }

  // R4: handle the `focus_omnibox` applyOp pushed by `Ctrl+L`.
  // Focuses the omnibox input and selects all existing text — the standard
  // address-bar behavior. Chained via prevApplyOp so existing handlers are
  // preserved.
  function wireFocusOmniboxOp() {
    var prevApplyOp =
      typeof window.mote.applyOp === "function" ? window.mote.applyOp : null;

    window.mote.applyOp = function (op, payload) {
      if (op !== "focus_omnibox") {
        if (prevApplyOp) prevApplyOp(op, payload);
        return;
      }
      var input = document.getElementById("omnibox-input");
      if (input) {
        input.focus();
        input.select();
      }
    };
  }

  function boot() {
    installInvoke();
    wireOmnibox();
    wireBookmarkToggle();
    wireCloseWindowButton();
    wireNewTab();
    wireNewTabShortcut();
    // Chain the urlbar_suggestions applyOp handler after the base handler is
    // installed above.  wireCompletionsOp() captures prevApplyOp at call-time.
    wireCompletionsOp();
    // Wire the activity-bar panel switching (tabs / bookmarks / history).
    var activityBar = wireActivityBar();
    // Chain the bookmark_list + history_list applyOp handlers last so they wrap
    // all prior handlers.
    wireSidePanelOps(activityBar);
    // P1: wire the workspace chip (moved to chrome-header) + popover.
    // Must run after all prior applyOp handlers are chained so the
    // prevApplyOp capture is complete.
    wireWorkspaceStrip();
    // R4: chain the focus_omnibox applyOp handler (Ctrl+L). Must run after
    // wireOmnibox installs the initial applyOp so prevApplyOp is defined.
    wireFocusOmniboxOp();
    // P1: install the tooltip primitive (delegated listener at the root).
    wireTooltip();
    // P1: wire the rail plugin-placeholder click handlers.
    wireRailPlaceholders();
    // P6: wire the settings rail button (slot 4 → navigate to settings panel).
    wireSettingsButton();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
