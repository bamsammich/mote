//! Declarative plugin module loading (DESIGN §Enforcement Rules step 2;
//! ADR-0001).
//!
//! Given a plugin's Lua source, [`load_plugin`] evaluates the chunk in a
//! sandboxed state to obtain the returned `M` table and extracts its
//! *declarative surface* — the manifest, and the **key names** declared in
//! `M.hooks` / `M.events` / `M.api` — **without calling `setup()`**. That last
//! property is the whole point: ADR-0001 makes contract conformance (load-step
//! 3) a static table inspection, so the runtime can read what a plugin *will*
//! do before any plugin code that might do something runs.
//!
//! ## Scope boundary
//!
//! This crate extracts and type-checks the declarative surface. It does **not**
//! validate `permissions` / `capabilities` / `consumes` against the permission
//! and capability registries (DESIGN §Enforcement Rules step 1) — that is
//! `mote-registry`'s responsibility, and it owns the registry vocabulary. Here
//! those lists are surfaced as raw `String`s exactly as declared. The two
//! fields with a shared-vocabulary validator in `mote-types` — `name`
//! ([`PluginName`]) and `schema` ([`SchemaVersion`]) — are parsed into their
//! validated types, because doing so requires no registry and a malformed value
//! there is unambiguously the plugin's fault.

use mlua::{Lua, Table, Value};
use mote_types::{PluginName, SchemaVersion};

use crate::error::LuaError;
use crate::sandbox::new_sandbox;

/// How a plugin scopes its identity (DESIGN §Manifest Example
/// `identity_scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityScope {
    /// One isolated state per browser identity.
    PerIdentity,
    /// A single state shared across all identities.
    Global,
    /// The user decides at install time.
    UserChoice,
}

impl IdentityScope {
    /// Parses the wire string used in a manifest (`per_identity`, `global`,
    /// `user_choice`).
    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "per_identity" => Some(Self::PerIdentity),
            "global" => Some(Self::Global),
            "user_choice" => Some(Self::UserChoice),
            _ => None,
        }
    }
}

/// The extracted, type-checked declarative manifest of a plugin.
///
/// `permissions`, `capabilities`, and `consumes` are surfaced verbatim as
/// declared; registry validation lives in `mote-registry` (see the module
/// documentation's scope boundary).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Manifest {
    /// Targeted manifest schema version (`schema = "v1"`).
    pub schema: SchemaVersion,
    /// The plugin's validated name.
    pub name: PluginName,
    /// The plugin's self-declared version string (opaque to this crate).
    pub version: String,
    /// Requested permissions, verbatim (registry-validated downstream).
    pub permissions: Vec<String>,
    /// Capabilities the plugin claims to fulfill, verbatim.
    pub capabilities: Vec<String>,
    /// Capabilities the plugin consumes, verbatim.
    pub consumes: Vec<String>,
    /// Identity scope, if declared.
    pub identity_scope: Option<IdentityScope>,
    /// Homepage URL, if declared.
    pub homepage: Option<String>,
    /// Integrity checksum string (`blake3:...`), if declared. Verbatim; parsing
    /// into `mote_types::Checksum` is the integrity layer's concern.
    pub checksum: Option<String>,
}

/// A plugin whose module body has been evaluated and whose declarative surface
/// has been extracted — but whose `setup()` has **not** been called.
///
/// Holds the validated [`Manifest`], the declared key names from
/// `M.hooks` / `M.events` / `M.api`, a flag for whether `M.setup` is present,
/// and a handle to the loaded `M` table for a later `setup()` / dispatch layer.
/// The owning [`Lua`] state is retained so the module handle stays valid.
#[derive(Debug)]
pub struct LoadedPlugin {
    lua: Lua,
    module: Table,
    manifest: Manifest,
    hook_keys: Vec<String>,
    event_keys: Vec<String>,
    api_keys: Vec<String>,
    has_setup: bool,
}

impl LoadedPlugin {
    /// The extracted, type-checked manifest.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The key names declared in `M.hooks` (e.g. `net:intercept_request`).
    ///
    /// Sorted for deterministic ordering; Lua table iteration order is
    /// otherwise unspecified.
    #[must_use]
    pub fn hook_keys(&self) -> &[String] {
        &self.hook_keys
    }

    /// The key names declared in `M.events` (e.g.
    /// `password-manager-form-services:form-detected`).
    #[must_use]
    pub fn event_keys(&self) -> &[String] {
        &self.event_keys
    }

    /// The function names declared in `M.api`.
    #[must_use]
    pub fn api_keys(&self) -> &[String] {
        &self.api_keys
    }

    /// Whether the module declares a `setup` field.
    ///
    /// Presence is detected during load; the function is **never** invoked here
    /// (ADR-0001). A later layer calls it after all four load-time checks pass.
    #[must_use]
    pub const fn has_setup(&self) -> bool {
        self.has_setup
    }

    /// The sandboxed Lua state the module was loaded into.
    ///
    /// Exposed so a later layer can marshal the host API and call `setup()` /
    /// dispatch handlers against the same state.
    #[must_use]
    pub const fn lua(&self) -> &Lua {
        &self.lua
    }

    /// The loaded `M` table.
    ///
    /// Exposed for the later `setup()` / dispatch layer to read handler
    /// functions out of `hooks` / `events` / `api`.
    #[must_use]
    pub const fn module(&self) -> &Table {
        &self.module
    }
}

/// Loads a plugin from Lua source in a fresh sandboxed state and extracts its
/// declarative surface, **without calling `setup()`**.
///
/// `chunk_name` labels the chunk in Lua error messages and tracebacks (use the
/// plugin path or name).
///
/// # Errors
///
/// Returns a [`LuaError`] — never a panic — if the sandbox cannot be built, the
/// chunk fails to compile or evaluate, the chunk does not return a table, or the
/// manifest is missing / ill-typed. Malformed input is an error, not a crash.
pub fn load_plugin(source: &str, chunk_name: &str) -> Result<LoadedPlugin, LuaError> {
    let lua = new_sandbox()?;
    load_plugin_in(lua, source, chunk_name)
}

/// Like [`load_plugin`] but loads into a caller-provided sandboxed state.
///
/// Useful when the caller wants to pre-install host API onto the state before
/// evaluating the module body. The state should have been built with
/// [`new_sandbox`](crate::sandbox::new_sandbox).
///
/// # Errors
///
/// Same conditions as [`load_plugin`].
pub fn load_plugin_in(lua: Lua, source: &str, chunk_name: &str) -> Result<LoadedPlugin, LuaError> {
    let returned: Value = lua
        .load(source)
        .set_name(chunk_name)
        .eval()
        .map_err(LuaError::Evaluate)?;

    let Value::Table(module) = returned else {
        return Err(LuaError::NotATable {
            got: returned.type_name(),
        });
    };

    let manifest = extract_manifest(&module)?;
    let hook_keys = string_keys(&module, "hooks")?;
    let event_keys = string_keys(&module, "events")?;
    let api_keys = string_keys(&module, "api")?;

    // Detect `setup` presence WITHOUT invoking it (ADR-0001). We only read the
    // field; we never call it.
    let setup: Value = module.get("setup").map_err(LuaError::Lua)?;
    let has_setup = !matches!(setup, Value::Nil);

    Ok(LoadedPlugin {
        lua,
        module,
        manifest,
        hook_keys,
        event_keys,
        api_keys,
        has_setup,
    })
}

/// Reads the sorted string key names of an optional module-level declaration
/// table (`hooks`, `events`, `api`).
///
/// Absent ⇒ empty. Present-but-not-a-table ⇒
/// [`LuaError::NotADeclarationTable`]. Non-string keys are ignored (Lua array
/// parts and numeric keys are not part of the declared surface).
fn string_keys(module: &Table, field: &'static str) -> Result<Vec<String>, LuaError> {
    let value: Value = module.get(field).map_err(LuaError::Lua)?;
    let table = match value {
        Value::Nil => return Ok(Vec::new()),
        Value::Table(t) => t,
        other => {
            return Err(LuaError::NotADeclarationTable {
                field,
                got: other.type_name(),
            });
        }
    };

    let mut keys = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        let (k, _v) = pair.map_err(LuaError::Lua)?;
        if let Value::String(s) = k {
            keys.push(s.to_str().map_err(LuaError::Lua)?.to_owned());
        }
    }
    keys.sort_unstable();
    Ok(keys)
}

/// Extracts and type-checks the `manifest` table.
fn extract_manifest(module: &Table) -> Result<Manifest, LuaError> {
    let manifest_val: Value = module.get("manifest").map_err(LuaError::Lua)?;
    let Value::Table(m) = manifest_val else {
        return Err(LuaError::MissingManifest);
    };

    let schema_str = required_string(&m, "schema")?;
    let schema = schema_str
        .parse::<SchemaVersion>()
        .map_err(LuaError::InvalidSchemaVersion)?;

    let name_str = required_string(&m, "name")?;
    let name = PluginName::new(name_str).map_err(LuaError::InvalidPluginName)?;

    let version = required_string(&m, "version")?;

    let permissions = string_array(&m, "permissions")?;
    let capabilities = string_array(&m, "capabilities")?;
    let consumes = string_array(&m, "consumes")?;

    let identity_scope = match optional_string(&m, "identity_scope")? {
        None => None,
        Some(s) => Some(
            IdentityScope::from_wire(&s).ok_or(LuaError::ManifestFieldType {
                field: "identity_scope",
                expected: "one of \"per_identity\" | \"global\" | \"user_choice\"",
                got: "other string",
            })?,
        ),
    };

    let homepage = optional_string(&m, "homepage")?;
    let checksum = optional_string(&m, "checksum")?;

    Ok(Manifest {
        schema,
        name,
        version,
        permissions,
        capabilities,
        consumes,
        identity_scope,
        homepage,
        checksum,
    })
}

/// Reads a required string manifest field.
fn required_string(m: &Table, field: &'static str) -> Result<String, LuaError> {
    match m.get::<Value>(field).map_err(LuaError::Lua)? {
        Value::String(s) => Ok(s.to_str().map_err(LuaError::Lua)?.to_owned()),
        Value::Nil => Err(LuaError::MissingManifestField { field }),
        other => Err(LuaError::ManifestFieldType {
            field,
            expected: "string",
            got: other.type_name(),
        }),
    }
}

/// Reads an optional string manifest field.
fn optional_string(m: &Table, field: &'static str) -> Result<Option<String>, LuaError> {
    match m.get::<Value>(field).map_err(LuaError::Lua)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_str().map_err(LuaError::Lua)?.to_owned())),
        other => Err(LuaError::ManifestFieldType {
            field,
            expected: "string",
            got: other.type_name(),
        }),
    }
}

/// Reads an optional array-of-strings manifest field.
///
/// Absent ⇒ empty. Present-but-not-a-table, or any non-string element ⇒
/// [`LuaError::ManifestFieldType`]. Preserves declaration order.
fn string_array(m: &Table, field: &'static str) -> Result<Vec<String>, LuaError> {
    let value: Value = m.get(field).map_err(LuaError::Lua)?;
    let table = match value {
        Value::Nil => return Ok(Vec::new()),
        Value::Table(t) => t,
        other => {
            return Err(LuaError::ManifestFieldType {
                field,
                expected: "table (array of strings)",
                got: other.type_name(),
            });
        }
    };

    let mut out = Vec::new();
    for item in table.sequence_values::<Value>() {
        match item.map_err(LuaError::Lua)? {
            Value::String(s) => out.push(s.to_str().map_err(LuaError::Lua)?.to_owned()),
            other => {
                return Err(LuaError::ManifestFieldType {
                    field,
                    expected: "string element",
                    got: other.type_name(),
                });
            }
        }
    }
    Ok(out)
}
