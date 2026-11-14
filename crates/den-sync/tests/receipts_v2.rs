//! Native/Web JSON boundary coverage for profile-routed playback receipts v2.

use den_assistant::{
    b64url, b64url_decode, kem_public, key_id, playback_receipt_v2_route, seal_playback_receipt_v2,
    PlaybackReceiptV2, PlaybackReceiptV2Aad, ESEED_LEN, STALE_MS,
};
use den_sync::{evaluate, evaluate_assistant, evaluate_core, evaluate_deferred, evaluate_startup};
use serde_json::{json, Value};

const HOUSEHOLD: &str = "00000000000000000000000000000001";
const ROUTE: &str = "d4b68422b2b00134a42a7a585c60a5ea";
const BINDING: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ID: &str = "94619420c64002287a3ad7ecad5dfa6b";
const DEVICE: &str = "a1b2c3d4e5f60718";
const NOW: u64 = 1_790_000_500_000;

fn answer(call: impl Fn(&str) -> String, request: &Value) -> Value {
    serde_json::from_str(&call(&request.to_string())).unwrap()
}

#[test]
fn native_and_lazy_web_boundaries_make_the_same_profile_authority_and_key() {
    let authority = json!({
        "op": "playback_receipt_v2_authority",
        "role": "owner",
        "random": "00".repeat(48),
    });
    assert_eq!(
        answer(evaluate, &authority),
        answer(evaluate_assistant, &authority)
    );
    let made = answer(evaluate, &authority)["ok"].clone();
    assert_eq!(made["authority"].as_str().unwrap().len(), 43);
    assert_eq!(made["binding"].as_str().unwrap().len(), 32);
    assert_eq!(made["route"].as_str().unwrap().len(), 32);
    assert_eq!(made["role"], "owner");
    let operation = json!({
        "binding": made["binding"], "generation": 1, "household": HOUSEHOLD,
        "issuedAt": NOW, "nonce": "01010101010101010101010101010101", "op": "activate",
        "route": made["route"], "v": 2,
    });
    let proof = json!({
        "op":"playback_receipt_v2_proof", "authority":made["authority"], "operation":operation,
    });
    assert_eq!(answer(evaluate, &proof), answer(evaluate_assistant, &proof));
    assert_eq!(
        answer(evaluate, &proof)["ok"]["proof"]
            .as_str()
            .unwrap()
            .len(),
        43
    );

    let visitor = json!({"op":"playback_receipt_v2_authority","role":"visitor"});
    assert_eq!(
        answer(evaluate, &visitor)["ok"],
        json!({"status":"no_receipt","v":2})
    );
    assert_eq!(
        answer(evaluate_assistant, &visitor)["ok"],
        json!({"status":"no_receipt","v":2})
    );
    // Reduced Tizen 3 links only evaluate_core, which keeps receipt crypto out and preserves its capable-device handoff.
    assert_eq!(
        answer(evaluate_core, &authority)["error"],
        "assistant_module"
    );

    let key = json!({"op":"playback_receipt_v2_keygen","random":"42".repeat(32)});
    assert_eq!(answer(evaluate, &key), answer(evaluate_assistant, &key));
    let made = answer(evaluate, &key)["ok"].clone();
    assert_eq!(
        b64url_decode(made["public"].as_str().unwrap())
            .unwrap()
            .len(),
        1_216
    );
    assert_eq!(
        b64url_decode(made["secret"].as_str().unwrap())
            .unwrap()
            .len(),
        32
    );
}

#[test]
fn strict_drain_then_single_named_key_open_returns_route_bound_atomic_facts() {
    let secret = [0x42; 32];
    let public = kem_public(&secret);
    let kid = key_id(&public);
    let context = PlaybackReceiptV2Aad {
        binding: BINDING.into(),
        generation: 3,
        household: HOUSEHOLD.into(),
        id: ID.into(),
        kid: kid.clone(),
        route: ROUTE.into(),
    };
    let title = json!({"type":"movie","id":550});
    let ended_at = NOW - 1_000;
    let sealed = seal_playback_receipt_v2(
        &public,
        &PlaybackReceiptV2 {
            aad: &context,
            title: &title,
            season: None,
            episode: None,
            position_seconds: 1_000,
            duration_seconds: 2_000,
            completed: false,
            ended_at,
            expires_at: ended_at + STALE_MS,
        },
        &[0x24; ESEED_LEN],
    )
    .unwrap();
    let response = json!({
        "binding": BINDING,
        "messages": [{"generation":3,"id":ID,"kid":kid,"sealed":sealed,"seq":1}],
        "route": ROUTE,
        "v": 2,
    })
    .to_string();
    let decoded = answer(
        evaluate_assistant,
        &json!({
            "op":"playback_receipt_v2_drain","response":response,"household":HOUSEHOLD,
            "route":ROUTE,"binding":BINDING,
        }),
    );
    let entry = &decoded["ok"]["messages"][0];
    assert_eq!(entry["id"], ID);
    assert!(entry["aad"]
        .as_str()
        .unwrap()
        .contains(&format!("\"id\":\"{ID}\"")));
    let wrong_lane = answer(
        evaluate_assistant,
        &json!({
            "op":"playback_receipt_v2_drain","response":response,"household":HOUSEHOLD,
            "route":"ffffffffffffffffffffffffffffffff","binding":BINDING,
        }),
    );
    assert_eq!(wrong_lane["error"], "invalid_drain");
    let mut empty_sealed: Value = serde_json::from_str(&response).unwrap();
    empty_sealed["messages"][0]["sealed"] = json!("");
    let empty_sealed = answer(
        evaluate_assistant,
        &json!({
            "op":"playback_receipt_v2_drain","response":empty_sealed.to_string(),
            "household":HOUSEHOLD,"route":ROUTE,"binding":BINDING,
        }),
    );
    assert_eq!(empty_sealed["error"], "invalid_drain");

    let opened = answer(
        evaluate_assistant,
        &json!({
            "op":"playback_receipt_v2_open",
            "sealed": entry["sealed"],
            "secret": b64url(&secret),
            "household": HOUSEHOLD,
            "route": ROUTE,
            "binding": BINDING,
            "generation": entry["generation"],
            "kid": entry["kid"],
            "id": entry["id"],
            "applied": [],
            "device": DEVICE,
            "now": NOW,
        }),
    );
    assert_eq!(opened["ok"]["accept"]["route"], ROUTE);
    assert_eq!(opened["ok"]["accept"]["id"], ID);
    assert_eq!(
        opened["ok"]["accept"]["write"]["at"],
        json!([ended_at, 0, DEVICE])
    );
    assert_eq!(opened["ok"]["accept"]["setting"], ID);

    let replay = answer(
        evaluate_assistant,
        &json!({
            "op":"playback_receipt_v2_open","sealed":entry["sealed"],"secret":b64url(&secret),
            "household":HOUSEHOLD,"route":ROUTE,"binding":BINDING,"generation":3,
            "kid":entry["kid"],"id":ID,"applied":[ID],"device":DEVICE,"now":NOW,
        }),
    );
    assert_eq!(replay["ok"], json!({"reject":"replay"}));
}

#[test]
fn exact_integer_rejection_happens_before_both_native_and_web_coercion() {
    let canonical = format!(
        "{{\"op\":\"playback_receipt_v2_open\",\"sealed\":\"AA\",\"secret\":\"{}\",\"household\":\"{}\",\"route\":\"{}\",\"binding\":\"{}\",\"generation\":1,\"kid\":\"00000000000000000000000000000000\",\"id\":\"{}\",\"applied\":[],\"device\":\"{}\",\"now\":{}}}",
        b64url(&[0; 32]), HOUSEHOLD, ROUTE, BINDING, ID, DEVICE, NOW
    );
    for replacement in ["1.0", "1e0", "-0", "true", "9007199254740992"] {
        let request = canonical.replacen(
            "\"generation\":1",
            &format!("\"generation\":{replacement}"),
            1,
        );
        let native: Value = serde_json::from_str(&evaluate(&request)).unwrap();
        let web: Value = serde_json::from_str(&evaluate_assistant(&request)).unwrap();
        assert_eq!(native["error"], "invalid_request", "{replacement}");
        assert_eq!(web["error"], "invalid_request", "{replacement}");
    }
    let unknown = json!({
        "op":"playback_receipt_v2_authority","role":"owner","random":"00".repeat(48),"extra":true,
    });
    assert_eq!(answer(evaluate, &unknown)["error"], "invalid_request");
    assert_eq!(
        answer(evaluate_assistant, &unknown)["error"],
        "invalid_request"
    );
    assert_eq!(answer(evaluate_core, &unknown)["error"], "assistant_module");
}

#[test]
fn nested_duplicate_operation_members_fail_closed_before_value_collapse() {
    let route = playback_receipt_v2_route(&[0; 32]);
    let canonical = format!(
        "{{\"op\":\"playback_receipt_v2_proof\",\"authority\":\"{}\",\"operation\":{{\"binding\":\"{}\",\"generation\":1,\"household\":\"{}\",\"issuedAt\":{},\"nonce\":\"01010101010101010101010101010101\",\"op\":\"activate\",\"route\":\"{}\",\"v\":2}}}}",
        b64url(&[0; 32]), BINDING, HOUSEHOLD, NOW, route,
    );
    let request = format!(
        "{{\"op\":\"playback_receipt_v2_proof\",\"authority\":\"{}\",\"operation\":{{\"binding\":\"{}\",\"generation\":1,\"household\":\"{}\",\"issuedAt\":{},\"nonce\":\"01010101010101010101010101010101\",\"op\":\"activate\",\"route\":\"{}\",\"\\u0072oute\":\"{}\",\"v\":2}}}}",
        b64url(&[0; 32]), BINDING, HOUSEHOLD, NOW, route, route,
    );
    for call in [evaluate as fn(&str) -> String, evaluate_assistant] {
        let answer: Value = serde_json::from_str(&call(&canonical)).unwrap();
        assert!(answer.get("ok").is_some(), "{answer}");
        let answer: Value = serde_json::from_str(&call(&request)).unwrap();
        assert_eq!(answer["error"], "invalid_request");
    }
    for call in [
        evaluate_core as fn(&str) -> String,
        evaluate_startup,
        evaluate_deferred,
    ] {
        let answer: Value = serde_json::from_str(&call(&canonical)).unwrap();
        assert_eq!(answer["error"], "assistant_module");
        let answer: Value = serde_json::from_str(&call(&request)).unwrap();
        assert_eq!(answer["error"], "invalid_request");
    }
}

#[test]
fn authority_seed_is_rejected_safely_when_role_is_missing_or_malformed() {
    for role in [None, Some("null"), Some("{}"), Some("17")] {
        let role = role.map_or(String::new(), |value| format!(",\"role\":{value}"));
        let request = format!(
            "{{\"op\":\"playback_receipt_v2_authority\",\"random\":\"{}\"{role}}}",
            "42".repeat(48),
        );
        for call in [evaluate as fn(&str) -> String, evaluate_assistant] {
            let answer: Value = serde_json::from_str(&call(&request)).unwrap();
            assert_eq!(answer["error"], "invalid_request", "{request}");
        }
    }
}
