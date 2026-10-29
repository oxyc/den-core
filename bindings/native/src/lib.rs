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
    fn season_projection_crosses_the_native_binding() {
        let season = json!({
            "format": 4,
            "kind": "season",
            "title": {"type": "tv", "id": 1399},
            "season": 1,
            "seasonReset": null,
            "episodes": {
                "1": {"progress": {"value": 1, "at": [1000, 0, "a1b2c3d4e5f60718"], "viewing": 0},
                    "imported": false, "plays": {"0": 1000}, "cleared": null},
                "2": {}
            }
        });
        let batch: Value = serde_json::from_str(&evaluate(
            json!({"op": "season_state_v4", "season": season, "now": 2000}).to_string(),
        ))
        .unwrap();
        for episode in [1, 2] {
            let single: Value = serde_json::from_str(&evaluate(
                json!({"op": "episode_state_v4", "season": season, "episode": episode, "now": 2000})
                    .to_string(),
            ))
            .unwrap();
            assert_eq!(batch["ok"][episode.to_string()], single["ok"]);
        }
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
