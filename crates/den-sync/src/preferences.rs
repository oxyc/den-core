//! Profile preference semantic operations (den-spec preferences-v1).

use serde_json::{json, Map, Value};

const PREFERRED: &str = "den.preferredSubtitleLanguage";
const EXPLICIT: &str = "den.hasExplicitSubtitlePreference";
const INVALID_PREFERENCES: &str = "invalid_subtitle_preference";
const INVALID_CHOICE: &str = "invalid_subtitle_choice";
const INTERFACE_LANGUAGE: &str = "den.interfaceLanguage";
const INVALID_INTERFACE_LANGUAGE: &str = "invalid_interface_language";

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

/// Read or encode the synced interface language without rewriting unknown future languages.
pub fn interface_language_v1(
    operation: &str,
    values: Option<&Value>,
    language: Option<&str>,
) -> Result<Value, String> {
    match operation {
        "read" if language.is_none() => {
            read_interface_language(values.ok_or(INVALID_INTERFACE_LANGUAGE)?)
        }
        "write" if values.is_none() => {
            write_interface_language(language.ok_or(INVALID_INTERFACE_LANGUAGE)?)
        }
        "read" | "write" => Err(INVALID_INTERFACE_LANGUAGE.into()),
        _ => Err("invalid_preferences_operation".into()),
    }
}

fn read_interface_language(values: &Value) -> Result<Value, String> {
    let values = values.as_object().ok_or(INVALID_INTERFACE_LANGUAGE)?;
    if values.len() > 4096 {
        return Err(INVALID_INTERFACE_LANGUAGE.into());
    }
    let Some(stored) = values.get(INTERFACE_LANGUAGE) else {
        return Ok(
            json!({"version":1,"selection":{"language":"en","explicit":false,"supported":true}}),
        );
    };
    if stored.is_null() {
        return Ok(
            json!({"version":1,"selection":{"language":"en","explicit":true,"supported":true}}),
        );
    }
    let code = exact_string(stored).ok_or(INVALID_INTERFACE_LANGUAGE)?;
    if !valid_language(code) {
        return Err(INVALID_INTERFACE_LANGUAGE.into());
    }
    if matches!(code, "en" | "es" | "sv") {
        return Ok(
            json!({"version":1,"selection":{"language":code,"explicit":true,"supported":true}}),
        );
    }
    Ok(
        json!({"version":1,"selection":{"language":"en","explicit":true,"supported":false,"storedLanguage":code}}),
    )
}

fn write_interface_language(language: &str) -> Result<Value, String> {
    if !matches!(language, "en" | "es" | "sv") {
        return Err(INVALID_INTERFACE_LANGUAGE.into());
    }
    Ok(json!({"version":1,"changes":{INTERFACE_LANGUAGE:{"string":language}}}))
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

    #[test]
    fn unknown_interface_languages_are_preserved_by_readers() {
        let answer = interface_language_v1(
            "read",
            Some(&json!({INTERFACE_LANGUAGE:{"string":"fr"}})),
            None,
        )
        .unwrap();
        assert_eq!(answer["selection"]["language"], "en");
        assert_eq!(answer["selection"]["storedLanguage"], "fr");
        assert_eq!(answer.get("changes"), None);
    }
}
