/*
 * panels.js — chrome-side structured-DOM renderers for the integrity panel and
 * the permission-approval dialog (ADR-0005 compliance).
 *
 * The shell pushes JSON view-models (IntegrityPanel / ApprovalRequest) through
 * `window.mote.applyOp('render_integrity_panel'|'show_approval_dialog', data)`.
 * These builders construct DOM trees via `document.createElement` /
 * `textContent` / `setAttribute` ONLY — there is no path here that ever assigns
 * `innerHTML` / `outerHTML` / `insertAdjacentHTML` from any plugin-derived
 * string. The single `textContent = ""` use is a tree-clear (no parsing).
 *
 * Plugin-authored fields (name, version, source, permission domain, dangerous-
 * combination strings, etc.) are interpolated as text nodes so a hostile
 * manifest like `name: "<script>alert(1)</script>"` renders as inert literal
 * text in the chrome document. The boundary test in `crates/mote-ui/src/lib.rs`
 * grep-asserts this file contains no `innerHTML = ` substring.
 *
 * Style hooks come from `components/integrity-panel.css` and
 * `components/approval-dialog.css`; nothing new in this file invents visual
 * language — the layout mirrors the static `integrity-panel.html` /
 * `approval-dialog.html` design references.
 */
(function () {
  "use strict";

  // ──────────────────────────────────────────────────────────────────────────
  // tiny DOM helpers — structured construction, never markup
  // ──────────────────────────────────────────────────────────────────────────

  /**
   * Build an element with optional className, attribute map, and children.
   * `children` may be strings (rendered as text nodes), `Node`s, or arrays.
   * Plugin-derived strings always arrive via text-node coercion, never markup.
   */
  function el(tag, opts, children) {
    var node = document.createElement(tag);
    if (opts) {
      if (opts.class) node.className = opts.class;
      if (opts.attrs) {
        for (var k in opts.attrs) {
          if (Object.prototype.hasOwnProperty.call(opts.attrs, k)) {
            node.setAttribute(k, String(opts.attrs[k]));
          }
        }
      }
      if (opts.text != null) {
        // textContent is the canonical injection-safe write path.
        node.textContent = String(opts.text);
      }
    }
    appendChildren(node, children);
    return node;
  }

  function appendChildren(node, children) {
    if (children == null) return;
    if (Array.isArray(children)) {
      for (var i = 0; i < children.length; i++) appendChildren(node, children[i]);
      return;
    }
    if (typeof children === "string") {
      node.appendChild(document.createTextNode(children));
      return;
    }
    if (children instanceof Node) {
      node.appendChild(children);
    }
  }

  /**
   * The [name] lockup motif: mono brackets in --accent, name in --fg.
   * Panel headers use this with `class="lockup"` (the brand variant);
   * section headers use `sectionLockup()` below, which emits the same
   * three-span structure under `class="section-label"` — the CSS rule
   * `.integrity-section-head .section-label .br` styles brackets per spec.
   */
  function lockup(name, opts) {
    var span = el("span", { class: "lockup", attrs: { "aria-hidden": "true" } });
    span.appendChild(el("span", { class: "br", text: "[" }));
    span.appendChild(el("span", { class: "name", text: String(name) }));
    span.appendChild(el("span", { class: "br", text: "]" }));
    if (opts && opts.ariaLabel) {
      // Replace aria-hidden when an explicit label is given.
      span.removeAttribute("aria-hidden");
      span.setAttribute("aria-label", String(opts.ariaLabel));
    }
    return span;
  }

  /**
   * Section-header variant of `lockup()`: emits `class="section-label"` so
   * the integrity-panel section CSS (`.integrity-section-head .section-label
   * .br`) styles the brackets in accent. Used by every integrity-section
   * header to keep the bracket-triplet structure in one place.
   */
  function sectionLockup(name) {
    var span = el("span", { class: "section-label" });
    span.appendChild(el("span", { class: "br", text: "[" }));
    span.appendChild(document.createTextNode(String(name)));
    span.appendChild(el("span", { class: "br", text: "]" }));
    return span;
  }

  function clear(node) {
    if (node) node.textContent = "";
  }

  // ──────────────────────────────────────────────────────────────────────────
  // integrity panel — structured DOM from IntegrityPanel JSON
  // ──────────────────────────────────────────────────────────────────────────

  // PluginKind serializes as either an object `{DeclaredGit:{source,commit}}` /
  // `{PathLocal:{path}}` / `{ImplicitLocal:{path}}` / `{DevMode:{path}}` or the
  // string `"Bundled"`. Mirror PluginKind::source_label (Rust) so the
  // structured DOM shows the same provenance line the static fixture shows.
  function kindGlyph(kind) {
    if (typeof kind === "string") return "·"; // "Bundled"
    if (kind && typeof kind === "object") {
      if ("DeclaredGit" in kind) return "○";
      if ("PathLocal" in kind) return "◐";
      if ("ImplicitLocal" in kind) return "◇";
      if ("DevMode" in kind) return "⊙";
    }
    return "·";
  }

  function kindSourceLabel(kind) {
    if (typeof kind === "string") return "bundled";
    if (kind && typeof kind === "object") {
      if ("DeclaredGit" in kind) {
        var g = kind.DeclaredGit || {};
        var commit = String(g.commit || "");
        var short = commit.length > 12 ? commit.slice(0, 12) : commit;
        return String(g.source || "") + " @ " + short;
      }
      if ("PathLocal" in kind) return "path:" + String(kind.PathLocal.path || "");
      if ("ImplicitLocal" in kind) return "implicit  " + String(kind.ImplicitLocal.path || "");
      if ("DevMode" in kind) return "dev  " + String(kind.DevMode.path || "");
    }
    return "";
  }

  function isDevKind(kind) {
    return kind && typeof kind === "object" && "DevMode" in kind;
  }

  // IntegrityStatus serializes as a bare string variant.
  function statusBadgeVariant(status) {
    switch (status) {
      case "Verified": return "success";
      case "Mismatch": return "danger";
      case "DevMode": return "accent";
      case "Bundled": return "info";
      default: return "";
    }
  }
  function statusLabel(status) {
    switch (status) {
      case "Verified": return "verified";
      case "Mismatch": return "mismatch";
      case "DevMode": return "dev mode";
      case "Bundled": return "bundled";
      default: return "unknown";
    }
  }
  function actionLabel(action) {
    switch (action) {
      case "AdjustScope": return "adjust scope";
      case "Revoke": return "revoke";
      case "Update": return "update";
      case "Rollback": return "rollback";
      case "Settings": return "settings";
      case "Reload": return "reload";
      default: return String(action || "");
    }
  }

  function buildPluginCard(p) {
    var classes = "plugin-card";
    if (isDevKind(p.kind)) classes += " is-dev";
    if (p.integrity === "Mismatch") classes += " is-mismatch";

    var card = el("article", {
      class: classes,
      attrs: { "aria-label": String(p.name || "") },
    });

    // header: glyph + name (+ [dev] marker) + version + status badge
    var nameWrap = el("div", { class: "plugin-card-name" });
    nameWrap.appendChild(el("span", {
      class: "prov-glyph",
      attrs: { "aria-hidden": "true" },
      text: kindGlyph(p.kind),
    }));
    if (isDevKind(p.kind)) {
      nameWrap.appendChild(el("span", {
        class: "dev-marker",
        attrs: { "aria-label": "dev mode" },
        text: "[dev]",
      }));
      nameWrap.appendChild(document.createTextNode(" "));
    }
    nameWrap.appendChild(document.createTextNode(" " + String(p.name || "")));

    var head = el("header", { class: "plugin-card-head" });
    head.appendChild(nameWrap);
    head.appendChild(el("span", {
      class: "plugin-card-version",
      text: p.version === "local" ? "local" : "v" + String(p.version || ""),
    }));
    var badges = el("div", { class: "plugin-card-badges" });
    var badge = el("span", { class: "badge " + statusBadgeVariant(p.integrity) });
    badge.appendChild(el("span", { class: "dot", attrs: { "aria-hidden": "true" } }));
    badge.appendChild(document.createTextNode(statusLabel(p.integrity)));
    badges.appendChild(badge);
    head.appendChild(badges);
    card.appendChild(head);

    // provenance row
    var prov = el("div", { class: "plugin-provenance" });
    prov.appendChild(el("span", { class: "prov-label", text: "source" }));
    prov.appendChild(el("span", { class: "prov-value", text: kindSourceLabel(p.kind) }));
    card.appendChild(prov);

    // fulfills / consumes capability rows (only when non-empty)
    if (Array.isArray(p.fulfills) && p.fulfills.length > 0) {
      var row = el("div", { class: "plugin-meta-row" });
      row.appendChild(el("span", { class: "meta-label", text: "fulfills" }));
      for (var i = 0; i < p.fulfills.length; i++) {
        row.appendChild(el("span", {
          class: "capability-tag",
          text: String(p.fulfills[i]),
        }));
      }
      card.appendChild(row);
    }
    if (Array.isArray(p.consumes) && p.consumes.length > 0) {
      var row2 = el("div", { class: "plugin-meta-row" });
      row2.appendChild(el("span", { class: "meta-label", text: "consumes" }));
      for (var j = 0; j < p.consumes.length; j++) {
        row2.appendChild(el("span", {
          class: "capability-tag",
          text: String(p.consumes[j]),
        }));
      }
      card.appendChild(row2);
    }

    // permissions
    if (Array.isArray(p.permissions) && p.permissions.length > 0) {
      var perms = el("ul", {
        class: "permission-list",
        attrs: { role: "list", "aria-label": "permission list" },
      });
      for (var k = 0; k < p.permissions.length; k++) {
        var pr = p.permissions[k];
        var liClass = "permission-row";
        if (pr.denied) liClass += " is-denied";
        var li = el("li", { class: liClass, attrs: { role: "listitem" } });
        li.appendChild(el("span", {
          class: "perm-dot",
          attrs: { "aria-hidden": "true" },
          text: "•",
        }));
        li.appendChild(el("span", {
          class: "perm-requested",
          text: String(pr.requested || ""),
        }));
        if (pr.narrowed && pr.effective && pr.effective !== pr.requested) {
          li.appendChild(el("span", {
            class: "perm-arrow",
            attrs: { "aria-label": "narrowed to" },
            text: "→",
          }));
          li.appendChild(el("span", {
            class: "perm-effective",
            attrs: { "aria-label": "effective scope" },
            text: String(pr.effective),
          }));
        }
        perms.appendChild(li);
      }
      card.appendChild(perms);
    }

    // last-used line
    if (p.last_used) {
      var lu = el("div", { class: "plugin-last-used" });
      lu.appendChild(el("span", { class: "last-used-label", text: "last used " }));
      lu.appendChild(el("span", { class: "last-used-value", text: String(p.last_used) }));
      card.appendChild(lu);
    }

    // actions row
    if (Array.isArray(p.actions) && p.actions.length > 0) {
      var footer = el("footer", { class: "plugin-actions", attrs: { "aria-label": "plugin actions" } });
      for (var a = 0; a < p.actions.length; a++) {
        var action = p.actions[a];
        var label = actionLabel(action);
        var btn = el("button", {
          class: "btn btn-ghost",
          attrs: {
            type: "button",
            "aria-label": label + " " + String(p.name || ""),
            "data-plugin": String(p.name || ""),
            "data-action": String(action),
          },
          text: label,
        });
        // T4 wires the click to the bridge by shape; the ops land in T5 and
        // will 404 until then — by design (the registry rejects unknown ops).
        btn.addEventListener("click", panelActionDispatcher(p.name, action));
        footer.appendChild(btn);
      }
      card.appendChild(footer);
    }

    return card;
  }

  // Op-name registry for plugin-card actions. Names mirror the T5 plan
  // (docs/plans/2026-05-27-phase3-approval-flow.md) so the chrome → bridge
  // calls land straight through once T5 registers the handlers; until then
  // the registry returns 404 for each (the documented T4 behaviour).
  //
  // `Settings` is intentionally NOT mapped: it has no entry in the T5 plan;
  // emitting a fabricated op name would lock T5 into an unreviewed contract.
  // The handler logs and is a no-op — Settings is out of scope for Phase 3.
  function panelActionDispatcher(plugin, action) {
    var op = (function () {
      switch (action) {
        case "AdjustScope": return "plugin_adjust_scope";
        case "Revoke": return "plugin_revoke";
        case "Update": return "plugin_update";
        case "Rollback": return "plugin_rollback";
        case "Reload": return "plugin_reload";
        case "Settings": return null; // out of scope for Phase 3
        default: return null;
      }
    })();
    return function () {
      if (action === "Settings") {
        // eslint-disable-next-line no-console
        console.warn("settings op not yet implemented (out of scope for Phase 3)");
        return;
      }
      if (!op || !window.mote || !window.mote.invoke) return;
      window.mote.invoke(op, { plugin: String(plugin) }).catch(function () {});
    };
  }

  function buildAuditSummary(rows) {
    var box = el("div", {
      class: "audit-summary",
      attrs: { role: "table", "aria-label": "network audit summary" },
    });
    if (!rows || rows.length === 0) {
      box.appendChild(el("div", {
        class: "denial-empty",
        attrs: { role: "row" },
        text: "no audited activity yet",
      }));
      return box;
    }
    for (var i = 0; i < rows.length; i++) {
      var r = rows[i];
      var row = el("div", { class: "audit-row", attrs: { role: "row" } });
      row.appendChild(el("span", {
        class: "audit-actor",
        attrs: { role: "cell" },
        text: String(r.actor || ""),
      }));
      row.appendChild(el("span", {
        class: "audit-count",
        attrs: { role: "cell" },
        text: String(r.count != null ? r.count : 0),
      }));
      var decision = String(r.decision || "").toLowerCase();
      row.appendChild(el("span", {
        class: "audit-decision " + decision,
        attrs: { role: "cell" },
        text: decision,
      }));
      if (r.detail) {
        row.appendChild(el("span", {
          class: "audit-detail",
          attrs: { role: "cell" },
          text: String(r.detail),
        }));
      }
      box.appendChild(row);
    }
    return box;
  }

  function buildStorageSummary(rows) {
    var box = el("div", {
      class: "storage-summary",
      attrs: { role: "table", "aria-label": "plugin storage usage" },
    });
    if (!rows || rows.length === 0) {
      box.appendChild(el("div", {
        class: "denial-empty",
        attrs: { role: "row" },
        text: "no plugin storage in use",
      }));
      return box;
    }
    // Peak bytes for the relative bar (avoids absolute scale skew).
    var peak = 1;
    for (var i = 0; i < rows.length; i++) {
      var sb = Number(rows[i].size_bytes || 0);
      if (sb > peak) peak = sb;
    }
    for (var j = 0; j < rows.length; j++) {
      var r = rows[j];
      var row = el("div", { class: "storage-row", attrs: { role: "row" } });
      row.appendChild(el("span", {
        class: "storage-plugin",
        attrs: { role: "cell" },
        text: String(r.plugin || ""),
      }));
      row.appendChild(el("span", {
        class: "storage-size",
        attrs: { role: "cell" },
        text: String(r.size_human || ""),
      }));
      var barWrap = el("div", {
        class: "storage-bar-wrap",
        attrs: { role: "presentation", "aria-hidden": "true" },
      });
      var pct = Math.max(0.5, Math.min(100, (Number(r.size_bytes || 0) / peak) * 100));
      var bar = el("div", { class: "storage-bar" });
      // Width is data-derived but the value is *clamped numeric percentage* —
      // never a plugin-supplied string. setAttribute on an inline style is the
      // narrowest safe affordance for a layout dimension.
      bar.setAttribute("style", "width:" + pct.toFixed(2) + "%");
      barWrap.appendChild(bar);
      row.appendChild(barWrap);
      if (r.label) {
        row.appendChild(el("span", {
          class: "storage-label",
          attrs: { role: "cell" },
          text: String(r.label),
        }));
      }
      box.appendChild(row);
    }
    return box;
  }

  function buildDenialList(rows) {
    var box = el("div", {
      class: "denial-list",
      attrs: { role: "table", "aria-label": "permission denial log" },
    });
    if (!rows || rows.length === 0) {
      box.appendChild(el("div", {
        class: "denial-empty",
        attrs: { role: "row", "aria-label": "no denials recorded" },
        text: "none",
      }));
      return box;
    }
    for (var i = 0; i < rows.length; i++) {
      var d = rows[i];
      var row = el("div", { class: "denial-row", attrs: { role: "row" } });
      row.appendChild(el("span", { class: "denial-plugin", text: String(d.plugin || "") }));
      row.appendChild(el("span", { class: "denial-permission", text: String(d.permission || "") }));
      row.appendChild(el("span", { class: "denial-when", text: String(d.when || "") }));
      box.appendChild(row);
    }
    return box;
  }

  /**
   * Build the integrity panel DOM tree from a parsed IntegrityPanel object.
   * The returned Node can be appended into any container — the renderer below
   * mounts it at `#mote-integrity-root`.
   */
  function buildPanelDom(panel) {
    if (!panel || typeof panel !== "object") panel = { plugins: [], network_audit: [], storage: [], denials: [] };

    var root = el("div", { class: "integrity-panel", attrs: { role: "main", "aria-label": "browser integrity" } });

    // header
    var header = el("header", { class: "integrity-header" });
    var heading = el("div", { class: "heading" });
    heading.appendChild(lockup("integrity", { ariaLabel: "integrity panel" }));
    header.appendChild(heading);
    header.appendChild(el("p", {
      class: "subhead",
      text: "active plugins · network audit · storage · permission denials · press esc to close",
    }));
    root.appendChild(header);

    // section 1 — active plugins
    var sec1 = el("section", { class: "integrity-section", attrs: { "aria-label": "active plugins" } });
    var head1 = el("div", { class: "integrity-section-head" });
    head1.appendChild(sectionLockup("active plugins"));
    head1.appendChild(el("span", {
      class: "section-count",
      attrs: { "aria-label": String((panel.plugins || []).length) + " plugins" },
      text: String((panel.plugins || []).length),
    }));
    sec1.appendChild(head1);
    if (!panel.plugins || panel.plugins.length === 0) {
      sec1.appendChild(el("div", {
        class: "denial-empty",
        text: "no plugins loaded",
      }));
    } else {
      for (var i = 0; i < panel.plugins.length; i++) {
        sec1.appendChild(buildPluginCard(panel.plugins[i]));
      }
    }
    root.appendChild(sec1);

    // section 2 — network audit
    var sec2 = el("section", { class: "integrity-section", attrs: { "aria-label": "network audit log" } });
    var head2 = el("div", { class: "integrity-section-head" });
    head2.appendChild(sectionLockup("network audit log"));
    head2.appendChild(el("span", {
      class: "section-count",
      attrs: { "aria-label": "last 24 hours" },
      text: "last 24 h",
    }));
    sec2.appendChild(head2);
    sec2.appendChild(buildAuditSummary(panel.network_audit));
    root.appendChild(sec2);

    // section 3 — storage
    var sec3 = el("section", { class: "integrity-section", attrs: { "aria-label": "storage audit" } });
    var head3 = el("div", { class: "integrity-section-head" });
    head3.appendChild(sectionLockup("storage audit"));
    sec3.appendChild(head3);
    sec3.appendChild(buildStorageSummary(panel.storage));
    root.appendChild(sec3);

    // section 4 — permission denials
    var sec4 = el("section", { class: "integrity-section", attrs: { "aria-label": "permission denials" } });
    var head4 = el("div", { class: "integrity-section-head" });
    head4.appendChild(sectionLockup("permission denials"));
    head4.appendChild(el("span", {
      class: "section-count",
      attrs: { "aria-label": "last 7 days" },
      text: "last 7 d",
    }));
    sec4.appendChild(head4);
    sec4.appendChild(buildDenialList(panel.denials));
    root.appendChild(sec4);

    return root;
  }

  function ensureIntegrityRoot() {
    var root = document.getElementById("mote-integrity-root");
    if (!root) {
      root = document.createElement("div");
      root.id = "mote-integrity-root";
      root.hidden = true;
      document.body.appendChild(root);
    }
    return root;
  }

  function renderIntegrityPanel(data) {
    var root = ensureIntegrityRoot();
    clear(root);
    root.appendChild(buildPanelDom(data));
    root.hidden = false;
  }

  function hideIntegrityPanel() {
    var root = document.getElementById("mote-integrity-root");
    if (root) {
      root.hidden = true;
      clear(root);
    }
  }

  // ──────────────────────────────────────────────────────────────────────────
  // approval dialog — structured DOM from ApprovalRequest JSON
  // ──────────────────────────────────────────────────────────────────────────

  // NarrowMode serializes as either `"GrantFull"`, `{GrantOrigins: [string]}`,
  // or `"Deny"`.
  function narrowKind(mode) {
    if (typeof mode === "string") {
      if (mode === "GrantFull") return "full";
      if (mode === "Deny") return "deny";
      return "full";
    }
    if (mode && typeof mode === "object" && "GrantOrigins" in mode) return "origins";
    return "full";
  }
  function narrowOrigins(mode) {
    if (mode && typeof mode === "object" && "GrantOrigins" in mode) {
      var arr = mode.GrantOrigins;
      return Array.isArray(arr) ? arr.slice() : [];
    }
    return [];
  }

  function buildOriginEditor(idx, origins, container) {
    // origins editor: input fields + remove button + "add another" button.
    // origins is mutated in place (the live state for the Approve button).
    //
    // Origin validation (format check, glob syntax, max length, max count)
    // happens at the approve_plugin op boundary in Task 5, not here. The
    // chrome must NOT silently filter user input — the bridge sees exactly
    // what the user typed and the Rust validator owns the rejection policy.
    var editor = el("div", {
      class: "origin-editor",
      attrs: { id: "origin-editor-" + idx, "aria-label": "origin patterns" },
    });
    var list = el("div", { class: "origin-list", attrs: { role: "list", "aria-label": "allowed origins" } });

    function refresh() {
      clear(list);
      for (var i = 0; i < origins.length; i++) {
        (function (i) {
          var entry = el("div", { class: "origin-entry", attrs: { role: "listitem" } });
          var input = el("input", {
            attrs: {
              type: "text",
              "aria-label": "origin pattern " + (i + 1),
              placeholder: "https://example.com/*",
              value: String(origins[i] || ""),
            },
          });
          // Setting `.value` on a freshly-created input doesn't always stick
          // before insertion; set after creation as well, and on input event
          // update the underlying state.
          input.value = String(origins[i] || "");
          input.addEventListener("input", function (ev) {
            origins[i] = ev.target.value;
          });
          var remove = el("button", {
            class: "origin-remove",
            attrs: { type: "button", "aria-label": "remove origin " + (i + 1) },
            text: "×",
          });
          remove.addEventListener("click", function () {
            origins.splice(i, 1);
            refresh();
          });
          entry.appendChild(input);
          entry.appendChild(remove);
          list.appendChild(entry);
        })(i);
      }
    }
    refresh();

    var add = el("button", {
      class: "origin-add",
      attrs: { type: "button", "aria-label": "add another origin pattern" },
      text: "+ add another origin",
    });
    add.addEventListener("click", function () {
      origins.push("");
      refresh();
    });

    editor.appendChild(list);
    editor.appendChild(add);
    container.appendChild(editor);
    return { refresh: refresh, hide: function () { editor.hidden = true; }, show: function () { editor.hidden = false; } };
  }

  /**
   * Build one narrow-mode radio (full / origins / deny) for a permission row.
   *
   * Hoisted out of `buildDialogDom`'s `forEach` so the read of the origin
   * editor handle happens through an explicit ref object (`originHandleRef`),
   * not an implicit closure over a `var` declared earlier in the same scope.
   * The previous shape worked only because event listeners fire asynchronously
   * — fragile under refactors. This shape makes the data flow obvious.
   *
   * `state` is the parallel `perState[idx]` slot the radio mutates.
   * `originHandleRef.value` is set AFTER `buildOriginEditor` returns; the
   * change-handler reads it at fire-time, so the assignment order is robust.
   */
  function makeRadio(radioName, state, originHandleRef, value, label, hint, denyClass) {
    var optClass = "narrow-mode-option" + (denyClass ? " is-deny" : "");
    var opt = el("label", { class: optClass });
    var input = el("input", {
      class: "narrow-mode-radio",
      attrs: {
        type: "radio",
        name: radioName,
        value: value,
        "aria-label": label,
      },
    });
    if (state.mode === value) {
      input.setAttribute("checked", "checked");
      input.checked = true;
    }
    input.addEventListener("change", function () {
      if (!input.checked) return;
      state.mode = value;
      var handle = originHandleRef.value;
      if (handle) {
        if (value === "origins") handle.show();
        else handle.hide();
      }
    });
    var labelWrap = el("span", { class: "narrow-mode-label" });
    labelWrap.appendChild(document.createTextNode(label));
    if (hint) {
      labelWrap.appendChild(el("span", { class: "mode-hint", text: hint }));
    }
    opt.appendChild(input);
    opt.appendChild(labelWrap);
    return opt;
  }

  /**
   * Build the approval-dialog DOM tree from a parsed ApprovalRequest object.
   * Returns `{ node, getDecision }` where `getDecision()` reads the per-
   * permission radio state + origin editor and returns the structured payload
   * the `approve_plugin` op will accept once T5 lands it.
   */
  function buildDialogDom(req) {
    if (!req || typeof req !== "object") req = { plugin: "", version: "", source: "", permissions: [], dangerous_combinations: [], is_update: false, new_permissions: [] };

    // perState: parallel array of {mode: 'full'|'origins'|'deny', origins: [string]}
    var perState = (req.permissions || []).map(function (p) {
      return { mode: narrowKind(p.mode), origins: narrowOrigins(p.mode) };
    });
    var newSet = {};
    if (Array.isArray(req.new_permissions)) {
      for (var n = 0; n < req.new_permissions.length; n++) newSet[String(req.new_permissions[n])] = true;
    }

    // Backdrop wraps the floating surface. The chrome host pre-creates
    // #mote-approval-root with position:fixed; the backdrop fills it.
    var backdrop = el("div", {
      class: "approval-backdrop",
      attrs: {
        role: "dialog",
        "aria-modal": "true",
        "aria-labelledby": "approval-dialog-title",
      },
    });

    var dialog = el("div", { class: "approval-dialog" });

    // ── header ──
    var header = el("header", { class: "approval-header" });
    var top = el("div", { class: "approval-header-top" });
    top.appendChild(el("h2", {
      class: "approval-plugin-name",
      attrs: { id: "approval-dialog-title" },
      text: String(req.plugin || ""),
    }));
    top.appendChild(el("span", {
      class: "badge",
      attrs: { "aria-label": req.is_update ? "update" : "install" },
      text: req.is_update ? "update" : "install",
    }));
    header.appendChild(top);
    var meta = el("div", { class: "approval-header-meta" });
    meta.appendChild(el("span", {
      class: "approval-source",
      text: String(req.source || ""),
    }));
    meta.appendChild(el("span", { class: "badge", text: "v" + String(req.version || "") }));
    header.appendChild(meta);
    dialog.appendChild(header);

    // ── body ──
    var body = el("div", { class: "approval-body" });

    // dangerous combinations (above permission list)
    if (Array.isArray(req.dangerous_combinations) && req.dangerous_combinations.length > 0) {
      var dangerBox = el("div", {
        class: "danger-warning-list",
        attrs: { "aria-label": "dangerous permission combinations" },
      });
      var dangerHead = el("div", { class: "danger-warning-head" });
      // Lucide triangle-alert glyph — keep as plain unicode warning marker; the
      // SVG version lives in approval-dialog.html for reference. The CSS sizes
      // the row regardless.
      dangerHead.appendChild(el("span", {
        attrs: { "aria-hidden": "true" },
        text: "⚠",
      }));
      dangerHead.appendChild(document.createTextNode(" dangerous combinations detected"));
      dangerBox.appendChild(dangerHead);
      for (var d = 0; d < req.dangerous_combinations.length; d++) {
        dangerBox.appendChild(el("div", {
          class: "danger-warning-entry",
          attrs: { role: "alert" },
          text: String(req.dangerous_combinations[d]),
        }));
      }
      body.appendChild(dangerBox);
    }

    // new-permissions banner (update flow)
    if (req.is_update && Array.isArray(req.new_permissions) && req.new_permissions.length > 0) {
      var newBox = el("div", {
        class: "danger-warning-list",
        attrs: { "aria-label": "new permissions since last approval" },
      });
      var newHead = el("div", { class: "danger-warning-head" });
      newHead.appendChild(el("span", { attrs: { "aria-hidden": "true" }, text: "◆" }));
      newHead.appendChild(document.createTextNode(" new since last approval"));
      newBox.appendChild(newHead);
      for (var nn = 0; nn < req.new_permissions.length; nn++) {
        newBox.appendChild(el("div", {
          class: "danger-warning-entry",
          text: String(req.new_permissions[nn]),
        }));
      }
      body.appendChild(newBox);
    }

    body.appendChild(el("div", { class: "permissions-head", text: "permissions requested" }));

    var permsList = el("div", {
      class: "approval-permissions",
      attrs: { role: "list", "aria-label": "permissions to approve" },
    });

    (req.permissions || []).forEach(function (perm, idx) {
      var classes = "perm-entry";
      if (perm.high_risk) classes += " is-high-risk";
      if (newSet[String(perm.domain)]) classes += " is-new";
      var entry = el("div", {
        class: classes,
        attrs: { role: "listitem", "aria-label": String(perm.domain || "") },
      });

      var entryHead = el("div", { class: "perm-entry-head" });
      entryHead.appendChild(el("span", {
        class: "perm-domain",
        text: String(perm.domain || ""),
      }));
      if (perm.requested_scope) {
        entryHead.appendChild(el("span", {
          class: "perm-scope-tag",
          text: "requested: " + String(perm.requested_scope),
        }));
      }
      if (perm.high_risk) {
        var riskWrap = el("span", { class: "perm-risk-badge" });
        riskWrap.appendChild(el("span", { class: "badge danger", text: "high risk" }));
        entryHead.appendChild(riskWrap);
      }
      if (newSet[String(perm.domain)]) {
        entryHead.appendChild(el("span", { class: "perm-new-marker", text: "new" }));
      }
      entry.appendChild(entryHead);

      entry.appendChild(el("p", {
        class: "perm-desc",
        text: String(perm.description || ""),
      }));

      var radioName = "perm-" + idx;
      var modes = el("div", {
        class: "narrow-modes",
        attrs: { role: "radiogroup", "aria-label": "grant mode for " + String(perm.domain || "") },
      });

      // `originHandleRef.value` is populated AFTER `buildOriginEditor` runs
      // (below). The radio change-handler reads from the ref each time it
      // fires, removing the implicit "buildOriginEditor must run before any
      // change event" ordering constraint that an unboxed `var originHandle`
      // would impose.
      var originHandleRef = { value: null };
      var state = perState[idx];

      if (perm.narrowable) {
        var hint = perm.requested_scope ? "any (" + String(perm.requested_scope) + ")" : null;
        modes.appendChild(makeRadio(radioName, state, originHandleRef, "full", "grant fully", hint, false));
        modes.appendChild(makeRadio(radioName, state, originHandleRef, "origins", "grant on specific origins", null, false));
        modes.appendChild(makeRadio(radioName, state, originHandleRef, "deny", "deny", null, true));
      } else {
        modes.appendChild(makeRadio(radioName, state, originHandleRef, "full", "grant fully", null, false));
        modes.appendChild(makeRadio(radioName, state, originHandleRef, "deny", "deny", null, true));
      }
      entry.appendChild(modes);

      // The origin editor renders only for narrowable permissions; it
      // mutates perState[idx].origins in place. Populating the ref AFTER
      // construction is safe — the radio change-handlers read from the ref
      // at fire-time, not at construction-time.
      if (perm.narrowable) {
        originHandleRef.value = buildOriginEditor(idx, perState[idx].origins, entry);
        if (perState[idx].mode !== "origins") originHandleRef.value.hide();
      }

      permsList.appendChild(entry);
    });
    body.appendChild(permsList);
    dialog.appendChild(body);

    // ── footer ──
    var footer = el("footer", { class: "approval-footer" });
    var hint = el("div", { class: "kbd-hint", attrs: { "aria-label": "keyboard shortcuts" } });
    hint.appendChild(el("kbd", { text: "⎋" }));
    hint.appendChild(document.createTextNode(" decline "));
    hint.appendChild(document.createTextNode(" · "));
    hint.appendChild(el("kbd", { text: "⏎" }));
    hint.appendChild(document.createTextNode(" approve"));
    footer.appendChild(hint);

    var declineBtn = el("button", {
      class: "btn btn-ghost",
      attrs: { type: "button", "aria-label": "decline installation" },
      text: "decline",
    });
    var approveBtn = el("button", {
      class: "btn btn-primary",
      attrs: { type: "button", "aria-label": "approve plugin with selected permissions" },
      text: req.is_update ? "approve update" : "install plugin",
    });

    function getDecision(kind) {
      var perms = (req.permissions || []).map(function (perm, i) {
        var state = perState[i];
        var entry = {
          domain: String(perm.domain || ""),
          // The op uses "action" / "mode" to disambiguate from existing
          // grants in the same permission domain (T5 spec).
          action: String(perm.requested_scope || ""),
          mode: state.mode === "origins" ? "origins" : (state.mode === "deny" ? "deny" : "full"),
        };
        if (state.mode === "origins") {
          entry.origins = state.origins.filter(function (o) { return String(o || "").trim() !== ""; });
        }
        return entry;
      });
      return {
        plugin: String(req.plugin || ""),
        decision: kind,
        permissions: kind === "deny" ? [] : perms,
      };
    }

    declineBtn.addEventListener("click", function () {
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("approve_plugin", getDecision("deny")).catch(function () {});
      }
      hideApprovalDialog();
    });
    approveBtn.addEventListener("click", function () {
      if (window.mote && window.mote.invoke) {
        window.mote.invoke("approve_plugin", getDecision("grant")).catch(function () {});
      }
      hideApprovalDialog();
    });
    footer.appendChild(declineBtn);
    footer.appendChild(approveBtn);
    dialog.appendChild(footer);

    backdrop.appendChild(dialog);
    return { node: backdrop, getDecision: getDecision };
  }

  function ensureApprovalRoot() {
    var root = document.getElementById("mote-approval-root");
    if (!root) {
      root = document.createElement("div");
      root.id = "mote-approval-root";
      root.hidden = true;
      document.body.appendChild(root);
    }
    return root;
  }

  function showApprovalDialog(data) {
    var root = ensureApprovalRoot();
    clear(root);
    var built = buildDialogDom(data);
    root.appendChild(built.node);
    root.hidden = false;
  }

  function hideApprovalDialog() {
    var root = document.getElementById("mote-approval-root");
    if (root) {
      root.hidden = true;
      clear(root);
    }
  }

  // ──────────────────────────────────────────────────────────────────────────
  // wire into window.mote.applyOp — preserves existing tab/url ops by chaining
  // ──────────────────────────────────────────────────────────────────────────

  window.mote = window.mote || {};
  // Expose pure builders too (the boundary tests pin on these names if a JS
  // test harness ever lands; today the discipline grep-checks the source).
  window.mote.buildPanelDom = buildPanelDom;
  window.mote.buildDialogDom = buildDialogDom;
  window.mote.renderIntegrityPanel = renderIntegrityPanel;
  window.mote.hideIntegrityPanel = hideIntegrityPanel;
  window.mote.showApprovalDialog = showApprovalDialog;
  window.mote.hideApprovalDialog = hideApprovalDialog;

  // Compose with host.js's applyOp: host.js installs its own switch first;
  // we wrap it so this module's ops fall through to the existing handler.
  var prevApplyOp = typeof window.mote.applyOp === "function" ? window.mote.applyOp : null;
  window.mote.applyOp = function (op, payload) {
    switch (op) {
      case "render_integrity_panel":
        renderIntegrityPanel(payload);
        return;
      case "hide_integrity_panel":
        hideIntegrityPanel();
        return;
      case "show_approval_dialog":
        showApprovalDialog(payload);
        return;
      case "hide_approval_dialog":
        hideApprovalDialog();
        return;
      default:
        if (prevApplyOp) prevApplyOp(op, payload);
    }
  };
})();
