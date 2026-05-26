//! Contract-conformance integration tests (DISCIPLINES §2).
//!
//! Loads minimal v1 plugins via `mote-lua` and asserts the registry's step-1
//! (schema validation) and step-3 (contract conformance) entry points behave:
//! a valid fulfiller passes both; negative fixtures fail the right step with a
//! precise error.

use mote_lua::load_plugin;
use mote_registry::{ConformanceError, Registry, SchemaValidationError};
use mote_types::SchemaVersion;

const VALID: &str = include_str!("fixtures/form_services.lua");
const MISSING_API: &str = include_str!("fixtures/form_services_missing_api.lua");

fn v1() -> Registry {
    Registry::load(SchemaVersion::V1).expect("v1 registry loads")
}

#[test]
fn valid_fulfiller_passes_step1_and_step3() {
    let registry = v1();
    let plugin = load_plugin(VALID, "form_services.lua").expect("fixture loads");
    let m = plugin.manifest();

    // Step 1: every permission/capability/consumes term is known.
    registry
        .validate_schema(&m.permissions, &m.capabilities, &m.consumes)
        .expect("step 1 passes for the valid fulfiller");

    // Step 3: the loaded module declares the required API + event surface.
    registry
        .check_conformance(&plugin)
        .expect("step 3 passes for the valid fulfiller");
}

#[test]
fn unknown_permission_fails_step1() {
    let registry = v1();
    let err = registry
        .validate_schema(&["page:teleport".to_owned()], &[], &[])
        .unwrap_err();
    match err {
        SchemaValidationError::UnknownPermission { domain, action, .. } => {
            assert_eq!(domain, "page");
            assert_eq!(action, "teleport");
        }
        other => panic!("expected UnknownPermission, got {other:?}"),
    }
}

#[test]
fn unknown_capability_fails_step1() {
    let registry = v1();
    let err = registry
        .validate_schema(&[], &["bogus:provider".to_owned()], &[])
        .unwrap_err();
    assert!(matches!(
        err,
        SchemaValidationError::UnknownCapability { .. }
    ));
}

#[test]
fn fulfiller_missing_required_api_fails_step3() {
    let registry = v1();
    let plugin = load_plugin(MISSING_API, "missing_api.lua").expect("fixture loads");
    let m = plugin.manifest();

    // Step 1 still passes: all declared terms are known.
    registry
        .validate_schema(&m.permissions, &m.capabilities, &m.consumes)
        .expect("step 1 passes; the term set is valid");

    // Step 3 fails: the required `inject_isolated` API function is missing.
    let err = registry.check_conformance(&plugin).unwrap_err();
    match err {
        ConformanceError::MissingApi {
            capability,
            missing,
            ..
        } => {
            assert_eq!(capability, "password-manager-form-services");
            assert_eq!(missing, "inject_isolated");
        }
        other => panic!("expected MissingApi, got {other:?}"),
    }
}

#[test]
fn consumes_only_capability_passes_step1_without_conformance() {
    // A pure consumer claims no capability but consumes a known one. Step 1
    // accepts the consumes term; step 3 finds nothing to check.
    let registry = v1();
    let plugin_src = r#"
        local M = {}
        M.manifest = {
          schema = "v1",
          name = "pure-consumer",
          version = "1.0.0",
          permissions = { "storage:persistent" },
          consumes = { "password-manager-form-services" },
        }
        M.events = {
          ["password-manager-form-services:form-detected"] = function() end,
        }
        return M
    "#;
    let plugin = load_plugin(plugin_src, "consumer.lua").expect("loads");
    let m = plugin.manifest();
    registry
        .validate_schema(&m.permissions, &m.capabilities, &m.consumes)
        .expect("step 1 passes");
    registry
        .check_conformance(&plugin)
        .expect("step 3 is a no-op for a non-fulfiller");
}
