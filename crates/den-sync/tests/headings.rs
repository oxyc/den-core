use den_sync::heading_v1;
use serde_json::{json, Value};

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/headings-v1.json")).unwrap()
}

#[test]
fn den_spec_headings_v1_vectors() {
    let vectors = vectors();
    for case in vectors["validCases"].as_array().unwrap() {
        let answer = heading_v1(&case["value"]).unwrap();
        assert_eq!(
            answer,
            json!({"version":1,"heading":case["normalized"]}),
            "{}",
            case["name"]
        );
    }
    for case in vectors["invalidCases"].as_array().unwrap() {
        assert!(heading_v1(&case["value"]).is_err(), "{}", case["name"]);
    }
}

#[test]
fn compact_and_typed_boundaries_match() {
    for case in vectors()["validCases"].as_array().unwrap() {
        let request = json!({"op":"heading_v1","heading":case["value"]});
        let compact: Value =
            serde_json::from_str(&den_sync::evaluate(&request.to_string())).unwrap();
        assert_eq!(
            compact,
            json!({"version":1,"ok":{"version":1,"heading":case["normalized"]}})
        );
    }
}
