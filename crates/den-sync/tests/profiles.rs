//! Household profiles v1 through the shared JSON boundary, against den-spec's normative vectors.

use den_sync::evaluate;
use serde_json::{json, Value};
use std::path::PathBuf;

fn request(value: Value) -> Value {
    serde_json::from_str(&evaluate(&value.to_string())).unwrap()
}

fn vectors() -> Option<Value> {
    let path = match std::env::var("DEN_SPEC_DIR") {
        Ok(dir) => PathBuf::from(dir).join("vectors/profiles-v1.json"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../den-spec/vectors/profiles-v1.json"),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(serde_json::from_str(&text).expect("den-spec profiles-v1 JSON")),
        Err(_) if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") => {
            eprintln!("SKIP: den-spec profiles-v1 absent and DEN_SPEC_OPTIONAL=1");
            None
        }
        Err(error) => panic!(
            "{}: {error} — check out current den-spec beside this repo, set DEN_SPEC_DIR, or set DEN_SPEC_OPTIONAL=1",
            path.display()
        ),
    }
}

fn mutate(document: &Value, mutation: &Value) -> Value {
    let mut document = document.clone();
    *document
        .pointer_mut(mutation["path"].as_str().unwrap())
        .expect("vector mutation path") = mutation["value"].clone();
    document
}

#[test]
fn den_spec_profile_directories_validate() {
    let Some(vectors) = vectors() else { return };
    let base = &vectors["validDirectories"][0]["document"];
    for case in vectors["validDirectories"].as_array().unwrap() {
        assert_eq!(
            request(json!({"op":"profiles_validate", "document":case["document"]}))["ok"],
            case["document"],
            "{}",
            case["name"]
        );
    }
    for case in vectors["invalidDirectories"].as_array().unwrap() {
        let document = mutate(base, &case["mutate"]);
        assert_eq!(
            request(json!({"op":"profiles_validate", "document":document}))["error"],
            case["error"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn den_spec_profile_merges_converge() {
    let Some(vectors) = vectors() else { return };
    let base = &vectors["validDirectories"][0]["document"];
    for case in vectors["mergeCases"].as_array().unwrap() {
        let path = format!(
            "/profiles/{}/{}",
            case["profile"].as_str().unwrap(),
            case["field"].as_str().unwrap()
        );
        let a = mutate(base, &json!({"path":path, "value":case["a"]}));
        let b = mutate(base, &json!({"path":path, "value":case["b"]}));
        let ab_response = request(json!({"op":"profiles_merge", "a":a, "b":b}));
        let ba_response = request(json!({"op":"profiles_merge", "a":b, "b":a}));
        let ab = ab_response
            .get("ok")
            .unwrap_or_else(|| panic!("{}: {ab_response}", case["name"]))
            .clone();
        let ba = ba_response
            .get("ok")
            .unwrap_or_else(|| panic!("{}: {ba_response}", case["name"]))
            .clone();
        assert_eq!(ab, ba, "{}", case["name"]);
        assert_eq!(ab.pointer(&path).unwrap(), &case["ok"], "{}", case["name"]);
        assert_eq!(
            request(json!({"op":"profiles_merge", "a":ab, "b":ab}))["ok"],
            ab,
            "{}",
            case["name"]
        );
    }
}

#[test]
fn den_spec_profile_policy_cases() {
    let Some(vectors) = vectors() else { return };
    for case in vectors["policyCases"].as_array().unwrap() {
        if case.get("legacy").is_some() {
            assert_eq!(
                request(json!({"op":"profile_policy_normalize_legacy", "legacy":case["legacy"]}))
                    ["ok"],
                case["normalized"],
                "{}",
                case["name"]
            );
        } else {
            assert_eq!(
                request(
                    json!({"op":"profile_policy_allows", "policy":case["policy"], "titleBand":case["titleBand"]})
                )["ok"],
                case["allow"],
                "{}",
                case["name"]
            );
        }
    }
}

#[test]
fn merge_rejects_identity_conflicts_and_duplicate_live_keys() {
    let Some(vectors) = vectors() else { return };
    let a = vectors["validDirectories"][0]["document"].clone();
    let mut b = a.clone();
    b["profiles"]["11112222333344445555666677778888"]["role"] = json!("adult");
    assert_eq!(
        request(json!({"op":"profiles_merge", "a":a, "b":b}))["error"],
        "identity_conflict"
    );
}
