use den_sync::{preferences_interface_language_v1, preferences_subtitle_v1};
use serde_json::{json, Value};

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/preferences-v1.json")).unwrap()
}

#[test]
fn den_spec_preferences_v1_vectors() {
    let vectors = vectors();
    for case in vectors["readCases"].as_array().unwrap() {
        let result = preferences_subtitle_v1("read", Some(&case["values"]), None).unwrap();
        assert_eq!(
            result,
            json!({"version":1,"choice":case["choice"]}),
            "{}",
            case["name"]
        );
    }
    for case in vectors["writeCases"].as_array().unwrap() {
        let result = preferences_subtitle_v1("write", None, Some(&case["choice"])).unwrap();
        assert_eq!(
            result,
            json!({"version":1,"changes":case["changes"]}),
            "{}",
            case["name"]
        );
    }
    for case in vectors["invalidCases"].as_array().unwrap() {
        let result = match case["operation"].as_str().unwrap() {
            "read" => preferences_subtitle_v1("read", Some(&case["values"]), None),
            "write" => preferences_subtitle_v1("write", None, Some(&case["choice"])),
            _ => unreachable!(),
        };
        assert!(result.is_err(), "{}", case["name"]);
    }
    for case in vectors["interfaceLanguageReadCases"].as_array().unwrap() {
        let result =
            preferences_interface_language_v1("read", Some(&case["values"]), None).unwrap();
        assert_eq!(
            result,
            json!({"version":1,"selection":case["selection"]}),
            "{}",
            case["name"]
        );
    }
    for case in vectors["interfaceLanguageWriteCases"].as_array().unwrap() {
        let result =
            preferences_interface_language_v1("write", None, case["language"].as_str()).unwrap();
        assert_eq!(
            result,
            json!({"version":1,"changes":case["changes"]}),
            "{}",
            case["name"]
        );
    }
    for case in vectors["interfaceLanguageInvalidCases"].as_array().unwrap() {
        let result = match case["operation"].as_str().unwrap() {
            "read" => preferences_interface_language_v1("read", Some(&case["values"]), None),
            "write" => preferences_interface_language_v1("write", None, case["language"].as_str()),
            _ => unreachable!(),
        };
        assert!(result.is_err(), "{}", case["name"]);
    }
}

#[test]
fn compact_and_typed_boundaries_match() {
    let requests = [
        json!({"op":"preferences_subtitle_v1","operation":"read","values":{}}),
        json!({"op":"preferences_subtitle_v1","operation":"read","values":{"den.preferredSubtitleLanguage":null}}),
        json!({"op":"preferences_subtitle_v1","operation":"write","choice":{"mode":"off"}}),
        json!({"op":"preferences_subtitle_v1","operation":"write","choice":{"mode":"language","language":"sv"}}),
        json!({"op":"preferences_interface_language_v1","operation":"read","values":{}}),
        json!({"op":"preferences_interface_language_v1","operation":"read","values":{"den.interfaceLanguage":{"string":"fr"}}}),
        json!({"op":"preferences_interface_language_v1","operation":"write","language":"es"}),
    ];
    for request in requests {
        let answer: Value =
            serde_json::from_str(&den_sync::evaluate(&request.to_string())).unwrap();
        assert!(answer.get("ok").is_some(), "{answer}");
    }
}
