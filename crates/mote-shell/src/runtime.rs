//! Phase-1 plugin runtime wiring + the live integrity-panel view-model.
//!
//! This module is the shell's bridge to the plugin subsystem. It:
//!
//! 1. **Instantiates the runtime** ([`build_runtime`]) over the shared
//!    `mote-storage` [`Store`], a `mote-audit` [`AuditLog`] (whose
//!    [`EventProducer`] is fed into the runtime so every gatekept `mote.*` call
//!    is audited), and the v1 [`Registry`].
//! 2. **Loads the bundled first-party plugins** (`plugins/urlbar`,
//!    `plugins/workspace-manager`, embedded at compile time) through
//!    [`mote_runtime::Runtime::load`]'s four-step pipeline with a
//!    [`GrantAsRequested`] approval policy — bundled/first-party code is granted
//!    as requested (DESIGN: bundled plugins are trusted). Their behaviour is
//!    still stubbed; the point is that they are **loaded and visible**.
//! 3. **Builds the integrity-panel view-model from LIVE data** ([`build_panel`]):
//!    the runtime's actually-loaded plugins (name / version / source=bundled /
//!    permissions requested→effective / capabilities / integrity status), the
//!    audit query (recent activity → denials), and per-plugin `mote-storage`
//!    sizes.
//!
//! The rendered HTML is produced by [`render_panel_html`] and served as the
//! `mote://chrome/integrity.html` overlay surface; the shell composites it
//! full-window on the `Ctrl+Shift+I` keybind.

use std::time::Duration;

use mote_audit::{AuditLog, Config};
use mote_registry::Registry;
use mote_runtime::{GrantAsRequested, IdentityContext, Runtime};
use mote_storage::{IdentityScope, Store};
use mote_types::{IdentityId, PluginName, SchemaVersion};
use mote_ui::{
    AuditDecision, AuditRow, DenialRow, IntegrityPanel, IntegrityStatus, PermissionRow, PluginKind,
    PluginRow, StorageRow,
};

/// One bundled first-party plugin: its directory name and embedded `init.lua`.
struct Bundled {
    name: &'static str,
    source: &'static str,
}

/// The bundled first-party plugins, embedded at compile time so the binary is
/// self-contained (the `Bundled` provenance — DESIGN §Integrity).
const BUNDLED: &[Bundled] = &[
    Bundled {
        name: "urlbar",
        source: include_str!("../../../plugins/urlbar/init.lua"),
    },
    Bundled {
        name: "workspace-manager",
        source: include_str!("../../../plugins/workspace-manager/init.lua"),
    },
];

/// The audit-log handle bundled with the runtime. The shell holds it so the
/// background audit thread stays alive and so the integrity panel can query it.
#[derive(Debug)]
pub(crate) struct PluginHost {
    pub(crate) runtime: Runtime,
    pub(crate) audit: AuditLog,
    pub(crate) store: Store,
    /// Names of the plugins that loaded successfully, in load order.
    pub(crate) loaded: Vec<PluginName>,
}

impl PluginHost {
    /// Stand up the runtime over `store`, then load every bundled plugin.
    ///
    /// Reuses the shell's shared `mote-storage` [`Store`] so plugin storage,
    /// the audit sink, and the session all live in one database.
    ///
    /// # Errors
    /// Returns a boxed error only if the registry or audit log cannot be
    /// created. A plugin that fails to load is logged and skipped (the window
    /// keeps running); it does not abort startup.
    pub(crate) fn boot(store: Store) -> Result<Self, Box<dyn std::error::Error>> {
        let registry = Registry::load(SchemaVersion::V1)?;
        let audit = AuditLog::new(
            &store,
            Config {
                // The integrity panel reads recent activity from the ring; keep a
                // generous window. Flush promptly so history survives a crash.
                ring_capacity: 512,
                flush_threshold: 16,
                flush_interval: Duration::from_millis(250),
            },
        )?;
        let mut runtime = Runtime::new(registry, store.clone(), audit.producer());

        // Bundled/first-party plugins run under the default identity and are
        // granted as requested (trusted bundle).
        let identity = IdentityContext::new(IdentityId::new(super::SESSION_IDENTITY));
        let policy = GrantAsRequested;

        let mut loaded = Vec::new();
        for b in BUNDLED {
            match runtime.load(b.source, identity, &policy) {
                Ok(running) => {
                    eprintln!(
                        "mote-shell: loaded bundled plugin `{}` (caps: {:?}, perms: {})",
                        running.name,
                        running.capabilities,
                        running.effective_permissions.len()
                    );
                    loaded.push(running.name);
                }
                Err(e) => {
                    eprintln!(
                        "mote-shell: bundled plugin `{}` failed to load: {e}",
                        b.name
                    );
                }
            }
        }
        eprintln!(
            "mote-shell: plugin runtime up; {}/{} bundled plugins loaded",
            loaded.len(),
            BUNDLED.len()
        );

        Ok(Self {
            runtime,
            audit,
            store,
            loaded,
        })
    }

    /// Build the integrity-panel view-model from the host's LIVE state.
    pub(crate) fn build_panel(&self) -> IntegrityPanel {
        let plugins: Vec<PluginRow> = self
            .loaded
            .iter()
            .filter_map(|name| self.runtime.running(name))
            .map(|running| {
                // For a bundled plugin granted as-requested, the effective grant
                // set IS the requested set, so each effective permission is also
                // its requested form (no narrowing, no denial).
                let permissions = running
                    .effective_permissions
                    .iter()
                    .map(|p| PermissionRow {
                        requested: p.clone(),
                        effective: p.clone(),
                        narrowed: false,
                        denied: false,
                    })
                    .collect();
                PluginRow {
                    name: running.name.as_str().to_owned(),
                    // `RunningPlugin` does not surface the manifest version; the
                    // shell owns the bundle, so it reads the version from the
                    // embedded source it loaded (see `bundled_version`).
                    version: bundled_version(running.name.as_str())
                        .unwrap_or("bundled")
                        .to_owned(),
                    fulfills: running.capabilities.clone(),
                    consumes: running.consumes.clone(),
                    permissions,
                    last_used: self.last_used(&running.name),
                    integrity: IntegrityStatus::Bundled,
                    kind: PluginKind::Bundled,
                    actions: Vec::new(),
                }
            })
            .collect();

        let query = self.audit.query();

        // Network/activity summary: collapse recent events into per-plugin counts.
        let mut counts: Vec<(String, usize)> = query
            .counts_per_plugin()
            .into_iter()
            .map(|(p, c)| (p.as_str().to_owned(), c))
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let network_audit = counts
            .into_iter()
            .map(|(actor, count)| AuditRow {
                actor,
                count: count as u64,
                decision: AuditDecision::Allowed,
                detail: None,
            })
            .collect();

        let denials = query
            .recent_denials(20)
            .into_iter()
            .map(|ev| DenialRow {
                plugin: ev.plugin.as_str().to_owned(),
                permission: ev.operation,
                when: relative_time(ev.timestamp),
            })
            .collect();

        let storage = self
            .loaded
            .iter()
            .filter_map(|name| {
                let bytes = self.storage_bytes(name);
                (bytes > 0).then(|| StorageRow {
                    plugin: name.as_str().to_owned(),
                    size_human: human_bytes(bytes),
                    size_bytes: bytes,
                    label: None,
                })
            })
            .collect();

        IntegrityPanel {
            plugins,
            network_audit,
            storage,
            denials,
        }
    }

    /// The human-relative timestamp of this plugin's most recent audited call.
    fn last_used(&self, name: &PluginName) -> Option<String> {
        self.audit
            .query()
            .recent_for_plugin(name, usize::MAX)
            .last()
            .map(|ev| relative_time(ev.timestamp))
    }

    /// Total bytes a plugin's storage namespace occupies (summing value sizes
    /// across its keys). Bundled plugins default to the `Global` scope.
    fn storage_bytes(&self, name: &PluginName) -> u64 {
        let ns = self.store.namespace(name, IdentityScope::Global);
        let Ok(keys) = ns.list_keys() else {
            return 0;
        };
        keys.iter()
            .filter_map(|k| ns.get(k).ok().flatten())
            .map(|v| v.len() as u64)
            .sum()
    }
}

/// The bundled plugin's declared `version`, read from the embedded source.
///
/// `RunningPlugin` does not expose the manifest version, but the shell owns the
/// bundle and the version is a stable `version = "x.y.z"` line in the embedded
/// `init.lua`. This is shell-side bundle metadata, not runtime introspection.
fn bundled_version(name: &str) -> Option<&'static str> {
    let src = BUNDLED.iter().find(|b| b.name == name)?.source;
    let after = src.split("version").nth(1)?;
    let start = after.find('"')? + 1;
    let rest = &after[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// A section heading with the `[label]` lockup and an optional count badge.
fn section_head(label: &str, count: Option<usize>) -> String {
    let count = count.map_or_else(String::new, |n| {
        format!("<span class=\"section-count\">{n}</span>")
    });
    format!(
        "<section class=\"integrity-section\"><div class=\"integrity-section-head\">\
         <span class=\"section-label\"><span class=\"br\">[</span>{label}\
         <span class=\"br\">]</span></span>{count}</div>"
    )
}

/// The active-plugins section: one card per loaded plugin (name / version /
/// provenance / fulfilled capabilities / requested→effective permissions).
fn plugins_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("active plugins", Some(panel.plugins.len()));
    if panel.plugins.is_empty() {
        s.push_str("<p class=\"empty\">no plugins loaded</p>");
    }
    for p in &panel.plugins {
        let _ = write!(
            s,
            "<article class=\"plugin-card\"><header class=\"plugin-card-head\">\
             <div class=\"plugin-card-name\"><span class=\"prov-glyph\">{}</span> {}</div>\
             <span class=\"plugin-card-version\">v{}</span>\
             <div class=\"plugin-card-badges\"><span class=\"badge {}\">\
             <span class=\"dot\"></span>{}</span></div></header>\
             <div class=\"plugin-provenance\"><span class=\"prov-label\">source</span> \
             <span class=\"prov-value\">{}</span></div>",
            esc(p.kind.glyph()),
            esc(&p.name),
            esc(&p.version),
            esc(p.integrity.badge_variant()),
            esc(p.integrity.label()),
            esc(&p.kind.source_label()),
        );
        if !p.fulfills.is_empty() {
            let caps = p
                .fulfills
                .iter()
                .map(|c| format!("<code>{}</code>", esc(c)))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                s,
                "<div class=\"plugin-caps\"><span class=\"prov-label\">fulfills</span> {caps}</div>"
            );
        }
        s.push_str("<ul class=\"perm-list\">");
        for perm in &p.permissions {
            let narrowed = if perm.narrowed {
                format!(" → <code>{}</code>", esc(&perm.effective))
            } else {
                String::new()
            };
            let _ = write!(
                s,
                "<li class=\"perm-row\"><code>{}</code>{narrowed}</li>",
                esc(&perm.requested),
            );
        }
        s.push_str("</ul>");
        if let Some(used) = &p.last_used {
            let _ = write!(
                s,
                "<div class=\"plugin-lastused\"><span class=\"prov-label\">last used</span> {}</div>",
                esc(used)
            );
        }
        s.push_str("</article>");
    }
    s.push_str("</section>");
    s
}

/// The activity-audit section: per-plugin call counts from the audit ring.
fn audit_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("activity audit", None);
    if panel.network_audit.is_empty() {
        s.push_str("<p class=\"empty\">no audited activity yet</p>");
    } else {
        s.push_str("<ul class=\"audit-list\">");
        for row in &panel.network_audit {
            let _ = write!(
                s,
                "<li class=\"audit-row\"><span class=\"audit-actor\">{}</span> \
                 <span class=\"badge\">{}</span> <span class=\"audit-count\">{} calls</span></li>",
                esc(&row.actor),
                esc(&row.decision.to_string()),
                row.count
            );
        }
        s.push_str("</ul>");
    }
    s.push_str("</section>");
    s
}

/// The storage section: per-plugin storage-namespace sizes.
fn storage_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("storage", None);
    if panel.storage.is_empty() {
        s.push_str("<p class=\"empty\">no plugin storage in use</p>");
    } else {
        s.push_str("<ul class=\"storage-list\">");
        for row in &panel.storage {
            let _ = write!(
                s,
                "<li class=\"storage-row\"><span>{}</span> <span>{}</span></li>",
                esc(&row.plugin),
                esc(&row.size_human)
            );
        }
        s.push_str("</ul>");
    }
    s.push_str("</section>");
    s
}

/// The permission-denials section: recent `Decision::Deny` audit events.
fn denials_section(panel: &IntegrityPanel) -> String {
    use std::fmt::Write as _;
    let mut s = section_head("permission denials", None);
    if panel.denials.is_empty() {
        s.push_str("<p class=\"empty\">no permission denials</p>");
    } else {
        s.push_str("<ul class=\"denial-list\">");
        for d in &panel.denials {
            let _ = write!(
                s,
                "<li class=\"denial-row\"><span>{}</span> <code>{}</code> \
                 <span class=\"denial-when\">{}</span></li>",
                esc(&d.plugin),
                esc(&d.permission),
                esc(&d.when)
            );
        }
        s.push_str("</ul>");
    }
    s.push_str("</section>");
    s
}

/// Render the integrity-panel view-model to a self-contained `mote://chrome`
/// HTML document, reusing `mote-ui`'s design-token + component CSS.
///
/// `mote-ui`'s `IntegrityPanel` has no `to_html` renderer and its shipped
/// `INTEGRITY_PANEL_HTML` is static sample markup with no data-injection seam,
/// so the shell renders the live document here (the doc-comment's "drive the
/// chrome surface directly" path). All plugin-derived strings are HTML-escaped.
pub(crate) fn render_panel_html(panel: &IntegrityPanel) -> String {
    let body = format!(
        "{}{}{}{}",
        plugins_section(panel),
        audit_section(panel),
        storage_section(panel),
        denials_section(panel),
    );
    format!(
        "<!doctype html><html lang=\"en\" data-theme=\"dusk\"><head>\
         <meta charset=\"utf-8\" />\
         <link rel=\"stylesheet\" href=\"tokens.css\" />\
         <link rel=\"stylesheet\" href=\"base.css\" />\
         <link rel=\"stylesheet\" href=\"components/badge.css\" />\
         <link rel=\"stylesheet\" href=\"components/card.css\" />\
         <link rel=\"stylesheet\" href=\"components/integrity-panel.css\" />\
         <style>body{{background:var(--bg);color:var(--fg);padding:var(--space-4);\
         font:14px/1.5 system-ui,sans-serif}}\
         .perm-list,.audit-list,.storage-list,.denial-list{{list-style:none;padding:0;margin:0}}\
         .perm-row,.audit-row,.storage-row,.denial-row{{padding:2px 0}}\
         .integrity-section{{margin-bottom:var(--space-4)}}\
         .plugin-card{{margin:var(--space-2) 0;padding:var(--space-3)}}\
         code{{font-family:ui-monospace,monospace}}\
         .empty{{opacity:.6}} .br{{opacity:.5}}</style>\
         <title>[integrity] — mote</title></head>\
         <body><div class=\"integrity-panel\" role=\"main\">\
         <header class=\"integrity-header\"><span class=\"lockup\">\
         <span class=\"br\">[</span><span class=\"name\">integrity</span>\
         <span class=\"br\">]</span></span>\
         <p class=\"subhead\">active plugins · activity audit · storage · permission denials \
         · press esc / ctrl+shift+i to close</p></header>{body}</div></body></html>"
    )
}

/// HTML-escape a string for safe insertion as text/attribute content. The
/// integrity panel renders plugin-derived strings (names, permissions); each is
/// escaped so a hostile manifest cannot inject markup into the privileged
/// chrome surface (bridge.rs caller discipline).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A coarse human-relative timestamp ("just now", "3m ago", "2h ago", "5d ago").
fn relative_time(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map_or(0, |d| d.as_secs());
    if secs < 5 {
        "just now".to_owned()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// A compact human byte size (B / KB / MB).
#[allow(
    clippy::cast_precision_loss,
    reason = "plugin storage sizes are far below 2^52; the f64 division is exact for display"
)]
fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_version_reads_embedded_source() {
        assert_eq!(bundled_version("urlbar"), Some("0.1.0"));
        assert_eq!(bundled_version("workspace-manager"), Some("0.1.0"));
        assert_eq!(bundled_version("nonexistent"), None);
    }

    #[test]
    fn esc_neutralizes_markup() {
        assert_eq!(esc("<script>"), "&lt;script&gt;");
        assert_eq!(esc("a&b\"c'"), "a&amp;b&quot;c&#39;");
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert!(human_bytes(3_000_000).ends_with("MB"));
    }

    #[test]
    fn render_panel_html_includes_loaded_plugins() {
        let panel = IntegrityPanel {
            plugins: vec![PluginRow {
                name: "urlbar".into(),
                version: "0.1.0".into(),
                fulfills: vec!["ui:urlbar_provider".into()],
                consumes: vec![],
                permissions: vec![PermissionRow {
                    requested: "events:emit".into(),
                    effective: "events:emit".into(),
                    narrowed: false,
                    denied: false,
                }],
                last_used: None,
                integrity: IntegrityStatus::Bundled,
                kind: PluginKind::Bundled,
                actions: vec![],
            }],
            network_audit: vec![],
            storage: vec![],
            denials: vec![],
        };
        let html = render_panel_html(&panel);
        assert!(html.contains("urlbar"));
        assert!(html.contains("ui:urlbar_provider"));
        assert!(html.contains("v0.1.0"));
        assert!(html.contains("bundled"));
        // No sample data leaked in.
        assert!(!html.contains("1password"));
        assert!(!html.contains("vim-mode"));
    }

    #[test]
    fn empty_panel_renders_placeholders() {
        let html = render_panel_html(&IntegrityPanel::empty());
        assert!(html.contains("no plugins loaded"));
    }
}
