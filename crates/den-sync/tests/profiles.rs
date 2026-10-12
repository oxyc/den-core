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
    let path = mutation["path"].as_str().unwrap();
    if let Some(rename) = mutation.get("rename").and_then(Value::as_str) {
        let (parent, key) = path.rsplit_once('/').expect("mutation path has key");
        let map = document
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .expect("vector mutation object");
        let value = map.remove(key).expect("vector mutation key");
        map.insert(rename.into(), value);
    } else {
        *document.pointer_mut(path).expect("vector mutation path") = mutation["value"].clone();
    }
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
            let response = request(json!({
                "op":"profile_policy_allows",
                "policy":case["policy"],
                "minimumAge":case["minimumAge"],
                "explicitApproval":case["approved"]
            }));
            if let Some(error) = case.get("error") {
                assert_eq!(response["error"], *error, "{}", case["name"]);
            } else {
                assert_eq!(response["ok"], case["allow"], "{}", case["name"]);
            }
        }
    }
}

#[test]
fn den_spec_profile_policy_partial_order() {
    let Some(vectors) = vectors() else { return };
    for case in vectors["policyOrderingCases"].as_array().unwrap() {
        assert_eq!(
            request(json!({"op":"profile_policy_relation", "a":case["a"], "b":case["b"]}))["ok"],
            case["relation"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn den_spec_kid_title_approval_documents_validate() {
    let Some(vectors) = vectors() else { return };
    let directory = &vectors["validDirectories"][0]["document"];
    let base = &vectors["validApprovalDocuments"][0]["document"];
    for case in vectors["validApprovalDocuments"].as_array().unwrap() {
        assert_eq!(
            request(json!({
                "op":"kid_title_approvals_validate",
                "logicalName":case["logicalName"],
                "document":case["document"],
                "directory":directory
            }))["ok"],
            case["document"],
            "{}",
            case["name"]
        );
    }
    for case in vectors["invalidApprovalDocuments"].as_array().unwrap() {
        let document = case
            .get("mutate")
            .map_or_else(|| base.clone(), |mutation| mutate(base, mutation));
        let directory = case
            .get("directoryMutate")
            .map_or_else(|| directory.clone(), |mutation| mutate(directory, mutation));
        let logical_name = case
            .get("logicalName")
            .unwrap_or(&vectors["validApprovalDocuments"][0]["logicalName"]);
        assert_eq!(
            request(json!({
                "op":"kid_title_approvals_validate",
                "logicalName":logical_name,
                "document":document,
                "directory":directory
            }))["error"],
            case["error"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn den_spec_kid_title_approval_merges_converge() {
    let Some(vectors) = vectors() else { return };
    let directory = &vectors["validDirectories"][0]["document"];
    let base = &vectors["validApprovalDocuments"][0];
    for case in vectors["approvalMergeCases"].as_array().unwrap() {
        let mut a = base["document"].clone();
        let mut b = a.clone();
        a["approvals"]["movie:550"] = case["a"].clone();
        b["approvals"]["movie:550"] = case["b"].clone();
        let request_merge = |a: &Value, b: &Value| {
            request(json!({
                "op":"kid_title_approvals_merge",
                "logicalName":base["logicalName"],
                "a":a,
                "b":b,
                "directory":directory
            }))
        };
        let ab = request_merge(&a, &b);
        let ba = request_merge(&b, &a);
        assert_eq!(ab["ok"], ba["ok"], "{}", case["name"]);
        assert_eq!(
            ab["ok"]["approvals"]["movie:550"], case["ok"],
            "{}",
            case["name"]
        );
        assert_eq!(
            request_merge(&ab["ok"], &ab["ok"])["ok"],
            ab["ok"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn den_spec_approval_document_unknown_sets_follow_the_newest_stamp() {
    let Some(vectors) = vectors() else { return };
    let directory = &vectors["validDirectories"][0]["document"];
    let base = &vectors["validApprovalDocuments"][0];
    for case in vectors["approvalUnknownSetCases"].as_array().unwrap() {
        let document = |stamp: &Value, unknown: &Value| {
            let mut document = json!({
                "format": 4,
                "kind": "kid-title-approvals",
                "schema": 1,
                "kid": "11112222333344445555666677778888",
                "approvals": {"movie:550": {"value":"allow", "at":stamp}}
            });
            document
                .as_object_mut()
                .unwrap()
                .extend(unknown.as_object().unwrap().clone());
            document
        };
        let a = document(&case["aNewestStamp"], &case["aUnknown"]);
        let b = document(&case["bNewestStamp"], &case["bUnknown"]);
        let merged = request(json!({
            "op":"kid_title_approvals_merge",
            "logicalName":base["logicalName"],
            "a":a,
            "b":b,
            "directory":directory
        }));
        for (key, value) in case["ok"].as_object().unwrap() {
            assert_eq!(&merged["ok"][key], value, "{}", case["name"]);
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
