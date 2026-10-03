//! The recovery code (den-spec `wire/recovery-code.md`) through `evaluate`, as both clients call it, against every
//! case in den-spec's `vectors/recovery-v1.json`. The sealed blobs there are the clients' to open (§12): den-core
//! derives the key they open with.

use den_sync::evaluate;
use serde_json::{json, Value};
use std::path::PathBuf;

fn call(request: Value) -> Value {
    serde_json::from_str(&evaluate(&request.to_string())).unwrap()
}

fn vectors() -> Option<Value> {
    let dir = match std::env::var("DEN_SPEC_DIR") {
        Ok(dir) => PathBuf::from(dir).join("vectors"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../den-spec/vectors"),
    };
    match std::fs::read_to_string(dir.join("recovery-v1.json")) {
        Ok(text) => Some(serde_json::from_str(&text).unwrap()),
        Err(_) if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") => {
            eprintln!("SKIP: den-spec absent and DEN_SPEC_OPTIONAL=1");
            None
        }
        Err(_) => panic!(
            "den-spec/vectors/recovery-v1.json not found — check out den-spec beside this repo, set DEN_SPEC_DIR, \
             or set DEN_SPEC_OPTIONAL=1 to skip deliberately."
        ),
    }
}

#[test]
fn den_spec_recovery_v1_vectors() {
    let Some(v) = vectors() else { return };
    assert_eq!(v["alphabet"], "ABCDEFGHJKLMNPQRSTUVWXYZ23456789");
    assert_eq!(
        v["kdf"],
        json!({"algorithm": "argon2id", "version": 19, "memoryKiB": 65536, "passes": 3, "parallelism": 1,
               "tagLength": 32, "salt": "64656e2f7265636f766572792f7631"}),
        "the vectors moved to parameters this implementation does not use"
    );

    let codes = v["codes"].as_array().unwrap();
    // Nine cases, ten once den-spec carries the `ſ` case (oxyc/den-spec, recovery §2 follow-up).
    assert!(codes.len() >= 9, "{} cases", codes.len());
    for case in codes {
        let got = call(json!({"op": "recovery_read", "text": case["input"]}));
        let want = &case["parsed"];
        match want.get("error") {
            Some(error) => assert_eq!(got["error"], *error, "{}", case["input"]),
            None => assert_eq!(got["ok"], *want, "{}", case["input"]),
        }
    }

    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        let made = call(json!({"op": "recovery_code", "random": entry["random"]}));
        assert_eq!(
            made["ok"],
            json!({"code": entry["code"], "data": entry["data"]})
        );
        assert_eq!(
            entry["code"].as_str().unwrap().replace('-', ""),
            format!(
                "{}{}",
                entry["data"].as_str().unwrap(),
                entry["check"].as_str().unwrap()
            )
        );
        let read = call(json!({"op": "recovery_read", "text": entry["code"]}));
        assert_eq!(read["ok"]["data"], entry["data"]);
        let derived = call(json!({"op": "recovery_derive", "data": entry["data"]}));
        assert_eq!(
            derived["ok"],
            json!({"locator": entry["locator"], "wrapKey": entry["wrapKey"]}),
            "{}",
            entry["code"]
        );
    }
}

#[test]
fn malformed_requests_are_errors() {
    for (request, error) in [
        (
            json!({"op": "recovery_code", "random": "e6e4"}),
            "invalid_random",
        ),
        (json!({"op": "recovery_read", "text": ""}), "mistyped"),
        (
            json!({"op": "recovery_derive", "data": "GEB2LP9UC63WQ95UNSLTXMFL"}),
            "mistyped",
        ),
        (json!({"op": "recovery_derive"}), "invalid_request"),
    ] {
        assert_eq!(call(request.clone())["error"], error, "{request}");
    }
}
