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
    for fixture in [
        include_str!("fixtures/policy-v1.json"),
        include_str!("fixtures/policy-v4.json"),
        include_str!("fixtures/profiles-v1.json"),
    ] {
        let fixture: Value = serde_json::from_str(fixture).unwrap();
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
}

/// The web's two modules together answer exactly what the native binding's one entry answers: every assistant op
/// through `evaluate_assistant`, every other op through `evaluate_core`, and each refuses the other's.
#[test]
fn the_web_split_answers_as_the_whole() {
    let call = |entry: fn(&str) -> String, request: &Value| -> Value {
        serde_json::from_str(&entry(&request.to_string())).unwrap()
    };
    let mut assistant = 0;
    for fixture in [
        include_str!("fixtures/policy-v1.json"),
        include_str!("fixtures/policy-v4.json"),
        include_str!("fixtures/profiles-v1.json"),
    ] {
        let fixture: Value = serde_json::from_str(fixture).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let request = &case["request"];
            let whole = call(evaluate, request);
            let ours = request["op"]
                .as_str()
                .is_some_and(|op| op.starts_with("assistant_"));
            if ours {
                assistant += 1;
                assert_eq!(
                    call(den_sync::evaluate_assistant, request),
                    whole,
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    call(den_sync::evaluate_core, request)["error"],
                    "assistant_module",
                    "{}",
                    case["name"]
                );
            } else {
                assert_eq!(
                    call(den_sync::evaluate_core, request),
                    whole,
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    call(den_sync::evaluate_assistant, request)["error"],
                    "invalid_request",
                    "{}",
                    case["name"]
                );
            }
        }
    }
    assert!(assistant > 0);
}

#[test]
fn deliver_settings_merge_as_a_join() {
    let row = |device: &str, at: u64, since: u64, holder: &str, epoch: u64, unverified: Value| {
        json!({"kind":"set","schema":2,"name":"deliver:simkl:42","values":{
            "since":{"at":[at,0,device],"value":{"string":format!("[{since},0,\"{device}\"]")}},
            "lease":{"at":[at,0,device],"value":{"strings":[holder, epoch.to_string()]}},
            "unverified":{"at":[at,0,device],"value":{"ints":unverified}}}})
    };
    let a = row(
        "aaaaaaaaaaaaaaaa",
        9000,
        3000,
        "aaaaaaaaaaaaaaaa",
        5,
        json!([2]),
    );
    let b = row("bbbbbbbbbbbbbbbb", 7000, 1000, "", 5, json!([4]));
    let c = row(
        "cccccccccccccccc",
        8000,
        2000,
        "cccccccccccccccc",
        6,
        json!([2, 3]),
    );
    assert_eq!(merge(&a, &b).unwrap(), merge(&b, &a).unwrap());
    assert_eq!(merge(&a, &a).unwrap(), a);
    let left = merge(&merge(&a, &b).unwrap(), &c).unwrap();
    let right = merge(&a, &merge(&b, &c).unwrap()).unwrap();
    assert_eq!(left, right);
    let values = &left["values"];
    // The earliest `since`, whatever its stamp; the greatest epoch; every epoch any version listed.
    assert_eq!(
        values["since"]["value"]["string"],
        "[1000,0,\"bbbbbbbbbbbbbbbb\"]"
    );
    assert_eq!(
        values["lease"]["value"]["strings"],
        json!(["cccccccccccccccc", "6"])
    );
    assert_eq!(values["unverified"]["value"]["ints"], json!([2, 3, 4]));
    // At one epoch an empty holder (a release) wins over a holder.
    assert_eq!(
        merge(&a, &b).unwrap()["values"]["lease"]["value"]["strings"],
        json!(["", "5"])
    );
    // Any other settings row keeps the later stamp.
    let other = |row: &Value| {
        let mut row = row.clone();
        row["name"] = json!("prefs");
        row
    };
    assert_eq!(
        merge(&other(&a), &other(&b)).unwrap()["values"]["since"]["value"]["string"],
        "[3000,0,\"aaaaaaaaaaaaaaaa\"]"
    );
}

/// The `set:deliver` merges as a property: random versions of `since`, `lease`, `unverified` and `removals` from three
/// devices — equal stamps, equal `at`s, equal epochs, empty holders and malformed values among them — merge commutatively,
/// associatively and idempotently.
#[test]
fn deliver_settings_merge_laws_hold_over_random_versions() {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    let devices = ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"];
    for _ in 0..500 {
        let mut versions = Vec::new();
        for device in devices {
            // One in three versions carries the first device's `at`, so two versions can tie on `at` exactly.
            let at_device = if next(3) == 0 { devices[0] } else { device };
            let at = json!([1000 + next(4) * 1000, 0, at_device]);
            let stamp = |t: u64| json!([t * 1000, 0, devices[(t % 3) as usize]]);
            let since = if next(5) == 0 {
                json!({"value": {"string": "nope"}, "at": at})
            } else {
                json!({"value": {"string": stamp(1 + next(3)).to_string()}, "at": at})
            };
            let lease = if next(5) == 0 {
                json!({"value": {"strings": ["x"]}, "at": at})
            } else {
                let holder = if next(2) == 0 { "" } else { device };
                json!({"value": {"strings": [holder, (1 + next(3)).to_string()]}, "at": at})
            };
            let unverified = if next(5) == 0 {
                json!({"value": {"ints": [1.5]}, "at": at})
            } else {
                let epochs: Vec<u64> = (2..6).filter(|_| next(2) == 0).collect();
                json!({"value": {"ints": epochs}, "at": at})
            };
            let removals = if next(5) == 0 {
                json!({"value": {"string": "\"held\""}, "at": at})
            } else {
                let mut latch = serde_json::Map::new();
                if next(2) == 0 {
                    latch.insert("approved".into(), stamp(1 + next(4)));
                }
                if next(2) == 0 {
                    latch.insert("held".into(), stamp(1 + next(4)));
                }
                json!({"value": {"string": Value::Object(latch).to_string()}, "at": at})
            };
            versions.push(
                json!({"kind": "set", "schema": 2, "name": "deliver:simkl:42", "values": {
                "since": since, "lease": lease, "unverified": unverified, "removals": removals}}),
            );
        }
        let (a, b, c) = (&versions[0], &versions[1], &versions[2]);
        let m = |x: &Value, y: &Value| merge(x, y).unwrap();
        assert_eq!(m(a, b), m(b, a), "commutative: {a} {b}");
        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative: {a} {b} {c}");
        let ab = m(a, b);
        assert_eq!(m(&ab, &ab), ab, "idempotent: {ab}");
    }
}

/// A download row's merge (library-v4 §17) as a property: random versions from three devices, each holding some of
/// the values, a removal among them, and starts stamped before and after it, merge commutatively, associatively and
/// idempotently — so a removal can't be undone by the order rows arrive in.
#[test]
fn download_rows_merge_as_a_join() {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    let devices = ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"];
    for _ in 0..500 {
        let mut versions = Vec::new();
        for device in devices {
            let mut values = serde_json::Map::new();
            for key in ["release", "queuedAt", "tried", "announced", "removed"] {
                if next(2) == 0 {
                    continue;
                }
                let at = json!([1000 * (1 + next(5)), 0, device]);
                let value = match key {
                    "release" => json!({"string": format!("{{\"identity\":\"r{}\"}}", next(3))}),
                    "queuedAt" => json!({"int": 1000 * next(5)}),
                    "tried" => {
                        json!({"strings": (0..next(3)).map(|i| format!("r{i}")).collect::<Vec<_>>()})
                    }
                    _ => json!({"bool": true}),
                };
                values.insert(key.into(), json!({"value": value, "at": at}));
            }
            versions.push(json!({"kind": "set", "schema": 2, "name": "download:movie:550:-1:-1", "values": values}));
        }
        let (a, b, c) = (&versions[0], &versions[1], &versions[2]);
        let m = |x: &Value, y: &Value| den_sync::download_merge(x, y).unwrap();
        assert_eq!(m(a, b), m(b, a), "commutative: {a} {b}");
        assert_eq!(m(&m(a, b), c), m(a, &m(b, c)), "associative: {a} {b} {c}");
        let ab = m(a, b);
        assert_eq!(m(&ab, &ab), ab, "idempotent: {ab}");
        if let Some(removed) = ab["values"]["removed"]["at"][0].as_u64() {
            for (key, value) in ab["values"].as_object().unwrap() {
                assert!(
                    value["at"][0].as_u64().unwrap() >= removed,
                    "{key} older than the removal survived: {ab}"
                );
            }
        }
    }
}

/// Past 100 live downloads the oldest are pruned, whatever their state.
#[test]
fn download_prune_caps_live_rows() {
    let rows: Vec<Value> = (0..103)
        .map(|i: i64| {
            let at = json!([1000 + i, 0, "aaaaaaaaaaaaaaaa"]);
            json!({"kind": "set", "schema": 2, "name": format!("download:movie:{}:-1:-1", i + 1), "values": {
                "release": {"value": {"string": "{\"identity\":\"x\"}"}, "at": at},
                "queuedAt": {"value": {"int": 1000 + i}, "at": at}}})
        })
        .collect();
    let pruned = den_sync::download_prune(
        &rows,
        &serde_json::Map::new(),
        &serde_json::Map::new(),
        10_000,
    )
    .unwrap();
    assert_eq!(
        pruned["remove"],
        json!([
            "download:movie:1:-1:-1",
            "download:movie:2:-1:-1",
            "download:movie:3:-1:-1"
        ])
    );
}

/// A ready row outlives any age at all once it has no watched state behind it — den#202's owner-reported
/// bug was `download_prune` aging a ready row out on a timer, which clears it from the Downloads page before
/// anyone has actually watched the episode or film it fetched.
#[test]
fn download_prune_keeps_a_ready_unwatched_row_at_any_age() {
    let at = json!([0, 0, "aaaaaaaaaaaaaaaa"]);
    let row = json!({"kind": "set", "schema": 2, "name": "download:movie:1:-1:-1", "values": {
        "release": {"value": {"string": "{\"identity\":\"x\"}"}, "at": at},
        "queuedAt": {"value": {"int": 0}, "at": at},
        "announced": {"value": {"bool": true}, "at": at}}});
    // A hundred years past queuedAt: the old two-day ready TTL would have pruned this long ago.
    let now = 100 * 365 * 86_400_000;
    let pruned = den_sync::download_prune(
        &[row],
        &serde_json::Map::new(),
        &serde_json::Map::new(),
        now,
    )
    .unwrap();
    assert_eq!(pruned["remove"], json!([]));
}

/// The same row remains for two days after landing once watched, so Downloads confirms what completed, then ages
/// out. `watched` is its own episode or film, never the series' standing (the caller's job).
#[test]
fn download_prune_keeps_recent_ready_history_then_drops_it() {
    let at = json!([0, 0, "aaaaaaaaaaaaaaaa"]);
    let row = json!({"kind": "set", "schema": 2, "name": "download:tv:1399:2:3", "values": {
        "release": {"value": {"string": "{\"identity\":\"x\"}"}, "at": at},
        "queuedAt": {"value": {"int": 0}, "at": at},
        "announced": {"value": {"bool": true}, "at": at}}});
    let mut watched = serde_json::Map::new();
    watched.insert("download:tv:1399:2:3".into(), json!(true));
    let recent = den_sync::download_prune(
        std::slice::from_ref(&row),
        &serde_json::Map::new(),
        &watched,
        2 * 86_400_000,
    )
    .unwrap();
    assert_eq!(recent["remove"], json!([]));
    let pruned = den_sync::download_prune(
        &[row],
        &serde_json::Map::new(),
        &watched,
        2 * 86_400_000 + 1,
    )
    .unwrap();
    assert_eq!(pruned["remove"], json!(["download:tv:1399:2:3"]));
}

#[test]
fn download_prune_never_expires_an_unfinished_request() {
    let at = json!([0, 0, "aaaaaaaaaaaaaaaa"]);
    let row = json!({"kind": "set", "schema": 2, "name": "download:tv:1399:2:1", "values": {
        "release": {"value": {"string": "{\"identity\":\"x\"}"}, "at": at},
        "queuedAt": {"value": {"int": 0}, "at": at},
        "exhausted": {"value": {"bool": true}, "at": at}}});
    let states =
        serde_json::Map::from_iter([("download:tv:1399:2:1".into(), json!("no_working_release"))]);
    let pruned = den_sync::download_prune(
        &[row],
        &states,
        &serde_json::Map::new(),
        100 * 365 * 86_400_000,
    )
    .unwrap();
    assert_eq!(pruned["remove"], json!([]));
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
