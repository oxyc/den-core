//! den-spec's `vectors/assistant-v1.json` `fixed` outputs, through this crate's API as den-mcp and den-edge call it.
//! The `cases` are den-core `evaluate` requests, replayed by den-sync's `tests/assistant.rs`.

use den_assistant::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn vectors() -> Option<Value> {
    let dir = match std::env::var("DEN_SPEC_DIR") {
        Ok(dir) => PathBuf::from(dir).join("vectors"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../den-spec/vectors"),
    };
    match std::fs::read_to_string(dir.join("assistant-v1.json")) {
        Ok(text) => Some(serde_json::from_str(&text).unwrap()),
        Err(_) if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") => {
            eprintln!("SKIP: den-spec absent and DEN_SPEC_OPTIONAL=1");
            None
        }
        Err(_) => panic!(
            "den-spec/vectors/assistant-v1.json not found — check out den-spec beside this repo, set DEN_SPEC_DIR, \
             or set DEN_SPEC_OPTIONAL=1 to skip deliberately."
        ),
    }
}

fn unhex(text: &Value) -> Vec<u8> {
    let text = text.as_str().unwrap();
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn seed(key: &Value) -> [u8; 32] {
    unhex(&key["seed"]).try_into().unwrap()
}

#[test]
fn den_spec_assistant_v1_fixed() {
    let Some(v) = vectors() else { return };
    assert_eq!(
        v["suite"],
        json!({"mode": 0, "kem": 0x647a, "kdf": 1, "aead": 2, "requestInfo": "den/assistant/v1",
            "tokenInfo": "den/assistant/token/v1", "wrapInfo": "den/assistant/wrap/v1",
            "signContext": "den/assistant/sig/v1\u{0}"})
    );
    let f = &v["fixed"];
    let library = f["library"].as_str().unwrap();
    let now = f["now"].as_u64().unwrap();
    for name in ["dropbox", "dropbox2", "stranger", "mcp"] {
        let key = &f["keys"][name];
        let public = kem_public(&seed(key));
        assert_eq!(b64url(&public), key["public"], "{name}");
        assert_eq!(key_id(&public), key["kid"], "{name}");
    }
    for name in ["grant", "limited", "revoked", "other"] {
        let key = &f["keys"][name];
        let grant = GrantKey::from_secret(&seed(key));
        assert_eq!(b64url(&grant.public()), key["public"], "{name}");
        assert_eq!(grant.id(), key["id"], "{name}");
    }

    // The request: den-mcp's seal reproduces it, and a device holding the drop-box key accepts it.
    let r = &f["request"];
    let fields = &r["fields"];
    let grant = GrantKey::from_secret(&seed(&f["keys"]["grant"]));
    let request = Request {
        library: fields["library"].as_str().unwrap(),
        grant: fields["grant"].as_str().unwrap(),
        id: fields["id"].as_str().unwrap(),
        at: fields["at"].as_u64().unwrap(),
        op: fields["op"].as_str().unwrap(),
        args: &fields["args"],
    };
    let msg = message(&request).unwrap();
    assert_eq!(String::from_utf8(msg.clone()).unwrap(), r["message"]);
    assert_eq!(signed_bytes(&msg), unhex(&r["signed"]));
    assert_eq!(sign(grant.secret(), &msg).to_vec(), unhex(&r["signature"]));
    let dropbox_public = kem_public(&seed(&f["keys"]["dropbox"]));
    let sealed = seal_request(&dropbox_public, &grant, &request, &unhex(&r["eseed"])).unwrap();
    assert_eq!(sealed, r["sealed"]);
    assert_eq!(sealed.len() as u64, r["sealedLength"].as_u64().unwrap());
    let grants = BTreeMap::from([(
        grant.id().to_owned(),
        Grant {
            public: grant.public(),
            ops: vec!["seen".into()],
            cap: 1,
            revoked: false,
            expires: now + 1,
        },
    )]);
    let accepted = check(
        &[seed(&f["keys"]["dropbox"])],
        library,
        &sealed,
        &grants,
        &BTreeMap::new(),
        now,
    )
    .unwrap();
    assert_eq!(
        (accepted.op.as_str(), &accepted.args),
        ("seen", &fields["args"])
    );
    assert_eq!(
        check(
            &[seed(&f["keys"]["stranger"])],
            library,
            &sealed,
            &grants,
            &BTreeMap::new(),
            now
        ),
        Err(Reject::DoesNotOpen)
    );

    // The token claim: den-edge's seal reproduces it; den-mcp opens it only for the token's own `sub`.
    let c = &f["claim"];
    let sub = c["sub"].as_str().unwrap();
    let mcp = seed(&f["keys"]["mcp"]);
    let claim = seal_claim(&kem_public(&mcp), sub, &grant, &unhex(&c["eseed"])).unwrap();
    assert_eq!(claim, c["claim"]);
    let opened = open_claim(&mcp, sub, &claim).unwrap();
    assert_eq!((opened.id(), opened.secret()), (grant.id(), grant.secret()));
    assert_eq!(c["grant"], grant.id());
    assert!(open_claim(&mcp, "00000000000000000000000000000000", &claim).is_err());

    // The wrap at rest: under the refresh secret's bytes and the session id, opened by nothing else.
    let w = &f["wrap"];
    let secret = unhex(&w["refreshSecret"]);
    let session = w["session"].as_str().unwrap();
    let token = w["refreshToken"].as_str().unwrap();
    let (sid, encoded) = token.split_once('.').unwrap();
    assert_eq!(
        (sid, b64url_decode(encoded).unwrap()),
        (session, secret.clone())
    );
    assert_eq!(wrap_key(&secret, session).to_vec(), unhex(&w["key"]));
    let nonce: [u8; 12] = unhex(&w["nonce"]).try_into().unwrap();
    let wrapped = wrap(&secret, session, &grant, &nonce);
    assert_eq!(wrapped, w["wrapped"]);
    assert_eq!(unwrap(&secret, session, &wrapped).unwrap().id(), grant.id());
    assert!(unwrap(&[0; 32], session, &wrapped).is_err());
    assert!(unwrap(&secret, "00000000000000000000000000000000", &wrapped).is_err());
    // A v1 wrap holds no read key.
    assert!(unwrap(&secret, session, &wrapped)
        .unwrap()
        .read_key()
        .is_none());

    // Reads (§15): the same grant with a read key — a `dr` claim, an 80-byte wrap, and a projection.
    let r = &f["read"];
    let read_key: [u8; 32] = b64url_decode(r["readKey"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let reader = GrantKey::from_secret(&seed(&f["keys"]["grant"])).with_read_key(&read_key);
    let claim = seal_read_claim(
        &kem_public(&mcp),
        sub,
        &reader,
        &unhex(&r["claim"]["eseed"]),
    )
    .unwrap();
    assert_eq!(claim, r["claim"]["claim"]);
    let opened = open_read_claim(&mcp, sub, &claim).unwrap();
    assert_eq!((opened.grant(), opened.key()), (reader.id(), &read_key));
    assert!(open_read_claim(&mcp, "00000000000000000000000000000000", &claim).is_err());
    // `dr` and `dw` are not interchangeable.
    assert!(open_claim(&mcp, sub, &claim).is_err());
    assert!(open_read_claim(&mcp, sub, c["claim"].as_str().unwrap()).is_err());
    let read_nonce: [u8; 12] = unhex(&r["wrap"]["nonce"]).try_into().unwrap();
    let read_wrapped = wrap(&secret, session, &reader, &read_nonce);
    assert_eq!(read_wrapped, r["wrap"]["wrapped"]);
    let back = unwrap(&secret, session, &read_wrapped).unwrap();
    assert_eq!((back.id(), back.read_key()), (reader.id(), Some(&read_key)));
    // A grant key with no read key seals no `dr`.
    assert_eq!(
        seal_read_claim(&kem_public(&mcp), sub, &grant, &[0; 64]).err(),
        Some(Error::InvalidRequest)
    );
    let p = &r["projection"];
    assert_eq!(
        String::from_utf8(projection_plaintext(&p["view"]).unwrap()).unwrap(),
        p["plaintext"]
    );
    let random: [u8; 32] = unhex(&p["random"]).try_into().unwrap();
    let sealed = seal_projection(&read_key, &p["view"], &random).unwrap();
    let parts: Vec<String> = serde_json::from_value(p["parts"].clone()).unwrap();
    assert_eq!(
        (sealed.set.as_str(), &sealed.parts),
        (p["set"].as_str().unwrap(), &parts)
    );
    assert_eq!(
        projection_aad(library, grant.id(), &sealed.set, 0, parts.len()),
        unhex(&p["aad0"])
    );
    assert_eq!(
        open_projection(&read_key, library, grant.id(), &sealed.set, &parts).unwrap(),
        p["view"]
    );
    assert!(open_projection(&[0; 32], library, grant.id(), &sealed.set, &parts).is_err());
    // The compact open gives back exactly the vector's compact plaintext.
    assert_eq!(
        &*open_projection_compact(&read_key, library, grant.id(), &sealed.set, &parts).unwrap(),
        p["plaintext"].as_str().unwrap().as_bytes()
    );
    assert!(open_projection_compact(&[0; 32], library, grant.id(), &sealed.set, &parts).is_err());
}

/// The inflate limit (§15): a part set that inflates past 64 MiB does not open, however small it is sealed.
#[test]
fn a_decompression_bomb_does_not_open() {
    let key = [5u8; 32];
    let library = "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e";
    let grant = "d7047d7faa4c6f77e1919c132bb6bd9f";
    let set = "00112233445566778899aabbccddeeff";
    let bomb = vec![b' '; MAX_PROJECTION_PLAINTEXT + 1];
    let deflated = miniz_oxide::deflate::compress_to_vec(&bomb, 9);
    assert!(deflated.len() < PART_BYTES);
    use aes_gcm::aead::{Aead, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit};
    let nonce = [1u8; 12];
    let ct = Aes256Gcm::new((&key).into())
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: &deflated,
                aad: &projection_aad(library, grant, set, 0, 1),
            },
        )
        .unwrap();
    let part = b64url(&[&nonce[..], &ct].concat());
    assert!(open_projection(&key, library, grant, set, &[part]).is_err());
}
