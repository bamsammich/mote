//! Permission model and enforcement primitives for Mote.
//!
//! Owns the permission grammar (`domain:action[:resource]`), parsing,
//! glob/negation matching, requested-vs-effective narrowing, and the
//! gatekeeper API the dispatch layer queries ("does plugin X hold permission P
//! for resource R?"). Holds the per-plugin effective grant set and enforces
//! deny precedence.
//!
//! This crate is a stub awaiting the `1A.3` implementation wave.
