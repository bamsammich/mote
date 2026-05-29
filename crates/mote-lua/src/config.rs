//! Restricted config-Lua context for evaluating `plugins.lua` and
//! `secrets.lua`.
//!
//! This module provides [`eval_config`], which evaluates a user config chunk
//! (e.g. the contents of `~/.config/mote/plugins.lua` or
//! `~/.config/mote/secrets.lua`) in a **restricted sandbox** that is separate
//! from the plugin sandbox ([`crate::sandbox::new_sandbox`]).
//!
//! ## How it differs from the plugin sandbox
//!
//! Both sandboxes apply the same hardening — `io`, `os`, `package`, `debug`,
//! `ffi`, and all dynamic-loading globals (`load`, `loadstring`, `loadfile`,
//! `dofile`, `require`) are removed. The config sandbox additionally exposes
//! **no plugin host API at all** (no `mote.on`, no event hooks, no capability
//! surface). Instead it exposes only four config-capture functions:
//!
//! | Function | What it does |
//! |---|---|
//! | `mote.plugins({ key = { source = "…", version = "…"? }, … })` | Captures the plugin declarations |
//! | `mote.dev_mode({ directories = {…}, plugins = {…} })` | Captures dev-mode dirs/plugins |
//! | `mote.updates.configure({ check_first_party = "weekly"\|"daily"\|"never" })` | Captures update cadence |
//! | `mote.secrets.define({ name = { backend = "…", … }, … })` | Captures raw secret declarations |
//!
//! These functions are Rust closures that record their argument into shared
//! state; they have **no side effects on the browser** — they only capture.
//!
//! ## Calling `mote.plugins()` more than once
//!
//! Calling `mote.plugins` more than once is allowed; entries are **accumulated**
//! across calls, with same-key-later-wins semantics:
//!
//! - Keys introduced by a later call that did not appear in an earlier call are
//!   **added** to the captured list.
//! - If a later call re-declares a key that was already captured, the later
//!   entry **replaces** that key's entry in place (preserving the key's original
//!   position in the list).
//!
//! This is required by the `import --write` flow (ADR-0006): that operation
//! appends a second `mote.plugins({…})` call to the user's existing
//! `plugins.lua`. Replacing the whole list would silently discard all
//! previously-declared plugins. Per-identity overlay replacement across
//! separate config *files* happens at the loader/compose level, not here.
//!
//! ## Extensibility
//!
//! This module is designed to grow additively. New `mote.*` config functions
//! can be added by installing additional closures into the `mote` table in
//! [`install_config_api`] before the chunk is evaluated. The deferred
//! settings/config-is-Lua system (ROADMAP: Phase 2 "Settings model — deferred")
//! will share this module rather than replacing it.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value};
use thiserror::Error;

use crate::sandbox::DENIED_BASE_GLOBALS;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The update check cadence for first-party bundled plugins.
///
/// Parsed from the `check_first_party` key in
/// `mote.updates.configure({ check_first_party = "…" })`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateCadence {
    /// Check for updates once per week (the default).
    #[default]
    Weekly,
    /// Check for updates once per day.
    Daily,
    /// Never check for updates automatically.
    Never,
}

impl UpdateCadence {
    /// Parses the wire string from the config file.
    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "weekly" => Some(Self::Weekly),
            "daily" => Some(Self::Daily),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// A single plugin declared in `plugins.lua` or `managed.lua`.
///
/// The `key` **is** the plugin's canonical `PluginName` (ADR-0006 / R4 —
/// resolved Option 2): `plugins.lua` keys must be valid quoted hyphenated
/// `PluginName`s (e.g. `["vim-mode"] = {…}`). The key is the authoritative
/// identity; it is validated into a [`mote_types::PluginName`] by
/// `mote-pluginmgr::compose` so this crate remains free of the validation
/// dependency. At sync, `mote-pluginmgr` confirms the key matches the resolved
/// manifest name (DESIGN §Manifest and lock file).
///
/// `source` is the **raw, unparsed** source string exactly as written by the
/// user. Parsing into a `Source` enum (e.g. `github:`, `path:`, `bundled`) is
/// the responsibility of `mote-pluginmgr::Source::parse`, not this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// The quoted `plugins.lua` key — a valid `PluginName` (lowercase
    /// hyphenated identifier). Validated by `mote-pluginmgr::compose`
    /// (ADR-0006 / R4: the key is the canonical plugin identity, not cosmetic).
    pub key: String,
    /// The raw source string, e.g. `"github:mote-browser/adblock"`,
    /// `"path:~/code/my-plugin"`, `"bundled"`.
    pub source: String,
    /// The optional version/tag/branch constraint.
    pub version: Option<String>,
}

/// Dev-mode configuration captured from `mote.dev_mode({…})`.
///
/// A plugin whose resolved directory is under any of `directories`, or whose
/// manifest name appears in `plugins`, is treated as a dev-mode plugin:
/// auto-approved on every load and every permission change, and visually marked
/// in the integrity panel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DevModeConfig {
    /// Filesystem directories whose contents are auto-approved for dev mode.
    pub directories: Vec<String>,
    /// Explicit plugin names (as written in `plugins`, not necessarily
    /// `PluginName`-validated) that are auto-approved for dev mode.
    pub plugins: Vec<String>,
}

/// Update check policy captured from `mote.updates.configure({…})`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatesConfig {
    /// How often to check for first-party plugin updates.
    pub check_first_party: UpdateCadence,
}

/// A raw parameter value from a secret entry.
///
/// The parser stays backend-agnostic; `mote-secrets` interprets these per
/// backend. Only string and boolean values are accepted — other Lua types
/// are rejected at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretParam {
    /// A string parameter value.
    Str(String),
    /// A boolean parameter value.
    Bool(bool),
}

/// A single secret declared in `secrets.lua` via `mote.secrets.define({…})`.
///
/// `name` is the map key; `backend` and `params` are raw — typed and validated
/// by `mote-pluginmgr`/`mote-secrets`, not here (mirrors [`PluginEntry`] where
/// `source` stays raw).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretEntry {
    /// The secret's logical name as written in the `mote.secrets.define` table.
    pub name: String,
    /// The raw backend identifier, e.g. `"env"`, `"keyring"`, `"file"`,
    /// `"age"`, `"password-manager"`. Validated by downstream crates.
    pub backend: String,
    /// All remaining fields from the entry table, captured as raw param values.
    /// Keys are field names; values are [`SecretParam::Str`] or
    /// [`SecretParam::Bool`].
    pub params: std::collections::BTreeMap<String, SecretParam>,
}

/// The fully-typed result of evaluating a `plugins.lua` config chunk.
///
/// All config functions contribute to this struct. Fields that were never
/// called retain their defaults (see each field's documentation).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSpec {
    /// Ordered list of plugin declarations. Empty if `mote.plugins` was never
    /// called (or was called with an empty table).
    pub plugins: Vec<PluginEntry>,
    /// Dev-mode configuration. All sub-fields are empty by default.
    pub dev_mode: DevModeConfig,
    /// Update policy. Defaults to `check_first_party = "weekly"`.
    pub updates: UpdatesConfig,
    /// Secret declarations. Empty if `mote.secrets.define` was never called.
    pub secrets: Vec<SecretEntry>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// An error raised while evaluating a config-Lua chunk.
///
/// Every error variant carries enough context to produce a clear, actionable
/// message — no panics, no opaque Lua errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The sandboxed Lua state could not be constructed.
    #[error("failed to construct config sandbox: {0}")]
    Sandbox(#[source] mlua::Error),

    /// The chunk failed to compile or raised a Lua error at runtime.
    #[error("config chunk evaluation failed: {0}")]
    Evaluate(#[source] mlua::Error),

    /// A config function received the wrong argument type (not a table).
    #[error("`{function}` requires a table argument, got {got}")]
    BadArgument {
        /// The function that received the bad argument.
        function: &'static str,
        /// The Lua type name actually received.
        got: String,
    },

    /// An entry in the `mote.plugins({…})` table had a non-table value, or
    /// its `source`/`version` field had the wrong type.
    #[error("plugin entry `{key}` is malformed: {got}")]
    BadEntry {
        /// The Lua key of the offending entry.
        key: String,
        /// Description of the problem (e.g. the unexpected type).
        got: String,
    },

    /// An entry in the `mote.plugins({…})` table was missing the required
    /// `source` field.
    #[error("plugin entry `{key}` is missing the required `source` field")]
    MissingSource {
        /// The Lua key of the offending entry.
        key: String,
    },

    /// The `check_first_party` value is not a recognized [`UpdateCadence`].
    #[error(
        "unrecognized `check_first_party` value {0:?}; expected \"weekly\", \"daily\", or \"never\""
    )]
    InvalidUpdateCadence(String),

    /// An entry in `mote.secrets.define({…})` had a non-table value, or one
    /// of its parameter fields had an unsupported type (neither string nor bool).
    #[error("secret entry `{name}` is malformed: {got}")]
    BadSecretEntry {
        /// The Lua key of the offending secret entry.
        name: String,
        /// Description of the problem (e.g. the unexpected type).
        got: String,
    },

    /// An entry in `mote.secrets.define({…})` was missing the required
    /// `backend` field.
    #[error("secret entry `{name}` is missing the required `backend` field")]
    MissingSecretBackend {
        /// The Lua key of the offending secret entry.
        name: String,
    },

    /// An unexpected Lua operation failed (e.g. a metamethod error while
    /// reading a field).
    #[error("unexpected Lua error while reading config: {0}")]
    Lua(#[source] mlua::Error),
}

// ---------------------------------------------------------------------------
// Shared capture state
// ---------------------------------------------------------------------------

/// Mutable state shared (via `Rc<RefCell<…>>`) between the Rust closures
/// installed into the `mote` table and the post-evaluation extraction logic.
#[derive(Debug, Default)]
struct Capture {
    plugins: Vec<PluginEntry>,
    dev_mode: DevModeConfig,
    updates: UpdatesConfig,
    secrets: Vec<SecretEntry>,
}

// ---------------------------------------------------------------------------
// Sandbox construction (config-specific)
// ---------------------------------------------------------------------------

/// Constructs a fresh config-sandbox [`Lua`] state.
///
/// Uses the same [`StdLib`] subset and the same base-global nil-out pass as
/// the plugin sandbox ([`crate::sandbox::new_sandbox`]), so the same hardening
/// properties hold. The config sandbox loads **no** plugin host API; callers
/// install the `mote.*` config-capture functions separately.
fn new_config_sandbox() -> Result<Lua, ConfigError> {
    // Same lib subset as the plugin sandbox: TABLE | STRING | MATH | BIT | JIT.
    // Explicitly built (not from ALL_SAFE) so a future mlua widening cannot
    // silently grant new modules here — same rationale as sandbox.rs.
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::BIT | StdLib::JIT;

    let lua = Lua::new_with(libs, LuaOptions::default()).map_err(ConfigError::Sandbox)?;

    let globals = lua.globals();

    // Remove the same base-library globals the plugin sandbox removes.
    for name in DENIED_BASE_GLOBALS {
        globals.set(*name, mlua::Nil).map_err(ConfigError::Lua)?;
    }

    // nil out `string.dump` (same as plugin sandbox).
    if let Value::Table(string_tbl) = globals.get::<Value>("string").map_err(ConfigError::Lua)? {
        string_tbl
            .set("dump", mlua::Nil)
            .map_err(ConfigError::Lua)?;
    }

    Ok(lua)
}

// ---------------------------------------------------------------------------
// API installation helpers
// ---------------------------------------------------------------------------

/// Builds the `mote.plugins` capture closure.
fn make_plugins_fn(lua: &Lua, capture: &Rc<RefCell<Capture>>) -> Result<Function, ConfigError> {
    let cap = Rc::clone(capture);
    lua.create_function(move |_lua, arg: Value| {
        let Value::Table(t) = arg else {
            return Err(mlua::Error::runtime(format!(
                "`mote.plugins` requires a table argument, got {}",
                arg.type_name()
            )));
        };
        let mut entries = Vec::new();
        for pair in t.pairs::<Value, Value>() {
            let (k, v) = pair?;
            let key = match &k {
                Value::String(s) => s.to_str()?.to_owned(),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "plugin key must be a string, got {}",
                        other.type_name()
                    )));
                }
            };
            let Value::Table(entry_tbl) = v else {
                return Err(mlua::Error::runtime(format!(
                    "plugin entry `{key}` must be a table, got {}",
                    v.type_name()
                )));
            };
            let source_val: Value = entry_tbl.get("source")?;
            let source = match source_val {
                Value::String(s) => s.to_str()?.to_owned(),
                Value::Nil => {
                    return Err(mlua::Error::runtime(format!(
                        "plugin entry `{key}` is missing the required `source` field"
                    )));
                }
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "plugin entry `{key}` `source` must be a string, got {}",
                        other.type_name()
                    )));
                }
            };
            let version_val: Value = entry_tbl.get("version")?;
            let version = match version_val {
                Value::Nil => None,
                Value::String(s) => Some(s.to_str()?.to_owned()),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "plugin entry `{key}` `version` must be a string, got {}",
                        other.type_name()
                    )));
                }
            };
            entries.push(PluginEntry {
                key,
                source,
                version,
            });
        }
        // Accumulate with same-key-later-wins: merge `entries` into the
        // captured list. For each incoming entry, if its key already exists
        // in the list it is updated in place (preserving position); new keys
        // are appended. This is required by `import --write` (ADR-0006), which
        // appends a second `mote.plugins({…})` call rather than rewriting the
        // file. Per-identity overlay replacement across separate files happens
        // at the loader/compose level, not here.
        let mut cap_mut = cap.borrow_mut();
        for entry in entries {
            if let Some(existing) = cap_mut.plugins.iter_mut().find(|e| e.key == entry.key) {
                *existing = entry;
            } else {
                cap_mut.plugins.push(entry);
            }
        }
        Ok(())
    })
    .map_err(ConfigError::Lua)
}

/// Builds the `mote.dev_mode` capture closure.
fn make_dev_mode_fn(lua: &Lua, capture: &Rc<RefCell<Capture>>) -> Result<Function, ConfigError> {
    let cap = Rc::clone(capture);
    lua.create_function(move |_lua, arg: Value| {
        let Value::Table(t) = arg else {
            return Err(mlua::Error::runtime(format!(
                "`mote.dev_mode` requires a table argument, got {}",
                arg.type_name()
            )));
        };
        let directories = string_array_field(&t, "directories")?;
        let plugins = string_array_field(&t, "plugins")?;
        cap.borrow_mut().dev_mode = DevModeConfig {
            directories,
            plugins,
        };
        Ok(())
    })
    .map_err(ConfigError::Lua)
}

/// Builds the `mote.updates.configure` capture closure.
fn make_configure_fn(lua: &Lua, capture: &Rc<RefCell<Capture>>) -> Result<Function, ConfigError> {
    let cap = Rc::clone(capture);
    lua.create_function(move |_lua, arg: Value| {
        let Value::Table(t) = arg else {
            return Err(mlua::Error::runtime(format!(
                "`mote.updates.configure` requires a table argument, got {}",
                arg.type_name()
            )));
        };
        let cadence_val: Value = t.get("check_first_party")?;
        let cadence = match cadence_val {
            Value::Nil => UpdateCadence::default(),
            Value::String(s) => {
                let s = s.to_str()?.to_owned();
                UpdateCadence::from_wire(&s).ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "unrecognized `check_first_party` value {s:?}; \
                         expected \"weekly\", \"daily\", or \"never\""
                    ))
                })?
            }
            other => {
                return Err(mlua::Error::runtime(format!(
                    "`check_first_party` must be a string, got {}",
                    other.type_name()
                )));
            }
        };
        cap.borrow_mut().updates = UpdatesConfig {
            check_first_party: cadence,
        };
        Ok(())
    })
    .map_err(ConfigError::Lua)
}

/// Builds the `mote.secrets.define` capture closure.
///
/// Mirrors `make_plugins_fn`: iterates the outer table (key = secret name);
/// each value must be a table with a required string `backend`; all other
/// fields are collected into `params` (`Value::String` → [`SecretParam::Str`],
/// `Value::Boolean` → [`SecretParam::Bool`]; other types → runtime error).
/// Same-name-later-wins accumulation mirrors the plugins behaviour.
fn make_secrets_fn(lua: &Lua, capture: &Rc<RefCell<Capture>>) -> Result<Function, ConfigError> {
    let cap = Rc::clone(capture);
    lua.create_function(move |_lua, arg: Value| {
        let Value::Table(t) = arg else {
            return Err(mlua::Error::runtime(format!(
                "`mote.secrets.define` requires a table argument, got {}",
                arg.type_name()
            )));
        };
        let mut entries = Vec::new();
        for pair in t.pairs::<Value, Value>() {
            let (k, v) = pair?;
            let name = match &k {
                Value::String(s) => s.to_str()?.to_owned(),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "secret key must be a string, got {}",
                        other.type_name()
                    )));
                }
            };
            let Value::Table(entry_tbl) = v else {
                return Err(mlua::Error::runtime(format!(
                    "secret entry `{name}` must be a table, got {}",
                    v.type_name()
                )));
            };
            // Extract required `backend` field.
            let backend_val: Value = entry_tbl.get("backend")?;
            let backend = match backend_val {
                Value::String(s) => s.to_str()?.to_owned(),
                Value::Nil => {
                    return Err(mlua::Error::runtime(format!(
                        "secret entry `{name}` is missing the required `backend` field"
                    )));
                }
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "secret entry `{name}` `backend` must be a string, got {}",
                        other.type_name()
                    )));
                }
            };
            // Collect remaining fields into params.
            let mut params = std::collections::BTreeMap::new();
            for field_pair in entry_tbl.pairs::<Value, Value>() {
                let (fk, fv) = field_pair?;
                let field_name = match &fk {
                    Value::String(s) => s.to_str()?.to_owned(),
                    _ => continue, // non-string keys are ignored
                };
                if field_name == "backend" {
                    continue; // already extracted
                }
                let param = match fv {
                    Value::String(s) => SecretParam::Str(s.to_str()?.to_owned()),
                    Value::Boolean(b) => SecretParam::Bool(b),
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "secret entry `{name}` param `{field_name}` must be a string or \
                             boolean, got {}",
                            other.type_name()
                        )));
                    }
                };
                params.insert(field_name, param);
            }
            entries.push(SecretEntry {
                name,
                backend,
                params,
            });
        }
        // Accumulate with same-name-later-wins (mirrors make_plugins_fn).
        let mut cap_mut = cap.borrow_mut();
        for entry in entries {
            if let Some(existing) = cap_mut.secrets.iter_mut().find(|e| e.name == entry.name) {
                *existing = entry;
            } else {
                cap_mut.secrets.push(entry);
            }
        }
        Ok(())
    })
    .map_err(ConfigError::Lua)
}

// ---------------------------------------------------------------------------
// API installation
// ---------------------------------------------------------------------------

/// Installs the `mote.plugins`, `mote.dev_mode`, `mote.updates.configure`,
/// and `mote.secrets.define` closures into `lua`, wired to `capture`.
///
/// All closures are Rust functions (not Lua functions) that validate their
/// argument and record into `capture`. They have **no side effects on the
/// browser** — they only capture.
fn install_config_api(lua: &Lua, capture: &Rc<RefCell<Capture>>) -> Result<(), ConfigError> {
    let globals = lua.globals();

    // Create the top-level `mote` table.
    let mote: Table = lua.create_table().map_err(ConfigError::Lua)?;

    mote.set("plugins", make_plugins_fn(lua, capture)?)
        .map_err(ConfigError::Lua)?;
    mote.set("dev_mode", make_dev_mode_fn(lua, capture)?)
        .map_err(ConfigError::Lua)?;

    // mote.updates is a sub-table with a `configure` function.
    let updates_tbl: Table = lua.create_table().map_err(ConfigError::Lua)?;
    updates_tbl
        .set("configure", make_configure_fn(lua, capture)?)
        .map_err(ConfigError::Lua)?;
    mote.set("updates", updates_tbl).map_err(ConfigError::Lua)?;

    // mote.secrets is a sub-table with a `define` function.
    let secrets_tbl: Table = lua.create_table().map_err(ConfigError::Lua)?;
    secrets_tbl
        .set("define", make_secrets_fn(lua, capture)?)
        .map_err(ConfigError::Lua)?;
    mote.set("secrets", secrets_tbl).map_err(ConfigError::Lua)?;

    globals.set("mote", mote).map_err(ConfigError::Lua)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Attempts to classify a Lua runtime error raised by one of the config
/// closures into a typed [`ConfigError`].
///
/// The config closures use [`mlua::Error::runtime`] with structured messages;
/// this function pattern-matches on those messages to surface typed errors
/// to the caller. If the message does not match a known pattern, the error
/// falls through as [`ConfigError::Evaluate`].
fn classify_lua_error(err: mlua::Error) -> ConfigError {
    let msg = err.to_string();

    if msg.contains("`mote.plugins` requires a table argument") {
        let got = msg
            .split("got ")
            .nth(1)
            .unwrap_or("unknown")
            .trim()
            .to_owned();
        return ConfigError::BadArgument {
            function: "mote.plugins",
            got,
        };
    }
    if msg.contains("`mote.dev_mode` requires a table argument") {
        let got = msg
            .split("got ")
            .nth(1)
            .unwrap_or("unknown")
            .trim()
            .to_owned();
        return ConfigError::BadArgument {
            function: "mote.dev_mode",
            got,
        };
    }
    if msg.contains("`mote.updates.configure` requires a table argument") {
        let got = msg
            .split("got ")
            .nth(1)
            .unwrap_or("unknown")
            .trim()
            .to_owned();
        return ConfigError::BadArgument {
            function: "mote.updates.configure",
            got,
        };
    }

    // "plugin entry `<key>` must be a table, got <type>"
    if msg.contains("must be a table, got")
        && let Some(key) = extract_key_from_message(&msg, "plugin entry `", "`")
    {
        let got = msg
            .split("got ")
            .last()
            .unwrap_or("unknown")
            .trim()
            .to_owned();
        return ConfigError::BadEntry { key, got };
    }

    // "plugin entry `<key>` `source` must be a string, got <type>"
    if msg.contains("must be a string, got")
        && msg.contains("plugin entry")
        && let Some(key) = extract_key_from_message(&msg, "plugin entry `", "`")
    {
        let got = msg
            .split("got ")
            .last()
            .unwrap_or("unknown")
            .trim()
            .to_owned();
        return ConfigError::BadEntry { key, got };
    }

    // "plugin entry `<key>` is missing the required `source` field"
    if msg.contains("is missing the required `source` field")
        && let Some(key) = extract_key_from_message(&msg, "plugin entry `", "`")
    {
        return ConfigError::MissingSource { key };
    }

    // "unrecognized `check_first_party` value "…";"
    if msg.contains("unrecognized `check_first_party` value")
        && let Some(value) = extract_quoted_value(&msg)
    {
        return ConfigError::InvalidUpdateCadence(value);
    }

    // "secret entry `<name>` must be a table, got <type>"
    // "secret entry `<name>` `backend` must be a string, got <type>"
    // "secret entry `<name>` param `<field>` must be a string or boolean, got <type>"
    if msg.contains("secret entry `")
        && msg.contains("must be")
        && let Some(name) = extract_key_from_message(&msg, "secret entry `", "`")
    {
        let got = msg
            .split("got ")
            .last()
            .unwrap_or("unknown")
            .trim()
            .to_owned();
        return ConfigError::BadSecretEntry { name, got };
    }

    // "secret entry `<name>` is missing the required `backend` field"
    if msg.contains("is missing the required `backend` field")
        && let Some(name) = extract_key_from_message(&msg, "secret entry `", "`")
    {
        return ConfigError::MissingSecretBackend { name };
    }

    ConfigError::Evaluate(err)
}

/// Extracts a key from a structured error message pattern:
/// `…prefix<KEY>suffix…`.
fn extract_key_from_message(msg: &str, prefix: &str, suffix: &str) -> Option<String> {
    let after_prefix = msg.split(prefix).nth(1)?;
    let key = after_prefix.split(suffix).next()?;
    Some(key.to_owned())
}

/// Extracts the double-quoted value from an error message like:
/// `… value "someday"; …`.
fn extract_quoted_value(msg: &str) -> Option<String> {
    let after = msg.split("value ").nth(1)?;
    let inner = after.trim_start_matches('"');
    let value = inner.split('"').next()?;
    Some(value.to_owned())
}

// ---------------------------------------------------------------------------
// Lua helper: read an optional array-of-strings field from a table
// ---------------------------------------------------------------------------

/// Reads an optional array-of-strings field from a Lua table.
///
/// Absent → empty `Vec`. Present-but-not-a-table or non-string element →
/// `mlua::Error::runtime` (propagates through the closure as a Lua error).
fn string_array_field(t: &Table, field: &'static str) -> Result<Vec<String>, mlua::Error> {
    let val: Value = t.get(field)?;
    match val {
        Value::Nil => Ok(Vec::new()),
        Value::Table(arr) => {
            let mut out = Vec::new();
            for item in arr.sequence_values::<Value>() {
                match item? {
                    Value::String(s) => out.push(s.to_str()?.to_owned()),
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "`{field}` elements must be strings, got {}",
                            other.type_name()
                        )));
                    }
                }
            }
            Ok(out)
        }
        other => Err(mlua::Error::runtime(format!(
            "`{field}` must be a table, got {}",
            other.type_name()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Evaluates a user config Lua chunk (e.g. the contents of `plugins.lua`) in a
/// **restricted config sandbox** and returns the captured [`ConfigSpec`].
///
/// The sandbox applies the same hardening as the plugin sandbox (no `io`, `os`,
/// `package`/`require`, `debug`, `ffi`, dynamic code loading) and exposes only
/// three config-capture functions via a `mote` global:
///
/// - `mote.plugins({…})` — captures plugin declarations (accumulated across calls; same-key-later-wins).
/// - `mote.dev_mode({…})` — captures dev-mode dirs/plugins.
/// - `mote.updates.configure({…})` — captures update cadence.
///
/// `chunk_name` is used in Lua error messages and tracebacks.
///
/// # Errors
///
/// Returns a [`ConfigError`] — never a panic — if the sandbox cannot be built,
/// the chunk fails to evaluate, or any config function receives a malformed
/// argument. Every error variant carries structured context for the caller.
pub fn eval_config(source: &str, chunk_name: &str) -> Result<ConfigSpec, ConfigError> {
    let lua = new_config_sandbox()?;
    let capture: Rc<RefCell<Capture>> = Rc::new(RefCell::new(Capture::default()));
    install_config_api(&lua, &capture)?;

    lua.load(source)
        .set_name(chunk_name)
        .exec()
        .map_err(classify_lua_error)?;

    let cap = capture.borrow();
    Ok(ConfigSpec {
        plugins: cap.plugins.clone(),
        dev_mode: cap.dev_mode.clone(),
        updates: cap.updates.clone(),
        secrets: cap.secrets.clone(),
    })
}
