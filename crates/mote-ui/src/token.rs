//! The design-token model and resolver — the CSS-var ↔ Lua bridge.
//!
//! Every value in Mote's visual system is a token (`spec/03_tokens.md`). Each
//! token has two faces:
//!
//! 1. a **CSS custom property** on `:root` / `[data-theme="…"]`
//!    (`--surface-1`), consumed by the chrome stylesheet, and
//! 2. a **Lua field** on `theme.tokens.<name>` (`theme.tokens.surface_1`),
//!    consumed by theme files and plugin `render` functions.
//!
//! The names mirror each other: the Lua name is the CSS name with the leading
//! `--` stripped and `-` mapped to `_`. [`lua_name`] / [`css_name`] perform
//! that bridge.
//!
//! This module is the Rust source of truth for the token *catalog* and the
//! per-theme default values. It mirrors `crates/mote-ui/chrome/tokens.css`; the
//! `tokens_css_in_sync` test asserts the two do not drift.

mod catalog;

pub use catalog::CANONICAL_TOKENS;

use core::fmt;

/// A token's CSS custom-property name **without** the leading `--`
/// (e.g. `surface-1`).
///
/// Stored bare so both faces are cheap to produce: [`Token::css_name`] prefixes
/// `--`, [`Token::lua_name`] maps `-` → `_`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Token(&'static str);

impl Token {
    /// Wrap a bare CSS-var name (no leading `--`).
    #[must_use]
    pub(crate) const fn new(bare: &'static str) -> Self {
        Self(bare)
    }

    /// The CSS custom-property name, with the leading `--`
    /// (e.g. `--surface-1`).
    #[must_use]
    pub fn css_name(self) -> String {
        format!("--{}", self.0)
    }

    /// The bare CSS-var name without the `--` prefix (e.g. `surface-1`).
    #[must_use]
    pub const fn bare(self) -> &'static str {
        self.0
    }

    /// The Lua field name on `theme.tokens` (e.g. `surface_1`).
    ///
    /// This is the CSS-var bridge: `-` becomes `_`.
    #[must_use]
    pub fn lua_name(self) -> String {
        self.0.replace('-', "_")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "--{}", self.0)
    }
}

/// A resolved token value.
///
/// Colors and dimensions are distinguished because the Lua bridge surfaces
/// dimensions as bare numbers (`host.tokens.space_4 == 16`) while colors stay
/// strings (`host.tokens.accent == "#E0A458"`), per `spec/03_tokens.md`
/// "Lua-side access".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenValue {
    /// A color, as a CSS color string (`#E0A458`, `rgba(...)`).
    Color(String),
    /// A pixel dimension. Lua surfaces this as the bare number.
    Dimension(i32),
    /// Any other value kept verbatim (font shorthand, easing curve, gradient).
    Raw(String),
}

impl TokenValue {
    /// Render the value as it appears on the right-hand side of a CSS
    /// declaration.
    #[must_use]
    pub fn css(&self) -> String {
        match self {
            Self::Color(s) | Self::Raw(s) => s.clone(),
            Self::Dimension(px) => format!("{px}px"),
        }
    }

    /// Render the value as the Lua `theme.tokens` field would expose it:
    /// dimensions as bare numbers, everything else as the string form.
    #[must_use]
    pub fn lua(&self) -> String {
        match self {
            Self::Color(s) | Self::Raw(s) => s.clone(),
            Self::Dimension(px) => px.to_string(),
        }
    }
}

/// One of the two first-class themes Mote ships.
///
/// Both are first-class: `dusk` (warm-ink dark, default) and `vellum`
/// (warm-paper light). A resolver is built per-theme; a theme switch is a
/// rebuild + CSS-var swap (instant, `spec/05`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Theme {
    /// Warm-ink dark — the default.
    #[default]
    Dusk,
    /// Warm-paper light.
    Vellum,
}

impl Theme {
    /// Both shipped themes.
    pub const ALL: [Self; 2] = [Self::Dusk, Self::Vellum];

    /// The theme's name as used in `[data-theme="<name>"]` and Lua.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dusk => "dusk",
            Self::Vellum => "vellum",
        }
    }

    /// Resolve a theme name to its [`Theme`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.name() == name)
    }
}

/// The maximum radius any token may carry, in px (`spec/07` "what themes
/// can't do": radius is capped at `--radius-3` = 6px). Enforced by
/// [`TokenResolver::set_override`].
pub const MAX_RADIUS_PX: i32 = 6;

/// An error rejecting a theme token override that would violate a design
/// constraint the runtime enforces (`spec/07`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideError {
    /// The token name is not in the canonical catalog.
    UnknownToken(String),
    /// A radius token was set above [`MAX_RADIUS_PX`].
    RadiusTooLarge {
        /// The bare token name.
        token: String,
        /// The rejected value, in px.
        px: i32,
    },
}

impl fmt::Display for OverrideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownToken(name) => write!(f, "unknown token: {name}"),
            Self::RadiusTooLarge { token, px } => write!(
                f,
                "radius token {token} = {px}px exceeds the {MAX_RADIUS_PX}px cap"
            ),
        }
    }
}

impl core::error::Error for OverrideError {}

/// Resolves token names to values for a single active theme.
///
/// Built from the canonical catalog for a [`Theme`], then optionally layered
/// with theme/user overrides (deep-merge semantics live in the caller; this
/// applies one override at a time, last-writer-wins, `spec/07`). Constraint
/// enforcement (radius cap) happens at override time.
#[derive(Debug, Clone)]
pub struct TokenResolver {
    theme: Theme,
    /// (bare-name, value) pairs in canonical order; overrides mutate in place.
    values: Vec<(Token, TokenValue)>,
}

impl TokenResolver {
    /// Build a resolver carrying the canonical defaults for `theme`.
    #[must_use]
    pub fn new(theme: Theme) -> Self {
        let values = CANONICAL_TOKENS
            .iter()
            .map(|entry| (Token::new(entry.bare), entry.value_for(theme)))
            .collect();
        Self { theme, values }
    }

    /// The theme this resolver was built for.
    #[must_use]
    pub const fn theme(&self) -> Theme {
        self.theme
    }

    /// Resolve a token to its value for the active theme.
    ///
    /// Accepts the bare name (`surface-1`), the CSS form (`--surface-1`), or
    /// the Lua form (`surface_1`).
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<&TokenValue> {
        let bare = normalize(name);
        self.values
            .iter()
            .find(|(tok, _)| tok.bare() == bare)
            .map(|(_, value)| value)
    }

    /// Resolve a token to its CSS-side value string (`var(--x)` is *not*
    /// returned — this is the fully resolved value).
    #[must_use]
    pub fn css_value(&self, name: &str) -> Option<String> {
        self.resolve(name).map(TokenValue::css)
    }

    /// Resolve a token to its Lua-side value string (dimensions as bare
    /// numbers), as `theme.tokens.<name>` would expose it.
    #[must_use]
    pub fn lua_value(&self, name: &str) -> Option<String> {
        self.resolve(name).map(TokenValue::lua)
    }

    /// Apply a single theme/user override, last-writer-wins.
    ///
    /// # Errors
    ///
    /// Returns [`OverrideError::UnknownToken`] if the token is not in the
    /// catalog, or [`OverrideError::RadiusTooLarge`] if a `radius-*` token is
    /// set above [`MAX_RADIUS_PX`] (the `spec/07` constraint, enforced here at
    /// token-set time).
    pub fn set_override(&mut self, name: &str, value: TokenValue) -> Result<(), OverrideError> {
        let bare = normalize(name);
        let slot = self
            .values
            .iter_mut()
            .find(|(tok, _)| tok.bare() == bare)
            .ok_or_else(|| OverrideError::UnknownToken(bare.clone()))?;

        if bare.starts_with("radius-")
            && bare != "radius-dot"
            && let TokenValue::Dimension(px) = value
            && px > MAX_RADIUS_PX
        {
            return Err(OverrideError::RadiusTooLarge { token: bare, px });
        }

        slot.1 = value;
        Ok(())
    }

    /// Iterate every resolved `(token, value)` pair in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = (Token, &TokenValue)> {
        self.values.iter().map(|(tok, value)| (*tok, value))
    }

    /// Render the resolver's tokens as a CSS custom-property block body
    /// (declarations only, no selector), for injection under
    /// `[data-theme="<name>"]`.
    #[must_use]
    pub fn to_css_block(&self) -> String {
        let mut out = String::new();
        for (tok, value) in &self.values {
            out.push_str("  ");
            out.push_str(&tok.css_name());
            out.push_str(": ");
            out.push_str(&value.css());
            out.push_str(";\n");
        }
        out
    }
}

/// Normalize any accepted token spelling to the bare CSS-var name.
fn normalize(name: &str) -> String {
    name.trim_start_matches("--").replace('_', "-")
}
