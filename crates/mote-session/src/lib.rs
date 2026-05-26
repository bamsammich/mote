//! Identity, workspace, and session state for Mote.
//!
//! The three-axis state model: identity (= Chromium profile via
//! `mote-cef::ProfileHandle`), workspace definitions and pinned tabs, and
//! session state (open tabs, scroll, history stack, form drafts, hidden-tab
//! metadata) persisted continuously to per-identity `SQLite`. Owns tab states
//! (active/hidden/closed), hidden-tab TTL and hold, active-tab discarding, and
//! the crash-recovery-equals-clean-exit restoration model.
//!
//! This crate is a stub awaiting the `2.1` implementation wave.
