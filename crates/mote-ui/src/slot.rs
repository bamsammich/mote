//! The fixed v0.1 slot registry.
//!
//! A [`Slot`] is a named layout region the runtime owns. Themes decide which
//! elements bind to which slot; plugins never choose placement. The set is
//! fixed in v0.1 (`spec/01_architecture.md` "Slots and element kinds").
//!
//! Two of these — [`Slot::UrlbarInline`] and [`Slot::TabRow`] — are *nested*
//! slots: they live inside the `top-bar` region (`urlbar-inline` inside the
//! urlbar element, `tab-row` inside the tab strip) rather than at a window
//! edge. [`Slot::position`] reports `Edge::Nested` for them.

use core::fmt;

/// Where a slot sits in the chrome window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    /// Across the top of the window.
    Top,
    /// The left dock.
    Left,
    /// The right dock.
    Right,
    /// Across the bottom of the window.
    Bottom,
    /// Nested inside another slot's element (not a window edge).
    Nested,
}

/// A fixed v0.1 layout slot.
///
/// The discriminant order matches the canonical taxonomy in
/// `spec/01_architecture.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    /// `top-bar` — urlbar, tabstrip, action buttons.
    TopBar,
    /// `left-sidebar` — sidebar panels, widgets.
    LeftSidebar,
    /// `right-sidebar` — sidebar panels, widgets.
    RightSidebar,
    /// `bottom-bar` — status indicators (the status line).
    BottomBar,
    /// `urlbar-inline` — urlbar extensions, nested within the urlbar.
    UrlbarInline,
    /// `tab-row` — the tabstrip and per-tab pieces, nested within the tab strip.
    TabRow,
}

impl Slot {
    /// Every fixed v0.1 slot, in canonical order.
    pub const ALL: [Self; 6] = [
        Self::TopBar,
        Self::LeftSidebar,
        Self::RightSidebar,
        Self::BottomBar,
        Self::UrlbarInline,
        Self::TabRow,
    ];

    /// The kebab-case slot name used in `data-slot` attributes and Lua layout
    /// keys.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TopBar => "top-bar",
            Self::LeftSidebar => "left-sidebar",
            Self::RightSidebar => "right-sidebar",
            Self::BottomBar => "bottom-bar",
            Self::UrlbarInline => "urlbar-inline",
            Self::TabRow => "tab-row",
        }
    }

    /// Where the slot sits in the chrome.
    #[must_use]
    pub const fn position(self) -> Edge {
        match self {
            Self::TopBar => Edge::Top,
            Self::LeftSidebar => Edge::Left,
            Self::RightSidebar => Edge::Right,
            Self::BottomBar => Edge::Bottom,
            Self::UrlbarInline | Self::TabRow => Edge::Nested,
        }
    }

    /// Whether the slot may be user-resized (the sidebars, per `spec/07`).
    #[must_use]
    pub const fn is_resizable(self) -> bool {
        matches!(self, Self::LeftSidebar | Self::RightSidebar)
    }

    /// Resolve a kebab-case slot name to its [`Slot`], if it is a known v0.1
    /// slot.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|slot| slot.name() == name)
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
