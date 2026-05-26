//! The fixed v0.1 element-kind registry and element references.
//!
//! An [`Element`] is a renderable content unit of a known [`ElementKind`] that
//! a plugin contributes. The eight kinds are fixed in v0.1
//! (`spec/01_architecture.md`). A theme places elements into slots by
//! [`ElementRef`] — either `<kind>`, `<kind>:<id>`, or the `<kind>:*` wildcard.

use core::fmt;

/// One of the eight fixed v0.1 element kinds.
///
/// The discriminant order matches the canonical taxonomy in
/// `spec/01_architecture.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ElementKind {
    /// The omnibox. Exactly one, always present.
    Urlbar,
    /// The tab strip. Exactly one, always present.
    Tabstrip,
    /// A horizontal bookmarks bar.
    BookmarksBar,
    /// A swappable sidebar panel (tabs, bookmarks, history, ...).
    SidebarPanel,
    /// A clickable toolbar action.
    ActionButton,
    /// A status-line segment.
    StatusIndicator,
    /// An inline extension within the urlbar.
    UrlbarExtension,
    /// Catch-all for non-standard plugin UI.
    Widget,
}

impl ElementKind {
    /// Every fixed v0.1 element kind, in canonical order.
    pub const ALL: [Self; 8] = [
        Self::Urlbar,
        Self::Tabstrip,
        Self::BookmarksBar,
        Self::SidebarPanel,
        Self::ActionButton,
        Self::StatusIndicator,
        Self::UrlbarExtension,
        Self::Widget,
    ];

    /// The kebab-case kind name used in Lua element references.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Urlbar => "urlbar",
            Self::Tabstrip => "tabstrip",
            Self::BookmarksBar => "bookmarks-bar",
            Self::SidebarPanel => "sidebar-panel",
            Self::ActionButton => "action-button",
            Self::StatusIndicator => "status-indicator",
            Self::UrlbarExtension => "urlbar-extension",
            Self::Widget => "widget",
        }
    }

    /// Whether the runtime guarantees exactly one instance of this kind always
    /// exists (`urlbar`, `tabstrip`).
    #[must_use]
    pub const fn is_singleton(self) -> bool {
        matches!(self, Self::Urlbar | Self::Tabstrip)
    }

    /// Resolve a kebab-case kind name to its [`ElementKind`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

impl fmt::Display for ElementKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A theme's reference to an element when placing it into a slot.
///
/// Parsed from the Lua layout forms `"<kind>"`, `"<kind>:<id>"`, and the
/// wildcard `"<kind>:*"` (`spec/07_themes.md`). The wildcard catches any
/// element of that kind not placed elsewhere — how a theme handles plugins it
/// was not written to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementRef {
    kind: ElementKind,
    selector: RefSelector,
}

/// The selector half of an [`ElementRef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefSelector {
    /// Any element of the kind (bare `<kind>`).
    Any,
    /// A specific element id (`<kind>:<id>`).
    Id(String),
    /// The wildcard catch-all (`<kind>:*`).
    Wildcard,
}

impl ElementRef {
    /// Parse a Lua element reference (`"<kind>"`, `"<kind>:<id>"`, `"<kind>:*"`).
    ///
    /// Returns [`None`] if the kind segment is not one of the eight fixed kinds.
    #[must_use]
    pub fn parse(reference: &str) -> Option<Self> {
        let (kind_name, selector) = reference.split_once(':').map_or_else(
            || (reference, RefSelector::Any),
            |(k, sel)| {
                let selector = if sel == "*" {
                    RefSelector::Wildcard
                } else {
                    RefSelector::Id(sel.to_owned())
                };
                (k, selector)
            },
        );
        Some(Self {
            kind: ElementKind::from_name(kind_name)?,
            selector,
        })
    }

    /// The referenced element kind.
    #[must_use]
    pub const fn kind(&self) -> ElementKind {
        self.kind
    }

    /// The reference's selector.
    #[must_use]
    pub const fn selector(&self) -> &RefSelector {
        &self.selector
    }

    /// Whether this reference is the `<kind>:*` wildcard.
    #[must_use]
    pub const fn is_wildcard(&self) -> bool {
        matches!(self.selector, RefSelector::Wildcard)
    }

    /// Whether this reference matches a concrete element of the given kind and
    /// id.
    #[must_use]
    pub fn matches(&self, kind: ElementKind, id: &str) -> bool {
        if self.kind != kind {
            return false;
        }
        match &self.selector {
            RefSelector::Any | RefSelector::Wildcard => true,
            RefSelector::Id(want) => want == id,
        }
    }
}

/// A concrete renderable element contributed by a plugin (or the runtime).
///
/// The element declares its [`ElementKind`]; the active theme decides which
/// slot it lands in. Rendering itself happens through the [`crate::UiHost`]
/// seam — this struct is the placement-side identity only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// The element's stable id (unique within its kind).
    pub id: String,
    /// The element's kind.
    pub kind: ElementKind,
    /// An optional human label (panel title, button tooltip).
    pub title: Option<String>,
}

impl Element {
    /// Build a new element of the given kind.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: ElementKind) -> Self {
        Self {
            id: id.into(),
            kind,
            title: None,
        }
    }

    /// Set the element's human label.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}
