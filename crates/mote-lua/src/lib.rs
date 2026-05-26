//! Sandboxed Lua runtime for Mote.
//!
//! Embeds `mlua` + `LuaJIT` and constructs the sandboxed Lua environment with
//! `io`, `os`, `debug`, and `loadstring` removed. Loads a plugin module
//! (constructs `M`, populates `M.api`, reads `M.hooks`/`M.events`/
//! `M.manifest`/`M.mcp_tools` declaratively) **without** calling `setup()`,
//! per the declarative-registration model (ADR-0001). Marshals Rust host
//! functions (the `mote.*` API surface) into Lua and owns the Lua side of
//! synchronous host calls in the hot path.
//!
//! This crate is a stub awaiting the `1C.1` implementation wave; the heavy
//! `mlua` dependency lands with that work.
