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

/// The maximum nesting depth `HostValue` will marshal in either direction.
///
/// A plugin-returned table can be arbitrarily deep or self-referential (a cycle).
/// Recursing without a bound risks a native stack overflow, which aborts the
/// **whole process** (M2) — and a cycle never terminates. Both
/// [`HostValue::from_lua`] and [`HostValue::to_lua`] count their recursion depth
/// and return a clean [`MarshalError::DepthExceeded`] past this cap instead of
/// recursing further, so a malicious/buggy value fails the single call rather
/// than taking down the runtime. 64 comfortably exceeds any legitimate host
/// payload (the JSON-ish trees Phase 1 exchanges are shallow) while staying far
/// below the native stack budget.
const MAX_DEPTH: usize = 64;

/// An error marshalling a value across the Lua-state boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum MarshalError {
    /// A table nested deeper than [`MAX_DEPTH`] (a pathologically deep value or
    /// a reference cycle). The call is rejected rather than risking a native
    /// stack overflow (which would abort the process) or an infinite loop.
    DepthExceeded,
    /// A Lua operation failed while reading or building a value (e.g. an invalid
    /// UTF-8 string, or `lua` rejecting table construction).
    Lua(String),
}

impl std::fmt::Display for MarshalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DepthExceeded => write!(
                f,
                "value nesting exceeded the maximum marshalling depth of {MAX_DEPTH} \
                 (too deep or a reference cycle)"
            ),
            Self::Lua(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for MarshalError {}

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
    /// memory) or if the value nests deeper than [`MAX_DEPTH`]. Never panics.
    pub fn to_lua(&self, lua: &Lua) -> Result<Value, String> {
        self.to_lua_depth(lua, 0).map_err(|e| e.to_string())
    }

    /// Depth-bounded recursion for [`to_lua`](Self::to_lua). `depth` is the
    /// current nesting level; past [`MAX_DEPTH`] we refuse rather than recurse,
    /// bounding both pathologically deep values and (defensively) any future
    /// shared structure that could blow the native stack.
    fn to_lua_depth(&self, lua: &Lua, depth: usize) -> Result<Value, MarshalError> {
        if depth > MAX_DEPTH {
            return Err(MarshalError::DepthExceeded);
        }
        match self {
            Self::Nil => Ok(Value::Nil),
            Self::Bool(b) => Ok(Value::Boolean(*b)),
            Self::Number(n) => Ok(Value::Number(*n)),
            Self::Str(s) => lua
                .create_string(s)
                .map(Value::String)
                .map_err(|e| MarshalError::Lua(e.to_string())),
            Self::List(items) => {
                let table = lua
                    .create_table()
                    .map_err(|e| MarshalError::Lua(e.to_string()))?;
                for (i, item) in items.iter().enumerate() {
                    let v = item.to_lua_depth(lua, depth + 1)?;
                    // Lua sequences are 1-indexed. Use a raw set so no
                    // host-installed metatable can intercept the write.
                    table
                        .raw_set(i + 1, v)
                        .map_err(|e| MarshalError::Lua(e.to_string()))?;
                }
                Ok(Value::Table(table))
            }
            Self::Map(entries) => {
                let table = lua
                    .create_table()
                    .map_err(|e| MarshalError::Lua(e.to_string()))?;
                for (k, v) in entries {
                    let lv = v.to_lua_depth(lua, depth + 1)?;
                    table
                        .raw_set(k.as_str(), lv)
                        .map_err(|e| MarshalError::Lua(e.to_string()))?;
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
    /// Lua error, or if the value nests deeper than [`MAX_DEPTH`] (a deep value
    /// or a reference cycle). Never panics.
    ///
    /// # Reading plugin-returned values
    ///
    /// All table reads here are **raw** (`raw_len` / `raw_get` / raw iteration
    /// that does not invoke `__pairs`). Values returned from plugin Lua are read
    /// after `mote-lua` has lifted the deadline/memory protections, so triggering
    /// a metamethod (`__index` / `__pairs`) on such a value could re-enter
    /// unbounded plugin code with no deadline and hang the runtime (M5). Raw
    /// access reads only the table's own storage, never a metatable hook.
    pub fn from_lua(value: &Value) -> Result<Self, String> {
        Self::from_lua_depth(value, 0).map_err(|e| e.to_string())
    }

    /// Depth-bounded recursion for [`from_lua`](Self::from_lua).
    fn from_lua_depth(value: &Value, depth: usize) -> Result<Self, MarshalError> {
        if depth > MAX_DEPTH {
            return Err(MarshalError::DepthExceeded);
        }
        match value {
            Value::Boolean(b) => Ok(Self::Bool(*b)),
            #[allow(clippy::cast_precision_loss)]
            Value::Integer(i) => Ok(Self::Number(*i as f64)),
            Value::Number(n) => Ok(Self::Number(*n)),
            Value::String(s) => Ok(Self::Str(
                s.to_str()
                    .map_err(|e| MarshalError::Lua(e.to_string()))?
                    .to_string(),
            )),
            Value::Table(t) => Self::from_table(t, depth),
            // `Value::Nil` and everything non-portable (functions, userdata,
            // threads, light userdata, errors) map to `Nil` — they cannot cross
            // a state boundary.
            _ => Ok(Self::Nil),
        }
    }

    /// Reads a Lua table as either a list or a map, using **only raw accessors**
    /// (no `__index` / `__pairs` metamethods — see [`from_lua`](Self::from_lua)).
    fn from_table(t: &mote_lua::Table, depth: usize) -> Result<Self, MarshalError> {
        let len = t.raw_len();
        // Detect a pure sequence: length > 0 and every key is in 1..=len.
        if len > 0 {
            let mut only_sequence = true;
            // Raw iteration: `Table::pairs` walks the table with `lua_next` and
            // does NOT invoke `__pairs`.
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, _) = pair.map_err(|e| MarshalError::Lua(e.to_string()))?;
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
                    // Raw read: never triggers `__index`.
                    let v: Value = t.raw_get(i).map_err(|e| MarshalError::Lua(e.to_string()))?;
                    items.push(Self::from_lua_depth(&v, depth + 1)?);
                }
                return Ok(Self::List(items));
            }
        }

        let mut map = BTreeMap::new();
        for pair in t.clone().pairs::<Value, Value>() {
            let (k, v) = pair.map_err(|e| MarshalError::Lua(e.to_string()))?;
            if let Value::String(s) = k {
                let key = s
                    .to_str()
                    .map_err(|e| MarshalError::Lua(e.to_string()))?
                    .to_string();
                map.insert(key, Self::from_lua_depth(&v, depth + 1)?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use mote_lua::{Value, new_sandbox};

    /// Builds a `HostValue` nested `depth` levels deep (a chain of single-element
    /// maps).
    fn deep_host_value(depth: usize) -> HostValue {
        let mut v = HostValue::Number(1.0);
        for _ in 0..depth {
            let mut m = BTreeMap::new();
            m.insert("next".to_owned(), v);
            v = HostValue::Map(m);
        }
        v
    }

    #[test]
    fn to_lua_rejects_excessive_depth_instead_of_overflowing() {
        let lua = new_sandbox().unwrap();
        // Far past the cap; a naive recursion would risk a native stack overflow
        // and abort the process. We must get a clean error.
        let v = deep_host_value(MAX_DEPTH + 100);
        let err = v
            .to_lua(&lua)
            .expect_err("over-deep value must be rejected");
        assert!(err.contains("depth"), "expected a depth error, got: {err}");
    }

    #[test]
    fn shallow_value_round_trips() {
        let lua = new_sandbox().unwrap();
        let v = deep_host_value(MAX_DEPTH - 1);
        let lv = v.to_lua(&lua).expect("a value within the cap marshals");
        let back = HostValue::from_lua(&lv).expect("and reads back");
        assert_eq!(v, back);
    }

    #[test]
    fn from_lua_rejects_excessive_depth_instead_of_overflowing() {
        let lua = new_sandbox().unwrap();
        // Build a deeply-nested table in Lua (well past the cap).
        let table: Value = lua
            .load(
                r"
                local depth = 256
                local t = { leaf = 1 }
                for _ = 1, depth do
                    t = { next = t }
                end
                return t
                ",
            )
            .eval()
            .expect("building a deep table succeeds");
        let err = HostValue::from_lua(&table).expect_err("over-deep table must be rejected");
        assert!(err.contains("depth"), "expected a depth error, got: {err}");
    }

    #[test]
    fn from_lua_terminates_on_self_referential_cycle() {
        let lua = new_sandbox().unwrap();
        // A table that references itself: naive recursion never terminates.
        let table: Value = lua
            .load(
                r"
                local t = {}
                t.self = t
                return t
                ",
            )
            .eval()
            .expect("building a cyclic table succeeds");
        // Must terminate with an error rather than hang/overflow.
        let err = HostValue::from_lua(&table).expect_err("a cycle must be rejected, not hang");
        assert!(err.contains("depth"), "expected a depth error, got: {err}");
    }

    #[test]
    fn from_lua_does_not_trigger_index_metamethod() {
        let lua = new_sandbox().unwrap();
        // A table with an `__index` metamethod that loops forever AND increments
        // a global if ever invoked. Reading the value with raw accessors must
        // neither hang nor invoke `__index`. The table has a normal own field so
        // marshalling has real data to read.
        let table: Value = lua
            .load(
                r#"
                _G.index_called = false
                local mt = {
                    __index = function(_t, _k)
                        _G.index_called = true
                        while true do end -- would hang if ever called
                    end,
                }
                local t = setmetatable({ own = "value" }, mt)
                return t
                "#,
            )
            .eval()
            .expect("building a metatable-laden table succeeds");

        // Raw reads must not hang and must read only the own field.
        let hv = HostValue::from_lua(&table).expect("raw read succeeds without metamethods");
        assert_eq!(hv.get("own").and_then(HostValue::as_str), Some("value"));

        // The `__index` metamethod must never have fired.
        let called: bool = lua
            .load("return _G.index_called")
            .eval()
            .expect("reading the flag");
        assert!(
            !called,
            "from_lua must not trigger __index on plugin values"
        );
    }
}
