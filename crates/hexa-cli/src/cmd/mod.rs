//! One module per subcommand group.

pub mod convert;
pub mod diff;
pub mod embedded;
pub mod exe;
pub mod hash;
pub mod info;
pub mod search;
pub mod strings;
pub mod sys;
pub mod template;
pub mod transform;

use serde::Serialize;

/// The serde name of a unit enum variant (`"intelHex"` …).
pub fn name_of<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// Splits comma-separated values and trims them.
pub fn split_list(items: &[String]) -> Vec<String> {
    items
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
