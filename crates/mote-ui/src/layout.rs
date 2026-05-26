//! Element-to-slot placement: the `default-layout` and a theme's `layout`.
//!
//! A [`Layout`] maps each [`Slot`] to an ordered list of [`ElementRef`]s. A
//! slot mapped to an empty list is *explicitly empty* and renders the
//! empty-slot motif (`spec/components/empty-slot.md`). A slot absent from the
//! map inherits from the layout it `inherits` (the runtime ships
//! `default-layout`; `spec/07`).
//!
//! This is the placement side only — actual rendering goes through
//! [`crate::UiHost`].

use crate::element::{ElementKind, ElementRef};
use crate::slot::Slot;
use std::collections::BTreeMap;

/// An element-to-slot placement map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    placements: BTreeMap<Slot, Vec<ElementRef>>,
}

impl Layout {
    /// An empty layout (no slots placed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The `default-layout` Mote ships (`docs/plans/02-browser-shell.md` §5.2,
    /// `spec/01`):
    ///
    /// - `top-bar` = `{ tabstrip, urlbar }`
    /// - `tab-row` = `{ tabstrip }` (the strip nests here)
    /// - `urlbar-inline` = `{ urlbar-extension:* }`
    /// - `left-sidebar` = `{ sidebar-panel:* }`
    /// - `bottom-bar` = `{ status-indicator:* }`
    /// - `right-sidebar` = `{}` (explicitly empty → empty-slot motif)
    #[must_use]
    pub fn default_layout() -> Self {
        let mut layout = Self::new();
        layout.place(Slot::TopBar, refs(&["tabstrip", "urlbar"]));
        layout.place(Slot::TabRow, refs(&["tabstrip"]));
        layout.place(Slot::UrlbarInline, refs(&["urlbar-extension:*"]));
        layout.place(Slot::LeftSidebar, refs(&["sidebar-panel:*"]));
        layout.place(Slot::BottomBar, refs(&["status-indicator:*"]));
        layout.place(Slot::RightSidebar, Vec::new());
        layout
    }

    /// Place an ordered list of element references into a slot, replacing any
    /// prior placement. An empty `elements` marks the slot explicitly empty.
    pub fn place(&mut self, slot: Slot, elements: Vec<ElementRef>) {
        self.placements.insert(slot, elements);
    }

    /// The element references placed in a slot, or [`None`] if the slot is not
    /// mentioned by this layout (it would inherit).
    #[must_use]
    pub fn slot(&self, slot: Slot) -> Option<&[ElementRef]> {
        self.placements.get(&slot).map(Vec::as_slice)
    }

    /// Whether the slot is *declared but empty* — mentioned with no elements,
    /// so the runtime renders the empty-slot motif.
    #[must_use]
    pub fn is_empty_slot(&self, slot: Slot) -> bool {
        self.placements.get(&slot).is_some_and(Vec::is_empty)
    }

    /// Resolve which placed reference a concrete element of `kind`/`id` binds
    /// to in `slot`, honoring wildcard fallthrough.
    ///
    /// A non-wildcard match wins over a `:*` wildcard in the same slot.
    #[must_use]
    pub fn binds(&self, slot: Slot, kind: ElementKind, id: &str) -> bool {
        let Some(refs) = self.slot(slot) else {
            return false;
        };
        let mut wildcard = false;
        for element_ref in refs {
            if element_ref.matches(kind, id) {
                if element_ref.is_wildcard() {
                    wildcard = true;
                } else {
                    return true;
                }
            }
        }
        wildcard
    }
}

/// Parse a list of Lua element-reference strings, dropping any with an unknown
/// kind segment.
fn refs(references: &[&str]) -> Vec<ElementRef> {
    references
        .iter()
        .filter_map(|reference| ElementRef::parse(reference))
        .collect()
}
