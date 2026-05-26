//! Browser composition root for Mote.
//!
//! Wires runtime + session + UI + secrets + pluginmgr + MCP into "a browser":
//! window management (single window in v0.1, multi-window working), tab
//! lifecycle bridging session ↔ CEF ↔ UI, the config loader (user `init.lua`
//! → runtime state), and the event-loop integration. This is the glue layer
//! where the integration seams live.
//!
//! This crate is a stub awaiting the `2.4` implementation wave.
