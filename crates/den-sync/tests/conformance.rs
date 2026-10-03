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
        include_str!("fixtures/policy-v3.json"),
        include_str!("fixtures/policy-v4.json"),
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
/// devices — equal stamps, equal epochs, empty holders and malformed values among them — merge commutatively,
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
            let at = json!([1000 + next(4) * 1000, 0, device]);
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

fn wat(entries: Value) -> Value {
    json!({"kind":"wat","schema":3,"title":{"type":"tv","id":1399},"season":1,"block":0,"seasonReset":null,"entries":entries})
}

#[test]
fn v3_names_keys_and_writer_devices() {
    assert_eq!(
        request(json!({"op":"watch_name","media":"tv","id":1399,"season":1,"episode":31}))["ok"],
        "wat:tv:1399:1:0"
    );
    assert_eq!(
        request(json!({"op":"watch_name","media":"tv","id":1399,"season":1,"episode":32}))["ok"],
        "wat:tv:1399:1:1"
    );
    assert_eq!(
        request(json!({"op":"watch_name","media":"movie","id":550,"season":0,"episode":0}))["ok"],
        "wat:movie:550:0:0"
    );
    assert_eq!(
        request(
            json!({"op":"receipt_name","provider":"simkl","account":"42","target":"rec:movie:550"})
        )["ok"],
        "snt:simkl:42:t3e0"
    );
    let invalid = request(
        json!({"op":"register_write","action":{"kind":"progress","value":0.5,"at":[1000,0,"NO"]},"current":null,"now":1000}),
    );
    assert_eq!(invalid["error"], "invalid_device");
}

#[test]
fn v3_merge_laws_and_bounded_plays() {
    let a = wat(
        json!({"01":{"imported":true,"plays":{},"cleared":null},"2":{"imported":false,"plays":{"0":1000,"1":2000,"2":3000,"3":4000,"4":5000},"cleared":null}}),
    );
    let b = wat(
        json!({"-1":{"imported":true,"plays":{},"cleared":null},"2":{"imported":true,"plays":{"0":900,"5":6000,"6":7000,"7":8000,"8":9000,"9":10000},"cleared":[2,[11000,0,"a1b2c3d4e5f60718"]]},"100000":{"imported":false,"plays":{},"cleared":null}}),
    );
    let ab = merge(&a, &b).unwrap();
    assert_eq!(ab, merge(&b, &a).unwrap());
    assert_eq!(ab, merge(&ab, &a).unwrap());
    assert!(
        ab["entries"].get("01").is_some()
            && ab["entries"].get("-1").is_some()
            && ab["entries"].get("100000").is_some()
    );
    assert_eq!(ab["entries"]["2"]["plays"].as_object().unwrap().len(), 8);
    assert_eq!(ab["entries"]["2"]["plays"]["0"], 900);
    let c = wat(json!({"2":{"imported":false,"plays":{"10":11000},"cleared":null}}));
    assert_eq!(
        merge(&merge(&a, &b).unwrap(), &c).unwrap(),
        merge(&a, &merge(&b, &c).unwrap()).unwrap()
    );
}

#[test]
fn v3_derivation_resets_future_and_imports() {
    let register = json!({"progress":{"value":1.0,"viewing":2,"at":[2000,0,"a1b2c3d4e5f60718"]},"imported":true,"plays":{"-9007199254739992":1000,"2":2000,"3":200000000},"cleared":null});
    let state = request(
        json!({"op":"episode_state","register":register,"resets":[[1500,0,"local"]],"now":3000}),
    )["ok"]
        .clone();
    assert_eq!(state["watched"], true);
    assert_eq!(state["viewing"], 2);
    assert_eq!(state["plays"], json!([[2, 2000]]));
    assert_eq!(state["first_play"], 2000);
    let hidden = request(
        json!({"op":"episode_state","register":register,"resets":[[2500,0,"local"]],"now":3000}),
    )["ok"]
        .clone();
    assert_eq!(hidden["watched"], false);
    assert_eq!(hidden["viewing"], 2);
}

#[test]
fn v3_receipt_settle_order_beats_clock() {
    let a = json!({"kind":"snt","schema":3,"provider":"simkl","account":"42","target":"wat:tv:1399:1:0","entries":{"2":["w",1,9000,[9_999_999_999_999i64,0,"a"],[2,1,"aaaaaaaaaaaaaaaa"]]} });
    let b = json!({"kind":"snt","schema":3,"provider":"simkl","account":"42","target":"wat:tv:1399:1:0","entries":{"2":["u",2,null,[1,0,"b"],[3,0,"bbbbbbbbbbbbbbbb"]]} });
    assert_eq!(merge(&a, &b).unwrap()["entries"]["2"][0], "u");
    assert_eq!(merge(&b, &a).unwrap(), merge(&a, &b).unwrap());
}

#[test]
fn v3_delivery_rating_rewatch_removals_and_lease() {
    let base_command = json!({"kind":"rating","at":2000,"current":true,"baseline":false,"episode":false,"added":false,"rating":7});
    let remote = json!({"authoritative":true,"account_matches":true,"simkl":true,"watched":null,"listed":null,"rated":{"at":1000,"value":6},"any_title_watch":false,"unknown_or_newer_title_watch":false,"episodes_complete":true});
    assert_eq!(
        request(json!({"op":"decide","command":base_command,"remote":remote}))["ok"]["action"],
        "acknowledge"
    );
    let valueless = json!({"authoritative":true,"account_matches":true,"simkl":true,"watched":null,"listed":null,"rated":{"at":null,"value":null},"any_title_watch":false,"unknown_or_newer_title_watch":false,"episodes_complete":true});
    let baseline = json!({"kind":"rating","at":0,"current":true,"baseline":true,"episode":false,"added":false,"rating":10});
    assert_eq!(
        request(json!({"op":"decide","command":baseline,"remote":valueless}))["ok"]["action"],
        "acknowledge"
    );
    assert_eq!(
        request(
            json!({"op":"lease","input":{"device":"aaaaaaaaaaaaaaaa","holder":"","epoch":2,"greatest_epoch":4,"elapsed":0,"observed":0,"fresh_generation":false}})
        )["ok"],
        json!({"action":"take","epoch":5})
    );
    assert_eq!(
        request(
            json!({"op":"lease","input":{"device":"aaaaaaaaaaaaaaaa","holder":"aaaaaaaaaaaaaaaa","epoch":5,"elapsed":120000}})
        )["ok"]["action"],
        "stop"
    );
}

#[test]
fn v3_fold_and_write_back_never_emit_v2_episode_or_event() {
    let ep = json!({"kind":"ep","schema":2,"title":{"type":"tv","id":1399},"season":1,"episode":2,"progress":{"value":1.0,"viewing":1,"at":[5000,0,"aaaaaaaaaaaaaaaa"]}});
    let form = request(json!({"op":"v3_form","rows":[ep],"now":5000}))["ok"].clone();
    assert_eq!(form[0]["kind"], "wat");
    assert_eq!(form[0]["entries"]["2"]["plays"]["1"], 5000);
    let back = request(json!({"op":"write_back","held":[ep],"log":[],"now":5000}))["ok"].clone();
    assert!(back
        .as_array()
        .unwrap()
        .iter()
        .all(|row| !matches!(row["kind"].as_str(), Some("ep") | Some("tracker-event"))));
}

/// A v1 tracker event as shipped (library v3 Appendix A): `capture`'s event inside `set:tracker-event:<id>`.
fn event_row(id: &str, before: &Value, after: &Value, at: Stamp) -> Value {
    let event = capture(before, after, &at, id).unwrap();
    json!({"kind":"set","schema":2,"name":format!("tracker-event:{id}"),"values":{"event":{"value":{"string":event.to_string()},"at":at}}})
}

fn episode(number: u64, value: f64, viewing: u64, t: i64) -> Value {
    json!({"kind":"ep","schema":2,"title":{"type":"tv","id":1399},"season":1,"episode":number,"progress":{"value":value,"viewing":viewing,"at":[t,0,"aaaaaaaaaaaaaaaa"]}})
}

/// A v2 log: episode 2 rewatched to half way (its row), whose first viewing finished only in a v1 event; and a
/// film watched only in a v1 event.
fn v2_log_with_events() -> Vec<Value> {
    let film = overlay(
        &vectors()["base"],
        &json!({"title":{"type":"movie","id":550}}),
    );
    let watched = overlay(
        &film,
        &json!({"status":{"value":"watched","at":[6000,0,"aaaaaaaaaaaaaaaa"]}}),
    );
    vec![
        episode(2, 0.5, 2, 7000),
        event_row(
            "e2",
            &episode(2, 0.0, 0, 1000),
            &episode(2, 1.0, 1, 5000),
            Stamp(5000, 0, "aaaaaaaaaaaaaaaa".into()),
        ),
        film.clone(),
        event_row(
            "f550",
            &film,
            &watched,
            Stamp(6000, 0, "aaaaaaaaaaaaaaaa".into()),
        ),
    ]
}

fn by_name(rows: &Value) -> std::collections::BTreeMap<String, Value> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let name = request(json!({"op":"name","row":row}))["ok"]
                .as_str()
                .unwrap()
                .to_owned();
            (name, row.clone())
        })
        .collect()
}

fn no_v2_rows(rows: &Value) -> bool {
    by_name(rows)
        .keys()
        .all(|name| !name.starts_with("set:tracker-event:") && !name.starts_with("ep:"))
}

#[test]
fn v3_form_folds_v1_events_and_keeps_none() {
    let form =
        request(json!({"op":"v3_form","rows":v2_log_with_events(),"now":8000}))["ok"].clone();
    assert!(no_v2_rows(&form), "{form}");
    let rows = by_name(&form);
    // §8: the event's finished first viewing is a play beside the row's rewatch in progress.
    let register = &rows["wat:tv:1399:1:0"]["entries"]["2"];
    let state = request(json!({"op":"episode_state","register":register,"resets":[],"now":8000}))
        ["ok"]
        .clone();
    assert_eq!(state["viewing"], 2);
    assert_eq!(state["plays"], json!([[1, 5000]]));
    assert_eq!(state["resume"]["value"], 0.5);
    // §8 Titles and Film plays: the film event's `after` merges into the `rec`, which yields the play.
    let rec = &rows["rec:movie:550"];
    assert_eq!(rec["status"]["value"], "watched");
    let film = request(json!({"op":"film_state","rec":rec,"register":rows["wat:movie:550:0:0"]["entries"]["0"],"resets":[],"now":8000}))["ok"].clone();
    assert_eq!(film["watched"], true);
    assert_eq!(film["watched_at"], 6000);

    let back = request(json!({"op":"write_back","held":v2_log_with_events(),"log":[],"now":8000}))
        ["ok"]
        .clone();
    assert!(no_v2_rows(&back), "{back}");
}

#[test]
fn v3_form_drops_an_event_a_reader_ignores() {
    let mut forged = event_row(
        "e2",
        &episode(2, 0.0, 0, 1000),
        &episode(2, 1.0, 1, 5000),
        Stamp(5000, 0, "aaaaaaaaaaaaaaaa".into()),
    );
    forged["name"] = json!("tracker-event:other");
    let form = request(json!({"op":"v3_form","rows":[forged],"now":8000}))["ok"].clone();
    assert_eq!(form, json!([]));
}

#[test]
fn v3_compact_folds_stray_rows_into_the_same_state() {
    let switched = request(json!({"op":"v3_form","rows":v2_log_with_events(),"now":8000}))["ok"]
        .as_array()
        .unwrap()
        .clone();
    let events = v2_log_with_events()
        .into_iter()
        .filter(|row| row["kind"] == "set")
        .collect::<Vec<_>>();
    // A library switched while v3_form still kept its v1 events: compaction drops them and changes nothing else.
    let kept = [switched.clone(), events.clone()].concat();
    let compacted = request(json!({"op":"v3_compact","rows":kept,"now":8000}))["ok"].clone();
    assert!(no_v2_rows(&compacted), "{compacted}");
    assert_eq!(compacted, json!(switched));

    // A stray episode row written after the switch is folded into the block the log already holds, as the
    // switch would have folded it.
    let stray = episode(3, 1.0, 0, 7500);
    let rows = [switched, events, vec![stray.clone()]].concat();
    let compacted = request(json!({"op":"v3_compact","rows":rows,"now":8000}))["ok"].clone();
    let mut log = v2_log_with_events();
    log.push(stray);
    let expected = request(json!({"op":"v3_form","rows":log,"now":8000}))["ok"].clone();
    assert!(no_v2_rows(&compacted), "{compacted}");
    assert_eq!(compacted, expected);
}

#[test]
fn v3_compact_does_not_rederive_film_plays_from_untouched_titles() {
    let rec = overlay(
        &vectors()["base"],
        &json!({"title":{"type":"movie","id":550},"status":{"value":"watched","at":[6000,0,"aaaaaaaaaaaaaaaa"]}}),
    );
    let compacted =
        request(json!({"op":"v3_compact","rows":[rec.clone()],"now":8000}))["ok"].clone();
    assert_eq!(compacted, json!([rec]));
}

#[test]
fn v3_form_preserves_shipped_film_episode_rows_without_deriving_a_watch() {
    let ep = json!({"kind":"ep","schema":2,"title":{"type":"movie","id":129},"season":1,"episode":1,"progress":{"value":1.0,"viewing":0,"at":[5000,0,"aaaaaaaaaaaaaaaa"]}});
    let form = request(json!({"op":"v3_form","rows":[ep],"now":5000}))["ok"].clone();
    assert_eq!(form, json!([ep]));
    assert!(form
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["kind"] != json!("wat")));
}

#[test]
fn v3_switch_moves_simkl_credentials_and_seeds_delivery() {
    let keys = json!({"kind":"set","schema":2,"name":"keys","values":{"simkl":{"value":{"string":"secret-token"},"at":[1000,0,"aaaaaaaaaaaaaaaa"]}}});
    let form = request(json!({
        "op":"v3_form",
        "rows":[keys],
        "now":2000,
        "context":{
            "performer":"aaaaaaaaaaaaaaaa",
            "stamp":[2000,0,"aaaaaaaaaaaaaaaa"],
            "base":7,
            "accounts":[{"provider":"simkl","account":"42","credential":"secret-token","connected_at":[1000,0,"aaaaaaaaaaaaaaaa"]}]
        }
    }))["ok"].as_array().unwrap().clone();
    let named = form
        .iter()
        .map(|row| {
            let name = request(json!({"op":"name","row":row}))["ok"]
                .as_str()
                .unwrap()
                .to_owned();
            (name, row)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(named["set:keys"]["values"]["simkl"]["value"], Value::Null);
    let connection = named["set:trackers"]["values"]["simkl:42"]["value"]["string"]
        .as_str()
        .unwrap();
    assert!(connection.contains("secret-token"));
    assert_eq!(
        named["set:deliver:simkl:42"]["values"]["lease"]["value"],
        json!({"strings":["","1"]})
    );
    assert_eq!(
        named["set:deliver:simkl:42"]["values"]["seededThrough"]["value"],
        json!({"int":10})
    );
}

#[test]
fn web_only_switch_delivers_missing_work_once_and_does_not_resend_present_work() {
    let target = json!({"key":"rec:movie:550:list","kind":"list","value":"in","stamp":[1000,0,"aaaaaaaaaaaaaaaa"]});
    let pending = request(json!({"op":"pending_targets","targets":[target],"receipts":{},"since":[2000,0,"aaaaaaaaaaaaaaaa"],"now":2000}))["ok"].clone();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["baseline"], true);

    let present = json!({"authoritative":true,"account_matches":true,"simkl":true,"watched":null,"listed":{"at":null},"rated":null,"any_title_watch":false,"unknown_or_newer_title_watch":false,"episodes_complete":true});
    let decision =
        request(json!({"op":"decide","command":pending[0],"remote":present}))["ok"].clone();
    assert_eq!(decision["action"], "acknowledge");
    let receipt = request(json!({"op":"settle","outcome":decision,"built_from":pending[0]["built_from"],"order":[1,1,"aaaaaaaaaaaaaaaa"]}))["ok"].clone();
    let after = request(json!({"op":"pending_targets","targets":[target],"receipts":{"rec:movie:550:list":receipt},"since":[2000,0,"aaaaaaaaaaaaaaaa"],"now":2000}))["ok"].clone();
    assert!(
        after.as_array().unwrap().is_empty(),
        "an acknowledged remote value is never sent twice"
    );

    let absent = json!({"authoritative":true,"account_matches":true,"simkl":true,"watched":null,"listed":null,"rated":null,"any_title_watch":false,"unknown_or_newer_title_watch":false,"episodes_complete":true});
    let send = request(json!({"op":"decide","command":pending[0],"remote":absent}))["ok"].clone();
    assert_eq!(
        send["action"], "send",
        "missing additive work is not dropped"
    );
}

#[test]
fn no_receipt_never_authorizes_a_destructive_tracker_command() {
    let targets = json!([
        {"key":"watch","kind":"episode","value":"unwatched","stamp":[1000,0,"aaaaaaaaaaaaaaaa"]},
        {"key":"list","kind":"list","value":"gone","stamp":[1000,0,"aaaaaaaaaaaaaaaa"]},
        {"key":"rating","kind":"rating","value":"none","stamp":[1000,0,"aaaaaaaaaaaaaaaa"]}
    ]);
    let pending = request(json!({"op":"pending_targets","targets":targets,"receipts":{},"since":[2000,0,"aaaaaaaaaaaaaaaa"],"now":2000}))["ok"].clone();
    assert!(pending.as_array().unwrap().is_empty());
}

#[test]
fn a_watch_receipt_prevents_resending_the_same_play() {
    let target = json!({
        "key":"wat:tv:95396:1:0#2", "kind":"episode", "value":"watched",
        "stamp":[2000,0,"aaaaaaaaaaaaaaaa"], "p":0, "watched_at":2000
    });
    let receipts = json!({
        "wat:tv:95396:1:0#2":["w",0,2000,[2000,0,"aaaaaaaaaaaaaaaa"],[1,1,"aaaaaaaaaaaaaaaa"]]
    });
    assert_eq!(
        request(json!({
            "op":"pending_targets", "targets":[target], "receipts":receipts,
            "since":[1000,0,"aaaaaaaaaaaaaaaa"], "now":3000
        }))["ok"],
        json!([])
    );
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
