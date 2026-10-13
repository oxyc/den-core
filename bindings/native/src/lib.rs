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
    }
}
