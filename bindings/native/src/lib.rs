//! Transport only. No sync policy is implemented in bindings.
uniffi::setup_scaffolding!();

#[uniffi::export]
pub fn evaluate(request: String) -> String {
    den_sync::evaluate(&request)
}

#[cfg(test)]
mod tests {
    use super::evaluate;
    use serde_json::{json, Value};

    fn mutate(mut document: Value, mutation: &Value) -> Value {
        let path = mutation["path"].as_str().unwrap();
        let (parent, key) = path.rsplit_once('/').unwrap();
        let map = document
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap();
        if let Some(rename) = mutation.get("rename").and_then(Value::as_str) {
            let value = map.remove(key).unwrap();
            map.insert(rename.into(), value);
        } else {
            map.insert(key.into(), mutation["value"].clone());
        }
        document
    }

    fn migration_input(fixture: &Value, case: &Value) -> Value {
        let mut input = case.clone();
        if case.get("observed").and_then(Value::as_str) == Some("committed")
            && !case["omitDirectory"].as_bool().unwrap_or(false)
        {
            input["directory"] = fixture["migrationBase"].clone();
            if let Some(mutation) = case.get("directoryMutation") {
                input["directory"] = mutate(input["directory"].clone(), mutation);
            }
        }
        if case.get("observed").and_then(Value::as_str) == Some("legacy")
            && case.get("state").and_then(Value::as_str) != Some("idle")
        {
            input["fence"] = fixture["migrationFence"].clone();
            if !case["omitObservedFence"].as_bool().unwrap_or(false) {
                input["observedFence"] = fixture["migrationFence"].clone();
            }
            if let Some(mutation) = case.get("fenceMutation") {
                input["fence"] = mutate(input["fence"].clone(), mutation);
            }
            if let Some(mutation) = case.get("observedFenceMutation") {
                input["observedFence"] = mutate(input["observedFence"].clone(), mutation);
            }
        }
        let state = case.get("state").and_then(Value::as_str);
        let observed = case.get("observed").and_then(Value::as_str);
        let needs_authorization = (state == Some("idle")
            && case["explicitIntent"].as_bool().unwrap_or(false))
            || state == Some("commit-unknown")
            || (observed == Some("legacy") && !matches!(state, Some("idle" | "restore-required")));
        if needs_authorization {
            let authorization = &fixture["migrationAuthorization"];
            let intent = authorization["intent"].clone();
            input["attempt"] = intent["attempt"].clone();
            input["householdId"] = intent["householdId"].clone();
            if state == Some("idle") {
                input["observedFence"] = intent["source"].clone();
            }
            if state == Some("awaiting-confirmation") {
                input
                    .as_object_mut()
                    .unwrap()
                    .entry("backupDigest")
                    .or_insert_with(|| authorization["evidence"]["backupDigest"].clone());
                input
                    .as_object_mut()
                    .unwrap()
                    .entry("stagedDigest")
                    .or_insert_with(|| authorization["evidence"]["stagedDigest"].clone());
                input
                    .as_object_mut()
                    .unwrap()
                    .entry("wireMin")
                    .or_insert(json!(5));
            }
            if case["commitAuthorized"].as_bool().unwrap_or(false) {
                let mut evidence = authorization["evidence"].clone();
                if state == Some("commit-unknown") {
                    for (key, value) in [
                        ("fence", intent["source"].clone()),
                        ("backupVerified", json!(true)),
                        ("candidateKey", evidence["candidateKey"].clone()),
                        ("candidateHead", evidence["candidateHead"].clone()),
                        ("ownerVerified", json!(true)),
                    ] {
                        input.as_object_mut().unwrap().entry(key).or_insert(value);
                    }
                }
                for key in [
                    "candidateKey",
                    "candidateHead",
                    "backupDigest",
                    "stagedDigest",
                    "wireMin",
                ] {
                    if let Some(value) = input.get(key) {
                        evidence[key] = value.clone();
                    } else if matches!(key, "backupDigest" | "stagedDigest" | "wireMin") {
                        input[key] = evidence[key].clone();
                    }
                }
                evidence["fence"] = input
                    .get("fence")
                    .cloned()
                    .unwrap_or_else(|| intent["source"].clone());
                let response: Value = serde_json::from_str(&evaluate(
                    json!({
                        "op":"profiles_migration_authorization",
                        "input":{"intent":intent, "evidence":evidence}
                    })
                    .to_string(),
                ))
                .unwrap();
                input["splitAuthorization"] = response
                    .get("ok")
                    .cloned()
                    .unwrap_or_else(|| authorization["prepared"].clone());
            } else {
                input["splitAuthorization"] = authorization["prepared"].clone();
            }
            if let Some(mutation) = case.get("splitAuthorizationMutation") {
                input["splitAuthorization"] = mutate(input["splitAuthorization"].clone(), mutation);
            }
            if case["omitSplitAuthorization"].as_bool().unwrap_or(false) {
                input.as_object_mut().unwrap().remove("splitAuthorization");
            }
            if let Some(attempt) = case.get("plannerAttempt") {
                input["attempt"] = attempt.clone();
            }
        }
        if case["omitState"].as_bool().unwrap_or(false) {
            input.as_object_mut().unwrap().remove("state");
        }
        if case["omitObserved"].as_bool().unwrap_or(false) {
            input.as_object_mut().unwrap().remove("observed");
        }
        input
    }

    #[test]
    fn shared_profile_contract_crosses_the_native_binding() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../crates/den-sync/tests/fixtures/profiles-v1.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let response: Value =
                serde_json::from_str(&evaluate(case["request"].to_string())).unwrap();
            let expected = if let Some(error) = case.get("error") {
                json!({"version": 1, "error": error})
            } else {
                json!({"version": 1, "ok": case["ok"]})
            };
            assert_eq!(response, expected, "{}", case["name"]);
        }
        for guard in fixture["partitionIdentityGuards"].as_array().unwrap() {
            for status in ["newer", "future"] {
                let opened = if status == "newer" {
                    json!({"status":"newer", "reason":"framing", "name":guard["identity"]})
                } else {
                    json!({"status":"row", "name":guard["identity"], "legacy":false,
                           "row":{"kind":"future", "schema":1}})
                };
                let request = json!({
                    "op":"profiles_migration_partition_rows",
                    "input":{"base":1, "ownerPolicyAt":[1, 0, "a1b2c3d4e5f60718"], "rows":[{
                        "k":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "seq":1, "v":"sealed", "opened":opened
                    }]}
                });
                let response: Value = serde_json::from_str(&evaluate(request.to_string())).unwrap();
                assert_eq!(
                    response,
                    json!({"version":1, "error":guard["error"]}),
                    "{} {status}",
                    guard["name"]
                );
            }
        }
        let authorization = &fixture["migrationAuthorization"];
        for (evidence, expected) in [
            (Value::Null, &authorization["prepared"]),
            (authorization["evidence"].clone(), &authorization["bound"]),
        ] {
            let request = json!({
                "op":"profiles_migration_authorization",
                "input":{"intent":authorization["intent"], "evidence":evidence}
            });
            let response: Value = serde_json::from_str(&evaluate(request.to_string())).unwrap();
            assert_eq!(response, json!({"version":1, "ok":expected}));
        }
        for case in fixture["migrationCases"].as_array().unwrap() {
            let request = json!({
                "op":"profiles_migration_next",
                "input":migration_input(&fixture, case),
            });
            let response: Value = serde_json::from_str(&evaluate(request.to_string())).unwrap();
            let expected = if let Some(error) = case.get("error") {
                json!({"version":1, "error":error})
            } else {
                json!({"version":1, "ok":case["expect"]})
            };
            assert_eq!(response, expected, "{}", case["name"]);
        }
        let capability = &fixture["profileCapabilityContract"];
        for case in capability["cases"]
            .as_array()
            .unwrap()
            .iter()
            .chain(capability["mixedHost"].as_array().unwrap())
        {
            let request = json!({"op":"profiles_capability", "input":case["input"]});
            let response: Value = serde_json::from_str(&evaluate(request.to_string())).unwrap();
            let expected = if let Some(error) = case.get("error") {
                json!({"version":1, "error":error})
            } else {
                json!({"version":1, "ok":case["expect"]})
            };
            assert_eq!(
                response,
                expected,
                "{}",
                case.get("name").unwrap_or(&case["household"])
            );
        }

        // The generic native transport must expose the two Kid-creation operations
        // without learning any profile policy of its own.
        let materialize: Value = serde_json::from_str(&evaluate(
            json!({"op":"profiles_materialize", "document":fixture["migrationBase"]}).to_string(),
        ))
        .unwrap();
        assert_eq!(materialize["ok"]["directory"], fixture["migrationBase"]);
        assert!(materialize["ok"]["jcs"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert!(materialize["ok"]["plaintext"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));

        let journal = json!({
            "version":1,
            "profile":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "key":"VVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVU=",
            "display":{"value":{"name":"Sam","avatar":"fox"},
                       "at":[1800000005000_u64,0,"a1b2c3d4e5f60718"]},
            "libraryAt":[1800000005000_u64,1,"a1b2c3d4e5f60718"],
            "maximumAge":8,
            "policyAt":[1800000005000_u64,2,"a1b2c3d4e5f60718"],
            "deletedAt":[1800000005000_u64,3,"a1b2c3d4e5f60718"]
        });
        for (input, effect) in [
            (json!({"state":"idle", "explicitIntent":false}), "no-op"),
            (
                json!({"state":"idle", "explicitIntent":true, "journal":journal}),
                "persist-journal",
            ),
        ] {
            let response: Value = serde_json::from_str(&evaluate(
                json!({"op":"profiles_create_next", "input":input}).to_string(),
            ))
            .unwrap();
            assert_eq!(response["ok"]["effect"], effect);
        }

        let depth = 1_500;
        let mut wire = String::from(
            r#"{"op":"profiles_materialize","document":{"migration":{},"futureNested":"#,
        );
        wire.push_str(&r#"{"next":"#.repeat(depth));
        wire.push_str("null");
        wire.push_str(&"}".repeat(depth));
        wire.push_str("}}");
        let response: Value = serde_json::from_str(&evaluate(wire)).unwrap();
        assert_eq!(response, json!({"version":1, "error":"invalid_request"}));
    }
}
