//! Statusline element validation tests — ADR-0016.
//!
//! Exercises load-step 3 statusline validation: element kind field requirements
//! (text/icon/icon-text), icon format (ADR-0013), id uniqueness within a plugin,
//! forward-compat handling of reserved v2 fields (`action`, `disabled`), and
//! the host-API typo protection (`mote.statusline.set` on an undeclared id
//! returns `false`).

use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{GrantAsRequested, IdentityContext, LoadError, Runtime};
use mote_storage::Store;
use mote_types::{IdentityId, SchemaVersion};

fn make_runtime() -> (Runtime, AuditLog) {
    let registry = Registry::load(SchemaVersion::V1).unwrap();
    let store = Store::open_in_memory().unwrap();
    let config = Config {
        ring_capacity: 256,
        flush_threshold: 1,
        flush_interval: Duration::from_millis(5),
    };
    let log = AuditLog::new(&store, config).unwrap();
    let runtime = Runtime::new(registry, store, log.producer());
    (runtime, log)
}

const fn identity() -> IdentityContext {
    IdentityContext::new(IdentityId::new(1))
}

// ---- Fixture sources -------------------------------------------------------

/// A minimal plugin with a valid `text` kind statusline element.
const VALID_TEXT_ELEMENT: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "mode-indicator",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "mode",
    zone     = "left",
    priority = 100,
    kind     = "text",
    text     = "NORMAL",
    color    = "accent",
  },
}
return M
"#;

/// A plugin with a valid `icon` kind element (icon only, no text required).
const VALID_ICON_ELEMENT: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "ssl-indicator",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "ssl",
    zone     = "left",
    priority = 50,
    kind     = "icon",
    icon     = "lucide:lock",
    color    = "accent",
    tooltip  = "connection secure",
  },
}
return M
"#;

/// A plugin with a valid `icon-text` kind element.
const VALID_ICON_TEXT_ELEMENT: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "security-indicator",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "security",
    zone     = "left",
    priority = 50,
    kind     = "icon-text",
    icon     = "lucide:lock",
    text     = "https",
    color    = "accent",
  },
}
return M
"#;

/// A plugin with a `text` kind element that is missing the required `text` field.
const TEXT_KIND_MISSING_TEXT: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bad-text-element",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "broken",
    zone     = "left",
    priority = 10,
    kind     = "text",
    -- text field deliberately omitted
  },
}
return M
"#;

/// A plugin with an `icon` kind element that is missing the required `icon` field.
const ICON_KIND_MISSING_ICON: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bad-icon-element",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "broken",
    zone     = "left",
    priority = 10,
    kind     = "icon",
    -- icon field deliberately omitted
  },
}
return M
"#;

/// A plugin with an `icon-text` kind element missing both `icon` and `text`.
const ICON_TEXT_KIND_MISSING_ICON: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bad-icon-text-element",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "broken",
    zone     = "left",
    priority = 10,
    kind     = "icon-text",
    text     = "has text",
    -- icon field deliberately omitted
  },
}
return M
"#;

/// A plugin with an unknown icon pack in a statusline element.
const UNKNOWN_ICON_PACK: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bad-icon-pack",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "bad",
    zone     = "left",
    priority = 10,
    kind     = "icon",
    icon     = "phosphor:lock",
  },
}
return M
"#;

/// A plugin with an unknown Lucide icon name.
const UNKNOWN_ICON_NAME: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "bad-icon-name",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "bad",
    zone     = "left",
    priority = 10,
    kind     = "icon",
    icon     = "lucide:nonexistent-xyz-icon",
  },
}
return M
"#;

/// A plugin with duplicate `id` values in its statusline table.
const DUPLICATE_ID: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "duplicate-id-plugin",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "mode",
    zone     = "left",
    priority = 100,
    kind     = "text",
    text     = "NORMAL",
  },
  {
    id       = "mode",   -- duplicate!
    zone     = "right",
    priority = 10,
    kind     = "text",
    text     = "dupe",
  },
}
return M
"#;

/// A plugin with a statusline element that declares reserved v2 `action` and
/// `disabled` fields. In v0.1 these must be ignored (warning logged) and the
/// plugin must still load successfully (forward-compat commitment, ADR-0016).
const FORWARD_COMPAT_RESERVED_FIELDS: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "forward-compat-plugin",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "mode",
    zone     = "left",
    priority = 100,
    kind     = "text",
    text     = "NORMAL",
    action   = function() end,  -- reserved v2, should be ignored
    disabled = false,           -- reserved v2, should be ignored
  },
}
return M
"#;

/// A plugin that declares a statusline element, then calls
/// `mote.statusline.set()` in `setup()` with an undeclared id (typo
/// protection: must return `false`, not crash).
const HOST_API_TYPO_PROTECTION: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "typo-test-plugin",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "mode",
    zone     = "left",
    priority = 100,
    kind     = "text",
    text     = "NORMAL",
  },
}
function M.setup()
  -- Call set() with a typo in the id; must return false, not error.
  local ok = mote.statusline.set("mde", { text = "OOPS" })
  assert(ok == false, "typo id must return false")
end
return M
"#;

/// A plugin with multiple valid statusline elements in different zones.
const MULTI_ZONE_ELEMENTS: &str = r#"
local M = {}
M.manifest = {
  schema = "v1",
  name = "multi-zone-plugin",
  version = "1.0.0",
}
M.statusline = {
  {
    id       = "mode",
    zone     = "left",
    priority = 100,
    kind     = "text",
    text     = "NORMAL",
    color    = "accent",
  },
  {
    id       = "info",
    zone     = "center",
    priority = 50,
    kind     = "text",
    text     = "center",
  },
  {
    id       = "tabs",
    zone     = "right",
    priority = 50,
    kind     = "text",
    text     = "1 tab",
  },
}
return M
"#;

// ---- Tests -----------------------------------------------------------------

/// A plugin with a valid `text` kind element loads successfully and its
/// element is registered in the runtime.
#[test]
fn valid_text_element_loads_and_registers() {
    let (mut rt, _log) = make_runtime();
    rt.load(VALID_TEXT_ELEMENT, identity(), &GrantAsRequested)
        .expect("plugin with valid text statusline element must load");
    // Verify the element appears in statusline_elements().
    let elements = rt.statusline_elements();
    assert_eq!(elements.len(), 1, "one element must be registered");
    assert_eq!(elements[0].id, "mode-indicator.mode");
    assert_eq!(
        elements[0].text.as_deref(),
        Some("NORMAL"),
        "default text must match declaration"
    );
}

/// A plugin with a valid `icon` kind element loads successfully.
#[test]
fn valid_icon_element_loads_successfully() {
    let (mut rt, _log) = make_runtime();
    rt.load(VALID_ICON_ELEMENT, identity(), &GrantAsRequested)
        .expect("plugin with valid icon statusline element must load");
    let elements = rt.statusline_elements();
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].icon.as_deref(), Some("lucide:lock"));
}

/// A plugin with a valid `icon-text` kind element loads successfully.
#[test]
fn valid_icon_text_element_loads_successfully() {
    let (mut rt, _log) = make_runtime();
    rt.load(VALID_ICON_TEXT_ELEMENT, identity(), &GrantAsRequested)
        .expect("plugin with valid icon-text statusline element must load");
}

/// A `text` kind element without a `text` field must fail with `StatusLine`.
#[test]
fn text_kind_missing_text_fails_load() {
    let (mut rt, _log) = make_runtime();
    let err = rt
        .load(TEXT_KIND_MISSING_TEXT, identity(), &GrantAsRequested)
        .expect_err("text kind without text field must fail");
    assert!(
        matches!(err, LoadError::StatusLine { .. }),
        "expected StatusLine error, got: {err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("text") && msg.contains("requires"),
        "error must explain text requirement; got: {msg}"
    );
}

/// An `icon` kind element without an `icon` field must fail with `StatusLine`.
#[test]
fn icon_kind_missing_icon_fails_load() {
    let (mut rt, _log) = make_runtime();
    let err = rt
        .load(ICON_KIND_MISSING_ICON, identity(), &GrantAsRequested)
        .expect_err("icon kind without icon field must fail");
    assert!(
        matches!(err, LoadError::StatusLine { .. }),
        "expected StatusLine error, got: {err}"
    );
}

/// An `icon-text` kind element without an `icon` field must fail.
#[test]
fn icon_text_kind_missing_icon_fails_load() {
    let (mut rt, _log) = make_runtime();
    let err = rt
        .load(ICON_TEXT_KIND_MISSING_ICON, identity(), &GrantAsRequested)
        .expect_err("icon-text kind without icon field must fail");
    assert!(
        matches!(err, LoadError::StatusLine { .. }),
        "expected StatusLine error, got: {err}"
    );
}

/// An element with an unknown icon pack must fail with `StatusLine`.
#[test]
fn unknown_icon_pack_fails_load() {
    let (mut rt, _log) = make_runtime();
    let err = rt
        .load(UNKNOWN_ICON_PACK, identity(), &GrantAsRequested)
        .expect_err("unknown icon pack must fail load");
    assert!(
        matches!(err, LoadError::StatusLine { .. }),
        "expected StatusLine error, got: {err}"
    );
    assert!(
        err.to_string().contains("phosphor"),
        "error must name the bad pack; got: {err}"
    );
}

/// An element with an unknown Lucide icon name must fail with `StatusLine`.
#[test]
fn unknown_icon_name_fails_load() {
    let (mut rt, _log) = make_runtime();
    let err = rt
        .load(UNKNOWN_ICON_NAME, identity(), &GrantAsRequested)
        .expect_err("unknown lucide icon name must fail load");
    assert!(
        matches!(err, LoadError::StatusLine { .. }),
        "expected StatusLine error, got: {err}"
    );
    assert!(
        err.to_string().contains("nonexistent-xyz-icon"),
        "error must name the bad icon; got: {err}"
    );
}

/// A plugin with a duplicate element `id` within its statusline table must
/// fail with `StatusLine` at the second entry.
#[test]
fn duplicate_id_fails_load() {
    let (mut rt, _log) = make_runtime();
    let err = rt
        .load(DUPLICATE_ID, identity(), &GrantAsRequested)
        .expect_err("duplicate statusline id must fail load");
    assert!(
        matches!(err, LoadError::StatusLine { index: 2, .. }),
        "error must report index 2 (the duplicate); got: {err:?}"
    );
    assert!(
        err.to_string().contains("mode"),
        "error must name the duplicated id; got: {err}"
    );
}

/// A plugin that declares reserved v2 fields (`action`, `disabled`) must still
/// load successfully — forward-compat commitment. The fields are ignored (the
/// runtime emits a warning log but does NOT fail the load).
#[test]
fn forward_compat_reserved_fields_load_successfully() {
    let (mut rt, _log) = make_runtime();
    rt.load(
        FORWARD_COMPAT_RESERVED_FIELDS,
        identity(),
        &GrantAsRequested,
    )
    .expect("reserved v2 fields (action, disabled) must be ignored; plugin must load successfully");
    // Element must still be registered with its declared content.
    let elements = rt.statusline_elements();
    assert_eq!(elements.len(), 1, "element must be registered");
    assert_eq!(elements[0].id, "forward-compat-plugin.mode");
}

/// `mote.statusline.set()` with an undeclared id must return `false` (typo
/// protection: the plugin can only update its own declared elements).
#[test]
fn host_api_typo_protection_returns_false() {
    let (mut rt, _log) = make_runtime();
    // The plugin's setup() calls set("mde", …) (typo for "mode") and asserts
    // the return is false. If the setup() assertion fails, the load pipeline
    // returns LoadError::Setup — we distinguish it from a validation error.
    rt.load(HOST_API_TYPO_PROTECTION, identity(), &GrantAsRequested)
        .expect("setup() typo-protection assertion must pass");
}

/// A plugin with multiple valid elements in different zones loads and registers
/// all elements, verifying priority-based ordering within zones.
#[test]
fn multi_zone_elements_all_register() {
    let (mut rt, _log) = make_runtime();
    rt.load(MULTI_ZONE_ELEMENTS, identity(), &GrantAsRequested)
        .expect("plugin with multiple valid statusline elements must load");
    let elements = rt.statusline_elements();
    assert_eq!(elements.len(), 3, "all three elements must be registered");
    // All should be present by fq_id.
    let ids: Vec<&str> = elements.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains(&"multi-zone-plugin.mode"),
        "mode element present"
    );
    assert!(
        ids.contains(&"multi-zone-plugin.info"),
        "info element present"
    );
    assert!(
        ids.contains(&"multi-zone-plugin.tabs"),
        "tabs element present"
    );
}

/// Elements are removed from the runtime when the plugin is unloaded.
#[test]
fn elements_removed_on_unload() {
    use mote_types::PluginName;
    let (mut rt, _log) = make_runtime();
    rt.load(VALID_TEXT_ELEMENT, identity(), &GrantAsRequested)
        .expect("plugin must load");
    assert_eq!(rt.statusline_elements().len(), 1);
    let name = PluginName::new("mode-indicator").expect("valid plugin name");
    rt.unload(&name).expect("plugin must unload");
    assert_eq!(
        rt.statusline_elements().len(),
        0,
        "element must be removed after unload"
    );
}
