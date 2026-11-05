use den_sync::heading_v1;
use serde_json::{json, Value};
use std::path::PathBuf;

/// The authoritative headings contract from den-spec. CI sets `DEN_SPEC_DIR` to its exact checkout; local
/// development follows the same sibling-checkout convention as the other den-sync contract suites.
fn vectors() -> Option<Value> {
    let path = match std::env::var("DEN_SPEC_DIR") {
        Ok(dir) => PathBuf::from(dir).join("vectors/headings-v1.json"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../den-spec/vectors/headings-v1.json"),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(serde_json::from_str(&text).expect("den-spec headings-v1 JSON")),
        Err(_) if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") => {
            eprintln!("SKIP: den-spec headings-v1 absent and DEN_SPEC_OPTIONAL=1");
            None
        }
        Err(error) => panic!(
            "{}: {error} — check out current den-spec beside this repo, set DEN_SPEC_DIR, or set DEN_SPEC_OPTIONAL=1",
            path.display()
        ),
    }
}

#[test]
fn den_spec_headings_v1_vectors() {
    let Some(vectors) = vectors() else {
        return;
    };
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
    let Some(vectors) = vectors() else {
        return;
    };
    for case in vectors["validCases"].as_array().unwrap() {
        let request = json!({"op":"heading_v1","heading":case["value"]});
        let compact: Value =
            serde_json::from_str(&den_sync::evaluate(&request.to_string())).unwrap();
        assert_eq!(
            compact,
            json!({"version":1,"ok":{"version":1,"heading":case["normalized"]}})
        );
    }
    for case in vectors["invalidCases"].as_array().unwrap() {
        let request = json!({"op":"heading_v1","heading":case["value"]});
        let compact: Value =
            serde_json::from_str(&den_sync::evaluate(&request.to_string())).unwrap();
        assert_eq!(
            compact,
            json!({"version":1,"error":"invalid_heading"}),
            "{}",
            case["name"]
        );
    }
}
