//! The host-owned marshalling value that crosses plugin state boundaries.
//!
//! Every plugin runs in its **own** sandboxed Lua state (per-plugin isolation;
//! DESIGN §Script Injection and Isolated Worlds). An `mlua::Value` belongs to
//! exactly one state — moving one into a *different* state is a hard panic in
//! mlua. The runtime routinely moves data across states:
//!
//! - a filter-chain `modify` payload cascades across plugins (dispatch);
//! - `events.emit` fans a payload out from one plugin to another's `M.events`
//!   handler;
//! - `capabilities.invoke` passes arguments from a consumer to a fulfiller and
//!   returns the result back.
//!
//! So nothing Lua-state-bound may escape a state. [`HostValue`] is the plain
//! Rust data interchange the runtime owns: a small JSON-ish tree sufficient for
//! the host payloads Phase 1 exercises. It converts to/from a [`Value`] *within*
//! a given state via [`HostValue::to_lua`] / [`HostValue::from_lua`], which are
//! the only places a value is materialized into or read out of a state.

use std::collections::BTreeMap;

use mote_lua::{Lua, Value};

/// A host-owned, state-independent value used to marshal payloads and results
/// across plugin Lua-state boundaries.
///
/// Deliberately a small JSON-ish tree: this is the interchange between plugins
/// and between a plugin and the host, not a faithful Lua value. Functions,
/// userdata, threads, and other non-portable Lua types are intentionally
/// unrepresentable — they cannot meaningfully cross a state boundary.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum HostValue {
    /// Lua `nil` / absence of a value.
    #[default]
    Nil,
    /// A boolean.
    Bool(bool),
    /// A number. Lua numbers are IEEE-754 doubles under `LuaJIT`; integers are
    /// represented here as their `f64` value.
    Number(f64),
    /// A UTF-8 string.
    Str(String),
    /// A sequence (Lua array part: contiguous integer keys from 1).
    List(Vec<Self>),
    /// A string-keyed map (Lua table hash part).
    Map(BTreeMap<String, Self>),
}

impl HostValue {
    /// Materializes this value into `lua`'s state as an mlua `Value`.
    ///
    /// This is one of the only two places a host value enters a Lua state; the
    /// produced value belongs to `lua` and must never be moved to another state.
    ///
    /// # Errors
    ///
    /// Returns an error string if `lua` rejects table construction (e.g. out of
    /// memory). Never panics.
    pub fn to_lua(&self, lua: &Lua) -> Result<Value, String> {
        match self {
            Self::Nil => Ok(Value::Nil),
            Self::Bool(b) => Ok(Value::Boolean(*b)),
            Self::Number(n) => Ok(Value::Number(*n)),
            Self::Str(s) => lua
                .create_string(s)
                .map(Value::String)
                .map_err(|e| e.to_string()),
            Self::List(items) => {
                let table = lua.create_table().map_err(|e| e.to_string())?;
                for (i, item) in items.iter().enumerate() {
                    let v = item.to_lua(lua)?;
                    // Lua sequences are 1-indexed.
                    table.set(i + 1, v).map_err(|e| e.to_string())?;
                }
                Ok(Value::Table(table))
            }
            Self::Map(entries) => {
                let table = lua.create_table().map_err(|e| e.to_string())?;
                for (k, v) in entries {
                    let lv = v.to_lua(lua)?;
                    table.set(k.as_str(), lv).map_err(|e| e.to_string())?;
                }
                Ok(Value::Table(table))
            }
        }
    }

    /// Reads a host value out of `lua`'s state from an mlua `Value`.
    ///
    /// A table with a non-empty contiguous `1..=n` integer sequence and no other
    /// keys is read as a [`HostValue::List`]; otherwise its string keys are read
    /// as a [`HostValue::Map`] (non-string keys are ignored). Unrepresentable
    /// Lua types (function/userdata/thread) become [`HostValue::Nil`] — they
    /// cannot cross a state boundary.
    ///
    /// # Errors
    ///
    /// Returns an error string if reading a string or iterating a table raises a
    /// Lua error. Never panics.
    pub fn from_lua(value: &Value) -> Result<Self, String> {
        match value {
            Value::Boolean(b) => Ok(Self::Bool(*b)),
            #[allow(clippy::cast_precision_loss)]
            Value::Integer(i) => Ok(Self::Number(*i as f64)),
            Value::Number(n) => Ok(Self::Number(*n)),
            Value::String(s) => Ok(Self::Str(
                s.to_str().map_err(|e| e.to_string())?.to_string(),
            )),
            Value::Table(t) => Self::from_table(t),
            // `Value::Nil` and everything non-portable (functions, userdata,
            // threads, light userdata, errors) map to `Nil` — they cannot cross
            // a state boundary.
            _ => Ok(Self::Nil),
        }
    }

    /// Reads a Lua table as either a list or a map.
    fn from_table(t: &mote_lua::Table) -> Result<Self, String> {
        let len = t.raw_len();
        // Detect a pure sequence: length > 0 and every key is in 1..=len.
        if len > 0 {
            let mut only_sequence = true;
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, _) = pair.map_err(|e| e.to_string())?;
                match k {
                    Value::Integer(i) if i >= 1 && usize::try_from(i).is_ok_and(|i| i <= len) => {}
                    _ => {
                        only_sequence = false;
                        break;
                    }
                }
            }
            if only_sequence {
                let mut items = Vec::with_capacity(len);
                for i in 1..=len {
                    let v: Value = t.get(i).map_err(|e| e.to_string())?;
                    items.push(Self::from_lua(&v)?);
                }
                return Ok(Self::List(items));
            }
        }

        let mut map = BTreeMap::new();
        for pair in t.clone().pairs::<Value, Value>() {
            let (k, v) = pair.map_err(|e| e.to_string())?;
            if let Value::String(s) = k {
                let key = s.to_str().map_err(|e| e.to_string())?.to_string();
                map.insert(key, Self::from_lua(&v)?);
            }
        }
        Ok(Self::Map(map))
    }

    /// Returns the string contents if this is a [`HostValue::Str`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the map entry under `key`, if this is a [`HostValue::Map`].
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Map(m) => m.get(key),
            _ => None,
        }
    }
}
