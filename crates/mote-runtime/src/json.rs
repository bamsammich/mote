//! Conversion helpers between [`HostValue`] and [`serde_json::Value`].
//!
//! This module is the glue layer for `mote.json.encode` / `mote.json.decode`.
//! It lives here so the marshalling logic is unit-testable in pure Rust,
//! independently of the Lua runtime.
//!
//! ## Array vs. object disambiguation
//!
//! The [`HostValue`] type already handles the Lua `table` → `List` / `Map`
//! disambiguation in `HostValue::from_lua`: a table with a non-empty contiguous
//! `1..=n` integer sequence and no other keys is read as `HostValue::List`
//! (→ JSON array); otherwise string keys are collected into `HostValue::Map`
//! (→ JSON object). Callers rely on `HostValue::from_lua` for this; the
//! conversions here are a straightforward structural mapping.
//!
//! ## `HostValue::Nil` ↔ JSON `null`
//!
//! `HostValue::Nil` maps to `serde_json::Value::Null` and vice-versa. In Lua,
//! `mote.json.decode("null")` returns `nil` because there is no other sensible
//! representation for JSON `null` in Lua.
//!
//! ## Bytes policy
//!
//! [`HostValue`] has **no `Bytes` variant** (the enum has `Nil`, `Bool`,
//! `Number`, `Str`, `List`, `Map`). The "bytes → JSON" question raised in the
//! implementation brief is therefore moot: no `Bytes` path exists to handle.
//! If a `Bytes` variant is added in the future, the appropriate policy would be
//! to encode as a UTF-8 string if valid, or return `None` (encode → `nil`) if
//! not — consistent with how the rest of the host API treats unrepresentable
//! values.

use serde_json::Value as Json;

use crate::value::HostValue;

/// Converts a [`HostValue`] to a [`serde_json::Value`].
///
/// The mapping is structural and total: every `HostValue` variant maps to a
/// corresponding JSON type. `HostValue::Nil` → `null`; `HostValue::Number` is
/// encoded as a JSON number (using `serde_json`'s `f64` representation).
///
/// Numbers that are integer-valued are emitted as integers where possible
/// (`serde_json` preserves the distinction so round-trips are exact for values
/// within the integer range).
#[must_use]
pub(crate) fn host_to_json(v: &HostValue) -> Json {
    match v {
        HostValue::Nil => Json::Null,
        HostValue::Bool(b) => Json::Bool(*b),
        HostValue::Number(n) => {
            // Prefer integer representation when the value is exactly integral
            // and within i64 range, so that JSON consumers see `42` not `42.0`.
            // The casts are intentional: we use `as i64` to truncate, then check
            // whether the f64 round-trip is exact; we accept the precision loss
            // in the comparison because that IS the test we want.
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let as_i64 = *n as i64;
            #[allow(clippy::float_cmp, clippy::cast_precision_loss)]
            if *n == as_i64 as f64 {
                Json::Number(as_i64.into())
            } else {
                // f64 values that are not representable as serde_json::Number
                // (NaN / infinity) fall back to null, matching the nil idiom.
                serde_json::Number::from_f64(*n).map_or(Json::Null, Json::Number)
            }
        }
        HostValue::Str(s) => Json::String(s.clone()),
        HostValue::List(items) => Json::Array(items.iter().map(host_to_json).collect()),
        HostValue::Map(entries) => {
            let map = entries
                .iter()
                .map(|(k, v)| (k.clone(), host_to_json(v)))
                .collect();
            Json::Object(map)
        }
    }
}

/// Converts a [`serde_json::Value`] to a [`HostValue`].
///
/// The mapping is total and structural. JSON `null` → `HostValue::Nil`; JSON
/// numbers are decoded as `HostValue::Number(f64)` (integers and floats both
/// go through `f64`, matching `HostValue`'s single numeric variant). JSON
/// arrays → `HostValue::List`; JSON objects → `HostValue::Map`.
#[must_use]
pub(crate) fn json_to_host(v: &Json) -> HostValue {
    match v {
        Json::Null => HostValue::Nil,
        Json::Bool(b) => HostValue::Bool(*b),
        Json::Number(n) => HostValue::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => HostValue::Str(s.clone()),
        Json::Array(items) => HostValue::List(items.iter().map(json_to_host).collect()),
        Json::Object(entries) => HostValue::Map(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), json_to_host(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value as Json, json};

    use super::*;
    use crate::value::HostValue;

    // -----------------------------------------------------------------------
    // host_to_json
    // -----------------------------------------------------------------------

    #[test]
    fn nil_to_null() {
        assert_eq!(host_to_json(&HostValue::Nil), Json::Null);
    }

    #[test]
    fn bool_to_json_bool() {
        assert_eq!(host_to_json(&HostValue::Bool(true)), Json::Bool(true));
        assert_eq!(host_to_json(&HostValue::Bool(false)), Json::Bool(false));
    }

    #[test]
    fn integer_number_emits_as_integer() {
        let j = host_to_json(&HostValue::Number(42.0));
        assert_eq!(j, json!(42));
        // Ensure the JSON text has no decimal point.
        assert_eq!(serde_json::to_string(&j).unwrap(), "42");
    }

    #[test]
    fn negative_integer_number_emits_as_integer() {
        let j = host_to_json(&HostValue::Number(-7.0));
        assert_eq!(j, json!(-7));
        assert_eq!(serde_json::to_string(&j).unwrap(), "-7");
    }

    #[test]
    fn float_number_emits_as_float() {
        // Use 1.5 (exact in f64, not close to a named constant) to avoid the
        // `approx_constant` lint that fires on values near π (3.14…).
        let j = host_to_json(&HostValue::Number(1.5));
        // serde_json should produce a number node; exact string form is impl-defined
        // but must round-trip as a float.
        if let Json::Number(n) = &j {
            let roundtrip = n.as_f64().unwrap();
            assert!((roundtrip - 1.5_f64).abs() < 1e-10, "float must round-trip");
        } else {
            panic!("expected Json::Number, got {j:?}");
        }
    }

    #[test]
    fn nan_and_infinity_become_null() {
        assert_eq!(host_to_json(&HostValue::Number(f64::NAN)), Json::Null);
        assert_eq!(host_to_json(&HostValue::Number(f64::INFINITY)), Json::Null);
        assert_eq!(
            host_to_json(&HostValue::Number(f64::NEG_INFINITY)),
            Json::Null
        );
    }

    #[test]
    fn str_to_json_string() {
        assert_eq!(
            host_to_json(&HostValue::Str("hello".to_owned())),
            Json::String("hello".to_owned())
        );
    }

    #[test]
    fn list_to_json_array() {
        let hv = HostValue::List(vec![HostValue::Number(1.0), HostValue::Number(2.0)]);
        assert_eq!(host_to_json(&hv), json!([1, 2]));
    }

    #[test]
    fn map_to_json_object() {
        let mut m = BTreeMap::new();
        m.insert("a".to_owned(), HostValue::Number(1.0));
        m.insert("b".to_owned(), HostValue::Bool(true));
        let hv = HostValue::Map(m);
        let j = host_to_json(&hv);
        assert_eq!(j["a"], json!(1));
        assert_eq!(j["b"], json!(true));
    }

    #[test]
    fn nested_list_of_maps() {
        let mut m1 = BTreeMap::new();
        m1.insert("x".to_owned(), HostValue::Number(1.0));
        let mut m2 = BTreeMap::new();
        m2.insert("x".to_owned(), HostValue::Number(2.0));
        let hv = HostValue::List(vec![HostValue::Map(m1), HostValue::Map(m2)]);
        let j = host_to_json(&hv);
        assert_eq!(j[0]["x"], json!(1));
        assert_eq!(j[1]["x"], json!(2));
    }

    // -----------------------------------------------------------------------
    // json_to_host
    // -----------------------------------------------------------------------

    #[test]
    fn null_to_nil() {
        assert_eq!(json_to_host(&Json::Null), HostValue::Nil);
    }

    #[test]
    fn json_bool_to_host_bool() {
        assert_eq!(json_to_host(&Json::Bool(true)), HostValue::Bool(true));
        assert_eq!(json_to_host(&Json::Bool(false)), HostValue::Bool(false));
    }

    #[test]
    fn json_integer_to_host_number() {
        let hv = json_to_host(&json!(42));
        assert_eq!(hv, HostValue::Number(42.0));
    }

    #[test]
    fn json_float_to_host_number() {
        // 1.5 is exact in f64 and not close to any named constant — avoids
        // the `approx_constant` lint that fires on values near π (3.14…).
        let hv = json_to_host(&json!(1.5));
        if let HostValue::Number(n) = hv {
            assert!((n - 1.5_f64).abs() < 1e-10);
        } else {
            panic!("expected Number");
        }
    }

    #[test]
    fn json_string_to_host_str() {
        let hv = json_to_host(&Json::String("world".to_owned()));
        assert_eq!(hv, HostValue::Str("world".to_owned()));
    }

    #[test]
    fn json_array_to_host_list() {
        let hv = json_to_host(&json!([1, 2, 3]));
        assert_eq!(
            hv,
            HostValue::List(vec![
                HostValue::Number(1.0),
                HostValue::Number(2.0),
                HostValue::Number(3.0),
            ])
        );
    }

    #[test]
    fn json_object_to_host_map() {
        let hv = json_to_host(&json!({"k": "v", "n": 5}));
        assert_eq!(hv.get("k"), Some(&HostValue::Str("v".to_owned())));
        assert_eq!(hv.get("n"), Some(&HostValue::Number(5.0)));
    }

    #[test]
    fn nested_round_trip_host_to_json_to_host() {
        let mut inner = BTreeMap::new();
        inner.insert("a".to_owned(), HostValue::Number(99.0));
        let hv = HostValue::List(vec![HostValue::Map(inner), HostValue::Bool(true)]);
        let back = json_to_host(&host_to_json(&hv));
        assert_eq!(back, hv);
    }
}
