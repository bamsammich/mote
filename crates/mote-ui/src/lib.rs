//! Chrome rendering model for Mote — slots, elements, themes, and the host seam.
//!
//! This crate is the **frontend** of Mote's chrome: the materialized design
//! system (`chrome/` — token + component CSS and the slot-grid HTML scaffold)
//! and the UI-independent Rust model the shell and plugins program against:
//!
//! - [`Slot`] — the fixed v0.1 layout regions the runtime owns.
//! - [`ElementKind`] / [`Element`] / [`ElementRef`] — the eight content kinds a
//!   plugin contributes and how a theme references them.
//! - [`Layout`] — element-to-slot placement (`default-layout` + theme layouts).
//! - [`Token`] / [`TokenValue`] / [`TokenResolver`] / [`Theme`] — the design
//!   tokens and the per-theme resolver, the CSS-var ↔ Lua bridge of `spec/03`.
//! - [`UiHost`] / [`Node`] — the host API a plugin's `render(host)` targets and
//!   the seam `mote-shell` drives, defined with **no** GPU/window/CEF deps.
//! - [`Compositor`] — the thin wgpu compositor (ADR-0003, plan §1.1): blits the
//!   focused page's OSR texture into the viewport rect, then the chrome texture
//!   over the full window (chrome-surrounds-content). Decoupled from CEF — it
//!   takes raw frame buffers ([`Compositor::update_chrome`] /
//!   [`Compositor::update_page`]), so this crate has **no** `mote-cef`
//!   dependency. The slot/element/theme model above stays
//!   rendering-backend-independent.
//!
//! ## Chrome assets
//!
//! The materialized stylesheet and scaffold live under `chrome/` and are
//! exposed as `&'static str` constants ([`TOKENS_CSS`], [`CHROME_HTML`], …) so
//! the shell can bundle them into the binary and serve them to the chrome CEF
//! browser. Both themes are covered token-only in [`TOKENS_CSS`].

mod compositor;
mod element;
mod host;
pub mod icon_registry;
pub mod integrity;
mod layout;
mod slot;
mod token;

pub use compositor::{Compositor, CompositorError, PixelFormat, ViewportRect};
pub use element::{Element, ElementKind, ElementRef, RefSelector};
pub use host::{Node, UiHost};
pub use icon_registry::{IconRegistry, LucideIcon, SetIconError};
pub use integrity::{
    ApprovalRequest, AuditDecision, AuditRow, DenialRow, IntegrityDetailPayload,
    IntegrityListPayload, IntegrityListRow, IntegrityPanel, IntegrityStatus, NarrowMode,
    NarrowablePermission, OpSummary, PermissionRow, PluginAction, PluginKind, PluginRow,
    SecretAccessRow, StorageRow,
};
pub use layout::Layout;
pub use slot::{Edge, Slot};
pub use token::{
    CANONICAL_TOKENS, MAX_RADIUS_PX, OverrideError, Theme, Token, TokenResolver, TokenValue,
};

/// The canonical design-token stylesheet (`:root` dusk + `[data-theme="vellum"]`).
pub const TOKENS_CSS: &str = include_str!("../chrome/tokens.css");

/// The global reset, slot grid, `[mote]` lockup, and system animations.
pub const BASE_CSS: &str = include_str!("../chrome/base.css");

/// The chrome document: the `[data-slot]` slot-grid scaffold (default layout).
pub const CHROME_HTML: &str = include_str!("../chrome/chrome.html");

/// Canonical bridge bootstrap (ADR-0005).
///
/// Wraps `window.cefQuery` into `window.mote.invoke`. Shared by `chrome.html`
/// and all privileged `mote://chrome` pages (settings sections). Must be loaded
/// BEFORE `host.js` on the chrome page and BEFORE `settings.js` on every
/// settings page.
pub const MOTE_BRIDGE_JS: &str = include_str!("../chrome/mote-bridge.js");

/// Shared roving-focus helper (CL-KBNAV).
///
/// Pure selection nav-math plus a dual-mode (`activedescendant` / `roving`) DOM
/// attach factory. Loaded BEFORE `host.js` so `window.mote.roving` exists when
/// the omnibox wires its completion dropdown; reused by the chrome's floating
/// surfaces.
pub const ROVING_JS: &str = include_str!("../chrome/roving.js");

/// The privileged chrome bootstrap JS: wraps `window.cefQuery` into the
/// structured `window.mote.invoke` API and wires the omnibox `navigate` op.
pub const HOST_JS: &str = include_str!("../chrome/host.js");

/// Chrome-side structured-DOM renderers (panel + dialog).
///
/// Loaded after `host.js`; wraps `applyOp` so its `render_integrity_panel` /
/// `show_approval_dialog` ops are handled here and the existing `set_tabs` /
/// `set_url` ops fall through to the base handler.
///
/// ADR-0005: this file MUST NEVER assign `innerHTML` / `outerHTML` /
/// `insertAdjacentHTML` from any plugin-derived string. The boundary test in
/// `tests::panels_js_never_uses_innerhtml` grep-asserts that.
pub const PANELS_JS: &str = include_str!("../chrome/panels.js");

/// The integrity panel HTML (About → Browser Integrity), pre-rendered with
/// sample data. The shell replaces the sample content with live data at runtime
/// via the host bridge.
pub const INTEGRITY_PANEL_HTML: &str = include_str!("../chrome/integrity-panel.html");

/// The permission-approval dialog HTML, pre-rendered with sample data. The
/// shell replaces the sample content with the real [`ApprovalRequest`] at
/// runtime via the host bridge.
pub const APPROVAL_DIALOG_HTML: &str = include_str!("../chrome/approval-dialog.html");

/// Per-component stylesheets, paired `(name, css)`, in load order.
pub const COMPONENT_CSS: &[(&str, &str)] = &[
    ("kbd", include_str!("../chrome/components/kbd.css")),
    ("button", include_str!("../chrome/components/button.css")),
    ("field", include_str!("../chrome/components/field.css")),
    ("card", include_str!("../chrome/components/card.css")),
    ("badge", include_str!("../chrome/components/badge.css")),
    ("omnibox", include_str!("../chrome/components/omnibox.css")),
    ("tabs", include_str!("../chrome/components/tabs.css")),
    (
        "status-line",
        include_str!("../chrome/components/status-line.css"),
    ),
    ("palette", include_str!("../chrome/components/palette.css")),
    ("sidebar", include_str!("../chrome/components/sidebar.css")),
    (
        "empty-slot",
        include_str!("../chrome/components/empty-slot.css"),
    ),
    (
        "integrity-panel",
        include_str!("../chrome/components/integrity-panel.css"),
    ),
    (
        "approval-dialog",
        include_str!("../chrome/components/approval-dialog.css"),
    ),
    // P1: tooltip primitive (Group B).
    ("tooltip", include_str!("../chrome/components/tooltip.css")),
];

/// Shared settings panel stylesheet — loaded by each settings section page.
///
/// Covers `.settings-root`, `.settings-header`, `.settings-tabs`,
/// `.settings-body`, the plugin list, integrity table, and keybinds table.
/// Token-only; no raw hex.
pub const SETTINGS_CSS: &str = include_str!("../chrome/settings/settings.css");

/// Shared settings panel JavaScript bootstrap — loaded by each settings page.
///
/// Wires tab navigation, section-specific controls, and the `keybinds_list`
/// bridge op. ADR-0005: never assigns innerHTML; uses textContent/createElement.
pub const SETTINGS_JS: &str = include_str!("../chrome/settings/settings.js");

/// Settings → General section HTML (theme, search engine, hw-accel, zoom-persist).
pub const SETTINGS_GENERAL_HTML: &str = include_str!("../chrome/settings/general.html");

/// Settings → Plugins section HTML (plugin list, capability chips, install button).
pub const SETTINGS_PLUGINS_HTML: &str = include_str!("../chrome/settings/plugins.html");

/// Settings → Integrity section HTML (sortable plugin table, filter, reverify).
pub const SETTINGS_INTEGRITY_HTML: &str = include_str!("../chrome/settings/integrity.html");

/// Settings → Keybinds section HTML (read-only chord reference, grouped by scope).
pub const SETTINGS_KEYBINDS_HTML: &str = include_str!("../chrome/settings/keybinds.html");

/// The `mote://newtab` page (ADR-0015).
///
/// Served from `mote://chrome/newtab.html` via the global CEF request context.
/// Declares a single `newtab.center` slot for future theme and plugin bindings;
/// the default theme leaves it empty.
pub const NEWTAB_HTML: &str = include_str!("../chrome/newtab.html");

/// The `[mote]` wordmark SVG.
pub const WORDMARK_SVG: &str = include_str!("../chrome/assets/wordmark.svg");

/// The `[·]` mark / favicon SVG.
pub const MARK_SVG: &str = include_str!("../chrome/assets/mark.svg");

/// The bundled Lucide icon sprite (ADR-0013). All chrome icons route through
/// this sprite set; no external fetches, no string interpolation.
pub const LUCIDE_SPRITE_SVG: &str = include_str!("../chrome/assets/lucide-sprite.svg");

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Slot registry ----

    #[test]
    fn all_six_v01_slots_present_and_named() {
        let names: Vec<_> = Slot::ALL.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [
                "top-bar",
                "left-sidebar",
                "right-sidebar",
                "bottom-bar",
                "urlbar-inline",
                "tab-row",
            ]
        );
    }

    #[test]
    fn slot_round_trips_through_name() {
        for slot in Slot::ALL {
            assert_eq!(Slot::from_name(slot.name()), Some(slot));
        }
        assert_eq!(Slot::from_name("not-a-slot"), None);
    }

    #[test]
    fn nested_slots_report_nested_edge() {
        assert_eq!(Slot::UrlbarInline.position(), Edge::Nested);
        assert_eq!(Slot::TabRow.position(), Edge::Nested);
        assert_eq!(Slot::TopBar.position(), Edge::Top);
        assert!(Slot::LeftSidebar.is_resizable());
        assert!(!Slot::TopBar.is_resizable());
    }

    // ---- Element-kind registry ----

    #[test]
    fn all_eight_v01_kinds_present_and_named() {
        let names: Vec<_> = ElementKind::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(
            names,
            [
                "urlbar",
                "tabstrip",
                "bookmarks-bar",
                "sidebar-panel",
                "action-button",
                "status-indicator",
                "urlbar-extension",
                "widget",
            ]
        );
    }

    #[test]
    fn kind_round_trips_and_singletons() {
        for kind in ElementKind::ALL {
            assert_eq!(ElementKind::from_name(kind.name()), Some(kind));
        }
        assert!(ElementKind::Urlbar.is_singleton());
        assert!(ElementKind::Tabstrip.is_singleton());
        assert!(!ElementKind::SidebarPanel.is_singleton());
    }

    #[test]
    fn element_ref_parses_all_three_forms() {
        let any = ElementRef::parse("tabstrip").unwrap();
        assert_eq!(any.kind(), ElementKind::Tabstrip);
        assert_eq!(any.selector(), &RefSelector::Any);

        let id = ElementRef::parse("sidebar-panel:bookmarks").unwrap();
        assert_eq!(id.kind(), ElementKind::SidebarPanel);
        assert!(id.matches(ElementKind::SidebarPanel, "bookmarks"));
        assert!(!id.matches(ElementKind::SidebarPanel, "tabs"));

        let wild = ElementRef::parse("status-indicator:*").unwrap();
        assert!(wild.is_wildcard());
        assert!(wild.matches(ElementKind::StatusIndicator, "anything"));

        assert_eq!(ElementRef::parse("bogus-kind:x"), None);
    }

    #[test]
    fn element_builder_sets_title() {
        let el = Element::new("git", ElementKind::StatusIndicator).with_title("git status");
        assert_eq!(el.kind, ElementKind::StatusIndicator);
        assert_eq!(el.title.as_deref(), Some("git status"));
    }

    // ---- Layout / default-layout ----

    #[test]
    fn default_layout_matches_plan_section_5_2() {
        let layout = Layout::default_layout();

        // right-sidebar is declared-but-empty → empty-slot motif.
        assert!(layout.is_empty_slot(Slot::RightSidebar));

        // top-bar carries the urlbar (maintainer decision: tabstrip moved to
        // the left-sidebar; the top-bar keeps the omnibox).
        assert!(layout.binds(Slot::TopBar, ElementKind::Urlbar, "core"));
        assert!(!layout.binds(Slot::TopBar, ElementKind::Tabstrip, "core"));

        // the tabstrip now lives in the left-sidebar, alongside sidebar panels.
        assert!(layout.binds(Slot::LeftSidebar, ElementKind::Tabstrip, "core"));

        // a wildcard slot binds arbitrary plugin elements of the kind.
        assert!(layout.binds(Slot::LeftSidebar, ElementKind::SidebarPanel, "git"));
        assert!(layout.binds(Slot::BottomBar, ElementKind::StatusIndicator, "git-status"));

        // wrong kind does not bind to a slot.
        assert!(!layout.binds(Slot::BottomBar, ElementKind::Widget, "x"));
    }

    #[test]
    fn explicit_id_wins_and_unmentioned_slot_inherits() {
        let mut layout = Layout::new();
        layout.place(
            Slot::LeftSidebar,
            vec![
                ElementRef::parse("sidebar-panel:tabs").unwrap(),
                ElementRef::parse("sidebar-panel:*").unwrap(),
            ],
        );
        assert!(layout.binds(Slot::LeftSidebar, ElementKind::SidebarPanel, "tabs"));
        assert!(layout.binds(Slot::LeftSidebar, ElementKind::SidebarPanel, "history"));

        // a slot never mentioned returns None (would inherit from default).
        assert_eq!(layout.slot(Slot::RightSidebar), None);
        assert!(!layout.is_empty_slot(Slot::RightSidebar));
    }

    // ---- Token resolution in BOTH themes ----

    #[test]
    fn dusk_and_vellum_resolve_distinct_semantic_colors() {
        let dusk = TokenResolver::new(Theme::Dusk);
        let vellum = TokenResolver::new(Theme::Vellum);

        assert_eq!(dusk.theme(), Theme::Dusk);
        assert_eq!(vellum.theme(), Theme::Vellum);

        // bg differs between themes (warm-ink vs warm-paper).
        assert_eq!(dusk.css_value("bg").as_deref(), Some("#14110f"));
        assert_eq!(vellum.css_value("bg").as_deref(), Some("#f4efe6"));

        // accent: dusk amber, vellum amber-deep.
        assert_eq!(dusk.css_value("accent").as_deref(), Some("#e0a458"));
        assert_eq!(vellum.css_value("accent").as_deref(), Some("#b47c36"));
    }

    #[test]
    fn shared_tokens_inherit_dusk_value_in_vellum() {
        let dusk = TokenResolver::new(Theme::Dusk);
        let vellum = TokenResolver::new(Theme::Vellum);
        // spacing/radius/motion are theme-independent.
        assert_eq!(dusk.css_value("space-4"), vellum.css_value("space-4"));
        assert_eq!(dusk.css_value("radius-3").as_deref(), Some("6px"));
        assert_eq!(vellum.css_value("radius-3").as_deref(), Some("6px"));
    }

    #[test]
    fn token_resolves_through_all_three_spellings() {
        let r = TokenResolver::new(Theme::Dusk);
        let css = r.css_value("--surface-1");
        let bare = r.css_value("surface-1");
        let lua = r.css_value("surface_1");
        assert_eq!(css, bare);
        assert_eq!(bare, lua);
        assert_eq!(css.as_deref(), Some("#1c1815"));
    }

    #[test]
    fn css_lua_bridge_maps_names() {
        let token = Token::new("surface-1");
        assert_eq!(token.css_name(), "--surface-1");
        assert_eq!(token.lua_name(), "surface_1");
        assert_eq!(token.bare(), "surface-1");
    }

    #[test]
    fn dimension_tokens_surface_as_numbers_in_lua() {
        let r = TokenResolver::new(Theme::Dusk);
        // CSS form carries px; Lua form is the bare number (spec/03).
        assert_eq!(r.css_value("space-4").as_deref(), Some("16px"));
        assert_eq!(r.lua_value("space-4").as_deref(), Some("16"));
        assert_eq!(r.lua_value("radius-2").as_deref(), Some("4"));
        // colors are strings in both faces.
        assert_eq!(r.lua_value("accent").as_deref(), Some("#e0a458"));
    }

    #[test]
    fn override_applies_and_radius_cap_is_enforced() {
        let mut r = TokenResolver::new(Theme::Dusk);

        // a user/theme override wins, last-writer-wins.
        r.set_override("accent", TokenValue::Color("#ff8800".into()))
            .unwrap();
        assert_eq!(r.css_value("accent").as_deref(), Some("#ff8800"));

        // radius cap (spec/07 "what themes can't do") enforced at set time.
        let err = r
            .set_override("radius-2", TokenValue::Dimension(12))
            .unwrap_err();
        assert!(matches!(err, OverrideError::RadiusTooLarge { px: 12, .. }));
        // at-the-cap is allowed.
        assert!(r.set_override("radius-3", TokenValue::Dimension(6)).is_ok());
        // radius-dot is exempt (pills for status dots).
        assert!(
            r.set_override("radius-dot", TokenValue::Dimension(9999))
                .is_ok()
        );

        // unknown token rejected.
        assert!(matches!(
            r.set_override("no-such-token", TokenValue::Dimension(1)),
            Err(OverrideError::UnknownToken(_))
        ));
    }

    #[test]
    fn to_css_block_emits_resolved_declarations() {
        let block = TokenResolver::new(Theme::Vellum).to_css_block();
        assert!(block.contains("--bg: #f4efe6;"));
        assert!(block.contains("--space-4: 16px;"));
        assert!(block.contains("--radius-dot: 9999px;"));
    }

    // ---- Chrome assets: both themes, token-only ----

    #[test]
    fn tokens_css_declares_both_themes() {
        assert!(TOKENS_CSS.contains(":root"));
        assert!(TOKENS_CSS.contains("[data-theme=\"vellum\"]"));
    }

    #[test]
    fn tokens_css_in_sync_with_catalog() {
        // every catalog token must appear as a declaration in the stylesheet.
        for entry in CANONICAL_TOKENS {
            let decl = format!("--{}:", entry.bare);
            assert!(
                TOKENS_CSS.contains(&decl),
                "tokens.css missing declaration for --{}",
                entry.bare
            );
        }
    }

    #[test]
    fn chrome_html_declares_all_six_slots() {
        // P1: right-sidebar is intentionally absent from the default HTML
        // (the slot still exists in the architecture model; plugins / custom
        // themes may declare it). All other slots must be present.
        let mandatory: &[Slot] = &[
            Slot::TopBar,
            Slot::LeftSidebar,
            Slot::BottomBar,
            Slot::TabRow,
        ];
        for slot in mandatory {
            let attr = format!("data-slot=\"{}\"", slot.name());
            assert!(
                CHROME_HTML.contains(&attr),
                "chrome.html missing {}",
                slot.name()
            );
        }
        // right-sidebar is explicitly absent from the P1 default theme.
        assert!(
            !CHROME_HTML.contains("data-slot=\"right-sidebar\""),
            "chrome.html must NOT render right-sidebar in the P1 default theme"
        );
        // boots in dusk; no AI surfaces.
        assert!(CHROME_HTML.contains("data-theme=\"dusk\""));
        assert!(!CHROME_HTML.contains("[ask]"));
        assert!(!CHROME_HTML.contains("aria-label=\"assistant\""));
    }

    #[test]
    fn component_css_bundle_is_complete() {
        let names: Vec<_> = COMPONENT_CSS.iter().map(|(n, _)| *n).collect();
        for want in [
            "kbd",
            "button",
            "field",
            "card",
            "badge",
            "omnibox",
            "tabs",
            "status-line",
            "palette",
            "sidebar",
            "empty-slot",
            "integrity-panel",
            "approval-dialog",
            // P1: tooltip primitive (Group B).
            "tooltip",
        ] {
            assert!(names.contains(&want), "missing component css: {want}");
        }
    }

    // ---- Integrity panel + approval dialog chrome surfaces ----

    #[test]
    fn integrity_panel_html_declares_both_themes_via_tokens() {
        // The panel HTML links tokens.css (which has both themes).
        assert!(INTEGRITY_PANEL_HTML.contains("tokens.css"));
        // Root data-theme is dusk (default boot theme).
        assert!(INTEGRITY_PANEL_HTML.contains("data-theme=\"dusk\""));
        // Both themes are covered by tokens.css — assert the link is present.
        assert!(TOKENS_CSS.contains("[data-theme=\"vellum\"]"));
    }

    #[test]
    fn integrity_panel_html_has_all_four_sections() {
        for section in [
            "active plugins",
            "network audit log",
            "storage audit",
            "permission denials",
        ] {
            assert!(
                INTEGRITY_PANEL_HTML.contains(section),
                "integrity-panel.html missing section: {section}"
            );
        }
    }

    #[test]
    fn integrity_panel_html_uses_lockup_pattern() {
        // The [integrity] lockup and section lockups must be present.
        assert!(
            INTEGRITY_PANEL_HTML.contains("[integrity]")
                || INTEGRITY_PANEL_HTML.contains("integrity-header")
        );
        // Lockup bracket spans use the class pattern from base.css.
        assert!(INTEGRITY_PANEL_HTML.contains("class=\"br\""));
        assert!(INTEGRITY_PANEL_HTML.contains("class=\"name\""));
    }

    #[test]
    fn integrity_panel_html_has_no_ai_surfaces() {
        // ADR-0002: no built-in AI surfaces.
        assert!(!INTEGRITY_PANEL_HTML.contains("[ask]"));
        assert!(!INTEGRITY_PANEL_HTML.contains("aria-label=\"assistant\""));
        assert!(!INTEGRITY_PANEL_HTML.contains("✨"));
    }

    #[test]
    fn integrity_panel_html_uses_token_only_css() {
        // The panel CSS must not contain raw hex literals.
        // (The CSS file is included in COMPONENT_CSS; check via the bundle.)
        let integrity_css = COMPONENT_CSS
            .iter()
            .find(|(n, _)| *n == "integrity-panel")
            .map(|(_, css)| *css)
            .expect("integrity-panel css missing from COMPONENT_CSS");
        // No raw hex colors — only var(--…).
        let hex_re = integrity_css
            .lines()
            .filter(|l| !l.trim_start().starts_with("/*") && !l.trim_start().starts_with('*'))
            .any(|l| {
                // Allow rgba() with numeric literals (they reference the palette
                // scale values, which are in rgba() wrappers matching the
                // existing pattern from badge.css / card.css — accepted by design).
                // Flag bare #xxxxxx hex values only.
                let without_comments = l.split("/*").next().unwrap_or(l);
                without_comments.contains('#') && !without_comments.contains("var(--")
            });
        assert!(
            !hex_re,
            "integrity-panel.css contains raw hex values outside var(--…)"
        );
    }

    #[test]
    fn approval_dialog_html_declares_danger_warnings_above_permissions() {
        // DISCIPLINES §4: dangerous combinations surfaced ABOVE per-permission list.
        let danger_pos = APPROVAL_DIALOG_HTML
            .find("danger-warning-list")
            .expect("approval-dialog.html missing danger-warning-list");
        let perms_pos = APPROVAL_DIALOG_HTML
            .find("approval-permissions")
            .expect("approval-dialog.html missing approval-permissions");
        assert!(
            danger_pos < perms_pos,
            "danger-warning-list must appear before approval-permissions in the DOM"
        );
    }

    #[test]
    fn approval_dialog_html_has_three_narrowing_modes() {
        // The dialog must render grant-fully, grant-on-origins, deny modes.
        assert!(APPROVAL_DIALOG_HTML.contains("grant fully"));
        assert!(APPROVAL_DIALOG_HTML.contains("grant on specific origins"));
        assert!(APPROVAL_DIALOG_HTML.contains("deny"));
        // Origin editor for the narrowable permission.
        assert!(APPROVAL_DIALOG_HTML.contains("origin-editor"));
        assert!(APPROVAL_DIALOG_HTML.contains("add another origin"));
    }

    #[test]
    fn approval_dialog_html_has_keycap_footer() {
        // install + decline keycap buttons.
        assert!(APPROVAL_DIALOG_HTML.contains("install plugin"));
        assert!(APPROVAL_DIALOG_HTML.contains("decline"));
        // Keyboard shortcut hints.
        assert!(APPROVAL_DIALOG_HTML.contains("<kbd>"));
    }

    #[test]
    fn approval_dialog_html_has_no_ai_surfaces() {
        assert!(!APPROVAL_DIALOG_HTML.contains("[ask]"));
        assert!(!APPROVAL_DIALOG_HTML.contains("aria-label=\"assistant\""));
    }

    #[test]
    fn approval_dialog_css_uses_shadow_2_on_dialog() {
        let css = COMPONENT_CSS
            .iter()
            .find(|(n, _)| *n == "approval-dialog")
            .map(|(_, css)| *css)
            .expect("approval-dialog css missing");
        // Floating surface must use shadow-2 (spec: shadows only on floating surfaces).
        assert!(
            css.contains("var(--shadow-2)"),
            "approval-dialog.css must use var(--shadow-2) on the floating surface"
        );
        // Inline elements must NOT have shadows (enforced by absence of
        // shadow on perm-entry).
        assert!(
            !css.contains(".perm-entry {")
                || !css
                    .split(".perm-entry {")
                    .nth(1)
                    .unwrap_or("")
                    .split('}')
                    .next()
                    .unwrap_or("")
                    .contains("box-shadow"),
            "perm-entry (inline) must not have box-shadow"
        );
    }

    #[test]
    fn integrity_and_approval_assets_are_exposed_as_constants() {
        // Both HTML surfaces must be reachable as &'static str constants.
        assert!(!INTEGRITY_PANEL_HTML.is_empty());
        assert!(!APPROVAL_DIALOG_HTML.is_empty());
    }

    // ---- ADR-0005 boundary tests: chrome JS never assigns markup ----
    //
    // The structured-DOM builders in `panels.js` are the load-bearing chrome-
    // side defence against plugin-derived strings escaping into the privileged
    // DOM as markup. These tests grep the JS source to assert no assignment to
    // `innerHTML` / `outerHTML` / `insertAdjacentHTML` exists — escaping HTML
    // strings is the REJECTED weaker mitigation (ADR-0005). The single
    // `textContent = ""` use is a tree-clear; it doesn't parse.

    /// Strip JS line/block comments and string literals from `src` so a grep
    /// over the remainder reflects *executable code* only — a comment that
    /// names `innerHTML` (as part of the rule it's enforcing) must not
    /// false-trigger the boundary check. Approximate but conservative.
    ///
    /// KNOWN LIMITATIONS — both are accepted because the companion raw-source
    /// assertions (`panels_js_no_string_concat_evasion`, ditto `host_js`)
    /// below catch the most-likely evasion shape at near-zero cost:
    ///   - **Backtick template literals are NOT stripped.** The chrome JS
    ///     intentionally avoids them (it targets ES5-ish for CEF), so a
    ///     `` `innerHTML` `` literal would survive into the grepped output
    ///     and trigger a (correct) regression. This is "stricter than
    ///     necessary by design" rather than a bug.
    ///   - **String concatenation evasion** like `"inner" + "HTML"` survives
    ///     comment+literal stripping (each fragment is a separate literal).
    ///     The raw-source assertions `panels_js_no_string_concat_evasion` /
    ///     `host_js_no_string_concat_evasion` below explicitly forbid that
    ///     shape on each JS file.
    fn js_strip_noncode(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut chars = src.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '/' if chars.peek() == Some(&'/') => {
                    // Line comment: skip until newline.
                    for next in chars.by_ref() {
                        if next == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                }
                '"' | '\'' => {
                    // Skip string literal contents (handles \\ and \").
                    let quote = c;
                    out.push(quote);
                    while let Some(next) = chars.next() {
                        out.push(next);
                        if next == '\\' {
                            if let Some(escaped) = chars.next() {
                                out.push(escaped);
                            }
                        } else if next == quote {
                            break;
                        }
                    }
                }
                other => out.push(other),
            }
        }
        out
    }

    #[test]
    fn panels_js_never_uses_innerhtml() {
        // Strip comments + string literals first so the rule's own prose
        // (which mentions `innerHTML` as the thing it forbids) doesn't
        // false-trigger. The grep then reflects executable code only.
        let code = js_strip_noncode(PANELS_JS);
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            assert!(
                !code.contains(needle),
                "panels.js executable code must not contain `{needle}` (ADR-0005)",
            );
        }
    }

    #[test]
    fn host_js_never_uses_innerhtml() {
        // host.js is the original chrome bridge bootstrap — the same rule
        // applies (it already builds tab rows via createElement/textContent).
        let code = js_strip_noncode(HOST_JS);
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            assert!(
                !code.contains(needle),
                "host.js executable code must not contain `{needle}` (ADR-0005)",
            );
        }
    }

    #[test]
    fn mote_bridge_js_never_uses_innerhtml() {
        // mote-bridge.js carries the canonical bridge bootstrap (ADR-0005);
        // the same structured-DOM discipline applies.
        let code = js_strip_noncode(MOTE_BRIDGE_JS);
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            assert!(
                !code.contains(needle),
                "mote-bridge.js executable code must not contain `{needle}` (ADR-0005)",
            );
        }
    }

    /// Raw-source check for the most-likely concat-evasion of the strip-and-
    /// grep tests above: `"inner" + "HTML"` survives comment + literal
    /// stripping because each fragment is a separate string literal. This
    /// pattern is virtually never legitimate code; banning it costs nothing.
    #[test]
    fn panels_js_no_string_concat_evasion() {
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            for evasion in [
                format!("+\"{needle}"),
                format!("+'{needle}"),
                format!("+ \"{needle}"),
                format!("+ '{needle}"),
            ] {
                assert!(
                    !PANELS_JS.contains(&evasion),
                    "panels.js must not concat `{needle}` via string fragments (`{evasion}`)"
                );
            }
        }
    }

    #[test]
    fn host_js_no_string_concat_evasion() {
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            for evasion in [
                format!("+\"{needle}"),
                format!("+'{needle}"),
                format!("+ \"{needle}"),
                format!("+ '{needle}"),
            ] {
                assert!(
                    !HOST_JS.contains(&evasion),
                    "host.js must not concat `{needle}` via string fragments (`{evasion}`)"
                );
            }
        }
    }

    #[test]
    fn mote_bridge_js_no_string_concat_evasion() {
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            for evasion in [
                format!("+\"{needle}"),
                format!("+'{needle}"),
                format!("+ \"{needle}"),
                format!("+ '{needle}"),
            ] {
                assert!(
                    !MOTE_BRIDGE_JS.contains(&evasion),
                    "mote-bridge.js must not concat `{needle}` via string fragments (`{evasion}`)"
                );
            }
        }
    }

    #[test]
    fn settings_js_never_uses_innerhtml() {
        // settings.js is the shared chrome bridge for the settings panel —
        // the ADR-0005 structured-DOM discipline applies equally.
        let code = js_strip_noncode(SETTINGS_JS);
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            assert!(
                !code.contains(needle),
                "settings.js executable code must not contain `{needle}` (ADR-0005)"
            );
        }
    }

    #[test]
    fn settings_js_no_string_concat_evasion() {
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            for evasion in [
                format!("+\"{needle}"),
                format!("+'{needle}"),
                format!("+ \"{needle}"),
                format!("+ '{needle}"),
            ] {
                assert!(
                    !SETTINGS_JS.contains(&evasion),
                    "settings.js must not concat `{needle}` via string fragments (`{evasion}`)"
                );
            }
        }
    }

    #[test]
    fn roving_js_never_uses_innerhtml() {
        // roving.js is the shared roving-focus helper loaded into the privileged
        // chrome root (window.mote.roving) BEFORE host.js — it was the one
        // chrome-root script outside the structured-DOM grep net (ADR-0005).
        let code = js_strip_noncode(ROVING_JS);
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            assert!(
                !code.contains(needle),
                "roving.js executable code must not contain `{needle}` (ADR-0005)"
            );
        }
    }

    #[test]
    fn roving_js_no_string_concat_evasion() {
        for needle in ["innerHTML", "outerHTML", "insertAdjacentHTML"] {
            for evasion in [
                format!("+\"{needle}"),
                format!("+'{needle}"),
                format!("+ \"{needle}"),
                format!("+ '{needle}"),
            ] {
                assert!(
                    !ROVING_JS.contains(&evasion),
                    "roving.js must not concat `{needle}` via string fragments (`{evasion}`)"
                );
            }
        }
    }

    #[test]
    fn panels_js_uses_textcontent_for_data_writes() {
        // Sanity: the structured-DOM path must reach for `textContent` (the
        // injection-safe write surface). A panels.js with zero `textContent`
        // uses would be a sign of a regression to string-templating.
        assert!(
            PANELS_JS.contains("textContent"),
            "panels.js must use textContent for plugin-derived strings"
        );
    }

    #[test]
    fn chrome_html_csp_blocks_inline_and_eval() {
        // The chrome document must ship a CSP that blocks inline scripts and
        // unsafe-eval (ADR-0005). `script-src 'self'` alone implies both —
        // assert the *exclusion* of the tokens that would weaken it.
        assert!(
            CHROME_HTML.contains("Content-Security-Policy"),
            "chrome.html must carry a CSP meta"
        );
        // Isolate the actual CSP meta-tag content; the comment block above the
        // meta tag also mentions `script-src 'self'` in prose and would
        // false-trigger a naive split.
        let meta_marker = "http-equiv=\"Content-Security-Policy\"";
        let meta_start = CHROME_HTML
            .find(meta_marker)
            .expect("chrome.html must declare the CSP meta");
        let after_meta = &CHROME_HTML[meta_start..];
        let content_start = after_meta
            .find("content=\"")
            .map(|i| meta_start + i + "content=\"".len())
            .expect("CSP meta must have a content attribute");
        let content_end = CHROME_HTML[content_start..]
            .find('"')
            .map(|i| content_start + i)
            .expect("CSP content attribute must be quoted");
        let csp = &CHROME_HTML[content_start..content_end];

        assert!(
            csp.contains("script-src 'self'"),
            "CSP must restrict script-src to 'self'; got: {csp}"
        );
        assert!(
            !csp.contains("'unsafe-eval'"),
            "CSP must not allow 'unsafe-eval'; got: {csp}"
        );
        // The script-src clause specifically must not allow 'unsafe-inline'.
        // style-src may keep it (the storage-bar uses a numeric width style).
        let script_src_clause = csp
            .split(';')
            .map(str::trim)
            .find(|c| c.starts_with("script-src"))
            .expect("CSP must declare a script-src clause");
        assert!(
            !script_src_clause.contains("'unsafe-inline'"),
            "script-src must not allow 'unsafe-inline'; got: {script_src_clause}"
        );
    }

    #[test]
    fn chrome_html_mounts_panel_and_dialog_roots() {
        // panels.js mounts the integrity panel and approval dialog into these
        // ids. Without them, the structured-DOM builders fall back to creating
        // them at runtime — but the chrome.html static mounts are the
        // contracted positions.
        assert!(CHROME_HTML.contains("mote-integrity-root"));
        assert!(CHROME_HTML.contains("mote-approval-root"));
    }

    #[test]
    fn chrome_html_loads_panels_js_after_host_js() {
        // panels.js depends on host.js's applyOp installation (it wraps it).
        // Match the actual `<script src=...>` tags rather than substring
        // occurrences — `panels.js` is mentioned in the CSP justification
        // comment near the top of the file, which would false-trigger a
        // plain `.find("panels.js")` ordering check.
        let host_pos = CHROME_HTML
            .find("src=\"host.js\"")
            .expect("chrome.html must load host.js via a script tag");
        let panels_pos = CHROME_HTML
            .find("src=\"panels.js\"")
            .expect("chrome.html must load panels.js via a script tag");
        assert!(
            host_pos < panels_pos,
            "chrome.html must load host.js before panels.js"
        );
    }

    #[test]
    fn chrome_html_loads_mote_bridge_before_host_js() {
        // mote-bridge.js installs window.mote.invoke; host.js's boot() guard
        // depends on it. mote-bridge.js must appear before host.js.
        let bridge_pos = CHROME_HTML
            .find("src=\"mote-bridge.js\"")
            .expect("chrome.html must load mote-bridge.js via a script tag");
        let host_pos = CHROME_HTML
            .find("src=\"host.js\"")
            .expect("chrome.html must load host.js via a script tag");
        assert!(
            bridge_pos < host_pos,
            "chrome.html must load mote-bridge.js before host.js"
        );
    }

    #[test]
    fn settings_html_pages_load_mote_bridge_before_settings_js() {
        // Each settings section page must load ../mote-bridge.js BEFORE
        // settings.js. The `../` prefix is required: settings pages are served
        // under `settings/`, so a bare `mote-bridge.js` would 404.
        let pages = [
            ("general.html", SETTINGS_GENERAL_HTML),
            ("plugins.html", SETTINGS_PLUGINS_HTML),
            ("integrity.html", SETTINGS_INTEGRITY_HTML),
            ("keybinds.html", SETTINGS_KEYBINDS_HTML),
        ];
        for (name, html) in pages {
            let bridge_pos = html.find("src=\"../mote-bridge.js\"").unwrap_or_else(|| {
                panic!(
                    "settings/{name} must load ../mote-bridge.js (with ../ path) via a script tag"
                )
            });
            let settings_pos = html.find("src=\"settings.js\"").unwrap_or_else(|| {
                panic!("settings/{name} must load settings.js via a script tag")
            });
            assert!(
                bridge_pos < settings_pos,
                "settings/{name} must load ../mote-bridge.js before settings.js"
            );
        }
    }

    #[test]
    fn hostile_approval_request_serializes_as_inert_json_strings() {
        // ADR-0005 wire-format defence: feed an ApprovalRequest whose plugin
        // name, source, and permission domain carry XSS payloads (script tag,
        // onerror attribute fragment, raw quotes) and assert the JSON serialiser
        // encodes each as a JSON string literal — not as raw markup. The
        // chrome-side `panels.js` then injects them via `textContent`, which
        // means a hostile manifest renders the literal characters and never
        // parses as HTML/JS. The grep tests above prove there is no
        // alternative DOM-write path that could change that.
        let mut req = ApprovalRequest::sample();
        req.plugin = "<script>alert('xss')</script>".into();
        req.source = "evil\" onerror=\"alert(1)".into();
        req.permissions[0].domain = "</script><img src=x onerror=alert(1)>".into();
        req.dangerous_combinations[0] = "\"></div><script>fetch('//attacker')</script>".into();

        let json = serde_json::to_string(&req).expect("serialize must succeed");

        // The angle brackets, quotes, and slashes survive — encoded inside a
        // JSON string. They must NOT appear as raw, unescaped HTML.
        // `serde_json` escapes `"` as `\"` and `</` is left intact (JSON spec),
        // so the script-tag substring appears verbatim INSIDE a JSON string
        // literal — that is the load-bearing property: it's data, not markup.
        assert!(
            json.contains(r"<script>alert('xss')</script>"),
            "JSON must carry the literal payload as string content: {json}"
        );
        // Quotes inside the source field are escaped (\"), proving JSON string
        // framing is preserved (the payload cannot break out of its string).
        assert!(
            json.contains(r#"evil\" onerror=\"alert(1)"#),
            "JSON must escape inner quotes: {json}"
        );
        // Re-parse round-trip succeeds — proves the encoding is well-formed.
        let parsed: ApprovalRequest =
            serde_json::from_str(&json).expect("malicious payload must round-trip");
        assert_eq!(parsed.plugin, req.plugin);
        assert_eq!(parsed.source, req.source);
        assert_eq!(parsed.permissions[0].domain, req.permissions[0].domain);
        assert_eq!(
            parsed.dangerous_combinations[0],
            req.dangerous_combinations[0]
        );
    }

    #[test]
    fn hostile_integrity_panel_serializes_as_inert_json_strings() {
        // Same boundary defence for the IntegrityPanel side: a hostile
        // plugin name / permission string / source label survives JSON encoding
        // as a literal string. The structured-DOM panel renderer injects via
        // `textContent`, so the literal characters appear in the chrome DOM as
        // inert text and the script tag never executes.
        let mut panel = IntegrityPanel::sample();
        panel.plugins[0].name = "<script>alert('xss')</script>".into();
        panel.plugins[0].permissions[0].requested = "\"><img src=x onerror=alert(1)>".into();
        if let PluginKind::DeclaredGit { source, .. } = &mut panel.plugins[0].kind {
            *source = "evil\"</span><script>1</script>".into();
        }
        panel.network_audit[0].detail = Some("</td><script>fetch('//x')</script>".into());
        // A hostile secret name must survive verbatim through the data model:
        // the chrome renders it via textContent (buildPluginCard secret rows),
        // so the literal characters stay inert text and never execute.
        panel.plugins[0].secrets = vec![SecretAccessRow {
            name: "</li><script>steal()</script>".into(),
            backend: "env".into(),
            last_read: Some("just now".into()),
        }];

        let json = serde_json::to_string(&panel).expect("serialize must succeed");

        // Payload survives as escaped JSON string content; round-trips
        // bit-exact through deserialize. The grep tests on PANELS_JS prove the
        // only chrome write path is textContent — so the strings stay inert.
        assert!(json.contains("<script>alert('xss')</script>"));
        assert!(json.contains("</li><script>steal()</script>"));
        let parsed: IntegrityPanel =
            serde_json::from_str(&json).expect("malicious payload must round-trip");
        assert_eq!(parsed.plugins[0].name, panel.plugins[0].name);
        assert_eq!(
            parsed.plugins[0].secrets[0].name,
            panel.plugins[0].secrets[0].name
        );
    }

    // ---- P3: newtab.html structure tests (ADR-0015) ----

    /// The `NEWTAB_HTML` constant is non-empty (`include_str` wired correctly).
    #[test]
    fn newtab_html_constant_is_non_empty() {
        assert!(
            !NEWTAB_HTML.is_empty(),
            "NEWTAB_HTML must include newtab.html"
        );
    }

    /// Page title must be exactly `new tab` so R2's `OnTitleChange` mirror
    /// surfaces it as the sidebar tab title and window title (ADR-0015 §title).
    #[test]
    fn newtab_html_title_is_new_tab() {
        assert!(
            NEWTAB_HTML.contains("<title>new tab</title>"),
            "newtab.html <title> must be exactly 'new tab'"
        );
    }

    /// The `newtab.center` slot must be declared so themes and plugins can bind
    /// it via `theme:bind_slot("newtab.center", element)` (ADR-0015 §the slot).
    #[test]
    fn newtab_html_declares_newtab_center_slot() {
        assert!(
            NEWTAB_HTML.contains("data-slot=\"newtab.center\""),
            "newtab.html must declare the newtab.center slot"
        );
    }

    /// The newtab page must carry a CSP meta that blocks inline scripts and
    /// unsafe-eval — same discipline as chrome.html (ADR-0005 + ADR-0015).
    #[test]
    fn newtab_html_csp_blocks_inline_and_eval() {
        assert!(
            NEWTAB_HTML.contains("Content-Security-Policy"),
            "newtab.html must carry a CSP meta"
        );
        let meta_marker = "http-equiv=\"Content-Security-Policy\"";
        let meta_start = NEWTAB_HTML
            .find(meta_marker)
            .expect("newtab.html must declare the CSP meta");
        let after_meta = &NEWTAB_HTML[meta_start..];
        let content_start = after_meta
            .find("content=\"")
            .map(|i| meta_start + i + "content=\"".len())
            .expect("CSP meta must have a content attribute");
        let content_end = NEWTAB_HTML[content_start..]
            .find('"')
            .map(|i| content_start + i)
            .expect("CSP content attribute must be quoted");
        let csp = &NEWTAB_HTML[content_start..content_end];

        assert!(
            csp.contains("script-src 'self'"),
            "newtab.html CSP must restrict script-src to 'self'; got: {csp}"
        );
        assert!(
            !csp.contains("'unsafe-eval'"),
            "newtab.html CSP must not allow 'unsafe-eval'; got: {csp}"
        );
        let script_src_clause = csp
            .split(';')
            .map(str::trim)
            .find(|c| c.starts_with("script-src"))
            .expect("newtab.html CSP must declare a script-src clause");
        assert!(
            !script_src_clause.contains("'unsafe-inline'"),
            "newtab.html script-src must not allow 'unsafe-inline'; got: {script_src_clause}"
        );
    }

    /// The newtab page must boot in the dusk theme (same as chrome.html). The
    /// `data-theme="dusk"` attribute on `<html>` enables the dusk token set.
    #[test]
    fn newtab_html_boots_in_dusk_theme() {
        assert!(
            NEWTAB_HTML.contains("data-theme=\"dusk\""),
            "newtab.html must boot in dusk theme via data-theme attribute"
        );
    }

    /// The newtab page must reference `assets/mark.svg` — the [·] brand mark
    /// at 96px is the primary visual element (ADR-0015 §page structure).
    #[test]
    fn newtab_html_references_mark_svg() {
        assert!(
            NEWTAB_HTML.contains("assets/mark.svg"),
            "newtab.html must reference assets/mark.svg for the brand mark"
        );
    }

    // ---- UiHost seam (in-memory implementor) ----

    struct TestHost {
        resolver: TokenResolver,
        emitted: Vec<Node>,
    }

    impl UiHost for TestHost {
        fn theme(&self) -> Theme {
            self.resolver.theme()
        }
        fn token(&self, name: &str) -> Option<TokenValue> {
            self.resolver.resolve(name).cloned()
        }
        fn token_var(&self, name: &str) -> Option<String> {
            self.resolver.resolve(name)?;
            Some(format!(
                "var(--{})",
                name.trim_start_matches("--").replace('_', "-")
            ))
        }
        fn emit(&mut self, node: Node) {
            self.emitted.push(node);
        }
    }

    #[test]
    fn uihost_render_builds_token_only_node_tree() {
        let mut host = TestHost {
            resolver: TokenResolver::new(Theme::Dusk),
            emitted: Vec::new(),
        };

        // a plugin's render(host) builds a subtree referencing tokens by name.
        assert_eq!(host.theme(), Theme::Dusk);
        assert_eq!(
            host.token("accent"),
            Some(TokenValue::Color("#e0a458".into()))
        );
        assert_eq!(
            host.token_var("surface_1").as_deref(),
            Some("var(--surface-1)")
        );
        assert_eq!(host.token_var("nope"), None);

        let tree = Node::element("div")
            .class("panel")
            .style_token("background", "surface-1")
            .child(Node::element("span").child(Node::text("git: clean")));
        host.emit(tree.clone());

        assert_eq!(host.emitted, vec![tree]);
    }
}
