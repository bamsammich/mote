//! The [`LuaMarshal`] that lets the dispatch engine carry a [`HostValue`]
//! payload across plugin Lua-state boundaries and read back a filter-chain
//! [`Decision`].
//!
//! DESIGN's reference filter-chain protocol (§What plugin authors need): a hook
//! handler returns a table `{ action = "block" | "modify" | "allow", ... }`, or
//! `nil` for `defer`. This marshal implements exactly that:
//!
//! - `nil` / unrecognized → [`Decision::Defer`].
//! - `{ action = "block", reason = "..." }` → [`Decision::Block`].
//! - `{ action = "modify", payload = <table> }` → [`Decision::Modify`] (the new
//!   payload is read back as a [`HostValue`]).
//! - `{ action = "allow" }` → [`Decision::Allow`].
//!
//! For a broadcast or keybind hook the return value is irrelevant; the engine
//! discards the decoded decision.

use mote_dispatch::{Decision, LuaMarshal};
use mote_lua::{Lua, Value};

use crate::value::HostValue;

/// Marshals [`HostValue`] payloads into and decisions out of a plugin's Lua
/// state for the dispatch engine.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HostMarshal;

impl LuaMarshal<HostValue> for HostMarshal {
    fn encode(&self, lua: &Lua, payload: &HostValue) -> Result<Value, String> {
        payload.to_lua(lua)
    }

    fn decode(&self, _lua: &Lua, value: Value) -> Result<Decision<HostValue>, String> {
        let Value::Table(table) = &value else {
            // nil / non-table → no opinion.
            return Ok(Decision::Defer);
        };

        // The decision table is a plugin-RETURNED value: `mote-lua` lifts the
        // deadline/memory protections before handing it back, so every field
        // read here MUST be raw (`raw_get`) — a `__index` metamethod could
        // otherwise re-enter unbounded plugin code with no deadline and hang the
        // dispatch (the post-deadline metamethod-reentry hazard, M5). `from_lua`
        // (below) is likewise raw.
        let action: Value = table.raw_get("action").map_err(|e| e.to_string())?;
        let Value::String(action) = action else {
            return Ok(Decision::Defer);
        };
        let action = action.to_str().map_err(|e| e.to_string())?;

        match action.as_ref() {
            "block" => {
                let reason: Value = table.raw_get("reason").map_err(|e| e.to_string())?;
                let reason = match reason {
                    Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
                    _ => "blocked".to_owned(),
                };
                Ok(Decision::Block { reason })
            }
            "modify" => {
                let payload: Value = table.raw_get("payload").map_err(|e| e.to_string())?;
                let hv = HostValue::from_lua(&payload)?;
                Ok(Decision::Modify { payload: hv })
            }
            "allow" => Ok(Decision::Allow),
            _ => Ok(Decision::Defer),
        }
    }
}
