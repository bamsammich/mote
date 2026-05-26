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
mod layout;
mod slot;
mod token;

pub use compositor::{Compositor, CompositorError, PixelFormat, ViewportRect};
pub use element::{Element, ElementKind, ElementRef, RefSelector};
pub use host::{Node, UiHost};
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
];

/// The `[mote]` wordmark SVG.
pub const WORDMARK_SVG: &str = include_str!("../chrome/assets/wordmark.svg");

/// The `[·]` mark / favicon SVG.
pub const MARK_SVG: &str = include_str!("../chrome/assets/mark.svg");

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

        // top-bar carries tabstrip + urlbar.
        assert!(layout.binds(Slot::TopBar, ElementKind::Tabstrip, "core"));
        assert!(layout.binds(Slot::TopBar, ElementKind::Urlbar, "core"));

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
        for slot in Slot::ALL {
            let attr = format!("data-slot=\"{}\"", slot.name());
            assert!(
                CHROME_HTML.contains(&attr),
                "chrome.html missing {}",
                slot.name()
            );
        }
        // boots in dusk, includes the empty-slot motif and no AI surfaces.
        assert!(CHROME_HTML.contains("data-theme=\"dusk\""));
        assert!(CHROME_HTML.contains("slot-empty"));
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
        ] {
            assert!(names.contains(&want), "missing component css: {want}");
        }
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
