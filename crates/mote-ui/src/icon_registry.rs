//! Themable icon registry — ADR-0013.
//!
//! Implements the `theme.icons.<action>` contract and the `theme:set_icon`
//! API enforcement gate. In v0.1 the only registered pack is `lucide`; every
//! chrome icon routes through the bundled sprite set.
//!
//! ## API contract
//!
//! - `IconRegistry::default()` — returns the registry pre-loaded with every
//!   `action → lucide:<name>` default mapping from `docs/assets/lucide-usage.md`.
//! - [`IconRegistry::set_icon`] — Lua `theme:set_icon(action, source)` land
//!   here. Rejects unknown pack names and unknown lucide names with clear
//!   errors (fail closed, never silently substitute).
//! - [`IconRegistry::resolve`] — returns the `LucideIcon` the chrome should
//!   render for an action, or `None` if the action is unrecognised.
//!
//! ## v0.1 constraints
//!
//! - Only `lucide:<name>` is accepted. Unknown packs → `SetIconError::UnknownPack`.
//! - The set of valid lucide names is the static list of icons Mote bundles in
//!   `chrome/assets/lucide-sprite.svg`. Any other name → `SetIconError::UnknownName`.
//! - `inline:<svg>` and other source kinds are explicitly out of scope; they
//!   return `SetIconError::UnknownPack` (the "lucide" prefix check gates first).

use std::collections::HashMap;

/// The pack prefix separator in icon source strings (`"lucide:x"`, etc.).
const PACK_SEP: char = ':';

/// The only pack registered in v0.1.
const LUCIDE_PACK: &str = "lucide";

/// Every lucide icon name bundled in `chrome/assets/lucide-sprite.svg`.
/// This list is the authoritative set of valid names for v0.1.
const BUNDLED_LUCIDE_NAMES: &[&str] = &[
    "arrow-left",
    "arrow-right",
    "bookmark",
    "circle-plus",
    "clock",
    "layers",
    "lock",
    "panel-left-close",
    "plus",
    "rotate-cw",
    "rss",
    "settings",
    "triangle-alert",
    "x",
];

/// A resolved lucide icon: the name is guaranteed to be in `BUNDLED_LUCIDE_NAMES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LucideIcon {
    /// The lucide icon name (e.g. `"x"`, `"bookmark"`, `"layers"`).
    pub name: String,
    /// The sprite symbol ID (`"icon-<name>"`).
    pub sprite_id: String,
}

impl LucideIcon {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            sprite_id: format!("icon-{name}"),
        }
    }
}

/// Errors returned by [`IconRegistry::set_icon`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetIconError {
    /// The source string does not contain a pack prefix (`"<pack>:<name>"`).
    #[error(
        "icon source `{raw}` is not in `<pack>:<name>` format; \
         expected e.g. `lucide:x`"
    )]
    MissingPackPrefix {
        /// The raw source string.
        raw: String,
    },

    /// The pack name is not registered in v0.1.
    #[error(
        "unknown icon pack `{pack}` in `{raw}`; \
         registered packs: lucide"
    )]
    UnknownPack {
        /// The unrecognised pack name.
        pack: String,
        /// The full source string (raw input).
        raw: String,
    },

    /// The lucide icon name is not in the bundled sprite set.
    #[error(
        "unknown lucide icon `{name}` in `{raw}`; \
         valid names: {valid_names}"
    )]
    UnknownName {
        /// The unrecognised icon name.
        name: String,
        /// The full source string (raw input).
        raw: String,
        /// A comma-separated list of valid lucide names for the error message.
        valid_names: String,
    },

    /// The action name is not in the registered action set.
    #[error(
        "unknown icon action `{action}`; \
         registered actions include: chrome.close, rail.tabs, …"
    )]
    UnknownAction {
        /// The unrecognised action name.
        action: String,
    },
}

/// The icon registry: maps `action` → `LucideIcon` with theme-override support.
///
/// Constructed via [`IconRegistry::default()`] which pre-loads all defaults
/// from the ADR-0013 action table in `docs/assets/lucide-usage.md`.
#[derive(Debug, Clone)]
pub struct IconRegistry {
    /// The effective mapping: action → resolved icon. Starts at defaults;
    /// `set_icon` overwrites individual entries.
    icons: HashMap<String, LucideIcon>,
}

impl IconRegistry {
    /// All action names the registry recognises.
    ///
    /// Derived from `docs/assets/lucide-usage.md`; every action name declared
    /// in ADR-0013 is listed here. The list is exhaustive for v0.1.
    const ACTIONS: &'static [(&'static str, &'static str)] = &[
        ("chrome.close", "x"),
        ("chrome.bookmark", "bookmark"),
        ("chrome.new_tab", "plus"),
        ("chrome.back", "arrow-left"),
        ("chrome.forward", "arrow-right"),
        ("chrome.reload", "rotate-cw"),
        ("tab.close", "x"),
        // tab.favicon_placeholder is CSS-only; no sprite entry in v0.1.
        ("rail.tabs", "layers"),
        ("rail.bookmarks", "bookmark"),
        ("rail.history", "clock"),
        ("rail.settings", "settings"),
        ("rail.plugin_unbound", "circle-plus"),
        ("collapse.sidebar", "panel-left-close"),
        ("statusline.security_https", "lock"),
        ("statusline.security_http", "triangle-alert"),
    ];

    /// Returns a registry pre-loaded with all defaults from ADR-0013.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut icons = HashMap::with_capacity(Self::ACTIONS.len());
        for (action, lucide_name) in Self::ACTIONS {
            icons.insert((*action).to_owned(), LucideIcon::new(lucide_name));
        }
        Self { icons }
    }

    /// Override the icon for `action` with a theme-supplied `source` string.
    ///
    /// `source` must be `"lucide:<name>"` where `<name>` is a bundled lucide
    /// name. Unknown packs and unknown names are rejected with a clear error —
    /// **fail closed, never silently substitute** (ADR-0013).
    ///
    /// # Errors
    ///
    /// Returns [`SetIconError`] if:
    /// - `source` has no pack prefix
    /// - the pack is not `lucide`
    /// - the lucide name is not in the bundled set
    /// - the `action` is not a known registry action
    pub fn set_icon(&mut self, action: &str, source: &str) -> Result<(), SetIconError> {
        // Step 1: validate the action exists.
        if !self.icons.contains_key(action) {
            return Err(SetIconError::UnknownAction {
                action: action.to_owned(),
            });
        }

        // Step 2: parse the source format.
        let (pack, name) =
            source
                .split_once(PACK_SEP)
                .ok_or_else(|| SetIconError::MissingPackPrefix {
                    raw: source.to_owned(),
                })?;

        // Step 3: validate the pack.
        if pack != LUCIDE_PACK {
            return Err(SetIconError::UnknownPack {
                pack: pack.to_owned(),
                raw: source.to_owned(),
            });
        }

        // Step 4: validate the lucide name is bundled.
        if !BUNDLED_LUCIDE_NAMES.contains(&name) {
            return Err(SetIconError::UnknownName {
                name: name.to_owned(),
                raw: source.to_owned(),
                valid_names: BUNDLED_LUCIDE_NAMES.join(", "),
            });
        }

        // All checks pass — update the mapping.
        self.icons.insert(action.to_owned(), LucideIcon::new(name));
        Ok(())
    }

    /// Returns the resolved icon for `action`, or `None` if the action is
    /// not registered.
    #[must_use]
    pub fn resolve(&self, action: &str) -> Option<&LucideIcon> {
        self.icons.get(action)
    }

    /// Whether `action` is a known registry entry.
    #[must_use]
    pub fn has_action(&self, action: &str) -> bool {
        self.icons.contains_key(action)
    }
}

impl Default for IconRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> IconRegistry {
        IconRegistry::with_defaults()
    }

    // ---- Defaults -----------------------------------------------------------

    #[test]
    fn defaults_load_and_are_nonempty() {
        let r = registry();
        // The registry must pre-load at least the core chrome actions.
        assert!(r.resolve("chrome.close").is_some());
        assert!(r.resolve("rail.tabs").is_some());
        assert!(r.resolve("rail.bookmarks").is_some());
        assert!(r.resolve("rail.history").is_some());
    }

    #[test]
    fn chrome_close_defaults_to_x() {
        let r = registry();
        let icon = r.resolve("chrome.close").unwrap();
        assert_eq!(icon.name, "x");
        assert_eq!(icon.sprite_id, "icon-x");
    }

    #[test]
    fn rail_tabs_defaults_to_layers() {
        let r = registry();
        let icon = r.resolve("rail.tabs").unwrap();
        assert_eq!(icon.name, "layers");
    }

    // ---- set_icon: valid overrides -----------------------------------------

    #[test]
    fn set_icon_accepts_valid_lucide_override() {
        let mut r = registry();
        // Override chrome.close with a valid bundled lucide icon.
        r.set_icon("chrome.close", "lucide:x").unwrap();
        let icon = r.resolve("chrome.close").unwrap();
        assert_eq!(icon.name, "x");
    }

    #[test]
    fn set_icon_override_is_reflected_in_resolve() {
        let mut r = registry();
        // Change chrome.bookmark to use a different bundled icon.
        r.set_icon("chrome.bookmark", "lucide:bookmark").unwrap();
        let icon = r.resolve("chrome.bookmark").unwrap();
        assert_eq!(icon.name, "bookmark");
        assert_eq!(icon.sprite_id, "icon-bookmark");
    }

    // ---- set_icon: fail-closed rejection -----------------------------------

    #[test]
    fn set_icon_rejects_unknown_pack() {
        let mut r = registry();
        let err = r.set_icon("chrome.close", "phosphor:x-circle").unwrap_err();
        assert!(
            matches!(err, SetIconError::UnknownPack { .. }),
            "expected UnknownPack, got {err:?}"
        );
    }

    #[test]
    fn set_icon_rejects_heroicons_pack() {
        let mut r = registry();
        let err = r.set_icon("chrome.close", "heroicons:x-mark").unwrap_err();
        assert!(matches!(err, SetIconError::UnknownPack { .. }));
    }

    #[test]
    fn set_icon_rejects_inline_svg_pack() {
        // inline:<svg> is explicitly out of scope in v0.1 (ADR-0013).
        let mut r = registry();
        let err = r
            .set_icon("chrome.close", "inline:<svg>...</svg>")
            .unwrap_err();
        assert!(matches!(err, SetIconError::UnknownPack { .. }));
    }

    #[test]
    fn set_icon_rejects_unknown_lucide_name() {
        let mut r = registry();
        // "made-up-icon" is not in BUNDLED_LUCIDE_NAMES.
        let err = r
            .set_icon("chrome.close", "lucide:made-up-icon")
            .unwrap_err();
        assert!(
            matches!(err, SetIconError::UnknownName { .. }),
            "expected UnknownName, got {err:?}"
        );
    }

    #[test]
    fn set_icon_rejects_missing_pack_prefix() {
        let mut r = registry();
        // No colon — cannot parse pack:name.
        let err = r.set_icon("chrome.close", "just-a-name").unwrap_err();
        assert!(
            matches!(err, SetIconError::MissingPackPrefix { .. }),
            "expected MissingPackPrefix, got {err:?}"
        );
    }

    #[test]
    fn set_icon_rejects_unknown_action() {
        let mut r = registry();
        let err = r.set_icon("not.a.real.action", "lucide:x").unwrap_err();
        assert!(
            matches!(err, SetIconError::UnknownAction { .. }),
            "expected UnknownAction, got {err:?}"
        );
    }

    // ---- resolve: missing action -------------------------------------------

    #[test]
    fn resolve_returns_none_for_unregistered_action() {
        let r = registry();
        assert_eq!(r.resolve("unknown.action"), None);
    }

    // ---- All ADR-0013 actions are registered --------------------------------

    #[test]
    fn all_adr_0013_actions_are_registered() {
        let r = registry();
        let required = [
            "chrome.close",
            "chrome.bookmark",
            "chrome.new_tab",
            "chrome.back",
            "chrome.forward",
            "chrome.reload",
            "tab.close",
            "rail.tabs",
            "rail.bookmarks",
            "rail.history",
            "rail.settings",
            "rail.plugin_unbound",
            "collapse.sidebar",
            "statusline.security_https",
            "statusline.security_http",
        ];
        for action in required {
            assert!(
                r.has_action(action),
                "icon registry missing action `{action}`"
            );
        }
    }

    // ---- All bundled lucide names are valid in set_icon ---------------------

    #[test]
    fn all_bundled_lucide_names_accepted_by_set_icon() {
        let mut r = registry();
        for name in BUNDLED_LUCIDE_NAMES {
            let source = format!("lucide:{name}");
            // Use chrome.close as the test action (always present).
            assert!(
                r.set_icon("chrome.close", &source).is_ok(),
                "set_icon should accept bundled lucide name `{name}`"
            );
        }
    }
}
