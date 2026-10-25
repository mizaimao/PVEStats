//! Lenient reads from API payloads: a missing field, a null and a wrong type
//! all read as absent, the way the app's `.get()` chains do.

use serde_json::Value;

pub fn num(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        // PVE sends loadavg as strings.
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn int(v: &Value, key: &str) -> Option<u64> {
    v.get(key)?.as_u64()
}

pub fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

pub fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Python truthiness for the flags PVE sends as 0/1.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}
