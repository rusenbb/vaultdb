//! Conversions between `vaultdb_core::Value` and `serde_json::Value`.
//!
//! The ORM uses serde as the round-trip mechanism: a vault `Record`'s
//! frontmatter is turned into a `serde_json::Value::Object` and then
//! deserialised into the user's typed struct, and vice-versa for writes.

use serde_json::Value as JsonValue;
use vaultdb_core::Value;

use crate::error::OrmError;

/// Convert a `vaultdb_core::Value` into a `serde_json::Value`.
///
/// Lossless except for `Value::Map` keys, which are already `String`s in
/// both representations.
pub fn value_to_json(v: &Value) -> JsonValue {
    match v {
        Value::Null => JsonValue::Null,
        Value::String(s) => JsonValue::String(s.clone()),
        Value::Integer(i) => JsonValue::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::List(items) => JsonValue::Array(items.iter().map(value_to_json).collect()),
        Value::Map(m) => {
            let obj = m
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            JsonValue::Object(obj)
        }
        // `Value` is `#[non_exhaustive]` — unknown future variants
        // round-trip as `null` and surface visibly in downstream code
        // rather than crashing the typed deserialiser. Bump `vaultdb-orm`
        // alongside any `Value` addition in `vaultdb-core` to teach this
        // match the new variant.
        _ => JsonValue::Null,
    }
}

/// Convert a `serde_json::Value` back into a `vaultdb_core::Value`.
///
/// Used when materialising a typed struct back into the wire shape for
/// `UpdateBuilder::set` calls.
pub fn json_to_value(v: JsonValue) -> Result<Value, OrmError> {
    Ok(match v {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                return Err(OrmError::Custom(format!("unrepresentable number: {n}")));
            }
        }
        JsonValue::String(s) => Value::String(s),
        JsonValue::Array(items) => {
            let mapped: Result<Vec<_>, _> = items.into_iter().map(json_to_value).collect();
            Value::List(mapped?)
        }
        JsonValue::Object(obj) => {
            let mut map = std::collections::BTreeMap::new();
            for (k, v) in obj {
                map.insert(k, json_to_value(v)?);
            }
            Value::Map(map)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn round_trip_primitives() {
        let cases = vec![
            Value::Null,
            Value::String("hi".into()),
            Value::Integer(42),
            Value::Float(std::f64::consts::PI),
            Value::Bool(true),
        ];
        for v in cases {
            let json = value_to_json(&v);
            let back = json_to_value(json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn round_trip_list() {
        let v = Value::List(vec![Value::Integer(1), Value::String("two".into())]);
        let back = json_to_value(value_to_json(&v)).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn round_trip_map() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), Value::Integer(1));
        m.insert("b".to_string(), Value::String("two".into()));
        let v = Value::Map(m);
        let back = json_to_value(value_to_json(&v)).unwrap();
        assert_eq!(v, back);
    }
}
