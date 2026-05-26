//! Declarative module-loading tests (DESIGN §Enforcement step 2; ADR-0001).

use mote_lua::{IdentityScope, LuaError, load_plugin};

/// A representative declarative plugin module, modeled on the DESIGN worked
/// example: a manifest plus declarative `hooks` / `events` / `api` tables and a
/// `setup` that, if ever run, would be observable.
const REPRESENTATIVE: &str = r#"
local M = {}

M.manifest = {
  schema = "v1",
  name = "password-manager-1password",
  version = "1.0.0",
  permissions = {
    "http:fetch:https://*.1password.com/*",
    "storage:persistent",
    "identity:read_current",
  },
  capabilities = { "password-manager:provider" },
  consumes = { "password-manager-form-services" },
  identity_scope = "user_choice",
  homepage = "https://example.com/1password",
  checksum = "blake3:abc123",
}

M.hooks = {
  ["net:intercept_request"] = { priority = 70 },
  ["page:on_load"] = {},
}

M.events = {
  ["password-manager-form-services:form-detected"] = function(form) return form end,
}

M.api = {
  show_autofill_picker = function(items) return items end,
  inject_isolated = function(script, world) return script end,
}

function M.setup()
  -- If this ever runs during load, the side effect below would be observable.
  error("setup() must not run during load")
end

return M
"#;

#[test]
fn extracts_manifest_fields() {
    let plugin = load_plugin(REPRESENTATIVE, "1password").expect("loads");
    let m = plugin.manifest();

    assert_eq!(m.schema, mote_types::SchemaVersion::V1);
    assert_eq!(m.name.as_str(), "password-manager-1password");
    assert_eq!(m.version, "1.0.0");
    assert_eq!(
        m.permissions,
        vec![
            "http:fetch:https://*.1password.com/*",
            "storage:persistent",
            "identity:read_current",
        ]
    );
    assert_eq!(m.capabilities, vec!["password-manager:provider"]);
    assert_eq!(m.consumes, vec!["password-manager-form-services"]);
    assert_eq!(m.identity_scope, Some(IdentityScope::UserChoice));
    assert_eq!(m.homepage.as_deref(), Some("https://example.com/1password"));
    assert_eq!(m.checksum.as_deref(), Some("blake3:abc123"));
}

#[test]
fn extracts_hook_event_and_api_key_names() {
    let plugin = load_plugin(REPRESENTATIVE, "1password").expect("loads");

    // Keys are returned sorted for determinism.
    assert_eq!(
        plugin.hook_keys(),
        ["net:intercept_request", "page:on_load"]
    );
    assert_eq!(
        plugin.event_keys(),
        ["password-manager-form-services:form-detected"]
    );
    assert_eq!(
        plugin.api_keys(),
        ["inject_isolated", "show_autofill_picker"]
    );
}

#[test]
fn detects_setup_presence_without_calling_it() {
    // The representative plugin's setup() raises if invoked; a successful load
    // proves it was not called.
    let plugin = load_plugin(REPRESENTATIVE, "1password").expect("load must not run setup()");
    assert!(plugin.has_setup(), "setup presence must be detected");
}

#[test]
fn setup_side_effects_do_not_occur_during_load() {
    // Stronger proof: setup() sets a global. After load, the global must still
    // be unset, because setup() was never called.
    let source = r#"
        local M = {}
        M.manifest = { schema = "v1", name = "side-effect", version = "0.1.0" }
        function M.setup()
          _G.SETUP_RAN = true
        end
        return M
    "#;
    let plugin = load_plugin(source, "side-effect").expect("loads");
    let ran: bool = plugin
        .lua()
        .load("return _G.SETUP_RAN == true")
        .eval()
        .expect("probe runs");
    assert!(!ran, "setup() side effect must not occur during load");
    assert!(plugin.has_setup());
}

#[test]
fn minimal_manifest_without_optional_fields_loads() {
    let source = r#"
        local M = {}
        M.manifest = { schema = "v1", name = "minimal", version = "0.0.1" }
        return M
    "#;
    let plugin = load_plugin(source, "minimal").expect("loads");
    let m = plugin.manifest();
    assert_eq!(m.name.as_str(), "minimal");
    assert!(m.permissions.is_empty());
    assert!(m.capabilities.is_empty());
    assert!(m.consumes.is_empty());
    assert_eq!(m.identity_scope, None);
    assert_eq!(m.homepage, None);
    assert!(plugin.hook_keys().is_empty());
    assert!(plugin.event_keys().is_empty());
    assert!(plugin.api_keys().is_empty());
    assert!(!plugin.has_setup());
}

#[test]
fn no_table_returned_is_a_clear_error_not_a_panic() {
    let err = load_plugin("return 42", "bad").expect_err("must fail");
    assert!(
        matches!(err, LuaError::NotATable { got: "integer" }),
        "got {err:?}"
    );
}

#[test]
fn nothing_returned_is_a_clear_error() {
    let err = load_plugin("local x = 1", "bad").expect_err("must fail");
    assert!(
        matches!(err, LuaError::NotATable { got: "nil" }),
        "got {err:?}"
    );
}

#[test]
fn missing_manifest_is_a_clear_error() {
    let err = load_plugin("return {}", "bad").expect_err("must fail");
    assert!(matches!(err, LuaError::MissingManifest), "got {err:?}");
}

#[test]
fn non_table_manifest_is_a_clear_error() {
    let source = r#"return { manifest = "not a table" }"#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(matches!(err, LuaError::MissingManifest), "got {err:?}");
}

#[test]
fn missing_required_manifest_field_is_a_clear_error() {
    // No `name`.
    let source = r#"return { manifest = { schema = "v1", version = "1.0.0" } }"#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(
        matches!(err, LuaError::MissingManifestField { field: "name" }),
        "got {err:?}"
    );
}

#[test]
fn invalid_plugin_name_is_a_clear_error() {
    let source = r#"return { manifest = { schema = "v1", name = "Bad Name", version = "1.0.0" } }"#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(matches!(err, LuaError::InvalidPluginName(_)), "got {err:?}");
}

#[test]
fn invalid_schema_version_is_a_clear_error() {
    let source = r#"return { manifest = { schema = "v99", name = "x", version = "1.0.0" } }"#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(
        matches!(err, LuaError::InvalidSchemaVersion(_)),
        "got {err:?}"
    );
}

#[test]
fn wrong_type_manifest_field_is_a_clear_error() {
    // version must be a string.
    let source = r#"return { manifest = { schema = "v1", name = "x", version = 1 } }"#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(
        matches!(
            err,
            LuaError::ManifestFieldType {
                field: "version",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn non_table_hooks_is_a_clear_error() {
    let source = r#"
        return {
            manifest = { schema = "v1", name = "x", version = "1.0.0" },
            hooks = "nope",
        }
    "#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(
        matches!(err, LuaError::NotADeclarationTable { field: "hooks", .. }),
        "got {err:?}"
    );
}

#[test]
fn non_string_permission_element_is_a_clear_error() {
    let source = r#"
        return {
            manifest = { schema = "v1", name = "x", version = "1.0.0", permissions = { 1, 2 } },
        }
    "#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(
        matches!(
            err,
            LuaError::ManifestFieldType {
                field: "permissions",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn syntax_error_in_chunk_is_a_clear_error() {
    let err = load_plugin("this is not lua ===", "bad").expect_err("must fail");
    assert!(matches!(err, LuaError::Evaluate(_)), "got {err:?}");
}

#[test]
fn runtime_error_in_module_body_is_a_clear_error() {
    // Error raised while building M (NOT in setup) surfaces as Evaluate.
    let err = load_plugin("error('boom')", "bad").expect_err("must fail");
    assert!(matches!(err, LuaError::Evaluate(_)), "got {err:?}");
}

#[test]
fn invalid_identity_scope_is_a_clear_error() {
    let source = r#"
        return {
            manifest = { schema = "v1", name = "x", version = "1.0.0", identity_scope = "bogus" },
        }
    "#;
    let err = load_plugin(source, "bad").expect_err("must fail");
    assert!(
        matches!(
            err,
            LuaError::ManifestFieldType {
                field: "identity_scope",
                ..
            }
        ),
        "got {err:?}"
    );
}
