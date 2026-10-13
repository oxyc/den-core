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
    }
}
