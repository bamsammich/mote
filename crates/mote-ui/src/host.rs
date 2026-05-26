//! The `UiHost` seam and the element-building primitives.
//!
//! [`UiHost`] is the host API a plugin's `render(host)` function targets, and
//! the surface `mote-shell` talks to (`docs/plans/02-browser-shell.md` §2,
//! §5.3). It is deliberately **UI-independent**: no wgpu, winit, or CEF types
//! appear here, so shell wiring is not blocked on the rendering backend.
//!
//! Two roles share the trait:
//!
//! - **token access** — `render` reads resolved design tokens by name
//!   (`host.tokens.accent`), mirroring the Lua bridge in `spec/03`.
//! - **element-building primitives** — `render` builds a [`Node`] subtree
//!   (`host:el / host:text`), which the runtime delivers into the slot the
//!   active theme placed the element in (the spike's `render(host)` → DOM op
//!   model). Phase 2 wires this path; only first-party panels exercise it.

use crate::token::{Theme, TokenValue};

/// A built UI node — the output of a plugin `render(host)` call.
///
/// A small, backend-agnostic tree the runtime serializes into chrome-document
/// DOM ops over the host bridge. It is **structure + tokens**, never raw colors
/// (token discipline holds across the bridge): styling references token names,
/// resolved to `var(--…)` on the chrome side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// An element with a tag, optional class list, token-named style props, and
    /// children.
    Element {
        /// The element tag (`div`, `span`, `button`, ...).
        tag: String,
        /// CSS class names applied to the element.
        classes: Vec<String>,
        /// `(css-property, token-name)` style bindings. The runtime rewrites
        /// each token name to `var(--<name>)` on the chrome side, preserving
        /// token discipline.
        styles: Vec<(String, String)>,
        /// Child nodes.
        children: Vec<Self>,
    },
    /// A text leaf.
    Text(String),
}

impl Node {
    /// A text leaf.
    #[must_use]
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text(content.into())
    }

    /// An empty element with the given tag.
    #[must_use]
    pub fn element(tag: impl Into<String>) -> Self {
        Self::Element {
            tag: tag.into(),
            classes: Vec::new(),
            styles: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Add a CSS class (builder style).
    ///
    /// # Panics
    ///
    /// Panics if called on a [`Node::Text`].
    #[must_use]
    pub fn class(mut self, class: impl Into<String>) -> Self {
        let Self::Element { classes, .. } = &mut self else {
            panic!("class() called on a text node");
        };
        classes.push(class.into());
        self
    }

    /// Bind a CSS property to a design-token name (builder style).
    ///
    /// The token name is resolved to `var(--<name>)` on the chrome side — pass
    /// the bare token name (`accent`, `surface-1`), never a raw color.
    ///
    /// # Panics
    ///
    /// Panics if called on a [`Node::Text`].
    #[must_use]
    pub fn style_token(mut self, property: impl Into<String>, token: impl Into<String>) -> Self {
        let Self::Element { styles, .. } = &mut self else {
            panic!("style_token() called on a text node");
        };
        styles.push((property.into(), token.into()));
        self
    }

    /// Append a child node (builder style).
    ///
    /// # Panics
    ///
    /// Panics if called on a [`Node::Text`].
    #[must_use]
    pub fn child(mut self, child: Self) -> Self {
        let Self::Element { children, .. } = &mut self else {
            panic!("child() called on a text node");
        };
        children.push(child);
        self
    }
}

/// The host API a plugin `render` targets and the shell drives.
///
/// UI-independent by contract: implementors hold the rendering backend, but the
/// trait exposes none of it. The runtime's production implementor lives behind
/// the chrome bridge; tests use a lightweight in-memory implementor.
pub trait UiHost {
    /// The active theme.
    fn theme(&self) -> Theme;

    /// Resolve a design token by name (bare, CSS, or Lua spelling) to its
    /// value for the active theme. Mirrors `host.tokens.<name>` in Lua.
    fn token(&self, name: &str) -> Option<TokenValue>;

    /// The CSS `var(--<name>)` reference for a token, for use in built node
    /// styles. Returns [`None`] if the token is unknown.
    fn token_var(&self, name: &str) -> Option<String>;

    /// Emit a built node subtree into the element's placed slot.
    ///
    /// The runtime serializes the tree into chrome-document DOM ops over the
    /// host bridge. Called by a plugin's `render(host)`; idempotent per render
    /// pass (a re-render replaces the prior subtree).
    fn emit(&mut self, node: Node);
}
