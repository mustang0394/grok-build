use serde::{Deserialize, Deserializer};

pub fn empty_string_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

/// Deserialize `Option<Option<T>>`: absent (`None`) leaves, `null` (`Some(None)`) clears, a value sets.
/// Requires `#[serde(default, deserialize_with = "…")]`.
pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// Deserialize a `u32` counter that may arrive as explicit JSON `null`.
/// Some OpenAI-compatible endpoints return `null` inside `usage` instead of omitting the field;
/// plain `u32` rejects `null` and fails the whole response parse.
/// `null` (and, with `#[serde(default)]`, a missing field) becomes `0`.
/// Requires `#[serde(default, deserialize_with = "…")]`.
// FORK: added in this fork for null-tolerant `usage` parsing.
pub fn null_as_zero<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<u32>::deserialize(deserializer)?.unwrap_or_default())
}
