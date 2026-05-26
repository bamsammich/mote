//! Model Context Protocol endpoint for Mote.
//!
//! The MCP server endpoint (loopback by default via `mcp:server:bind_loopback`;
//! public only via `mcp:server:bind_public`). Aggregates tools from all
//! plugins fulfilling the non-exclusive `mcp:server` capability, namespaced
//! `<plugin-name>.<tool-name>`, exposed at one endpoint. Routes incoming tool
//! calls to the owning plugin via `mote-dispatch`, executing under the owning
//! plugin's permissions, and implements the `mcp:client:<server-name>` outbound
//! path. Feeds MCP activity to the audit log for the integrity panel.
//!
//! This crate is a stub awaiting the Phase 8 implementation wave.
