//! Config-Lua context tests (Phase 3, work unit 3.1d; Phase 4, Task 1).
//!
//! These tests cover the restricted config sandbox exposed by
//! [`mote_lua::eval_config`]. The config context is a *separate* restricted
//! sandbox from the plugin sandbox: it exposes only `mote.plugins`,
//! `mote.dev_mode`, `mote.updates.configure`, and `mote.secrets.define`, and
//! nothing from the plugin host API (no permission declarations, no event
//! hooks, no capability surfaces).

use mote_lua::{ConfigError, SecretParam, UpdateCadence, eval_config};

// ---------------------------------------------------------------------------
// Representative plugins.lua → correct ConfigSpec
// ---------------------------------------------------------------------------

/// A representative `plugins.lua` exercising all three config functions.
const REPRESENTATIVE: &str = r#"
mote.plugins({
  adblock         = { source = "github:mote-browser/adblock" },
  vim_mode        = { source = "github:mote-browser/vim-mode" },
  cool_plugin     = { source = "github:them/cool-plugin", version = "v1.2.3" },
  my_local_plugin = { source = "path:~/code/my-plugin" },
})

mote.dev_mode({
  directories = { "~/code/my-plugin", "~/code/other-plugin" },
  plugins     = { "cool-plugin" },
})

mote.updates.configure({
  check_first_party = "weekly",
})
"#;

#[test]
fn representative_config_parses_to_correct_spec() {
    let spec = eval_config(REPRESENTATIVE, "plugins.lua").expect("representative config parses");

    // Four plugin entries, in declaration order.
    assert_eq!(spec.plugins.len(), 4, "plugin count");

    let adblock = spec.plugins.iter().find(|p| p.key == "adblock").unwrap();
    assert_eq!(adblock.source, "github:mote-browser/adblock");
    assert_eq!(adblock.version, None);

    let vim = spec.plugins.iter().find(|p| p.key == "vim_mode").unwrap();
    assert_eq!(vim.source, "github:mote-browser/vim-mode");
    assert_eq!(vim.version, None);

    let cool = spec
        .plugins
        .iter()
        .find(|p| p.key == "cool_plugin")
        .unwrap();
    assert_eq!(cool.source, "github:them/cool-plugin");
    assert_eq!(cool.version.as_deref(), Some("v1.2.3"));

    let local = spec
        .plugins
        .iter()
        .find(|p| p.key == "my_local_plugin")
        .unwrap();
    assert_eq!(local.source, "path:~/code/my-plugin");
    assert_eq!(local.version, None);
}

#[test]
fn representative_dev_mode_parses_correctly() {
    let spec = eval_config(REPRESENTATIVE, "plugins.lua").expect("representative config parses");

    assert_eq!(
        spec.dev_mode.directories,
        vec!["~/code/my-plugin", "~/code/other-plugin"],
    );
    assert_eq!(spec.dev_mode.plugins, vec!["cool-plugin"]);
}

#[test]
fn representative_updates_config_parses_correctly() {
    let spec = eval_config(REPRESENTATIVE, "plugins.lua").expect("representative config parses");
    assert_eq!(spec.updates.check_first_party, UpdateCadence::Weekly);
}

// ---------------------------------------------------------------------------
// Source is kept as a raw unparsed string
// ---------------------------------------------------------------------------

#[test]
fn source_is_carried_verbatim_not_parsed() {
    // The `source` field must be the raw string from the config, not a parsed
    // Source enum or stripped prefix. mote-pluginmgr is responsible for parsing.
    let source = r#"
mote.plugins({
  x = { source = "github:owner/repo" },
  y = { source = "path:~/foo" },
  z = { source = "git+https://example.com/plugin.git" },
  b = { source = "bundled" },
})
"#;
    let spec = eval_config(source, "plugins.lua").expect("parses");
    let sources: Vec<&str> = spec.plugins.iter().map(|p| p.source.as_str()).collect();
    assert!(sources.contains(&"github:owner/repo"));
    assert!(sources.contains(&"path:~/foo"));
    assert!(sources.contains(&"git+https://example.com/plugin.git"));
    assert!(sources.contains(&"bundled"));
}

// ---------------------------------------------------------------------------
// Minimal config (plugins only, no dev_mode / updates)
// ---------------------------------------------------------------------------

#[test]
fn minimal_plugins_only_config_parses() {
    let source = r#"
mote.plugins({
  adblock = { source = "github:mote-browser/adblock" },
})
"#;
    let spec = eval_config(source, "plugins.lua").expect("minimal config parses");
    assert_eq!(spec.plugins.len(), 1);
    assert_eq!(spec.plugins[0].key, "adblock");
    assert_eq!(spec.plugins[0].source, "github:mote-browser/adblock");
    // Defaults when not called.
    assert!(spec.dev_mode.directories.is_empty());
    assert!(spec.dev_mode.plugins.is_empty());
    assert_eq!(spec.updates.check_first_party, UpdateCadence::Weekly);
}

// ---------------------------------------------------------------------------
// Empty config (no calls at all)
// ---------------------------------------------------------------------------

#[test]
fn empty_config_gives_empty_spec() {
    let spec = eval_config("-- empty config", "plugins.lua").expect("empty config parses");
    assert!(spec.plugins.is_empty());
    assert!(spec.dev_mode.directories.is_empty());
    assert!(spec.dev_mode.plugins.is_empty());
}

// ---------------------------------------------------------------------------
// Accumulate-with-same-key-later-wins for repeated mote.plugins() calls
// ---------------------------------------------------------------------------

#[test]
fn repeated_mote_plugins_call_accumulates_with_later_key_wins() {
    // Calling mote.plugins twice: the second call's new keys are added; a
    // repeated key is overridden; unrelated keys from the first call survive.
    let source = r#"
mote.plugins({
  adblock  = { source = "github:mote-browser/adblock" },
  vim_mode = { source = "github:mote-browser/vim-mode" },
})
mote.plugins({
  vim_mode  = { source = "github:mote-browser/vim-mode-v2" },
  dark_mode = { source = "github:mote-browser/dark-mode" },
})
"#;
    let spec = eval_config(source, "plugins.lua").expect("parses");

    // All three distinct keys must be present.
    assert_eq!(spec.plugins.len(), 3, "union of both calls");

    // adblock survives from the first call.
    let adblock = spec.plugins.iter().find(|p| p.key == "adblock");
    assert!(adblock.is_some(), "adblock must survive from first call");
    assert_eq!(
        adblock.unwrap().source,
        "github:mote-browser/adblock",
        "adblock source unchanged"
    );

    // vim_mode is overridden by the second call.
    let vim = spec.plugins.iter().find(|p| p.key == "vim_mode");
    assert!(vim.is_some(), "vim_mode must be present");
    assert_eq!(
        vim.unwrap().source,
        "github:mote-browser/vim-mode-v2",
        "vim_mode updated to second-call value"
    );

    // dark_mode is new from the second call.
    let dark = spec.plugins.iter().find(|p| p.key == "dark_mode");
    assert!(dark.is_some(), "dark_mode must be added from second call");
    assert_eq!(
        dark.unwrap().source,
        "github:mote-browser/dark-mode",
        "dark_mode source correct"
    );
}

#[test]
fn two_plugins_calls_overlapping_and_disjoint_keys_union() {
    // Focused regression: two mote.plugins calls with overlapping + disjoint
    // keys produce the union, with the repeated key taking the second call's value.
    let source = r#"
mote.plugins({
  ["plugin-a"] = { source = "github:owner/plugin-a" },
  ["plugin-b"] = { source = "github:owner/plugin-b" },
})
mote.plugins({
  ["plugin-b"] = { source = "github:owner/plugin-b-v2" },
  ["plugin-c"] = { source = "github:owner/plugin-c" },
})
"#;
    let spec = eval_config(source, "plugins.lua").expect("parses");

    assert_eq!(spec.plugins.len(), 3, "union must have 3 entries");

    let a = spec.plugins.iter().find(|p| p.key == "plugin-a").unwrap();
    assert_eq!(a.source, "github:owner/plugin-a");

    let b = spec.plugins.iter().find(|p| p.key == "plugin-b").unwrap();
    assert_eq!(
        b.source, "github:owner/plugin-b-v2",
        "second call's value wins for plugin-b"
    );

    let c = spec.plugins.iter().find(|p| p.key == "plugin-c").unwrap();
    assert_eq!(c.source, "github:owner/plugin-c");
}

// ---------------------------------------------------------------------------
// Sandbox restrictions
// ---------------------------------------------------------------------------

#[test]
fn io_is_unavailable_in_config_sandbox() {
    let source = r#"
local f = io.open("/etc/passwd", "r")
mote.plugins({})
"#;
    let err = eval_config(source, "bad.lua").expect_err("io must be unavailable");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

#[test]
fn os_is_unavailable_in_config_sandbox() {
    let source = r#"
local r = os.execute("echo hi")
mote.plugins({})
"#;
    let err = eval_config(source, "bad.lua").expect_err("os must be unavailable");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

#[test]
fn require_is_unavailable_in_config_sandbox() {
    let source = r#"
local x = require("string")
mote.plugins({})
"#;
    let err = eval_config(source, "bad.lua").expect_err("require must be unavailable");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

#[test]
fn loadstring_is_unavailable_in_config_sandbox() {
    let source = r#"
local f = loadstring("return 1")
mote.plugins({})
"#;
    let err = eval_config(source, "bad.lua").expect_err("loadstring must be unavailable");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

#[test]
fn debug_library_is_unavailable_in_config_sandbox() {
    let source = r"
local info = debug.getinfo(1)
mote.plugins({})
";
    let err = eval_config(source, "bad.lua").expect_err("debug must be unavailable");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

#[test]
fn package_library_is_unavailable_in_config_sandbox() {
    let source = r"
local p = package.path
mote.plugins({})
";
    let err = eval_config(source, "bad.lua").expect_err("package must be unavailable");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Malformed configs → typed errors, not panics
// ---------------------------------------------------------------------------

#[test]
fn non_table_arg_to_mote_plugins_is_a_clear_error() {
    // mote.plugins receives a string instead of a table.
    let source = r#"mote.plugins("not a table")"#;
    let err = eval_config(source, "bad.lua").expect_err("non-table must error");
    assert!(
        matches!(
            err,
            ConfigError::BadArgument {
                function: "mote.plugins",
                ..
            }
        ),
        "expected BadArgument for mote.plugins, got {err:?}"
    );
}

#[test]
fn mote_plugins_entry_missing_source_is_a_clear_error() {
    // An entry in the plugins table that omits `source`.
    let source = r#"
mote.plugins({
  adblock = { version = "v1.0.0" },
})
"#;
    let err = eval_config(source, "bad.lua").expect_err("missing source must error");
    assert!(
        matches!(&err, ConfigError::MissingSource { key } if key == "adblock"),
        "expected MissingSource for 'adblock', got {err:?}"
    );
}

#[test]
fn mote_plugins_entry_non_table_value_is_a_clear_error() {
    // An entry whose value is not a table.
    let source = r#"
mote.plugins({
  adblock = "not-a-table",
})
"#;
    let err = eval_config(source, "bad.lua").expect_err("non-table entry must error");
    assert!(
        matches!(&err, ConfigError::BadEntry { key, .. } if key == "adblock"),
        "expected BadEntry for 'adblock', got {err:?}"
    );
}

#[test]
fn mote_plugins_source_non_string_is_a_clear_error() {
    // `source` is present but not a string.
    let source = r"
mote.plugins({
  adblock = { source = 42 },
})
";
    let err = eval_config(source, "bad.lua").expect_err("non-string source must error");
    assert!(
        matches!(&err, ConfigError::BadEntry { .. }),
        "expected BadEntry error, got {err:?}"
    );
}

#[test]
fn non_table_arg_to_mote_dev_mode_is_a_clear_error() {
    let source = r#"mote.dev_mode("oops")"#;
    let err = eval_config(source, "bad.lua").expect_err("non-table must error");
    assert!(
        matches!(
            err,
            ConfigError::BadArgument {
                function: "mote.dev_mode",
                ..
            }
        ),
        "expected BadArgument for mote.dev_mode, got {err:?}"
    );
}

#[test]
fn non_table_arg_to_mote_updates_configure_is_a_clear_error() {
    let source = r#"mote.updates.configure("oops")"#;
    let err = eval_config(source, "bad.lua").expect_err("non-table must error");
    assert!(
        matches!(
            err,
            ConfigError::BadArgument {
                function: "mote.updates.configure",
                ..
            }
        ),
        "expected BadArgument for mote.updates.configure, got {err:?}"
    );
}

#[test]
fn invalid_update_cadence_is_a_clear_error() {
    let source = r#"mote.updates.configure({ check_first_party = "someday" })"#;
    let err = eval_config(source, "bad.lua").expect_err("invalid cadence must error");
    assert!(
        matches!(err, ConfigError::InvalidUpdateCadence(_)),
        "expected InvalidUpdateCadence, got {err:?}"
    );
}

#[test]
fn syntax_error_in_config_chunk_is_a_clear_error() {
    let err = eval_config("this === is not lua", "bad.lua").expect_err("syntax error must fail");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

#[test]
fn runtime_error_in_config_chunk_is_a_clear_error() {
    let err = eval_config("error('boom')", "bad.lua").expect_err("runtime error must fail");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// update cadence variants
// ---------------------------------------------------------------------------

#[test]
fn update_cadence_never() {
    let source = r#"mote.updates.configure({ check_first_party = "never" })"#;
    let spec = eval_config(source, "plugins.lua").expect("parses");
    assert_eq!(spec.updates.check_first_party, UpdateCadence::Never);
}

#[test]
fn update_cadence_daily() {
    let source = r#"mote.updates.configure({ check_first_party = "daily" })"#;
    let spec = eval_config(source, "plugins.lua").expect("parses");
    assert_eq!(spec.updates.check_first_party, UpdateCadence::Daily);
}

// ---------------------------------------------------------------------------
// dev_mode optional sub-keys
// ---------------------------------------------------------------------------

#[test]
fn dev_mode_directories_only() {
    let source = r#"
mote.dev_mode({ directories = { "~/code/my-plugin" } })
"#;
    let spec = eval_config(source, "plugins.lua").expect("parses");
    assert_eq!(spec.dev_mode.directories, vec!["~/code/my-plugin"]);
    assert!(spec.dev_mode.plugins.is_empty());
}

#[test]
fn dev_mode_plugins_only() {
    let source = r#"
mote.dev_mode({ plugins = { "my-plugin" } })
"#;
    let spec = eval_config(source, "plugins.lua").expect("parses");
    assert!(spec.dev_mode.directories.is_empty());
    assert_eq!(spec.dev_mode.plugins, vec!["my-plugin"]);
}

// ---------------------------------------------------------------------------
// Plugin host API surface is NOT available in the config context
// ---------------------------------------------------------------------------

#[test]
fn plugin_host_on_event_is_not_available() {
    // mote.on() is a plugin-sandbox function; it must not exist here.
    let source = r#"
mote.on("tabs:on_change", function(t) end)
mote.plugins({})
"#;
    // The chunk should error because mote.on is nil/missing in the config context.
    let err = eval_config(source, "bad.lua").expect_err("mote.on must not exist in config context");
    assert!(
        matches!(err, ConfigError::Evaluate(_)),
        "expected Evaluate error, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Phase 4 Task 1: mote.secrets.define parser
// ---------------------------------------------------------------------------

/// Basic happy path: one secret with a string param.
#[test]
fn secrets_define_single_entry_with_string_param() {
    let source = r#"
mote.secrets.define({
  k = { backend = "env", var = "X" },
})
"#;
    let spec = eval_config(source, "secrets.lua").expect("parses");
    assert_eq!(spec.secrets.len(), 1);
    let entry = &spec.secrets[0];
    assert_eq!(entry.name, "k");
    assert_eq!(entry.backend, "env");
    assert_eq!(
        entry.params.get("var"),
        Some(&SecretParam::Str("X".to_owned()))
    );
}

/// A boolean param (`opt_in = true`) is captured as `Bool(true)`.
#[test]
fn secrets_define_captures_bool_param() {
    let source = r#"
mote.secrets.define({
  my_file_secret = { backend = "file", path = "/run/secrets/key", opt_in = true },
})
"#;
    let spec = eval_config(source, "secrets.lua").expect("parses");
    assert_eq!(spec.secrets.len(), 1);
    let entry = &spec.secrets[0];
    assert_eq!(entry.backend, "file");
    assert_eq!(entry.params.get("opt_in"), Some(&SecretParam::Bool(true)));
    assert_eq!(
        entry.params.get("path"),
        Some(&SecretParam::Str("/run/secrets/key".to_owned()))
    );
}

/// Missing `backend` field → `MissingSecretBackend` error naming the secret.
#[test]
fn secrets_define_missing_backend_is_clear_error() {
    let source = r#"
mote.secrets.define({
  my_secret = { var = "X" },
})
"#;
    let err = eval_config(source, "secrets.lua").expect_err("missing backend must error");
    assert!(
        matches!(&err, ConfigError::MissingSecretBackend { name } if name == "my_secret"),
        "expected MissingSecretBackend for 'my_secret', got {err:?}"
    );
}

/// Non-table entry → `BadSecretEntry` error naming the secret.
#[test]
fn secrets_define_non_table_entry_is_clear_error() {
    let source = r#"
mote.secrets.define({
  bad_secret = "not-a-table",
})
"#;
    let err = eval_config(source, "secrets.lua").expect_err("non-table entry must error");
    assert!(
        matches!(&err, ConfigError::BadSecretEntry { name, .. } if name == "bad_secret"),
        "expected BadSecretEntry for 'bad_secret', got {err:?}"
    );
}

/// Non-string, non-bool param value → `BadSecretEntry` error.
#[test]
fn secrets_define_unsupported_param_type_is_clear_error() {
    let source = r#"
mote.secrets.define({
  weird = { backend = "env", var = 42 },
})
"#;
    let err = eval_config(source, "secrets.lua").expect_err("unsupported param type must error");
    assert!(
        matches!(&err, ConfigError::BadSecretEntry { name, .. } if name == "weird"),
        "expected BadSecretEntry for 'weird', got {err:?}"
    );
}

/// Two `define` calls with the same name → later wins.
#[test]
fn secrets_define_repeated_name_later_wins() {
    let source = r#"
mote.secrets.define({
  api_key = { backend = "env", var = "OLD_VAR" },
})
mote.secrets.define({
  api_key = { backend = "keyring", id = "mote/api_key" },
})
"#;
    let spec = eval_config(source, "secrets.lua").expect("parses");
    assert_eq!(spec.secrets.len(), 1, "same name → only one entry");
    let entry = &spec.secrets[0];
    assert_eq!(entry.backend, "keyring");
    assert_eq!(
        entry.params.get("id"),
        Some(&SecretParam::Str("mote/api_key".to_owned()))
    );
}

/// Two `define` calls with different names → both accumulate.
#[test]
fn secrets_define_different_names_accumulate() {
    let source = r#"
mote.secrets.define({
  first  = { backend = "env", var = "FIRST" },
})
mote.secrets.define({
  second = { backend = "env", var = "SECOND" },
})
"#;
    let spec = eval_config(source, "secrets.lua").expect("parses");
    assert_eq!(spec.secrets.len(), 2, "two distinct names → two entries");
    let names: Vec<&str> = spec.secrets.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"first"));
    assert!(names.contains(&"second"));
}

/// A `secrets.lua` that only calls `mote.secrets.define` → `plugins` is empty.
#[test]
fn secrets_only_config_gives_empty_plugins() {
    let source = r#"
mote.secrets.define({
  anthropic_key = { backend = "env", var = "ANTHROPIC_API_KEY" },
})
"#;
    let spec = eval_config(source, "secrets.lua").expect("parses");
    assert!(
        spec.plugins.is_empty(),
        "no mote.plugins call → empty plugins"
    );
    assert_eq!(spec.secrets.len(), 1);
}
