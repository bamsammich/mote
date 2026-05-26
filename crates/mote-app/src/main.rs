//! The Mote binary.
//!
//! `main` parses args and dispatches to `mote-cli` (management subcommands) or
//! boots `mote-shell` (the browser). It also owns the CEF subprocess entry shim
//! that Chromium's multi-process model requires the binary to handle.
//!
//! This is a stub awaiting the `mote-shell`/`mote-cli` wiring; it currently does
//! nothing but exit cleanly.

const fn main() {}
