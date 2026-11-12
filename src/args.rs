//! Argument getters with uniform messages (`missing string argument "name"`).

use serde_json::Value;

/// A required string.
pub fn string<'a>(a: &'a Value, k: &str) -> Result<&'a str, String> {
    a[k].as_str().ok_or_else(|| format!("missing string argument \"{k}\""))
}

/// A required integer.
pub fn integer(a: &Value, k: &str) -> Result<i64, String> {
    a[k].as_i64().ok_or_else(|| format!("missing integer argument \"{k}\""))
}

/// An optional boolean with a default.
pub fn boolean(a: &Value, k: &str, default: bool) -> bool {
    a[k].as_bool().unwrap_or(default)
}

/// An optional string.
pub fn opt_string<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a[k].as_str()
}

/// A required array of strings.
pub fn strings(a: &Value, k: &str) -> Result<Vec<String>, String> {
    a[k].as_array()
        .ok_or_else(|| format!("missing array argument \"{k}\""))?
        .iter()
        .map(|v| v.as_str().map(String::from).ok_or_else(|| format!("\"{k}\" must contain strings")))
        .collect()
}
