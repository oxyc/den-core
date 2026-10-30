//! Cross-client semantic headings (den-spec headings-v1).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const INVALID: &str = "invalid_heading";
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// One named formatter argument. Restricting the wire to text and safe integers keeps Swift, JavaScript and Rust
/// renderers exact without duplicating boolean branching or floating-point formatting rules.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum HeadingArgument {
    Text(String),
    Integer(i64),
}

/// A locale-independent heading. `fallback` is completed English copy and remains usable by older clients and
/// clients that do not yet know `id`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Heading {
    pub id: String,
    pub args: BTreeMap<String, HeadingArgument>,
    pub fallback: String,
}

/// Validate a headings-v1 object, or turn a legacy string into a fallback-only value without guessing its meaning.
pub fn normalize_v1(value: &Value) -> Result<Value, String> {
    let heading = if let Some(fallback) = value.as_str() {
        validate_text(fallback, 1, 1024)?;
        Heading {
            id: "den.legacy".into(),
            args: BTreeMap::new(),
            fallback: fallback.into(),
        }
    } else {
        let heading: Heading = serde_json::from_value(value.clone()).map_err(|_| INVALID)?;
        validate(&heading)?;
        heading
    };
    Ok(json!({"version":1,"heading":heading}))
}

fn validate(heading: &Heading) -> Result<(), String> {
    validate_identifier(&heading.id, 128)?;
    validate_text(&heading.fallback, 1, 1024)?;
    if heading.args.len() > 16 {
        return Err(INVALID.into());
    }
    for (name, value) in &heading.args {
        validate_identifier(name, 64)?;
        match value {
            HeadingArgument::Text(value) => validate_text(value, 0, 512)?,
            HeadingArgument::Integer(value)
                if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value) => {}
            HeadingArgument::Integer(_) => return Err(INVALID.into()),
        }
    }
    Ok(())
}

fn validate_text(value: &str, minimum: usize, maximum: usize) -> Result<(), String> {
    let length = value.chars().count();
    if length < minimum || length > maximum {
        return Err(INVALID.into());
    }
    Ok(())
}

fn validate_identifier(value: &str, maximum: usize) -> Result<(), String> {
    validate_text(value, 1, maximum)?;
    let mut parts = value.split(['.', '_', '-']);
    let valid_part = |part: &str| {
        let mut bytes = part.bytes();
        matches!(bytes.next(), Some(first) if first.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    if !parts.all(valid_part) {
        return Err(INVALID.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_english_heading_is_not_reclassified() {
        assert_eq!(
            normalize_v1(&json!("Because you watched Dune")).unwrap(),
            json!({"version":1,"heading":{
                "id":"den.legacy","args":{},"fallback":"Because you watched Dune"
            }})
        );
    }

    #[test]
    fn external_copy_that_matches_product_copy_stays_explicit() {
        let input =
            json!({"id":"den.verbatim","args":{"value":"Watchlist"},"fallback":"Watchlist"});
        assert_eq!(normalize_v1(&input).unwrap()["heading"], input);
    }
}
