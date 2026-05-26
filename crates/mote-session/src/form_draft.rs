//! Form draft storage — conservative save of in-progress form input.
//!
//! Opt-in by default (disabled). When enabled, the rules are conservative:
//!
//! - Saves a field value only after more than 20 characters are typed.
//! - Never saves `type=password` fields.
//! - Never saves fields with `autocomplete=off` or `autocomplete=cc-*`.
//! - Drafts expire after 7 days.
//! - Per-site opt-out is a v0.2+ concern (`session:exclude_forms`).
//!
//! See `DESIGN.md` §Form drafts.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::serde_helpers::system_time as serde_sys_time;

/// Configuration for form-draft persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormDraftConfig {
    /// Whether form-draft saving is enabled at all.
    ///
    /// Off by default — opt-in per `DESIGN.md` §Form drafts.
    pub enabled: bool,
    /// Minimum character count before a field value is saved.
    ///
    /// The design specifies `>20` chars, so the default threshold is 20
    /// (values with `len > 20` are saved; values with `len == 20` are not).
    pub min_chars: usize,
    /// How long a draft survives before automatic deletion.
    pub ttl: Duration,
}

impl Default for FormDraftConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_chars: 20,
            ttl: Duration::from_hours(168), // 7 days
        }
    }
}

/// A single saved form-field draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormDraftEntry {
    /// The form field `name` or `id` attribute.
    pub field_name: String,
    /// The saved value.
    pub value: String,
    /// When this draft was saved (used for TTL expiry).
    #[serde(with = "serde_sys_time")]
    pub saved_at: SystemTime,
}

/// Runtime store for form drafts, keyed by page origin URL.
///
/// Flushed to and restored from session `SQLite` as part of [`Session`](crate::Session)
/// persistence.
#[derive(Debug, Clone)]
pub struct FormDraftStore {
    config: FormDraftConfig,
    /// Map from page origin URL to the drafts for that origin.
    pub(crate) drafts: HashMap<String, Vec<FormDraftEntry>>,
}

impl FormDraftStore {
    /// Creates an empty store with the given configuration.
    #[must_use]
    pub fn new(config: FormDraftConfig) -> Self {
        Self {
            config,
            drafts: HashMap::new(),
        }
    }

    /// Attempts to save a form field value, applying all sensitivity filters.
    ///
    /// The save is silently skipped when any of these conditions hold:
    ///
    /// - Form drafts are disabled in config ([`FormDraftConfig::enabled`]).
    /// - The value length is ≤ [`FormDraftConfig::min_chars`].
    /// - `is_password` is `true` (field has `type=password`).
    /// - `autocomplete_blocked` is `true` (field has `autocomplete=off` or
    ///   `autocomplete=cc-*`).
    pub fn try_save(
        &mut self,
        origin: String,
        field_name: &str,
        value: &str,
        is_password: bool,
        autocomplete_blocked: bool,
    ) {
        if !self.config.enabled
            || value.len() <= self.config.min_chars
            || is_password
            || autocomplete_blocked
        {
            return;
        }

        let entry = FormDraftEntry {
            field_name: field_name.to_owned(),
            value: value.to_owned(),
            saved_at: SystemTime::now(),
        };

        let entries = self.drafts.entry(origin).or_default();
        if let Some(existing) = entries.iter_mut().find(|e| e.field_name == field_name) {
            *existing = entry;
        } else {
            entries.push(entry);
        }
    }

    /// Returns the saved draft value for a field at the given origin, if any.
    #[must_use]
    pub fn get(&self, origin: &str, field_name: &str) -> Option<&str> {
        self.drafts
            .get(origin)?
            .iter()
            .find(|e| e.field_name == field_name)
            .map(|e| e.value.as_str())
    }

    /// Removes all drafts older than the configured TTL.
    pub fn reap_expired(&mut self) {
        let ttl = self.config.ttl;
        let now = SystemTime::now();
        for entries in self.drafts.values_mut() {
            entries.retain(|e| now.duration_since(e.saved_at).unwrap_or(Duration::ZERO) < ttl);
        }
        self.drafts.retain(|_, entries| !entries.is_empty());
    }

    /// Serializes all drafts to JSON for storage.
    ///
    /// # Errors
    ///
    /// Returns [`serde_json::Error`] if serialization fails (should be
    /// unreachable for well-formed in-memory data).
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&self.drafts)
    }

    /// Deserializes drafts from stored JSON.
    ///
    /// # Errors
    ///
    /// Returns [`serde_json::Error`] if the stored bytes are not valid JSON
    /// or do not match the expected schema.
    pub fn from_json(config: FormDraftConfig, bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let drafts: HashMap<String, Vec<FormDraftEntry>> = serde_json::from_slice(bytes)?;
        Ok(Self { config, drafts })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A config with drafts enabled for testing purposes.
    fn cfg_enabled() -> FormDraftConfig {
        FormDraftConfig {
            enabled: true,
            ..FormDraftConfig::default()
        }
    }

    #[test]
    fn save_long_field_stores_draft() {
        let mut store = FormDraftStore::new(cfg_enabled());
        let origin = "https://example.com".to_owned();
        let long_value = "a".repeat(25);
        store.try_save(origin.clone(), "comment", &long_value, false, false);
        assert!(store.get(&origin, "comment").is_some());
    }

    #[test]
    fn short_field_not_saved() {
        let mut store = FormDraftStore::new(cfg_enabled());
        // "short" has 5 chars which is ≤ min_chars (20).
        store.try_save("https://example.com".to_owned(), "q", "short", false, false);
        assert!(store.get("https://example.com", "q").is_none());
    }

    #[test]
    fn disabled_by_default() {
        // Default config has enabled=false; no saves even for long values.
        let mut store = FormDraftStore::new(FormDraftConfig::default());
        let long_value = "a".repeat(30);
        store.try_save(
            "https://example.com".to_owned(),
            "bio",
            &long_value,
            false,
            false,
        );
        assert!(store.get("https://example.com", "bio").is_none());
    }

    #[test]
    fn password_field_never_saved() {
        let mut store = FormDraftStore::new(cfg_enabled());
        let long_value = "p".repeat(30);
        store.try_save(
            "https://example.com".to_owned(),
            "pass",
            &long_value,
            true, // is_password
            false,
        );
        assert!(store.get("https://example.com", "pass").is_none());
    }

    #[test]
    fn autocomplete_off_never_saved() {
        let mut store = FormDraftStore::new(cfg_enabled());
        let long_value = "x".repeat(30);
        store.try_save(
            "https://example.com".to_owned(),
            "secret",
            &long_value,
            false,
            true, // autocomplete_blocked
        );
        assert!(store.get("https://example.com", "secret").is_none());
    }

    #[test]
    fn overwrite_existing_entry() {
        let mut store = FormDraftStore::new(cfg_enabled());
        let v1 = "a".repeat(25);
        let v2 = "b".repeat(25);
        store.try_save("https://example.com".to_owned(), "bio", &v1, false, false);
        store.try_save("https://example.com".to_owned(), "bio", &v2, false, false);
        assert_eq!(store.get("https://example.com", "bio"), Some(v2.as_str()));
    }

    #[test]
    fn expired_entries_cleared() {
        let mut store = FormDraftStore::new(cfg_enabled());
        let long_value = "a".repeat(30);
        store.try_save(
            "https://example.com".to_owned(),
            "text",
            &long_value,
            false,
            false,
        );
        // Manually age the entry to 8 days ago (beyond the 7-day TTL).
        let entry = store
            .drafts
            .get_mut("https://example.com")
            .unwrap()
            .iter_mut()
            .find(|e| e.field_name == "text")
            .unwrap();
        entry.saved_at = SystemTime::now() - Duration::from_hours(192); // 8 days
        store.reap_expired();
        assert!(store.get("https://example.com", "text").is_none());
    }

    #[test]
    fn fresh_entries_not_cleared() {
        let mut store = FormDraftStore::new(cfg_enabled());
        let long_value = "a".repeat(30);
        store.try_save(
            "https://example.com".to_owned(),
            "text",
            &long_value,
            false,
            false,
        );
        store.reap_expired();
        assert!(store.get("https://example.com", "text").is_some());
    }

    #[test]
    fn draft_store_roundtrip_json() {
        let mut store = FormDraftStore::new(cfg_enabled());
        let long_value = "hello world this is long enough".to_owned();
        store.try_save(
            "https://example.com".to_owned(),
            "bio",
            &long_value,
            false,
            false,
        );
        let json_bytes = store.to_json().unwrap();
        let back = FormDraftStore::from_json(cfg_enabled(), &json_bytes).unwrap();
        assert_eq!(
            back.get("https://example.com", "bio"),
            Some(long_value.as_str())
        );
    }

    #[test]
    fn missing_field_returns_none() {
        let store = FormDraftStore::new(cfg_enabled());
        assert!(store.get("https://example.com", "nonexistent").is_none());
    }
}
