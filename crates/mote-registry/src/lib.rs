//! Versioned permission, capability, and token registries for Mote.
//!
//! Loads and validates the machine-readable registry files, resolves a
//! plugin's targeted `schema = "vN"` to the correct registry version, and
//! provides capability **contract** descriptors (required API surface,
//! required events, exclusivity, dispatch shape, `critical` flag) that the
//! loader uses for conformance checks. Enforces additive-only-within-version.
//!
//! This crate is a stub awaiting the `1B.1` implementation wave.
