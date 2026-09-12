use den_sync::{capture, commands, evaluate, merge, Stamp};
use serde_json::{json, Value};

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/merge-v2.json")).unwrap()
}

#[test]
fn signed_zero_progress_keeps_the_newer_stamp() {
    let base = vectors()["base"].clone();
    let newer = overlay(
        &base,
        &json!({"resume":{"value":-0.0,"viewing":0,"at":[2000,0,"a"]}}),
    );
    assert_eq!(
        merge(&base, &newer).unwrap()["resume"]["at"],
        json!([2000, 0, "a"])
    );
    assert_eq!(
        merge(&newer, &base).unwrap()["resume"]["at"],
        json!([2000, 0, "a"])
    );
}

#[test]
fn shared_binding_contract() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/policy-v1.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let result = request(case["request"].clone());
        assert_eq!(result["version"], 1);
        if case.get("error").is_some() {
            assert_eq!(result["error"], case["error"], "{}", case["name"]);
            assert!(result.get("ok").is_none());
        } else {
            assert_eq!(result["ok"], case["ok"], "{}", case["name"]);
            assert!(result.get("error").is_none());
        }
    }
}
fn overlay(base: &Value, delta: &Value) -> Value {
    let mut result = base.as_object().unwrap().clone();
    result.extend(delta.as_object().unwrap().clone());
    Value::Object(result)
}
fn request(value: Value) -> Value {
    serde_json::from_str(&evaluate(&value.to_string())).unwrap()
}

#[test]
fn den_spec_merge_and_clock_vectors() {
    let data = vectors();
    for case in data["merge"].as_array().unwrap() {
        let a = overlay(&data["base"], &case["a"]);
        let b = overlay(&data["base"], &case["b"]);
        let expected = overlay(&data["base"], &case["merged"]);
        assert_eq!(merge(&a, &b).unwrap(), expected, "{}", case["case"]);
        assert_eq!(merge(&b, &a).unwrap(), expected);
        assert_eq!(merge(&a, &a).unwrap(), a);
    }
    for case in data["settings"].as_array().unwrap() {
        assert_eq!(merge(&case["a"], &case["b"]).unwrap(), case["merged"]);
        assert_eq!(merge(&case["b"], &case["a"]).unwrap(), case["merged"]);
    }
    for case in data["clock"].as_array().unwrap() {
        let result = request(
            json!({"op":"issue", "last":case["last"], "seen":case["seen"], "now":case["now"], "device":"dddd"}),
        );
        assert_eq!(result["ok"], case["issued"]);
    }
}

#[test]
fn explicit_fields_only_and_supersession() {
    let before = vectors()["base"].clone();
    let after = overlay(
        &before,
        &json!({"status":{"value":"watched","at":[2000,0,"peer"]}, "reaction":{"value":"love","at":[3000,0,"local"]}}),
    );
    let event = capture(&before, &after, &Stamp(3000, 0, "local".into()), "action").unwrap();
    assert_eq!(event["changes"].as_object().unwrap().len(), 1);
    let pushes = commands(&event, &after).unwrap();
    assert_eq!(pushes[0]["kind"], "rating");
    assert_eq!(pushes[0]["rating"], 10);
    assert_eq!(pushes[0]["eventID"], "action:reaction");
    let newer = overlay(
        &after,
        &json!({"reaction":{"value":"like","at":[4000,0,"peer"]}}),
    );
    assert_eq!(commands(&event, &newer).unwrap(), json!([]));
    let mut forged = event.clone();
    forged["changes"]["status"] = json!({"before":before["status"], "after":after["status"]});
    assert!(commands(&forged, &after).is_err());
}

#[test]
fn higher_viewing_undo_and_completion_boundary() {
    let before = json!({"kind":"ep","schema":2,"title":{"type":"tv","id":1399},"season":1,"episode":1,"progress":{"value":1,"viewing":1,"at":[5000,0,"a"]}});
    for (progress, kind) in [
        (0.0, Some("unwatched")),
        (0.94, None),
        (0.95, Some("watched")),
        (1.0, Some("watched")),
    ] {
        let after = overlay(
            &before,
            &json!({"progress":{"value":progress,"viewing":2,"at":[2000,0,"b"]}}),
        );
        assert_eq!(merge(&before, &after).unwrap(), after);
        assert_eq!(merge(&after, &before).unwrap(), after);
        let event = capture(&before, &after, &Stamp(2000, 0, "b".into()), "episode").unwrap();
        let pushes = commands(&event, &after).unwrap();
        assert_eq!(
            pushes.as_array().unwrap().len(),
            usize::from(kind.is_some())
        );
        if let Some(kind) = kind {
            assert_eq!(pushes[0]["kind"], kind);
        }
    }
}

#[test]
fn title_intent_matrix() {
    for media in ["movie", "tv"] {
        for status in ["none", "watchlist", "watched"] {
            for next in ["none", "watchlist", "watched"] {
                let before = overlay(
                    &vectors()["base"],
                    &json!({"title":{"type":media,"id":550},"status":{"value":status,"at":[1000,0,"a"]}}),
                );
                let after = overlay(&before, &json!({"status":{"value":next,"at":[2000,0,"a"]}}));
                let event =
                    capture(&before, &after, &Stamp(2000, 0, "a".into()), "action").unwrap();
                let expected = match (media, status, next) {
                    (_, _, "watchlist") => Some("list"),
                    ("movie", _, "watched") => Some("watched"),
                    ("movie", "watched", "none") => Some("unwatched"),
                    _ => None,
                };
                let pushes = commands(&event, &after).unwrap();
                assert_eq!(
                    pushes.as_array().unwrap().len(),
                    usize::from(expected.is_some())
                );
                if let Some(kind) = expected {
                    assert_eq!(pushes[0]["kind"], kind);
                }
            }
        }
    }
    for (reaction, rating) in [
        (json!("love"), json!(10)),
        (json!("like"), json!(7)),
        (json!("dislike"), json!(2)),
        (Value::Null, Value::Null),
    ] {
        let before = vectors()["base"].clone();
        let after = overlay(
            &before,
            &json!({"reaction":{"value":reaction,"at":[2000,0,"a"]}}),
        );
        let event = capture(&before, &after, &Stamp(2000, 0, "a".into()), "reaction").unwrap();
        assert_eq!(commands(&event, &after).unwrap()[0]["rating"], rating);
    }
}

#[test]
fn removals_do_not_erase_unrelated_remote_state() {
    let mut command = json!({"kind":"unwatched","at":2000,"current":true,"baseline":false,"episode":false,"added":false,"rating":null});
    let mut remote = json!({"authoritative":true,"account_matches":true,"simkl":true,"watched":{"at":1000},"listed":null,"rated":{"at":1000,"value":10},"any_title_watch":true,"unknown_or_newer_title_watch":false,"episodes_complete":false});
    let decision = |c: &Value, r: &Value| {
        request(json!({"op":"decide","command":c,"remote":r}))["ok"]["action"].clone()
    };
    assert_eq!(decision(&command, &remote), "hold");
    remote["rated"] = Value::Null;
    assert_eq!(decision(&command, &remote), "send");
    for at in [Value::Null, json!(0), json!(3000)] {
        remote["watched"]["at"] = at;
        assert_eq!(decision(&command, &remote), "hold");
    }
    remote["watched"] = Value::Null;
    assert_eq!(decision(&command, &remote), "acknowledge");
    command["episode"] = json!(true);
    assert_eq!(decision(&command, &remote), "hold");
    remote["episodes_complete"] = json!(true);
    assert_eq!(decision(&command, &remote), "acknowledge");
    remote["account_matches"] = json!(false);
    assert_eq!(decision(&command, &remote), "hold");
    remote["authoritative"] = json!(false);
    assert_eq!(decision(&command, &remote), "hold");
    command["current"] = json!(false);
    assert_eq!(decision(&command, &remote), "superseded");
}

#[test]
fn errors_are_not_absence_or_acknowledgement() {
    for value in ["not json", "{\"op\":\"merge\",\"a\":{},\"b\":{}}"] {
        let result: Value = serde_json::from_str(&evaluate(value)).unwrap();
        assert!(result.get("error").is_some());
        assert!(result.get("ok").is_none());
    }
    assert!(request(json!({"op":"issue","last":[9007199254740991u64,9007199254740991u64,"a"],"now":0,"device":"a"})).get("error").is_some());
    assert_eq!(
        request(json!({"op":"retry","attempts":99,"now":1000,"retry_after":2000000}))["ok"],
        2000000
    );
}
