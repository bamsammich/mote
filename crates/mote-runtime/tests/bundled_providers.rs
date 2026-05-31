//! Conformance test for the bundled first-party provider plugins.
//!
//! Loads `plugins/workspace-manager/init.lua`, `plugins/bookmarks/init.lua`,
//! and (after commit B1) `plugins/history/init.lua` through the runtime's full
//! four-step load pipeline against the real v1 registry, asserting:
//!
//! 1. Step-1 (schema validation): every declared permission, capability, and
//!    consumes term is known to the registry.
//! 3. Step-3 (contract conformance): the loaded module's `M.api` contains
//!    every `required_api` function and `M.events`/`M.hooks` contains every
//!    `required_events` handler declared in the capability contract.
//!
//! The full four-step pipeline (steps 1–4 + `setup()`) is exercised via
//! `Runtime::load` with the `GrantAsRequested` approval policy, proving the
//! plugins load end-to-end without errors.
//!
//! NOTE: The standalone `urlbar` plugin has been removed. History owns
//! `ui:urlbar_provider` from Phase 5a onwards. There is no `urlbar` entry in
//! this test file; searching for "urlbar" plugin by name here should find none.
//!
//! These plugins are the navigation/workspace POLICY floor (docs/plans/
//! 02-browser-shell.md §8). The shell owns the mechanism; these own the policy.

use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{GrantAsRequested, IdentityContext, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, PluginName, SchemaVersion};

/// The bundled workspace-manager provider plugin source.
const WORKSPACE_MANAGER_SRC: &str = include_str!("../../../plugins/workspace-manager/init.lua");

/// The bundled bookmarks provider plugin source.
const BOOKMARKS_SRC: &str = include_str!("../../../plugins/bookmarks/init.lua");

/// The bundled history provider plugin source.
/// History owns both `ui:history_provider` and `ui:urlbar_provider`.
const HISTORY_SRC: &str = include_str!("../../../plugins/history/init.lua");

fn make_runtime() -> (Runtime, AuditLog) {
    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let store = Store::open_in_memory().expect("in-memory store");
    let config = Config {
        ring_capacity: 256,
        flush_threshold: 1,
        flush_interval: Duration::from_millis(5),
    };
    let log = AuditLog::new(&store, config).expect("audit log starts");
    let runtime = Runtime::new(registry, store, log.producer());
    (runtime, log)
}

fn plugin(name: &str) -> PluginName {
    PluginName::new(name).expect("valid plugin name")
}

const fn identity() -> IdentityContext {
    IdentityContext::new(IdentityId::new(1))
}

/// The workspace-manager plugin loads through the full four-step pipeline and
/// passes step-1 and step-3 against the real v1 registry.
#[test]
fn workspace_manager_provider_loads_and_conforms() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    let running = runtime
        .load(WORKSPACE_MANAGER_SRC, identity(), &policy)
        .expect("workspace-manager plugin must load cleanly through the four-step pipeline");

    // The plugin loaded with the correct name.
    assert_eq!(running.name, plugin("workspace-manager"));

    // It fulfills the `workspace:provider` capability.
    assert!(
        running
            .capabilities
            .contains(&"workspace:provider".to_owned()),
        "workspace-manager must claim workspace:provider; got: {:?}",
        running.capabilities
    );

    // It is tracked as loaded.
    assert!(runtime.is_loaded(&plugin("workspace-manager")));

    log.shutdown().expect("audit log shuts down cleanly");
}

#[test]
fn workspace_manager_passes_step1_and_step3_in_isolation() {
    use mote_lua::load_plugin;

    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let loaded = load_plugin(WORKSPACE_MANAGER_SRC, "plugins/workspace-manager/init.lua")
        .expect("workspace-manager module loads without error");
    let m = loaded.manifest();

    // Step 1.
    registry
        .validate_schema(&m.permissions, &m.capabilities, &m.consumes)
        .expect("workspace-manager: step-1 schema validation must pass");

    // Step 3.
    registry
        .check_conformance(&loaded)
        .expect("workspace-manager: step-3 contract conformance must pass");
}

/// The bookmarks plugin loads through the full four-step pipeline and passes
/// step-1 (schema validation) and step-3 (contract conformance) against the
/// real v1 registry.
#[test]
fn bookmarks_provider_loads_and_conforms() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    let running = runtime
        .load(BOOKMARKS_SRC, identity(), &policy)
        .expect("bookmarks plugin must load cleanly through the four-step pipeline");

    // The plugin loaded with the correct name.
    assert_eq!(running.name, plugin("bookmarks"));

    // It fulfills the `ui:bookmarks_provider` capability.
    assert!(
        running
            .capabilities
            .contains(&"ui:bookmarks_provider".to_owned()),
        "bookmarks must claim ui:bookmarks_provider; got: {:?}",
        running.capabilities
    );

    // It is tracked as loaded.
    assert!(runtime.is_loaded(&plugin("bookmarks")));

    log.shutdown().expect("audit log shuts down cleanly");
}

/// Step-1 + step-3 can be exercised in isolation (without running `setup()`)
/// using `mote_lua::load_plugin` + `Registry` directly. This proves the two
/// steps pass for the bookmarks plugin without any side effects.
#[test]
fn bookmarks_passes_step1_and_step3_in_isolation() {
    use mote_lua::load_plugin;

    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let loaded = load_plugin(BOOKMARKS_SRC, "plugins/bookmarks/init.lua")
        .expect("bookmarks module loads without error");
    let m = loaded.manifest();

    // Step 1.
    registry
        .validate_schema(&m.permissions, &m.capabilities, &m.consumes)
        .expect("bookmarks: step-1 schema validation must pass");

    // Step 3.
    registry
        .check_conformance(&loaded)
        .expect("bookmarks: step-3 contract conformance must pass");
}

/// The history plugin loads through the full four-step pipeline and passes
/// step-1 (schema validation) and step-3 (contract conformance) against the
/// real v1 registry.  History claims BOTH `ui:history_provider` AND
/// `ui:urlbar_provider` — the two capabilities must both appear in its
/// `running.capabilities` set.
#[test]
fn history_provider_loads_and_conforms() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    let running = runtime
        .load(HISTORY_SRC, identity(), &policy)
        .expect("history plugin must load cleanly through the four-step pipeline");

    // The plugin loaded with the correct name.
    assert_eq!(running.name, plugin("history"));

    // It fulfills BOTH the history and urlbar capabilities.
    assert!(
        running
            .capabilities
            .contains(&"ui:history_provider".to_owned()),
        "history must claim ui:history_provider; got: {:?}",
        running.capabilities
    );
    assert!(
        running
            .capabilities
            .contains(&"ui:urlbar_provider".to_owned()),
        "history must claim ui:urlbar_provider; got: {:?}",
        running.capabilities
    );

    // It is tracked as loaded.
    assert!(runtime.is_loaded(&plugin("history")));

    log.shutdown().expect("audit log shuts down cleanly");
}

/// Step-1 + step-3 can be exercised in isolation for the history plugin.
#[test]
fn history_passes_step1_and_step3_in_isolation() {
    use mote_lua::load_plugin;

    let registry = Registry::load(SchemaVersion::V1).expect("v1 registry loads");
    let loaded = load_plugin(HISTORY_SRC, "plugins/history/init.lua")
        .expect("history module loads without error");
    let m = loaded.manifest();

    // Step 1.
    registry
        .validate_schema(&m.permissions, &m.capabilities, &m.consumes)
        .expect("history: step-1 schema validation must pass");

    // Step 3.
    registry
        .check_conformance(&loaded)
        .expect("history: step-3 contract conformance must pass");
}

/// Multiple bundled providers can coexist in a single runtime without
/// exclusive-capability conflicts when they fulfill DIFFERENT exclusive
/// capabilities.
///
/// NOTE: The standalone urlbar plugin has been removed (history owns
/// `ui:urlbar_provider`). This test uses bookmarks + workspace-manager as a
/// representative pair of distinct exclusive-capability fulfillers.
#[test]
fn multiple_providers_coexist_in_single_runtime() {
    let (mut runtime, mut log) = make_runtime();
    let policy = GrantAsRequested;

    let bm = runtime
        .load(BOOKMARKS_SRC, identity(), &policy)
        .expect("bookmarks loads");

    let wm = runtime
        .load(WORKSPACE_MANAGER_SRC, identity(), &policy)
        .expect("workspace-manager loads alongside bookmarks (different exclusive capabilities)");

    assert_eq!(bm.name, plugin("bookmarks"));
    assert_eq!(wm.name, plugin("workspace-manager"));

    assert!(runtime.is_loaded(&plugin("bookmarks")));
    assert!(runtime.is_loaded(&plugin("workspace-manager")));

    log.shutdown().expect("audit log shuts down cleanly");
}
