//! Status-line element types shared across the plugin runtime (ADR-0016).
//!
//! [`StatusLineElement`] is the v1 schema for an element that a plugin (or the
//! chrome bootstrap) registers in the status-line. Both plugin-declared elements
//! (`M.statusline = { … }`) and the two core built-in elements
//! (`mote.security`, `mote.tabcount`) share this exact schema so the rendering
//! code has no special-case branches. (`mote.mode` — vim NORMAL/INSERT — is
//! provided by the editing-paradigm plugin, not core; see ADR-0019.)
//!
//! The `action` and `disabled` fields are **reserved for v2** and are not defined
//! here; the v1 load pipeline rejects them with a warning log and continues
//! loading (forward-compatibility commitment, ADR-0016).

use serde::{Deserialize, Serialize};

/// The display zone within the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum StatusZone {
    /// Left zone — elements ordered high-priority towards the far-left edge.
    Left,
    /// Center zone.
    Center,
    /// Right zone — elements ordered high-priority towards the far-right edge.
    Right,
}

impl StatusZone {
    /// Parse the wire string used in a plugin manifest (`"left"`, `"center"`,
    /// `"right"`).
    ///
    /// Returns `None` for any other string (caller maps to a validation error).
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "left" => Some(Self::Left),
            "center" => Some(Self::Center),
            "right" => Some(Self::Right),
            _ => None,
        }
    }

    /// Returns the canonical wire string for this zone.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
        }
    }
}

/// The rendering kind of a status-line element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum StatusKind {
    /// Plain text only. `text` field is required.
    Text,
    /// Icon only (from the ADR-0013 icon registry). `icon` field is required.
    Icon,
    /// Icon followed by text. Both `icon` and `text` fields are required.
    IconText,
}

impl StatusKind {
    /// Parse the wire string used in a plugin manifest.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "icon" => Some(Self::Icon),
            "icon-text" => Some(Self::IconText),
            _ => None,
        }
    }

    /// Returns the canonical wire string for this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Icon => "icon",
            Self::IconText => "icon-text",
        }
    }

    /// Whether `text` is required for this kind.
    #[must_use]
    pub const fn requires_text(self) -> bool {
        matches!(self, Self::Text | Self::IconText)
    }

    /// Whether `icon` is required for this kind.
    #[must_use]
    pub const fn requires_icon(self) -> bool {
        matches!(self, Self::Icon | Self::IconText)
    }
}

/// The color token applied to a status-line element.
///
/// Maps to a CSS custom property token (`var(--fg)`, `var(--accent)`, …).
/// Raw color values are rejected at load time; plugins must use token names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum StatusColor {
    /// Foreground color (`var(--fg)`). Default when the field is absent.
    #[default]
    Fg,
    /// Accent color (`var(--accent)`).
    Accent,
    /// Warning color (`var(--warn)`).
    Warn,
    /// Muted foreground (`var(--fg-mute)`).
    Mute,
}

impl StatusColor {
    /// Parse the wire string used in a plugin manifest.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "fg" => Some(Self::Fg),
            "accent" => Some(Self::Accent),
            "warn" => Some(Self::Warn),
            "mute" => Some(Self::Mute),
            _ => None,
        }
    }

    /// Returns the canonical wire string for this color.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fg => "fg",
            Self::Accent => "accent",
            Self::Warn => "warn",
            Self::Mute => "mute",
        }
    }
}

/// A single status-line element (v1 schema, ADR-0016).
///
/// Both plugin-declared elements (extracted from `M.statusline`) and the
/// three built-in chrome elements share this exact type so the runtime and
/// renderer have no special-case branches.
///
/// Fields match the ADR-0016 schema table. The `action` and `disabled` v2
/// fields are **not** defined here; their presence in a Lua table triggers a
/// warning log + ignore at the load boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StatusLineElement {
    /// Fully-qualified element id: `<plugin>.<id>` after namespacing.
    ///
    /// For built-in elements the namespace prefix is `mote` (e.g. `mote.mode`).
    /// Unique within the runtime-wide element registry.
    pub id: String,

    /// Display zone.
    pub zone: StatusZone,

    /// Display priority. Higher value ⇒ closer to the zone's outer edge.
    /// Ties broken by `id` alphabetically.
    pub priority: i32,

    /// Rendering kind.
    pub kind: StatusKind,

    /// Text content. Required when `kind ∈ {Text, IconText}`.
    pub text: Option<String>,

    /// Icon source string (`"lucide:<name>"` per ADR-0013).
    /// Required when `kind ∈ {Icon, IconText}`.
    pub icon: Option<String>,

    /// Color token. Defaults to [`StatusColor::Fg`] when absent.
    pub color: StatusColor,

    /// Optional tooltip shown after 200ms hover (P1 tooltip primitive).
    pub tooltip: Option<String>,
}

impl StatusLineElement {
    /// Constructs a [`StatusLineElement`] from individual field values.
    ///
    /// Provided so callers outside this crate can build instances without
    /// being blocked by the `#[non_exhaustive]` attribute (which prevents
    /// struct-literal construction in foreign crates).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    // Cannot be `const fn`: takes `String` (heap-allocated) which is not
    // valid in a const context.
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(
        id: String,
        zone: StatusZone,
        priority: i32,
        kind: StatusKind,
        text: Option<String>,
        icon: Option<String>,
        color: StatusColor,
        tooltip: Option<String>,
    ) -> Self {
        Self {
            id,
            zone,
            priority,
            kind,
            text,
            icon,
            color,
            tooltip,
        }
    }

    // NOTE: there is intentionally no `builtin_mode()`. The `mote.mode` (vim
    // NORMAL/INSERT) status element is provided by the editing-paradigm plugin,
    // not core (ADR-0019). A core-hardcoded `NORMAL` chip was the CL-SPECDRIFT
    // B1 drift and has been removed.

    /// Construct the `mote.security` built-in element for HTTPS.
    #[must_use]
    pub fn builtin_security_https() -> Self {
        Self {
            id: "mote.security".to_owned(),
            zone: StatusZone::Left,
            priority: 50,
            kind: StatusKind::IconText,
            text: Some("https \u{00b7} tls 1.3".to_owned()),
            icon: Some("lucide:lock".to_owned()),
            color: StatusColor::Accent,
            tooltip: Some("connection is secure".to_owned()),
        }
    }

    /// Construct the `mote.security` built-in element for HTTP (insecure).
    #[must_use]
    pub fn builtin_security_http() -> Self {
        Self {
            id: "mote.security".to_owned(),
            zone: StatusZone::Left,
            priority: 50,
            kind: StatusKind::IconText,
            text: Some("http \u{00b7} insecure".to_owned()),
            icon: Some("lucide:triangle-alert".to_owned()),
            color: StatusColor::Warn,
            tooltip: Some("connection is not encrypted".to_owned()),
        }
    }

    /// Construct the `mote.tabcount` built-in element.
    #[must_use]
    pub fn builtin_tabcount(count: usize) -> Self {
        let plural = if count == 1 { "tab" } else { "tabs" };
        Self {
            id: "mote.tabcount".to_owned(),
            zone: StatusZone::Right,
            priority: 50,
            kind: StatusKind::Text,
            text: Some(format!("{count} {plural}")),
            icon: None,
            color: StatusColor::Fg,
            tooltip: None,
        }
    }
}
