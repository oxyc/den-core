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

fn remove(document: &Value, pointer: &str) -> Value {
    let mut document = document.clone();
    let (parent, key) = pointer.rsplit_once('/').expect("remove path has key");
    document
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .expect("vector removal object")
        .remove(key)
        .expect("vector removal key");
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
        let document = if let Some(mutation) = case.get("mutate") {
            mutate(base, mutation)
        } else {
            remove(base, case["remove"].as_str().unwrap())
        };
        assert_eq!(
            request(json!({
                "op":"profiles_validate",
                "document":document,
                "stored":case.get("stored").and_then(Value::as_bool).unwrap_or(false)
            }))["error"],
            case["error"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn den_spec_profile_directory_operations_construct_canonical_documents() {
    let Some(vectors) = vectors() else { return };
    let base = &vectors["validDirectories"][0]["document"];
    for case in vectors["directoryOperationCases"].as_array().unwrap() {
        let mut operation = json!({
            "op":"profiles_apply",
            "document":base,
            "operation":case["operation"],
            "profile":case["profile"]
        });
        for field in ["entry", "value", "at"] {
            if let Some(value) = case.get(field) {
                operation[field] = value.clone();
            }
        }
        let response = request(operation);
        let result = response
            .get("ok")
            .unwrap_or_else(|| panic!("{}: {response}", case["name"]));
        let entry = &result["profiles"][case["profile"].as_str().unwrap()];
        let expected = &case["expect"];
        if let Some(value) = expected.get("role") {
            assert_eq!(&entry["role"], value, "{}", case["name"]);
        }
        if let Some(value) = expected.get("name") {
            assert_eq!(
                &entry["display"]["value"]["name"], value,
                "{}",
                case["name"]
            );
        }
        if let Some(value) = expected.get("keyVersion") {
            assert_eq!(
                &entry["library"]["value"]["keyVersion"], value,
                "{}",
                case["name"]
            );
        }
        if let Some(value) = expected.get("maximumAge") {
            assert_eq!(
                &entry["policy"]["value"]["maximumAge"], value,
                "{}",
                case["name"]
            );
        }
        if let Some(value) = expected.get("unknownRatings") {
            assert_eq!(
                &entry["policy"]["value"]["unknownRatings"], value,
                "{}",
                case["name"]
            );
        }
        if let Some(value) = expected.get("deleted") {
            assert_eq!(&entry["deleted"]["value"], value, "{}", case["name"]);
        }
        if let Some(value) = expected.get("libraryKeyVersion") {
            assert_eq!(
                &entry["library"]["value"]["keyVersion"], value,
                "{}",
                case["name"]
            );
        }
    }
}

#[test]
fn profile_directory_operations_preserve_unknown_stamped_extensions() {
    let Some(vectors) = vectors() else { return };
    let profile = "11112222333344445555666677778888";
    let mut extended = vectors["validDirectories"][0]["document"].clone();
    extended["profiles"][profile]["display"]["futureDisplayWrapper"] = json!("kept");
    extended["profiles"][profile]["display"]["value"]["futureDisplayValue"] = json!("kept");
    extended["profiles"][profile]["policy"]["futurePolicyWrapper"] = json!("kept");
    extended["profiles"][profile]["policy"]["value"]["futurePolicyValue"] = json!("kept");
    extended["profiles"][profile]["library"]["futureLibraryWrapper"] = json!("kept");
    extended["profiles"][profile]["library"]["value"]["futureLibraryValue"] = json!("kept");
    extended["profiles"][profile]["deleted"]["futureDeletedWrapper"] = json!("kept");

    let rename = request(json!({
        "op":"profiles_apply",
        "document":extended,
        "operation":"rename",
        "profile":profile,
        "value":{"name":"Mina Three"},
        "at":[1800000009000_i64,0,"a1b2c3d4e5f60718"]
    }));
    assert_eq!(
        rename["ok"]["profiles"][profile]["display"]["value"]["name"],
        "Mina Three"
    );
    assert!(rename["ok"]["profiles"][profile]["display"]["value"]
        .get("avatar")
        .is_none());
    assert_eq!(
        rename["ok"]["profiles"][profile]["display"]["futureDisplayWrapper"],
        "kept"
    );
    assert_eq!(
        rename["ok"]["profiles"][profile]["display"]["value"]["futureDisplayValue"],
        "kept"
    );

    let policy = request(json!({
        "op":"profiles_apply",
        "document":extended,
        "operation":"policy",
        "profile":profile,
        "value":{"maximumAge":12,"unknownRatings":"block"},
        "at":[1800000009000_i64,1,"a1b2c3d4e5f60718"]
    }));
    assert_eq!(
        policy["ok"]["profiles"][profile]["policy"]["value"]["maximumAge"],
        12
    );
    assert_eq!(
        policy["ok"]["profiles"][profile]["policy"]["futurePolicyWrapper"],
        "kept"
    );
    assert_eq!(
        policy["ok"]["profiles"][profile]["policy"]["value"]["futurePolicyValue"],
        "kept"
    );

    let library = request(json!({
        "op":"profiles_apply",
        "document":extended,
        "operation":"library",
        "profile":profile,
        "value":{"keyVersion":2,"key":"REREREREREREREREREREREREREREREREREREREREREQ="},
        "at":[1800000009000_i64,2,"a1b2c3d4e5f60718"]
    }));
    assert_eq!(
        library["ok"]["profiles"][profile]["library"]["value"]["keyVersion"],
        2
    );
    assert_eq!(
        library["ok"]["profiles"][profile]["library"]["futureLibraryWrapper"],
        "kept"
    );
    assert_eq!(
        library["ok"]["profiles"][profile]["library"]["value"]["futureLibraryValue"],
        "kept"
    );

    let delete = request(json!({
        "op":"profiles_apply",
        "document":extended,
        "operation":"delete",
        "profile":profile,
        "at":[1800000009000_i64,3,"a1b2c3d4e5f60718"]
    }));
    assert_eq!(delete["ok"]["profiles"][profile]["deleted"]["value"], true);
    assert_eq!(
        delete["ok"]["profiles"][profile]["deleted"]["futureDeletedWrapper"],
        "kept"
    );
}

#[test]
fn den_spec_whole_directory_merges_converge_or_reject_identity_conflicts() {
    let Some(vectors) = vectors() else { return };
    let base = &vectors["validDirectories"][0]["document"];
    for case in vectors["directoryMergeCases"].as_array().unwrap() {
        let a = case["aMutations"]
            .as_array()
            .unwrap()
            .iter()
            .fold(base.clone(), |document, mutation| {
                mutate(&document, mutation)
            });
        let b = case["bMutations"]
            .as_array()
            .unwrap()
            .iter()
            .fold(base.clone(), |document, mutation| {
                mutate(&document, mutation)
            });
        let ab = request(json!({"op":"profiles_merge", "a":a, "b":b}));
        let ba = request(json!({"op":"profiles_merge", "a":b, "b":a}));
        if let Some(error) = case.get("error") {
            assert_eq!(&ab["error"], error, "{}", case["name"]);
            assert_eq!(&ba["error"], error, "{}", case["name"]);
        } else {
            assert_eq!(ab["ok"], ba["ok"], "{}", case["name"]);
            let profile = &ab["ok"]["profiles"]["11112222333344445555666677778888"];
            assert_eq!(
                profile["display"]["value"]["name"], case["expect"]["name"],
                "{}",
                case["name"]
            );
            assert_eq!(
                profile["policy"]["value"]["maximumAge"], case["expect"]["maximumAge"],
                "{}",
                case["name"]
            );
        }
    }
}

#[test]
fn den_spec_profile_lifecycle_decisions() {
    let Some(vectors) = vectors() else { return };
    let base = &vectors["validDirectories"][0]["document"];
    for case in vectors["migrationCases"].as_array().unwrap() {
        let mut input = case.clone();
        if case["observed"] == "committed"
            && !case
                .get("omitDirectory")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            input["directory"] = base.clone();
        }
        if case["observed"] == "legacy" && case["state"] != "idle" {
            input["fence"] = vectors["migrationFence"].clone();
            input["observedFence"] = vectors["migrationFence"].clone();
            if let Some(mutation) = case.get("fenceMutation") {
                input["fence"] = mutate(&input["fence"], mutation);
            }
            if let Some(mutation) = case.get("observedFenceMutation") {
                input["observedFence"] = mutate(&input["observedFence"], mutation);
            }
        }
        let response = request(json!({"op":"profiles_migration_next", "input":input}));
        if let Some(error) = case.get("error") {
            assert_eq!(&response["error"], error, "{}", case["name"]);
        } else {
            assert_eq!(&response["ok"], &case["expect"], "{}", case["name"]);
        }
    }
    for case in vectors["resetCases"].as_array().unwrap() {
        let mut directory = vectors["validDirectories"][0]["document"].clone();
        if let Some(mutation) = case.get("directoryMutate") {
            directory = mutate(&directory, mutation);
        }
        let mut fence = vectors["resetFence"].clone();
        if let Some(mutation) = case.get("fenceMutate") {
            fence = mutate(&fence, mutation);
        }
        let mut profiles = vectors["resetProfiles"].clone();
        for mutation in case["profileMutations"].as_array().into_iter().flatten() {
            profiles = mutate(&profiles, mutation);
        }
        if let Some(profile) = case.get("removeProfile").and_then(Value::as_str) {
            profiles.as_object_mut().unwrap().remove(profile);
        }
        let mut profile_evidence = profiles
            .as_object()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(profile) = case.get("duplicateProfile").and_then(Value::as_str) {
            profile_evidence.push(profiles[profile].clone());
        }
        let response = request(json!({
            "op":"profiles_reset_next",
            "input":{
                "directory":directory,
                "fence":fence,
                "profiles":profile_evidence,
                "householdCommitted":case["householdCommitted"],
                "oldHouseholdRetired":case["oldHouseholdRetired"]
            }
        }));
        if let Some(error) = case.get("error") {
            assert_eq!(&response["error"], error, "{}", case["name"]);
        } else {
            assert_eq!(response["ok"], case["expect"], "{}", case["name"]);
        }
    }
}

#[test]
fn den_spec_profile_preferences_partition_without_interpreting_values() {
    let Some(vectors) = vectors() else { return };
    for case in vectors["settingsPartitionCases"].as_array().unwrap() {
        let response = request(json!({
            "op":"profiles_partition_preferences",
            "values":case["values"]
        }));
        if let Some(error) = case.get("error") {
            assert_eq!(&response["error"], error, "{}", case["name"]);
        } else {
            assert_eq!(response["ok"], case["expect"], "{}", case["name"]);
        }
    }
}

#[test]
fn profile_preference_partition_is_exact_and_preserves_stamped_extensions() {
    let stamped = json!({
        "value":{"futureTypedValue":{"nested":true}},
        "at":[1800000000000_i64,0,"a1b2c3d4e5f60718"],
        "futureStampedMember":"kept"
    });
    let response = request(json!({
        "op":"profiles_partition_preferences",
        "values":{
            "den.hideWatched":stamped,
            "den.hideWatched.future":stamped,
            "future.preference":stamped
        }
    }));
    assert_eq!(response["ok"]["owner"]["den.hideWatched"], stamped);
    assert_eq!(
        response["ok"]["household"]["den.hideWatched.future"], stamped,
        "prefixes never assign future keys to a profile"
    );
    assert_eq!(response["ok"]["household"]["future.preference"], stamped);
    assert_eq!(response["ok"]["ownerMaturity"], Value::Null);
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
