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

  // ---- H14/H16: live integrity surfaces -------------------------------------
  //
  // The settings page is served at the privileged mote://chrome origin and so
  // carries the host-bridge (window.mote.invoke) — PageRole::Overlay governs
  // the nav guard, not the origin-gated bridge. So we call the integrity ops
  // directly; the shell pushes results straight back to this page (the active
  // content tab) via the named callbacks below, the same pattern the tab picker
  // uses (window.__motePicker).

  function requestIntegrityList() {
    invoke("integrity_list", {}).catch(function () {});
  }

  function requestIntegrityDetail(pluginName) {
    invoke("integrity_plugin_detail", { plugin: pluginName }).catch(function () {});
  }

  // Shell push targets — the shell's eval_js calls window.__moteIntegrityList /
  // __moteIntegrityDetail on this page (handleIntegrity* are hoisted below).
  window.__moteIntegrityList = function (payload) {
    handleIntegrityList(payload);
  };
  window.__moteIntegrityDetail = function (payload) {
    handleIntegrityDetail(payload);
  };

  // ---- H14: rebuild tbody from live render_integrity_list payload -----

  // Map IntegrityStatus variant → badge CSS class and label text.
  // Mirrors the statusBadgeVariant/statusLabel helpers in panels.js.
  function integrityBadgeVariant(status) {
    switch (status) {
      case "Verified": return "success";
      case "Mismatch": return "danger";
      case "DevMode": return "accent";
      case "Bundled": return "info";
      default: return "";
    }
  }

  function integrityStatusLabel(status) {
    switch (status) {
      case "Verified": return "verified";
      case "Mismatch": return "mismatch";
      case "DevMode": return "dev mode";
      case "Bundled": return "bundled";
      default: return "unknown";
    }
  }

  // Lower-case status value written to data-status so the existing
  // filterIntegrityTable() selector (`rowStatus === status`) matches the
  // <select> option values ("verified", "mismatch", "bundled", "dev-mode",
  // "unknown").
  function statusDataAttr(status) {
    switch (status) {
      case "Verified": return "verified";
      case "Mismatch": return "mismatch";
      case "DevMode": return "dev-mode";
      case "Bundled": return "bundled";
      default: return "unknown";
    }
  }

  function handleIntegrityList(payload) {
    if (!payload || !Array.isArray(payload.plugins)) return;
    var table = document.querySelector(".integrity-table");
    if (!table) return;
    var tbody = table.querySelector("tbody");
    if (!tbody) return;

    // Rebuild tbody — all plugin-derived strings go through textContent.
    tbody.textContent = "";
    for (var i = 0; i < payload.plugins.length; i++) {
      var p = payload.plugins[i] || {};
      tbody.appendChild(buildIntegrityRow(p));
    }

    // Re-wire sort headers (their click targets tr[data-plugin] rows; clearing
    // the tbody removes prior event listeners attached to old rows — the header
    // listeners are already wired at section-init time and remain valid since
    // they query tr[data-plugin] fresh on each invocation).
    //
    // Re-wire the detail click on newly created rows.
    wireIntegrityRowClicks();
  }

  function buildIntegrityRow(p) {
    var name = String(p.name || "");
    var version = String(p.version || "");
    var status = String(p.integrity || "");
    var sourceLabel = String(p.source_label || "");

    var tr = document.createElement("tr");
    tr.setAttribute("data-plugin", name);
    tr.setAttribute("data-status", statusDataAttr(status));
    tr.setAttribute("data-verified", "");

    // plugin name + version
    var tdName = document.createElement("td");
    tdName.textContent = name + (version ? "  " + version : "");
    tr.appendChild(tdName);

    // integrity badge
    var tdStatus = document.createElement("td");
    var badge = document.createElement("span");
    badge.className = "badge " + integrityBadgeVariant(status);
    badge.textContent = integrityStatusLabel(status);
    tdStatus.appendChild(badge);
    // source label alongside badge
    if (sourceLabel) {
      var src = document.createElement("span");
      src.style.cssText = "margin-left:8px;color:var(--fg-3);font:var(--text-mono-sm)";
      src.textContent = sourceLabel;
      tdStatus.appendChild(src);
    }
    tr.appendChild(tdStatus);

    // last verified placeholder — the list payload carries no timestamp;
    // show an honest "—" rather than a fabricated value.
    var tdVerified = document.createElement("td");
    tdVerified.style.color = "var(--fg-2)";
    tdVerified.textContent = "—"; // "—"
    tr.appendChild(tdVerified);

    return tr;
  }

  // ---- H16: drill-down detail panel ----------------------------------------

  // Show / hide the detail expansion below the selected row.
  function showIntegrityDetail(plugin) {
    requestIntegrityDetail(plugin);
    // Visually mark the selected row while waiting for the response.
    document.querySelectorAll(".integrity-table tbody tr[data-plugin]").forEach(function (r) {
      r.classList.toggle("is-detail-open", r.getAttribute("data-plugin") === plugin);
    });
  }

  function handleIntegrityDetail(payload) {
    if (!payload || typeof payload !== "object") return;
    var panel = ensureDetailPanel();
    renderDetailPanel(panel, payload);
  }

  function ensureDetailPanel() {
    var existing = document.getElementById("integrity-detail-panel");
    if (existing) return existing;
    var panel = document.createElement("div");
    panel.id = "integrity-detail-panel";
    panel.className = "integrity-detail-panel";
    panel.setAttribute("role", "region");
    panel.setAttribute("aria-label", "plugin integrity detail");
    // Insert after the table.
    var table = document.querySelector(".integrity-table");
    if (table && table.parentNode) {
      table.parentNode.insertBefore(panel, table.nextSibling);
    }
    return panel;
  }

  function renderDetailPanel(panel, d) {
    // Wipe and rebuild — all plugin-derived values go through textContent.
    panel.textContent = "";

    var name = String(d.name || "");
    var integrity = String(d.integrity || "");
    var isMismatch = integrity === "Mismatch";

    // Header: [plugin-name] + badge
    var header = document.createElement("div");
    header.className = "detail-header";

    var headerLockup = document.createElement("span");
    headerLockup.className = "detail-name";
    var lb = document.createElement("span");
    lb.className = "br";
    lb.textContent = "[";
    var nm = document.createElement("span");
    nm.textContent = name;
    var rb = document.createElement("span");
    rb.className = "br";
    rb.textContent = "]";
    headerLockup.appendChild(lb);
    headerLockup.appendChild(nm);
    headerLockup.appendChild(rb);
    header.appendChild(headerLockup);

    var badge = document.createElement("span");
    badge.className = "badge " + integrityBadgeVariant(integrity);
    badge.textContent = integrityStatusLabel(integrity);
    header.appendChild(badge);

    // Close button
    var closeBtn = document.createElement("button");
    closeBtn.className = "btn btn-ghost detail-close";
    closeBtn.type = "button";
    closeBtn.setAttribute("aria-label", "close detail");
    closeBtn.textContent = "close";
    closeBtn.addEventListener("click", function () {
      panel.textContent = "";
      panel.classList.remove("is-visible");
      document.querySelectorAll(".integrity-table tbody tr.is-detail-open").forEach(function (r) {
        r.classList.remove("is-detail-open");
      });
    });
    header.appendChild(closeBtn);
    panel.appendChild(header);

    // CRITICAL — mismatch: show both expected and actual checksum prominently.
    if (isMismatch) {
      var mismatchBanner = document.createElement("div");
      mismatchBanner.className = "detail-mismatch-banner";
      var mismatchTitle = document.createElement("div");
      mismatchTitle.className = "detail-mismatch-title";
      mismatchTitle.textContent = "checksum mismatch — plugin files do not match the lock entry";
      mismatchBanner.appendChild(mismatchTitle);

      if (d.checksum != null) {
        var expRow = document.createElement("div");
        expRow.className = "detail-checksum-row";
        var expLabel = document.createElement("span");
        expLabel.className = "detail-checksum-label";
        expLabel.textContent = "expected";
        var expVal = document.createElement("span");
        expVal.className = "detail-checksum-value";
        expVal.textContent = String(d.checksum);
        expRow.appendChild(expLabel);
        expRow.appendChild(expVal);
        mismatchBanner.appendChild(expRow);
      }

      if (d.actual_checksum != null) {
        var actRow = document.createElement("div");
        actRow.className = "detail-checksum-row detail-checksum-actual";
        var actLabel = document.createElement("span");
        actLabel.className = "detail-checksum-label";
        actLabel.textContent = "actual";
        var actVal = document.createElement("span");
        actVal.className = "detail-checksum-value";
        actVal.textContent = String(d.actual_checksum);
        actRow.appendChild(actLabel);
        actRow.appendChild(actVal);
        mismatchBanner.appendChild(actRow);
      }

      panel.appendChild(mismatchBanner);
    }

    // Fields table: lock_source, pinned_commit, checksum (non-mismatch).
    var fields = document.createElement("dl");
    fields.className = "detail-fields";

    function addField(label, value, opts) {
      var dt = document.createElement("dt");
      dt.className = "detail-field-label";
      dt.textContent = label;
      var dd = document.createElement("dd");
      dd.className = "detail-field-value" + (opts && opts.extraClass ? " " + opts.extraClass : "");
      dd.textContent = value != null ? String(value) : "—";
      if (value == null && opts && opts.nullLabel) {
        dd.textContent = opts.nullLabel;
        dd.style.color = "var(--fg-3)";
      }
      fields.appendChild(dt);
      fields.appendChild(dd);
    }

    addField("lock source", d.lock_source, { nullLabel: "bundled — no lock entry" });
    addField("pinned commit", d.pinned_commit, { nullLabel: "—" });

    // Only show the single checksum field when NOT a mismatch (mismatch shows
    // both above in the danger banner).
    if (!isMismatch) {
      addField("checksum", d.checksum, { nullLabel: "—" });
    }

    panel.appendChild(fields);
    panel.classList.add("is-visible");
  }

  function wireIntegrityRowClicks() {
    document.querySelectorAll(".integrity-table tbody tr[data-plugin]").forEach(function (row) {
      // Remove old listeners by cloning the node — the simplest safe approach.
      var fresh = row.cloneNode(true);
      row.parentNode.replaceChild(fresh, row);
      fresh.addEventListener("click", function () {
        var plugin = fresh.getAttribute("data-plugin");
        if (plugin) showIntegrityDetail(plugin);
      });
    });
  }

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

    // Wire drill-down on the static seed rows (live rows are wired in
    // wireIntegrityRowClicks after handleIntegrityList rebuilds the tbody).
    wireIntegrityRowClicks();

    // H14: request the live integrity list from the chrome document.
    requestIntegrityList();
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
