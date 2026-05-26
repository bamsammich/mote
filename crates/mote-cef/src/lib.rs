//! CEF isolation wrapper for Mote.
//!
//! This is the **only** crate permitted to depend on `cef`/`cef_rs`. It wraps
//! the Chromium Embedded Framework lifecycle (initialize/shutdown, message
//! loop), browser hosts (tab lifecycle), resource-request hooks (network
//! interception), off-screen rendering, isolated-world script injection, and
//! Chromium profile (= identity) management, translating CEF C++ idioms into
//! safe Rust types. When a CEF upgrade breaks, the breakage is contained here.
//!
//! This crate is a stub awaiting the `1A.5` implementation wave; it carries no
//! functional code yet and does not depend on `cef` until that work lands.
