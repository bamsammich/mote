//! The `mote` command-line surface.
//!
//! Thin argument layer over [`mote_pluginmgr::PluginManager`]. Parses
//! `mote plugin <subcommand>` (and the Phase-4-stub `mote secrets link`),
//! opens the real on-disk store, builds a [`PluginManager`], calls the façade,
//! formats output to stdout/stderr, and returns the appropriate
//! [`std::process::ExitCode`].
//!
//! # Path resolution
//!
//! The production paths follow the XDG Base Directory specification:
//!
//! - **`config_dir`**: `$XDG_CONFIG_HOME/mote` if `XDG_CONFIG_HOME` is set,
//!   otherwise `$HOME/.config/mote`. This is where `plugins.lua`,
//!   `managed.lua`, `plugins.lock`, and `plugins/<name>` live.
//! - **`cache_dir`**: `$XDG_CACHE_HOME/mote/plugins` if `XDG_CACHE_HOME` is
//!   set, otherwise `$HOME/.cache/mote/plugins`. This is the content-addressed
//!   plugin cache.
//! - **`store_path`**: `<config_dir>/state.db` — the `SQLite` approval-state
//!   store.
//!
//! [`PluginManager::default_dirs`] already encodes `$HOME/.config/mote` /
//! `$HOME/.cache/mote/plugins` as the fallback pair; this crate extends that
//! with `$XDG_CONFIG_HOME` / `$XDG_CACHE_HOME` awareness.
//!
//! # Architecture
//!
//! `run()` is the public entry-point called by `mote-app`. It:
//! 1. Parses args with clap.
//! 2. Resolves paths and opens the [`mote_storage::Store`].
//! 3. Constructs a [`PluginManager`].
//! 4. Delegates to [`dispatch()`], which maps commands → façade calls →
//!    formatted output.
//!
//! [`dispatch()`] takes a pre-built `PluginManager` reference so tests can
//! inject a tempdir-backed manager without touching `$HOME`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mote_pluginmgr::{
    DeltaKind, ImportOutcome, ManagerError, PluginManager, RemoveOutcome, SyncReport, UpdateOutcome,
};
use mote_storage::Store;
use mote_types::{PluginName, PluginNameError};

// ---------------------------------------------------------------------------
// Clap argument types
// ---------------------------------------------------------------------------

/// Parse a `PluginName` from a CLI argument.
///
/// Used as the `value_parser` for `<name>` arguments so clap surfaces a clean
/// error message when the name does not satisfy the `[a-z0-9-]` grammar.
fn parse_plugin_name(s: &str) -> Result<PluginName, PluginNameError> {
    PluginName::new(s)
}

/// The `mote` command.
#[derive(Debug, Parser)]
#[command(name = "mote", about = "Mote browser management CLI")]
pub struct Cli {
    /// The top-level subcommand group.
    #[command(subcommand)]
    pub command: TopCommand,
}

/// Top-level subcommand groups.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum TopCommand {
    /// Plugin lifecycle management.
    Plugin {
        /// The plugin subcommand.
        #[command(subcommand)]
        cmd: PluginCommand,
    },
    /// Secret ↔ vault mapping helpers (Phase 4; stub in Phase 3).
    Secrets {
        /// The secrets subcommand.
        #[command(subcommand)]
        cmd: SecretsCommand,
    },
}

/// `mote plugin <subcommand>`.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum PluginCommand {
    /// Fetch and install a plugin from `<source>`, writing a `managed.lua` entry.
    Add {
        /// Source string: `github:<owner>/<repo>`, `git+https://…`, or
        /// `path:<local-path>`.
        source: String,
        /// Optional version/tag/branch constraint.
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove a managed plugin (must be in `managed.lua`; prints guidance for
    /// user-config-only plugins).
    Remove {
        /// The plugin name.
        #[arg(value_parser = parse_plugin_name)]
        name: PluginName,
    },
    /// Fetch the latest commit and apply (or queue for re-approval if
    /// permissions expanded).
    Update {
        /// Specific plugin to update; if omitted, all managed plugins.
        #[arg(value_parser = parse_plugin_name)]
        name: Option<PluginName>,
    },
    /// Change the source of a managed plugin.
    Source {
        /// The plugin name.
        #[arg(value_parser = parse_plugin_name)]
        name: PluginName,
        /// The new source string.
        new_source: String,
    },
    /// Reconcile the cache and symlinks with the declared specs.
    Sync,
    /// Roll back a plugin to the previously-cached commit.
    Rollback {
        /// The plugin name.
        #[arg(value_parser = parse_plugin_name)]
        name: PluginName,
    },
    /// Show the pending permission diff for a plugin (headless approval view).
    Diff {
        /// The plugin name.
        #[arg(value_parser = parse_plugin_name)]
        name: PluginName,
    },
    /// Migrate a plugin from `managed.lua` to `plugins.lua`.
    Import {
        /// The plugin name.
        #[arg(value_parser = parse_plugin_name)]
        name: PluginName,
        /// Append the snippet to `plugins.lua` instead of printing it.
        #[arg(long)]
        write: bool,
    },
    /// Reclaim unreferenced cache entries.
    Gc,
    /// Show and approve pending permission changes for a plugin.
    Review {
        /// The plugin name.
        #[arg(value_parser = parse_plugin_name)]
        name: PluginName,
    },
    /// Checksum-pin and approve the current state of a plugin.
    Pin {
        /// The plugin name.
        #[arg(value_parser = parse_plugin_name)]
        name: PluginName,
    },
}

/// `mote secrets <subcommand>` (Phase 4 stub).
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum SecretsCommand {
    /// Map a secret name to a vault item (Phase 4; not yet implemented).
    Link {
        /// The secret name.
        name: String,
    },
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Resolves `(config_dir, cache_dir)` for production use.
///
/// Prefers `$XDG_CONFIG_HOME/mote` / `$XDG_CACHE_HOME/mote/plugins`; falls
/// back to `$HOME/.config/mote` / `$HOME/.cache/mote/plugins` via
/// [`PluginManager::default_dirs`].
///
/// Returns `None` if neither `HOME` nor the relevant `XDG_*` variable is set.
#[must_use]
pub fn resolve_dirs() -> Option<(PathBuf, PathBuf)> {
    let config = if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("mote")
    } else {
        let home = std::env::var_os("HOME")?;
        PathBuf::from(home).join(".config").join("mote")
    };

    let cache = if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(xdg).join("mote").join("plugins")
    } else {
        let home = std::env::var_os("HOME")?;
        PathBuf::from(home)
            .join(".cache")
            .join("mote")
            .join("plugins")
    };

    Some((config, cache))
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Renders a [`mote_pluginmgr::DiffReport`] to stdout in the DESIGN §7.1 format.
///
/// Each added term prints as `  + <term>  (NEW)` and each removed term as
/// `  - <term>  (REMOVED)`. If there are no changes, prints `  (no permission changes)`.
fn print_diff_report(report: &mote_pluginmgr::DiffReport) {
    let has_changes = !report.permission_changes.is_empty()
        || !report.capability_changes.is_empty()
        || !report.consumes_changes.is_empty()
        || report.identity_scope_change.is_some();

    if !has_changes {
        println!("  (no permission changes)");
        return;
    }

    for delta in &report.permission_changes {
        match delta.kind {
            DeltaKind::Added => println!("  + {}  (NEW)", delta.term),
            DeltaKind::Removed => println!("  - {}  (REMOVED)", delta.term),
        }
    }
    for delta in &report.capability_changes {
        match delta.kind {
            DeltaKind::Added => println!("  + {}  (NEW)", delta.term),
            DeltaKind::Removed => println!("  - {}  (REMOVED)", delta.term),
        }
    }
    for delta in &report.consumes_changes {
        match delta.kind {
            DeltaKind::Added => println!("  + {}  (NEW)", delta.term),
            DeltaKind::Removed => println!("  - {}  (REMOVED)", delta.term),
        }
    }
    if let Some((old, new)) = &report.identity_scope_change {
        let old_str = old
            .as_ref()
            .map_or_else(|| "(none)".to_owned(), |s| format!("{s:?}"));
        let new_str = new
            .as_ref()
            .map_or_else(|| "(none)".to_owned(), |s| format!("{s:?}"));
        println!("  identity_scope: {old_str} → {new_str}  (CHANGED)");
    }
}

/// Renders a [`SyncReport`] to stdout.
///
/// Prints one OK or FAILED line per plugin. Returns `ExitCode::FAILURE` when
/// any plugin failed (R5: individual failures are recoverable — the rest
/// continue).
fn print_sync_report(report: &SyncReport) -> ExitCode {
    for outcome in &report.ok {
        println!("  {} OK ({:?})", outcome.name, outcome.integrity);
    }
    for (name, err) in &report.failed {
        eprintln!("  {name} FAILED: {err}");
    }

    if report.failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------
// dispatch — testable command → façade → output
// ---------------------------------------------------------------------------

/// Maps a [`PluginCommand`] to the appropriate [`PluginManager`] call, writes
/// formatted output to stdout (errors to stderr), and returns the exit code.
///
/// This function is the testable core of the CLI: tests build a
/// tempdir-backed [`PluginManager`] and call this directly, bypassing path
/// resolution and `Store::open`.
///
/// # Exit codes
///
/// - `0` — success.
/// - `1` — any error (manager error, not-found, etc.).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn dispatch(cmd: &PluginCommand, mgr: &PluginManager) -> ExitCode {
    match cmd {
        PluginCommand::Add { source, version } => match mgr.add(source, version.clone()) {
            Ok((name, label)) => {
                println!("Added {name} ({label})");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },

        PluginCommand::Remove { name } => match mgr.remove(name) {
            Ok(RemoveOutcome::Removed) => {
                println!("Removed {name}");
                ExitCode::SUCCESS
            }
            Ok(RemoveOutcome::UserConfigOnly) => {
                eprintln!(
                    "error: {name} is declared in your plugins.lua (not in managed.lua).\n\
                         Mote cannot modify your hand-authored config. Edit plugins.lua \
                         directly to remove it."
                );
                ExitCode::FAILURE
            }
            Ok(RemoveOutcome::NotFound) => {
                eprintln!("error: plugin {name} not found in any config layer");
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },

        PluginCommand::Update { name: Some(name) } => {
            match mgr.update(name) {
                Ok(UpdateOutcome::Applied { commit }) => {
                    println!("Updated {name} → {commit}");
                    ExitCode::SUCCESS
                }
                Ok(UpdateOutcome::NeedsReapproval { report }) => {
                    println!("Permission changes for {name}:");
                    print_diff_report(&report);
                    println!(
                        "{name} requires re-approval before it will load.\n\
                         Run `mote plugin review {name}` to view and approve."
                    );
                    // Not a failure — the update was performed; it just needs review.
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        PluginCommand::Update { name: None } => {
            // Update all managed plugins — not directly supported by the façade
            // as a single call; the façade exposes per-plugin `update`. For now,
            // print a message indicating the caller should name a plugin.
            eprintln!(
                "error: `mote plugin update` without a plugin name is not yet implemented.\n\
                 Specify a plugin: `mote plugin update <name>`"
            );
            ExitCode::FAILURE
        }

        PluginCommand::Source { name, new_source } => match mgr.set_source(name, new_source) {
            Ok(()) => {
                println!("Source for {name} updated to {new_source}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },

        PluginCommand::Sync => match mgr.sync() {
            Ok(report) => {
                println!("Syncing plugins…");
                print_sync_report(&report)
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },

        PluginCommand::Rollback { name } => match mgr.rollback(name) {
            Ok(()) => {
                println!("Rolled back {name} to previous commit");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },

        PluginCommand::Diff { name } => match mgr.diff(name) {
            Ok(report) => {
                println!("Permission changes for {name}:");
                print_diff_report(&report);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },

        PluginCommand::Import { name, write } => {
            match mgr.import(name, *write) {
                Ok(ImportOutcome::Snippet(snippet)) => {
                    println!("Paste the following snippet into your plugins.lua:\n\n{snippet}");
                    ExitCode::SUCCESS
                }
                Ok(ImportOutcome::Written) => {
                    println!("Appended {name} entry to plugins.lua and removed from managed.lua");
                    ExitCode::SUCCESS
                }
                Ok(ImportOutcome::PluginsLuaDoesNotParse(snippet)) => {
                    eprintln!(
                        "warning: plugins.lua does not parse — the file was NOT modified.\n\
                         Paste the following snippet manually into your plugins.lua:\n\n{snippet}"
                    );
                    // Still exit 0 — the snippet was produced; the user has what they need.
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        PluginCommand::Gc => match mgr.gc() {
            Ok(report) => {
                if report.reclaimed.is_empty() {
                    println!("Nothing to reclaim.");
                } else {
                    for (name, commit) in &report.reclaimed {
                        println!("Reclaimed {name}/{commit}");
                    }
                    println!(
                        "Reclaimed {} cache {}.",
                        report.reclaimed.len(),
                        if report.reclaimed.len() == 1 {
                            "entry"
                        } else {
                            "entries"
                        }
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },

        PluginCommand::Review { name } => {
            // Show the diff first, then approve.
            let diff = match mgr.diff(name) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            println!("Pending changes for {name}:");
            print_diff_report(&diff);

            match mgr.approve(name) {
                Ok(()) => {
                    println!("{name} approved — will load without prompting on next launch.");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error approving {name}: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        PluginCommand::Pin { name } => match mgr.pin(name) {
            Ok(()) => {
                println!("Pinned {name}: current hash recorded and approved.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Dispatches a `mote secrets` subcommand.
fn dispatch_secrets(cmd: &SecretsCommand, mgr: &PluginManager) -> ExitCode {
    match cmd {
        SecretsCommand::Link { name } => match mgr.link(name) {
            Ok(()) => {
                println!("Linked secret {name}");
                ExitCode::SUCCESS
            }
            Err(ManagerError::SecretsNotAvailable) => {
                eprintln!(
                    "mote secrets link: the secrets backend lands in Phase 4 — \
                     `mote-secrets` is not yet wired."
                );
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

// ---------------------------------------------------------------------------
// run — the public entry-point
// ---------------------------------------------------------------------------

/// Parses `args`, resolves production paths, opens the on-disk store, builds a
/// [`PluginManager`], and dispatches to [`dispatch()`].
///
/// This is the function `mote-app::main` calls when the first non-program
/// argument is `plugin` or `secrets`.
///
/// # Exit codes
///
/// - `0` — success.
/// - `1` — any manager or I/O error.
/// - Clap's own exit code (typically `2`) for usage / parse errors (clap
///   prints its own error; we propagate the code).
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            // clap prints its own error/help; propagate its exit code.
            let code = e.exit_code();
            e.print().ok();
            return ExitCode::from(u8::try_from(code).unwrap_or(1));
        }
    };

    let Some((config_dir, cache_dir)) = resolve_dirs() else {
        eprintln!("error: cannot determine home directory (HOME is not set)");
        return ExitCode::FAILURE;
    };

    run_with_dirs(&cli.command, &config_dir, &cache_dir)
}

/// Runs a parsed command against explicit config/cache directories.
///
/// Split out from [`run`] so the directory-creation + store-open + dispatch
/// path is testable without depending on the real `$HOME`/XDG environment.
///
/// Creates `config_dir` and `cache_dir` if missing — the state store
/// (`<config_dir>/state.db`), `plugins.lock`, and `managed.lua` all live under
/// `config_dir`, and `Store::open` (`SQLite`) cannot create the database in a
/// directory that does not yet exist. Then opens the state store and dispatches.
#[must_use]
pub fn run_with_dirs(command: &TopCommand, config_dir: &Path, cache_dir: &Path) -> ExitCode {
    if let Err(e) = std::fs::create_dir_all(config_dir) {
        eprintln!(
            "error: could not create config directory {}: {e}",
            config_dir.display()
        );
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        eprintln!(
            "error: could not create cache directory {}: {e}",
            cache_dir.display()
        );
        return ExitCode::FAILURE;
    }

    let store_path = config_dir.join("state.db");
    let store = match Store::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: could not open state store at {}: {e}",
                store_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let mgr = PluginManager::new(config_dir, cache_dir, &store);

    match command {
        TopCommand::Plugin { cmd } => dispatch(cmd, &mgr),
        TopCommand::Secrets { cmd } => dispatch_secrets(cmd, &mgr),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use mote_pluginmgr::{ImportOutcome, ManagedFile, PluginManager};
    use mote_storage::Store;
    use mote_types::PluginName;

    use super::*;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn name(s: &str) -> PluginName {
        PluginName::new(s).unwrap()
    }

    struct Fixture {
        _config: tempfile::TempDir,
        _cache: tempfile::TempDir,
        config_dir: PathBuf,
        mgr: PluginManager,
        /// Kept alive so the in-memory store connection is not dropped; the
        /// `PluginManager` borrows it by reference internally.
        _store: Store,
    }

    fn fixture() -> Fixture {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let config_dir = config.path().to_path_buf();
        let cache_dir = cache.path().to_path_buf();
        let store = Store::open_in_memory().unwrap();
        let mgr = PluginManager::new(&config_dir, &cache_dir, &store);
        Fixture {
            _config: config,
            _cache: cache,
            config_dir,
            mgr,
            _store: store,
        }
    }

    /// Writes a minimal valid plugin dir to `path`.
    fn write_plugin(dir: &Path, plugin_name: &str, permissions: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        let perms = permissions
            .iter()
            .map(|p| format!(r#""{p}""#))
            .collect::<Vec<_>>()
            .join(", ");
        let lua = format!(
            r#"
local M = {{}}
M.manifest = {{
    schema = "v1",
    name = "{plugin_name}",
    version = "1",
    permissions = {{ {perms} }},
    identity_scope = "global",
}}
return M
"#
        );
        fs::write(dir.join("init.lua"), lua).unwrap();
    }

    fn write_plugins_lua(config_dir: &Path, entries: &[(&str, &str)]) {
        let body = entries
            .iter()
            .map(|(k, src)| format!(r#"  ["{k}"] = {{ source = "{src}" }},"#))
            .collect::<Vec<_>>()
            .join("\n");
        let lua = format!("mote.plugins({{\n{body}\n}})\n");
        fs::write(config_dir.join("plugins.lua"), lua).unwrap();
    }

    // -----------------------------------------------------------------------
    // Clap parsing tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_add_with_source_and_version() {
        let cli = Cli::try_parse_from(["mote", "plugin", "add", "github:x/y", "--version", "v1"])
            .unwrap();
        match cli.command {
            TopCommand::Plugin {
                cmd: PluginCommand::Add { source, version },
            } => {
                assert_eq!(source, "github:x/y");
                assert_eq!(version.as_deref(), Some("v1"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parse_add_without_version() {
        let cli = Cli::try_parse_from(["mote", "plugin", "add", "path:~/code/myplugin"]).unwrap();
        match cli.command {
            TopCommand::Plugin {
                cmd: PluginCommand::Add { source, version },
            } => {
                assert_eq!(source, "path:~/code/myplugin");
                assert!(version.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_import_with_write_flag() {
        let cli =
            Cli::try_parse_from(["mote", "plugin", "import", "my-plugin", "--write"]).unwrap();
        match cli.command {
            TopCommand::Plugin {
                cmd: PluginCommand::Import { name, write },
            } => {
                assert_eq!(name, PluginName::new("my-plugin").unwrap());
                assert!(write);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_import_without_write_flag() {
        let cli = Cli::try_parse_from(["mote", "plugin", "import", "my-plugin"]).unwrap();
        match cli.command {
            TopCommand::Plugin {
                cmd: PluginCommand::Import { name, write },
            } => {
                assert_eq!(name, PluginName::new("my-plugin").unwrap());
                assert!(!write);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn invalid_plugin_name_rejected_by_value_parser() {
        // "Foo_Bar" is invalid: uppercase and underscore are both rejected.
        let result = Cli::try_parse_from(["mote", "plugin", "remove", "Foo_Bar"]);
        assert!(
            result.is_err(),
            "invalid plugin name must cause a parse error"
        );
    }

    #[test]
    fn plugin_with_no_subcommand_errors() {
        let result = Cli::try_parse_from(["mote", "plugin"]);
        assert!(
            result.is_err(),
            "`mote plugin` with no subcommand must fail"
        );
    }

    #[test]
    fn parse_sync_subcommand() {
        let cli = Cli::try_parse_from(["mote", "plugin", "sync"]).unwrap();
        assert!(matches!(
            cli.command,
            TopCommand::Plugin {
                cmd: PluginCommand::Sync
            }
        ));
    }

    #[test]
    fn parse_secrets_link() {
        let cli = Cli::try_parse_from(["mote", "secrets", "link", "my-secret"]).unwrap();
        match cli.command {
            TopCommand::Secrets {
                cmd: SecretsCommand::Link { name },
            } => {
                assert_eq!(name, "my-secret");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // dispatch tests against a tempdir PluginManager
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_sync_path_plugin_returns_success() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "test-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("test-plugin", &src)]);

        let cmd = PluginCommand::Sync;
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "sync of a valid path: plugin must succeed"
        );
    }

    #[test]
    fn dispatch_diff_never_approved_shows_new_lines() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "diff-plugin", &["storage:persistent"]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        // No approval yet — diff must succeed (shows all perms as NEW).
        let cmd = PluginCommand::Diff {
            name: name("diff-plugin"),
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "diff of an unapproved plugin must succeed"
        );
    }

    #[test]
    fn dispatch_import_no_write_prints_parseable_snippet() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "snap-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        // Verify the façade returns a parseable snippet (mirrors manager.rs test).
        let outcome = f.mgr.import(&name("snap-plugin"), false).unwrap();
        match outcome {
            ImportOutcome::Snippet(snippet) => {
                mote_lua::eval_config(&snippet, "snippet").unwrap();
            }
            other => panic!("expected Snippet, got {other:?}"),
        }

        // And dispatch returns success.
        let cmd = PluginCommand::Import {
            name: name("snap-plugin"),
            write: false,
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn dispatch_remove_not_found_returns_failure() {
        let f = fixture();
        let cmd = PluginCommand::Remove {
            name: name("nonexistent"),
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "removing a nonexistent plugin must fail"
        );
    }

    #[test]
    fn dispatch_remove_user_config_only_returns_failure() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "user-plugin", &[]);
        let src = format!("path:{}", plugin_dir.path().display());
        write_plugins_lua(&f.config_dir, &[("user-plugin", &src)]);
        // Do NOT add to managed — it's user-config-only.

        let cmd = PluginCommand::Remove {
            name: name("user-plugin"),
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "removing a user-config-only plugin must return failure with guidance"
        );
    }

    #[test]
    fn dispatch_gc_empty_returns_success() {
        let f = fixture();
        let code = dispatch(&PluginCommand::Gc, &f.mgr);
        assert_eq!(code, ExitCode::SUCCESS, "gc on empty cache must succeed");
    }

    #[test]
    fn dispatch_pin_returns_success() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "pin-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        let cmd = PluginCommand::Pin {
            name: name("pin-plugin"),
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn dispatch_rollback_no_previous_commit_returns_failure() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "roll-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        // No previous commit exists for a path: plugin; rollback must fail.
        let cmd = PluginCommand::Rollback {
            name: name("roll-plugin"),
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "rollback with no prior commit must fail"
        );
    }

    #[test]
    fn dispatch_secrets_link_returns_failure_with_stub_message() {
        let f = fixture();
        let code = dispatch_secrets(
            &SecretsCommand::Link {
                name: "my-vault-secret".to_owned(),
            },
            &f.mgr,
        );
        assert_eq!(
            code,
            ExitCode::FAILURE,
            "Phase 3 secrets link stub must return failure"
        );
    }

    #[test]
    fn dispatch_review_returns_success() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "review-plugin", &["storage:persistent"]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        let cmd = PluginCommand::Review {
            name: name("review-plugin"),
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "review of an installed plugin must succeed"
        );
    }

    #[test]
    fn dispatch_add_path_plugin_succeeds() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "new-plugin", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        let cmd = PluginCommand::Add {
            source: src,
            version: None,
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "add of a valid path: plugin must succeed"
        );

        // Verify managed.lua was written.
        let managed = ManagedFile::load(&f.config_dir.join("managed.lua")).unwrap();
        assert_eq!(managed.entries().count(), 1);
    }

    #[test]
    fn dispatch_remove_managed_plugin_succeeds() {
        let f = fixture();
        let plugin_dir = tempfile::tempdir().unwrap();
        write_plugin(plugin_dir.path(), "removable", &[]);

        let src = format!("path:{}", plugin_dir.path().display());
        f.mgr.add(&src, None).unwrap();

        let cmd = PluginCommand::Remove {
            name: name("removable"),
        };
        let code = dispatch(&cmd, &f.mgr);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "remove of a managed plugin must succeed"
        );
    }

    #[test]
    fn run_with_dirs_creates_missing_dirs_then_syncs() {
        // Regression (caught by an end-to-end run): `run_with_dirs` must create
        // the config and cache directories before opening the SQLite store —
        // `Store::open` cannot create its database in a directory that does not
        // exist. A fresh machine has neither directory; `mote plugin sync` must
        // still succeed and materialise them.
        let root = tempfile::tempdir().unwrap();
        let config_dir = root.path().join("config/mote"); // does not exist yet
        let cache_dir = root.path().join("cache/mote/plugins"); // does not exist yet
        assert!(!config_dir.exists());

        let cmd = TopCommand::Plugin {
            cmd: PluginCommand::Sync,
        };
        let code = run_with_dirs(&cmd, &config_dir, &cache_dir);

        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "sync on a fresh empty config must succeed"
        );
        assert!(config_dir.exists(), "config dir must be created");
        assert!(cache_dir.exists(), "cache dir must be created");
        assert!(
            config_dir.join("state.db").exists(),
            "state store must be created under the config dir"
        );
    }
}
