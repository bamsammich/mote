//! Chrome rendering and widgets for Mote.
//!
//! The slot/element/theme rendering surface and the runtime-owned UI: tab
//! strip, urlbar host, sidebar, integrity panel, permission-approval dialog,
//! and workspace tab picker. Hosts plugin-provided elements into
//! theme-arranged slots. The `UiHost` seam the shell talks to can be defined
//! early; the rendering backend is **gated on the UI-framework ADR** and lands
//! only after it resolves.
//!
//! This crate is a stub awaiting the `2.3`/`2.5` implementation waves.
