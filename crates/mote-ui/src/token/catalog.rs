//! The canonical token catalog — the Rust mirror of `chrome/tokens.css`.
//!
//! Each [`TokenEntry`] carries the dusk (default) value and, where it differs,
//! the vellum override. Values are **fully resolved** (the `var(--ink-800)`
//! indirection in the CSS is dereferenced to concrete values here) so the
//! resolver returns final colors/dimensions, which is what the Lua bridge and
//! the compositor's CSS-var rewrite both need.
//!
//! The `tokens_css_in_sync` test (in `lib.rs`) asserts every bare name here
//! appears in `chrome/tokens.css`, guarding against drift.

use super::{Theme, TokenValue};

/// A single token's canonical definition across both themes.
#[derive(Debug, Clone, Copy)]
pub struct TokenEntry {
    /// The bare CSS-var name (no `--`).
    pub bare: &'static str,
    /// The dusk (default) value.
    pub dusk: Value,
    /// The vellum override, or [`None`] to inherit the dusk value.
    pub vellum: Option<Value>,
}

/// A compile-time token value (the `const` form of [`TokenValue`]).
#[derive(Debug, Clone, Copy)]
pub enum Value {
    /// A color string.
    Color(&'static str),
    /// A pixel dimension.
    Dim(i32),
    /// Any verbatim value.
    Raw(&'static str),
}

impl Value {
    /// Materialize the static value into an owned [`TokenValue`].
    fn materialize(self) -> TokenValue {
        match self {
            Self::Color(s) => TokenValue::Color(s.to_owned()),
            Self::Dim(px) => TokenValue::Dimension(px),
            Self::Raw(s) => TokenValue::Raw(s.to_owned()),
        }
    }
}

impl TokenEntry {
    /// The resolved value for the given theme (vellum falls back to dusk).
    #[must_use]
    pub fn value_for(&self, theme: Theme) -> TokenValue {
        match theme {
            Theme::Dusk => self.dusk.materialize(),
            Theme::Vellum => self.vellum.unwrap_or(self.dusk).materialize(),
        }
    }
}

use Value::{Color as C, Dim as D, Raw as R};

/// Every semantic, spacing, radius, motion, typography, and layout token.
///
/// Mirrors `chrome/tokens.css`. The raw `--ink-*` / `--paper-*` scales are
/// intentionally **not** included: components never reference them, so the
/// resolver does not surface them. Their resolved values are inlined into the
/// semantic tokens below.
pub static CANONICAL_TOKENS: &[TokenEntry] = &[
    // ---- Color — semantic ----
    e("bg", C("#14110f"), Some(C("#f4efe6"))),
    e("surface-1", C("#1c1815"), Some(C("#fbf8f1"))),
    e("surface-2", C("#241f1b"), Some(C("#eae3d5"))),
    e("surface-3", C("#2e2823"), Some(C("#ddd3bf"))),
    e("surface-sunk", C("#0e0c0a"), Some(C("#eae3d5"))),
    e("border", C("#2e2823"), Some(C("#ddd3bf"))),
    e("border-strong", C("#3a332d"), Some(C("#c7b89c"))),
    e("border-subtle", C("#241f1b"), Some(C("#eae3d5"))),
    e("fg", C("#ece5d8"), Some(C("#14110f"))),
    e("fg-1", C("#c9c0b0"), Some(C("#3a332d"))),
    e("fg-2", C("#8a8278"), Some(C("#6b6359"))),
    e("fg-3", C("#5c544b"), Some(C("#a89b82"))),
    e("fg-inverse", C("#0e0c0a"), Some(C("#fbf8f1"))),
    e("accent", C("#e0a458"), Some(C("#b47c36"))),
    e("accent-soft", C("#f1c893"), None),
    e("accent-deep", C("#b47c36"), Some(C("#8b5a1f"))),
    e("accent-on", C("#0e0c0a"), Some(C("#fbf8f1"))),
    e("success", C("#6b8e4e"), Some(C("#4f7035"))),
    e("danger", C("#c84a2c"), Some(C("#a83a1f"))),
    e("info", C("#5b7ca3"), Some(C("#3f5f86"))),
    e("special", C("#8e6fa0"), Some(C("#6f5184"))),
    e("focus", C("#e0a458"), Some(C("#b47c36"))),
    // ---- Color — syntax ----
    e("syn-keyword", C("#88a3c3"), Some(C("#3f5f86"))),
    e("syn-string", C("#93ae76"), Some(C("#4f7035"))),
    e("syn-number", C("#f1c893"), Some(C("#b47c36"))),
    e("syn-comment", C("#8a8278"), Some(C("#6b6359"))),
    e("syn-fn", C("#b398c0"), Some(C("#6f5184"))),
    e("syn-punct", C("#b5aea3"), Some(C("#6b6359"))),
    // ---- Spacing — 4px base grid ----
    e("space-0", D(0), None),
    e("space-px", D(1), None),
    e("space-1", D(4), None),
    e("space-2", D(8), None),
    e("space-3", D(12), None),
    e("space-4", D(16), None),
    e("space-5", D(20), None),
    e("space-6", D(24), None),
    e("space-7", D(32), None),
    e("space-8", D(40), None),
    e("space-9", D(48), None),
    e("space-10", D(64), None),
    e("space-11", D(80), None),
    e("space-12", D(96), None),
    // ---- Radius ----
    e("radius-0", D(0), None),
    e("radius-1", D(2), None),
    e("radius-2", D(4), None),
    e("radius-3", D(6), None),
    e("radius-dot", D(9999), None),
    // ---- Shadow (theme-tuned) ----
    e(
        "shadow-1",
        R("0 1px 2px rgba(14, 12, 10, 0.4)"),
        Some(R("0 1px 2px rgba(20, 17, 15, 0.1)")),
    ),
    e(
        "shadow-2",
        R("0 8px 24px rgba(14, 12, 10, 0.5)"),
        Some(R("0 8px 24px rgba(20, 17, 15, 0.14)")),
    ),
    e(
        "shadow-3",
        R("0 16px 48px rgba(14, 12, 10, 0.6)"),
        Some(R("0 16px 48px rgba(20, 17, 15, 0.18)")),
    ),
    // ---- Motion ----
    e("ease-out", R("cubic-bezier(0.2, 0, 0, 1)"), None),
    e("ease-in", R("cubic-bezier(0.6, 0, 1, 0.4)"), None),
    e("ease-in-out", R("cubic-bezier(0.4, 0, 0.2, 1)"), None),
    e("dur-micro", R("80ms"), None),
    e("dur-base", R("120ms"), None),
    e("dur-entrance", R("200ms"), None),
    // ---- Typography — families ----
    e(
        "font-sans",
        R("\"Geist\", ui-sans-serif, system-ui, sans-serif"),
        None,
    ),
    e(
        "font-mono",
        R("\"JetBrains Mono\", ui-monospace, \"SF Mono\", Menlo, monospace"),
        None,
    ),
    e(
        "font-serif",
        R("\"Instrument Serif\", ui-serif, Georgia, serif"),
        None,
    ),
    // ---- Typography — ramps ----
    e("text-display", R("600 48px/1.05 var(--font-sans)"), None),
    e("text-h1", R("600 32px/1.15 var(--font-sans)"), None),
    e("text-h2", R("600 24px/1.2 var(--font-sans)"), None),
    e("text-h3", R("600 18px/1.3 var(--font-sans)"), None),
    e("text-body-lg", R("400 16px/1.55 var(--font-sans)"), None),
    e("text-body", R("400 14px/1.5 var(--font-sans)"), None),
    e("text-small", R("400 12px/1.4 var(--font-sans)"), None),
    e("text-micro", R("500 11px/1.3 var(--font-sans)"), None),
    e("text-mono", R("400 13px/1.4 var(--font-mono)"), None),
    e("text-mono-sm", R("400 11px/1.3 var(--font-mono)"), None),
    e("text-kbd", R("500 11px/1 var(--font-mono)"), None),
    e(
        "text-serif-display",
        R("400 56px/1.05 var(--font-serif)"),
        None,
    ),
    e(
        "text-serif-quote",
        R("400 28px/1.3 var(--font-serif)"),
        None,
    ),
    // ---- Typography — tracking ----
    e("tracking-tight", R("-0.01em"), None),
    e("tracking-normal", R("0"), None),
    e("tracking-wide", R("0.04em"), None),
    e("tracking-mono", R("-0.01em"), None),
    // ---- Color — accent-mute (P1: favicon placeholder, empty-zone dots) ----
    e(
        "accent-mute",
        R("rgba(224, 164, 88, 0.30)"),
        Some(R("rgba(180, 124, 54, 0.25)")),
    ),
    // ---- Layout (Mote-specific) ----
    e("chrome-header", D(52), None), // P1: one-row header
    e("chrome-tabbar", D(40), None),
    e("chrome-omnibox", D(36), None),
    e("chrome-statusline", D(22), None),
    e("gutter-xs", D(240), None),
    e("gutter-sm", D(320), None),
    e("gutter-md", D(400), None),
    e("gutter-lg", D(480), None),
    e("palette-w", D(640), None),
    e(
        "dots",
        R("radial-gradient(rgba(236, 229, 216, 0.06) 1px, transparent 1px) 0 0 / 4px 4px"),
        Some(R(
            "radial-gradient(rgba(20, 17, 15, 0.08) 1px, transparent 1px) 0 0 / 4px 4px",
        )),
    ),
];

/// Terse constructor for a catalog entry.
const fn e(bare: &'static str, dusk: Value, vellum: Option<Value>) -> TokenEntry {
    TokenEntry { bare, dusk, vellum }
}
