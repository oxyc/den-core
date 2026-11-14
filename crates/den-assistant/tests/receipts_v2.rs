//! den-spec playback-receipts-v2 vectors plus Core-owned deterministic HPKE and state-machine coverage.

use den_assistant::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;

const HOUSEHOLD: &str = "00000000000000000000000000000001";
const ROUTE: &str = "d4b68422b2b00134a42a7a585c60a5ea";
const BINDING: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ID: &str = "94619420c64002287a3ad7ecad5dfa6b";
const NOW: u64 = 1_790_000_500_000;

fn vectors() -> Option<Value> {
    let path = match std::env::var("DEN_SPEC_DIR") {
        Ok(dir) => PathBuf::from(dir).join("vectors/playback-receipts-v2.json"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../den-spec/vectors/playback-receipts-v2.json"),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(serde_json::from_str(&text).expect("den-spec playback receipts v2 JSON")),
        Err(_) if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") => {
            eprintln!("SKIP: den-spec playback-receipts-v2 absent and DEN_SPEC_OPTIONAL=1");
            None
        }
        Err(error) => panic!(
            "{}: {error} — check out current den-spec beside this repo, set DEN_SPEC_DIR, or set DEN_SPEC_OPTIONAL=1",
            path.display()
        ),
    }
}

fn decode32(text: &str) -> [u8; 32] {
    b64url_decode(text).unwrap().try_into().unwrap()
}

fn aad(kid: String) -> PlaybackReceiptV2Aad {
    PlaybackReceiptV2Aad {
        binding: BINDING.into(),
        generation: 3,
        household: HOUSEHOLD.into(),
        id: ID.into(),
        kid,
        route: ROUTE.into(),
    }
}

#[test]
fn den_spec_derivation_proof_key_aad_drain_and_integer_vectors() {
    let Some(vectors) = vectors() else { return };
    let authority = decode32(vectors["derivation"]["authority"].as_str().unwrap());
    assert_eq!(
        playback_receipt_v2_route(&authority),
        vectors["derivation"]["route"]
    );
    for case in vectors["proofs"].as_array().unwrap() {
        let operation = serde_json::to_vec(&case["operation"]).unwrap();
        let proof = case["proof"].as_str().unwrap();
        assert_eq!(
            playback_receipt_v2_proof(&authority, &operation).unwrap(),
            proof
        );
        assert!(verify_playback_receipt_v2_proof(
            &authority, &operation, proof
        ));
    }
    let mut operation = vectors["proofs"][0]["operation"].clone();
    operation["route"] = json!("ffffffffffffffffffffffffffffffff");
    assert_eq!(
        playback_receipt_v2_proof(&authority, &serde_json::to_vec(&operation).unwrap()),
        Err(Error::InvalidRequest)
    );
    operation = vectors["proofs"][0]["operation"].clone();
    operation["extra"] = json!(true);
    assert!(
        validate_playback_receipt_v2_operation(&serde_json::to_vec(&operation).unwrap()).is_none()
    );

    let public = vectors["publicKey"]["public"].as_str().unwrap();
    let kid = vectors["publicKey"]["kid"].as_str().unwrap();
    assert_eq!(
        playback_receipt_v2_public(public, kid).unwrap().len(),
        KEM_PUBLIC_LEN
    );
    assert!(playback_receipt_v2_public(&public[..public.len() - 2], kid).is_none());
    assert!(playback_receipt_v2_public(public, "00000000000000000000000000000000").is_none());

    for case in vectors["integerTokenCases"].as_array().unwrap() {
        assert_eq!(
            playback_receipt_v2_exact_integer_token(case["token"].as_str().unwrap().as_bytes()),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }

    let drain_bytes = serde_json::to_vec(&vectors["drainResponse"]).unwrap();
    let drain = parse_playback_receipt_v2_drain(&drain_bytes).unwrap();
    assert_eq!(drain.messages.len(), 2);
    let entry_aad = drain.messages[1].aad(HOUSEHOLD, &drain.route, &drain.binding);
    assert_eq!(
        String::from_utf8(entry_aad.canonical().unwrap()).unwrap(),
        vectors["envelope"]["aadJcs"]
    );

    let mut missing = vectors["drainResponse"].clone();
    missing["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("kid");
    assert!(parse_playback_receipt_v2_drain(&serde_json::to_vec(&missing).unwrap()).is_none());
    missing = vectors["drainResponse"].clone();
    missing["messages"][0].as_object_mut().unwrap().remove("id");
    assert!(parse_playback_receipt_v2_drain(&serde_json::to_vec(&missing).unwrap()).is_none());
    missing = vectors["drainResponse"].clone();
    missing["messages"][0]["sealed"] = json!("");
    assert!(parse_playback_receipt_v2_drain(&serde_json::to_vec(&missing).unwrap()).is_none());
    assert!(parse_playback_receipt_v2_drain(
        String::from_utf8(drain_bytes)
            .unwrap()
            .replacen("\"generation\":2", "\"generation\":2.0", 1)
            .as_bytes()
    )
    .is_none());
}

#[test]
fn exact_json_rejects_duplicate_members_at_every_depth() {
    assert!(playback_receipt_v2_exact_integers(
        br#"{"outer":{"a":1},"v":2}"#
    ));
    assert!(!playback_receipt_v2_exact_integers(
        br#"{"outer":{"a":1,"a":1},"v":2}"#
    ));
    assert!(!playback_receipt_v2_exact_integers(
        br#"{"items":[{"a":1,"a":1}],"v":2}"#
    ));
}

#[test]
fn den_spec_receipt_and_retirement_tag_vectors() {
    let Some(vectors) = vectors() else { return };
    let receipt = vectors["envelope"]["receiptJcs"].as_str().unwrap();
    let parsed = parse_playback_receipt_v2(receipt.as_bytes()).unwrap();
    assert_eq!(
        parsed.aad.canonical().unwrap(),
        vectors["envelope"]["aadJcs"].as_str().unwrap().as_bytes()
    );
    assert_eq!(parsed.title, json!({"id":1399,"type":"tv"}));

    let mut overlong_season: Value = serde_json::from_str(receipt).unwrap();
    overlong_season["season"] = json!(10_000);
    assert!(parse_playback_receipt_v2(&serde_json::to_vec(&overlong_season).unwrap()).is_none());

    let retirement = &vectors["retirement"];
    let tag = playback_receipt_v2_retired_tag(
        &decode32(retirement["key"].as_str().unwrap()),
        retirement["household"].as_str().unwrap(),
        retirement["route"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(hex(&tag), retirement["tag"]);
}

#[test]
fn deterministic_v2_hpke_is_single_key_route_bound_and_replay_safe() {
    let secret = [0x42; 32];
    let public = kem_public(&secret);
    let context = aad(key_id(&public));
    let title = json!({"type":"tv","id":1399});
    let receipt = PlaybackReceiptV2 {
        aad: &context,
        title: &title,
        season: Some(2),
        episode: Some(3),
        position_seconds: 2_310,
        duration_seconds: 2_400,
        completed: false,
        ended_at: NOW - 500_000,
        expires_at: NOW - 500_000 + STALE_MS,
    };
    let sealed = seal_playback_receipt_v2(&public, &receipt, &[0x24; ESEED_LEN]).unwrap();
    // A digest keeps this source fixture readable while pinning every byte of the 1,904-byte envelope.
    assert_eq!(b64url_decode(&sealed).unwrap().len(), 1_543);
    assert_eq!(
        hex(&Sha256::digest(sealed.as_bytes())),
        "c5fbeef0d947ffbffe5d122b7e4e186058dc0482181b11661c889ec5d70fbc55"
    );
    let opened =
        open_playback_receipt_v2(&secret, &context, &sealed, &BTreeSet::new(), NOW).unwrap();
    assert_eq!(opened.aad, context);

    let stranger = [0x43; 32];
    assert_eq!(
        open_playback_receipt_v2(&stranger, &context, &sealed, &BTreeSet::new(), NOW),
        Err(PlaybackReceiptV2Reject::DoesNotOpen)
    );
    let mut wrong_route = context.clone();
    wrong_route.route = "ffffffffffffffffffffffffffffffff".into();
    assert_eq!(
        open_playback_receipt_v2(&secret, &wrong_route, &sealed, &BTreeSet::new(), NOW),
        Err(PlaybackReceiptV2Reject::DoesNotOpen)
    );
    let mut applied = BTreeSet::new();
    applied.insert(ID.into());
    assert_eq!(
        open_playback_receipt_v2(&secret, &context, &sealed, &applied, NOW),
        Err(PlaybackReceiptV2Reject::Replay)
    );

    // Released v1 uses a distinct info/AAD and cannot substitute into this lane.
    let v1 = b64url(
        &hpke_seal(
            &public,
            PLAYBACK_RECEIPT_INFO,
            HOUSEHOLD.as_bytes(),
            b"{}",
            &[0x24; ESEED_LEN],
        )
        .unwrap(),
    );
    assert_eq!(
        open_playback_receipt_v2(&secret, &context, &v1, &BTreeSet::new(), NOW),
        Err(PlaybackReceiptV2Reject::DoesNotOpen)
    );
}

#[test]
fn live_tombstone_and_tag_share_one_atomic_512_slot_reservation() {
    let previous = PlaybackReceiptV2RetirementKey {
        version: 7,
        key: [0x00; 32],
    };
    let current = PlaybackReceiptV2RetirementKey {
        version: 8,
        key: [0x20; 32],
    };
    let mut history = PlaybackReceiptV2History::new(previous).unwrap();
    assert_eq!(
        history.register(HOUSEHOLD, ROUTE),
        PlaybackReceiptV2Admission::Admitted
    );
    for number in 0..PLAYBACK_RECEIPT_V2_HISTORY_CAPACITY - 1 {
        assert_eq!(
            history.register(HOUSEHOLD, &format!("{number:032x}")),
            PlaybackReceiptV2Admission::Admitted
        );
    }
    assert_eq!(history.reserved(), 512);
    history.retire(ROUTE).unwrap();
    assert_eq!(history.counts(), (511, 1, 0));
    assert_eq!(
        history.register(HOUSEHOLD, "ffffffffffffffffffffffffffffffff"),
        PlaybackReceiptV2Admission::HistoryFull
    );
    history.compact(HOUSEHOLD, ROUTE, 7).unwrap();
    assert_eq!(history.counts(), (511, 0, 1));
    assert_eq!(history.reserved(), 512);
    history.rotate_retirement_key(current).unwrap();
    assert_eq!(history.retained_versions(), vec![8, 7]);
    assert_eq!(
        history.register(HOUSEHOLD, ROUTE),
        PlaybackReceiptV2Admission::Retired
    );
    assert_eq!(
        history.rotate_retirement_key(PlaybackReceiptV2RetirementKey {
            version: 9,
            key: [0x40; 32]
        }),
        Err(PlaybackReceiptV2HistoryError::PreviousKeyInUse)
    );
    assert_eq!(history.retained_versions(), vec![8, 7]);

    let tag = playback_receipt_v2_retired_tag(&[0x00; 32], HOUSEHOLD, ROUTE).unwrap();
    let restored = PlaybackReceiptV2History::restore(
        PlaybackReceiptV2RetirementKey {
            version: 8,
            key: [0x20; 32],
        },
        Some(PlaybackReceiptV2RetirementKey {
            version: 7,
            key: [0x00; 32],
        }),
        [],
        [],
        [(7, tag)],
    )
    .unwrap();
    let mut restored = restored;
    assert_eq!(
        restored.register(HOUSEHOLD, ROUTE),
        PlaybackReceiptV2Admission::Retired
    );
    assert!(matches!(
        PlaybackReceiptV2History::restore(
            PlaybackReceiptV2RetirementKey {
                version: 8,
                key: [0x20; 32]
            },
            None,
            [],
            [],
            [(7, tag)],
        ),
        Err(PlaybackReceiptV2HistoryError::Invalid)
    ));
}

#[test]
fn visitor_and_tizen3_are_explicitly_outside_the_core_authority_constructor() {
    // Core accepts only random authority/binding material. Role policy remains a caller decision, so there is no
    // API which can synthesize Visitor or Tizen 3 authority from Owner/current-profile state.
    assert!(PlaybackReceiptV2Authority::from_random(&[0; 47]).is_err());
    let authority = PlaybackReceiptV2Authority::from_random(&[0; 48]).unwrap();
    assert_eq!(authority.authority().len(), 43);
    assert_eq!(authority.binding().len(), 32);
    assert_eq!(authority.route().len(), 32);
}
