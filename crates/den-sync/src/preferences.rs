//! Profile preference semantic operations (den-spec preferences-v1).

use serde_json::{json, Map, Value};

const PREFERRED: &str = "den.preferredSubtitleLanguage";
const EXPLICIT: &str = "den.hasExplicitSubtitlePreference";
const INVALID_PREFERENCES: &str = "invalid_subtitle_preference";
const INVALID_CHOICE: &str = "invalid_subtitle_choice";

/// Read or encode the atomic three-state subtitle preference.
pub fn subtitle_v1(
    operation: &str,
    values: Option<&Value>,
    choice: Option<&Value>,
) -> Result<Value, String> {
    match operation {
        "read" if choice.is_none() => read(values.ok_or(INVALID_PREFERENCES)?),
        "write" if values.is_none() => write(choice.ok_or(INVALID_CHOICE)?),
        "read" => Err(INVALID_PREFERENCES.into()),
        "write" => Err(INVALID_CHOICE.into()),
        _ => Err("invalid_preferences_operation".into()),
    }
}

fn read(values: &Value) -> Result<Value, String> {
    let values = values.as_object().ok_or(INVALID_PREFERENCES)?;
    if values.len() > 4096 {
        return Err(INVALID_PREFERENCES.into());
    }
    let has_language = values.contains_key(PREFERRED);
    let has_explicit = values.contains_key(EXPLICIT);
    let language = values.get(PREFERRED).filter(|value| !value.is_null());
    let explicit = values.get(EXPLICIT).filter(|value| !value.is_null());

    if let Some(language) = language {
        let code = exact_string(language).ok_or(INVALID_PREFERENCES)?;
        if !valid_language(code) {
            return Err(INVALID_PREFERENCES.into());
        }
        if has_explicit && explicit != Some(&json!({"bool": true})) {
            return Err(INVALID_PREFERENCES.into());
        }
        return Ok(json!({"version":1,"choice":{"mode":"language","language":code}}));
    }
    if has_explicit {
        return match explicit {
            None => Ok(json!({"version":1,"choice":{"mode":"automatic"}})),
            Some(value) if value == &json!({"bool": true}) => {
                Ok(json!({"version":1,"choice":{"mode":"off"}}))
            }
            _ => Err(INVALID_PREFERENCES.into()),
        };
    }
    let mode = if has_language { "off" } else { "automatic" };
    Ok(json!({"version":1,"choice":{"mode":mode}}))
}

fn write(choice: &Value) -> Result<Value, String> {
    let choice = choice.as_object().ok_or(INVALID_CHOICE)?;
    let mode = choice
        .get("mode")
        .and_then(Value::as_str)
        .ok_or(INVALID_CHOICE)?;
    let mut changes = Map::new();
    match mode {
        "automatic" if choice.len() == 1 => {
            changes.insert(PREFERRED.into(), Value::Null);
            changes.insert(EXPLICIT.into(), Value::Null);
        }
        "off" if choice.len() == 1 => {
            changes.insert(PREFERRED.into(), Value::Null);
            changes.insert(EXPLICIT.into(), json!({"bool":true}));
        }
        "language" if choice.len() == 2 => {
            let code = choice
                .get("language")
                .and_then(Value::as_str)
                .ok_or(INVALID_CHOICE)?;
            if !valid_language(code) {
                return Err(INVALID_CHOICE.into());
            }
            changes.insert(PREFERRED.into(), json!({"string":code}));
            changes.insert(EXPLICIT.into(), json!({"bool":true}));
        }
        _ => return Err(INVALID_CHOICE.into()),
    }
    Ok(json!({"version":1,"changes":changes}))
}

fn exact_string(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    (object.len() == 1)
        .then(|| object.get("string")?.as_str())
        .flatten()
}

fn valid_language(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_preferences_do_not_enter_the_patch() {
        let answer = subtitle_v1("write", None, Some(&json!({"mode":"off"}))).unwrap();
        assert_eq!(answer["changes"].as_object().unwrap().len(), 2);
        assert!(answer["changes"].get("future.preference").is_none());
    }
}
