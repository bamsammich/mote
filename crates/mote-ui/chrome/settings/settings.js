/*
 * settings.js — shared settings panel bootstrap (ADR-0005, ADR-0017).
 *
 * Each settings section page includes this script. It:
 *   1. Reads the active section from the URL path on DOMContentLoaded.
 *   2. Wires the section tab strip for same-panel navigation via navigate op.
 *   3. Sets up a shared mote.invoke guard so the page degrades gracefully if
 *      the privileged transport is absent (test harness / static preview).
 *
 * ADR-0005: this file MUST NEVER assign innerHTML / outerHTML / insertAdjacentHTML
 * from any user-supplied or plugin-supplied string. Data is always injected via
 * textContent or structured DOM builders.
 */
(function () {
  "use strict";

  // ---- Bridge guard ----------------------------------------------------------
  // In the live chrome the host.js bootstrap runs in the parent frame's context;
  // settings pages are loaded as full documents via navigate, so they get the
  // cefQuery binding only if the privileged transport is present. Guard every
  // window.mote.invoke call so the page renders in dev / static mode too.

  function invoke(op, params) {
    if (window.mote && typeof window.mote.invoke === "function") {
      return window.mote.invoke(op, params || {});
    }
    return Promise.resolve({ ok: false, error: "no transport" });
  }

  // ---- Section routing -------------------------------------------------------

  // Derive the active section from the URL path:
  //   mote://chrome/settings/general  → "general"
  //   mote://chrome/settings/plugins  → "plugins"
  //   mote://chrome/settings/integrity → "integrity"
  //   mote://chrome/settings/keybinds → "keybinds"
  var VALID_SECTIONS = ["general", "plugins", "integrity", "keybinds"];

  function activeSection() {
    var path = window.location.pathname; // "/settings/general"
    var parts = path.split("/");
    var last = parts[parts.length - 1] || "";
    // Strip .html suffix if any (static-file preview).
    last = last.replace(/\.html$/, "");
    return VALID_SECTIONS.indexOf(last) >= 0 ? last : "general";
  }

  // ---- Tab wire-up -----------------------------------------------------------

  function wireSettingsTabs() {
    var tabs = document.querySelectorAll(".settings-tab[data-section]");
    var current = activeSection();
    tabs.forEach(function (tab) {
      var sec = tab.getAttribute("data-section");
      if (sec === current) {
        tab.classList.add("is-active");
        tab.setAttribute("aria-current", "page");
      }
      tab.addEventListener("click", function (ev) {
        ev.preventDefault();
        if (sec !== current) {
          invoke("navigate", { url: "mote://chrome/settings/" + sec }).catch(
            function () {}
          );
        }
      });
    });
  }

  // ---- General section -------------------------------------------------------

  function wireGeneralSection() {
    // Theme dropdown.
    var themeSelect = document.getElementById("theme-select");
    if (themeSelect) {
      themeSelect.addEventListener("change", function () {
        invoke("set_theme", { theme: themeSelect.value }).catch(function () {});
      });
    }

    // Search engine fields.
    var searchName = document.getElementById("search-engine-name");
    var searchUrl = document.getElementById("search-engine-url");
    function saveSearchEngine() {
      if (!searchName || !searchUrl) return;
      invoke("set_search_engine", {
        name: searchName.value,
        url_template: searchUrl.value,
      }).catch(function () {});
    }
    if (searchName) searchName.addEventListener("change", saveSearchEngine);
    if (searchUrl) searchUrl.addEventListener("change", saveSearchEngine);

    // Hardware acceleration toggle.
    var hwSwitch = document.getElementById("hw-accel-switch");
    if (hwSwitch) {
      hwSwitch.addEventListener("click", function () {
        var on = hwSwitch.classList.toggle("on");
        hwSwitch.setAttribute("aria-checked", on ? "true" : "false");
        invoke("set_hw_accel", { enabled: on }).catch(function () {});
      });
    }

    // Per-origin zoom persistence toggle.
    var zoomSwitch = document.getElementById("zoom-persist-switch");
    if (zoomSwitch) {
      zoomSwitch.addEventListener("click", function () {
        var on = zoomSwitch.classList.toggle("on");
        zoomSwitch.setAttribute("aria-checked", on ? "true" : "false");
        invoke("set_zoom_persist", { enabled: on }).catch(function () {});
      });
    }
  }

  // ---- Plugins section -------------------------------------------------------

  function wirePluginsSection() {
    // Disable buttons.
    document.querySelectorAll("[data-action='plugin-disable']").forEach(function (btn) {
      btn.addEventListener("click", function (ev) {
        ev.stopPropagation();
        var plugin = btn.getAttribute("data-plugin");
        if (!plugin) return;
        invoke("plugin_disable", { plugin: plugin }).catch(function () {});
      });
    });

    // Uninstall buttons.
    document.querySelectorAll("[data-action='plugin-uninstall']").forEach(function (btn) {
      btn.addEventListener("click", function (ev) {
        ev.stopPropagation();
        var plugin = btn.getAttribute("data-plugin");
        if (!plugin) return;
        invoke("plugin_uninstall", { plugin: plugin }).catch(function () {});
      });
    });

    // Capability chip click → show detail.
    document.querySelectorAll(".capability-chip").forEach(function (chip) {
      chip.addEventListener("click", function (ev) {
        ev.stopPropagation();
        var cap = chip.getAttribute("data-cap");
        var detail = document.getElementById("cap-detail");
        if (!detail || !cap) return;
        var nameEl = detail.querySelector(".cap-detail-name");
        var descEl = detail.querySelector(".cap-detail-desc");
        if (nameEl) nameEl.textContent = cap;
        if (descEl) descEl.textContent = capabilityDescription(cap);
        // Position near the chip.
        var rect = chip.getBoundingClientRect();
        detail.style.top = (rect.bottom + 6) + "px";
        detail.style.left = rect.left + "px";
        detail.classList.add("is-visible");
      });
    });

    // Close cap detail on outside click.
    document.addEventListener("click", function () {
      var detail = document.getElementById("cap-detail");
      if (detail) detail.classList.remove("is-visible");
    });

    // Install plugin button → file picker trigger.
    var installBtn = document.getElementById("plugin-install-btn");
    if (installBtn) {
      installBtn.addEventListener("click", function () {
        invoke("plugin_install_picker", {}).catch(function () {});
      });
    }

    // Search filter.
    var search = document.getElementById("plugin-search");
    if (search) {
      search.addEventListener("input", function () {
        var q = search.value.toLowerCase();
        document.querySelectorAll(".plugin-row").forEach(function (row) {
          var name = (row.querySelector(".plugin-name") || {}).textContent || "";
          row.style.display = name.toLowerCase().indexOf(q) >= 0 ? "" : "none";
        });
      });
    }
  }

  // Static capability descriptions (v0.1 — wired to the live registry in v0.2).
  function capabilityDescription(cap) {
    var descriptions = {
      "ui:sidebar_panel": "registers a panel in the left sidebar",
      "ui:bookmarks_provider": "reads and writes the bookmark list",
      "ui:history_provider": "reads the browsing history",
      "workspace:provider": "manages named browser workspaces",
      "ui:urlbar_provider": "adds autocomplete suggestions to the address bar",
      "secret:read": "reads secrets from the secrets store",
    };
    return descriptions[cap] || cap;
  }

  // ---- Integrity section -----------------------------------------------------

  function wireIntegritySection() {
    // Reverify all button.
    var reverifyBtn = document.getElementById("reverify-all-btn");
    if (reverifyBtn) {
      reverifyBtn.addEventListener("click", function () {
        invoke("integrity_reverify_all", {}).catch(function () {});
      });
    }

    // Search filter.
    var search = document.getElementById("integrity-search");
    if (search) {
      search.addEventListener("input", function () {
        filterIntegrityTable();
      });
    }

    // Status filter dropdown.
    var statusFilter = document.getElementById("integrity-status-filter");
    if (statusFilter) {
      statusFilter.addEventListener("change", function () {
        filterIntegrityTable();
      });
    }

    // Column sort (click header → toggle asc/desc).
    document.querySelectorAll(".integrity-table th[data-col]").forEach(function (th) {
      th.addEventListener("click", function () {
        sortIntegrityTable(th.getAttribute("data-col"), th);
      });
    });

    // Row drill-down click → detail op (no-op in v0.1; shell logs).
    document.querySelectorAll(".integrity-table tbody tr[data-plugin]").forEach(function (row) {
      row.addEventListener("click", function () {
        var plugin = row.getAttribute("data-plugin");
        if (plugin) invoke("integrity_plugin_detail", { plugin: plugin }).catch(function () {});
      });
    });
  }

  function filterIntegrityTable() {
    var search = document.getElementById("integrity-search");
    var statusFilter = document.getElementById("integrity-status-filter");
    var q = search ? search.value.toLowerCase() : "";
    var status = statusFilter ? statusFilter.value : "";
    document.querySelectorAll(".integrity-table tbody tr[data-plugin]").forEach(function (row) {
      var name = row.getAttribute("data-plugin") || "";
      var rowStatus = row.getAttribute("data-status") || "";
      var matchName = name.toLowerCase().indexOf(q) >= 0;
      var matchStatus = !status || rowStatus === status;
      row.style.display = matchName && matchStatus ? "" : "none";
    });
  }

  function sortIntegrityTable(col, clickedTh) {
    var table = document.querySelector(".integrity-table");
    if (!table) return;
    var tbody = table.querySelector("tbody");
    if (!tbody) return;

    // Determine new sort direction.
    var asc = !clickedTh.classList.contains("sorted-asc");
    document.querySelectorAll(".integrity-table th").forEach(function (th) {
      th.classList.remove("sorted-asc", "sorted-desc");
    });
    clickedTh.classList.add(asc ? "sorted-asc" : "sorted-desc");

    // Collect rows and sort.
    var rows = Array.prototype.slice.call(tbody.querySelectorAll("tr[data-plugin]"));
    rows.sort(function (a, b) {
      var aVal = (a.getAttribute("data-" + col) || "").toLowerCase();
      var bVal = (b.getAttribute("data-" + col) || "").toLowerCase();
      if (aVal < bVal) return asc ? -1 : 1;
      if (aVal > bVal) return asc ? 1 : -1;
      return 0;
    });
    rows.forEach(function (row) {
      tbody.appendChild(row);
    });
  }

  // ---- Keybinds section ------------------------------------------------------

  function wireKeybindsSection() {
    // Search filter: hides rows whose action doesn't match.
    var search = document.getElementById("keybinds-search");
    if (search) {
      search.addEventListener("input", function () {
        var q = search.value.toLowerCase();
        document.querySelectorAll(".keybinds-table tbody tr").forEach(function (row) {
          var action = (row.querySelector(".kb-action") || {}).textContent || "";
          var chord = (row.querySelector(".kb-chord") || {}).textContent || "";
          var match = action.toLowerCase().indexOf(q) >= 0 || chord.toLowerCase().indexOf(q) >= 0;
          row.style.display = match ? "" : "none";
        });
        // Hide empty scope sections.
        document.querySelectorAll(".keybinds-scope-block").forEach(function (block) {
          var visible = Array.prototype.some.call(
            block.querySelectorAll(".keybinds-table tbody tr"),
            function (r) { return r.style.display !== "none"; }
          );
          block.style.display = visible ? "" : "none";
        });
      });
    }

    // Load keybinds from the live registry via the bridge op.
    invoke("keybinds_list", {}).then(function (result) {
      if (!result || !Array.isArray(result.keybinds)) return;
      renderKeybinds(result.keybinds);
    }).catch(function () {
      // Fallback: static seed rows remain (loaded in HTML).
    });
  }

  // Render keybinds JSON into the scope-grouped tables. Each keybind has:
  //   { action, chord, scope, source }
  // Grouped by scope in the order: global, chrome, content, captured-modal.
  function renderKeybinds(keybinds) {
    var SCOPES = ["global", "chrome", "content", "captured-modal"];
    SCOPES.forEach(function (scope) {
      var block = document.getElementById("scope-block-" + scope);
      if (!block) return;
      var tbody = block.querySelector("tbody");
      if (!tbody) return;

      // Clear seed rows.
      tbody.textContent = "";

      var rows = keybinds.filter(function (kb) { return kb.scope === scope; });
      rows.forEach(function (kb) {
        var tr = document.createElement("tr");

        // action
        var tdAction = document.createElement("td");
        tdAction.className = "kb-action";
        tdAction.textContent = kb.action || "";
        tr.appendChild(tdAction);

        // chord: render as <kbd> elements.
        var tdChord = document.createElement("td");
        tdChord.className = "kb-chord";
        var chordGroup = document.createElement("span");
        chordGroup.className = "chord-group";
        var chord = String(kb.chord || "");
        // Split on '+' to produce individual keycap elements but only at
        // the modifier boundary; avoid splitting the '+' keybind itself.
        var parts = chord === "+" ? ["+"] : chord.split("+").filter(function (p) { return p !== ""; });
        parts.forEach(function (part, i) {
          if (i > 0) {
            var plus = document.createElement("span");
            plus.textContent = "+";
            plus.style.color = "var(--fg-2)";
            plus.style.margin = "0 1px";
            chordGroup.appendChild(plus);
          }
          var kbd = document.createElement("kbd");
          kbd.textContent = part;
          chordGroup.appendChild(kbd);
        });
        tdChord.appendChild(chordGroup);
        tr.appendChild(tdChord);

        // source badge
        var tdSource = document.createElement("td");
        var badge = document.createElement("span");
        badge.className = "source-badge";
        badge.textContent = kb.source || "built-in";
        tdSource.appendChild(badge);
        tr.appendChild(tdSource);

        tbody.appendChild(tr);
      });

      // Show/hide the block based on whether there are rows.
      block.style.display = rows.length > 0 ? "" : "none";
    });
  }

  // ---- Boot ------------------------------------------------------------------

  document.addEventListener("DOMContentLoaded", function () {
    wireSettingsTabs();

    var section = activeSection();
    if (section === "general") wireGeneralSection();
    else if (section === "plugins") wirePluginsSection();
    else if (section === "integrity") wireIntegritySection();
    else if (section === "keybinds") wireKeybindsSection();
  });
})();
