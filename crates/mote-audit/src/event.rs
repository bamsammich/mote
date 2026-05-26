//! [`AuditEvent`] — the structured record of a single permission check.

use std::time::SystemTime;

use mote_types::PluginName;
use serde::{Deserialize, Serialize};

/// The outcome of a permission check or capability dispatch.
///
/// Variants are non-exhaustive so new outcomes (e.g. `Throttled`) can be added
/// without breaking downstream pattern matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Decision {
    /// The permission call was granted and executed.
    Allow,
    /// The permission call was explicitly denied (not in grant set, revoked, or
    /// narrowed below the requested scope).
    Deny,
    /// The result was deferred (plugin timed out or returned no decision).
    Defer,
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allow => write!(f, "allow"),
            Self::Deny => write!(f, "deny"),
            Self::Defer => write!(f, "defer"),
        }
    }
}

/// A single audit record: one permission check or capability dispatch.
///
/// Events are created by callers on the hot path and sent over the channel
/// to the audit thread. All fields are cheap to copy (small strings, a
/// timestamp, and an enum), so cloning an event for ring-buffer storage is
/// inexpensive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Wall-clock time when the check occurred (serialized as Unix timestamp
    /// in nanoseconds for portability across serde formats).
    #[serde(
        serialize_with = "serialize_systemtime",
        deserialize_with = "deserialize_systemtime"
    )]
    pub timestamp: SystemTime,

    /// The plugin that made the call.
    ///
    /// Uses the validated [`PluginName`] newtype; the string representation
    /// round-trips through serde unchanged.
    #[serde(
        serialize_with = "serialize_plugin_name",
        deserialize_with = "deserialize_plugin_name"
    )]
    pub plugin: PluginName,

    /// The operation in `domain:action[:resource]` format (e.g.
    /// `"net:intercept_request"`, `"http:fetch:https://api.example.com/*"`).
    pub operation: String,

    /// The outcome of the check.
    pub decision: Decision,

    /// Optional call latency in microseconds.
    ///
    /// `None` when the caller does not time the call (e.g. for pre-call audit
    /// entries) or when latency is not yet known.
    pub latency_us: Option<u64>,

    /// Optional free-form detail (truncated at 512 bytes by the producer).
    ///
    /// Examples: the narrowed scope pattern that caused a denial, the timeout
    /// budget that was exceeded.
    pub detail: Option<String>,
}

impl AuditEvent {
    /// Constructs a minimal event with `timestamp = now`, no latency, and no
    /// detail.
    #[must_use]
    pub fn new(plugin: PluginName, operation: impl Into<String>, decision: Decision) -> Self {
        Self {
            timestamp: SystemTime::now(),
            plugin,
            operation: operation.into(),
            decision,
            latency_us: None,
            detail: None,
        }
    }

    /// Builder: sets the optional latency in microseconds.
    #[must_use]
    pub const fn with_latency(mut self, us: u64) -> Self {
        self.latency_us = Some(us);
        self
    }

    /// Builder: sets the optional detail string.
    ///
    /// Detail strings longer than 512 bytes are silently truncated at the
    /// nearest UTF-8 boundary.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let s: String = detail.into();
        self.detail = Some(if s.len() > 512 {
            // Truncate at a valid UTF-8 boundary.
            let boundary = s
                .char_indices()
                .take_while(|(i, _)| *i < 512)
                .last()
                .map_or(0, |(i, c)| i + c.len_utf8());
            s[..boundary].to_owned()
        } else {
            s
        });
        self
    }
}

// ---------------------------------------------------------------------------
// Custom serde helpers for SystemTime and PluginName
// ---------------------------------------------------------------------------

fn serialize_systemtime<S>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let nanos = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // u128 fits in u64 for the next ~500 years; cast is safe for practical use.
    #[allow(clippy::cast_possible_truncation)]
    s.serialize_u64(nanos as u64)
}

fn deserialize_systemtime<'de, D>(d: D) -> Result<SystemTime, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let nanos = u64::deserialize(d)?;
    Ok(SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(nanos))
}

fn serialize_plugin_name<S>(name: &PluginName, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    s.serialize_str(name.as_str())
}

fn deserialize_plugin_name<'de, D>(d: D) -> Result<PluginName, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let s = String::deserialize(d)?;
    PluginName::new(s).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str) -> PluginName {
        PluginName::new(name).unwrap()
    }

    #[test]
    fn new_sets_defaults() {
        let ev = AuditEvent::new(plugin("adblock"), "net:intercept_request", Decision::Allow);
        assert_eq!(ev.operation, "net:intercept_request");
        assert_eq!(ev.decision, Decision::Allow);
        assert!(ev.latency_us.is_none());
        assert!(ev.detail.is_none());
    }

    #[test]
    fn with_latency_sets_field() {
        let ev = AuditEvent::new(plugin("vim-mode"), "keys:bind", Decision::Allow).with_latency(42);
        assert_eq!(ev.latency_us, Some(42));
    }

    #[test]
    fn with_detail_short_string_unchanged() {
        let ev = AuditEvent::new(plugin("adblock"), "net:intercept_request", Decision::Deny)
            .with_detail("scope narrowed to *.example.com");
        assert_eq!(ev.detail.unwrap(), "scope narrowed to *.example.com");
    }

    #[test]
    fn with_detail_truncates_long_strings() {
        let long = "a".repeat(1024);
        let ev = AuditEvent::new(plugin("adblock"), "net:intercept_request", Decision::Deny)
            .with_detail(long);
        assert!(ev.detail.unwrap().len() <= 512);
    }

    #[test]
    fn serde_round_trip() {
        let ev = AuditEvent::new(
            plugin("password-manager-1password"),
            "http:fetch",
            Decision::Deny,
        )
        .with_latency(99)
        .with_detail("test detail");
        let json = serde_json::to_string(&ev).unwrap();
        let back: AuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.plugin, ev.plugin);
        assert_eq!(back.operation, ev.operation);
        assert_eq!(back.decision, ev.decision);
        assert_eq!(back.latency_us, ev.latency_us);
        assert_eq!(back.detail, ev.detail);
    }

    #[test]
    fn decision_display() {
        assert_eq!(Decision::Allow.to_string(), "allow");
        assert_eq!(Decision::Deny.to_string(), "deny");
        assert_eq!(Decision::Defer.to_string(), "defer");
    }
}
