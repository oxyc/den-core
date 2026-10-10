//! The message encoding and the signature, seal and claim checks at their edges, through this crate's API.

use den_assistant::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::convert::Infallible;

const LIBRARY: &str = "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e";
const ID: &str = "00112233445566778899aabbccddeeff";
const NOW: u64 = 1_790_000_000_000;
const DROPBOX: [u8; 32] = [3; 32];
const SEED: [u8; 32] = [9; 32];

/// A counter, standing in for a CSPRNG where only distinct draws matter.
struct Counter(u8);

impl rand_core::TryRng for Counter {
    type Error = Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        rand_core::utils::next_word_via_fill(self)
    }
    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        rand_core::utils::next_word_via_fill(self)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Infallible> {
        for b in dest {
            self.0 = self.0.wrapping_add(1);
            *b = self.0;
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Counter {}

fn args() -> Value {
    json!({"title": {"type": "movie", "id": 550}})
}

fn request<'a>(grant: &'a str, args: &'a Value) -> Request<'a> {
    Request {
        library: LIBRARY,
        grant,
        id: ID,
        at: NOW,
        op: "watchlist_add",
        args,
    }
}

fn grants_for(public: [u8; 32]) -> BTreeMap<String, Grant> {
    BTreeMap::from([(
        grant_id(&public),
        Grant {
            public,
            ops: vec!["watchlist_add".into()],
            cap: 10,
            revoked: false,
            expires: NOW + 86_400_000,
        },
    )])
}

/// Seals arbitrary plaintext as a request for `LIBRARY`.
fn sealed(plaintext: &[u8]) -> String {
    b64url(
        &hpke_seal(
            &kem_public(&DROPBOX),
            REQUEST_INFO,
            LIBRARY.as_bytes(),
            plaintext,
            &[5; 64],
        )
        .unwrap(),
    )
}

fn open(plaintext: &[u8], grants: &BTreeMap<String, Grant>) -> Result<Accepted, Reject> {
    check(
        &[DROPBOX],
        LIBRARY,
        &sealed(plaintext),
        grants,
        &BTreeMap::new(),
        NOW,
    )
}

#[test]
fn a_message_is_its_jcs() {
    let key = GrantKey::from_secret(&SEED);
    let args =
        json!({"title": {"type": "tv", "id": 1399}, "season": 1, "episode": 2, "value": false});
    let mut r = request(key.id(), &args);
    r.op = "seen";
    let expected = format!(
        "{{\"args\":{{\"episode\":2,\"season\":1,\"title\":{{\"id\":1399,\"type\":\"tv\"}},\"value\":false}},\
         \"at\":{NOW},\"grant\":\"{}\",\"id\":\"{ID}\",\"library\":\"{LIBRARY}\",\"op\":\"seen\",\"v\":1}}",
        key.id()
    );
    assert_eq!(String::from_utf8(message(&r).unwrap()).unwrap(), expected);
    assert!(parse_message(expected.as_bytes()).is_some());
}

#[test]
fn only_the_exact_jcs_is_a_message() {
    let key = GrantKey::from_secret(&SEED);
    let args = args();
    let good = String::from_utf8(message(&request(key.id(), &args)).unwrap()).unwrap();
    let g = key.id();
    let rest = format!("\"grant\":\"{g}\",\"id\":\"{ID}\",\"library\":\"{LIBRARY}\",\"op\":\"watchlist_add\",\"v\":1");
    let title = "\"args\":{\"title\":{\"id\":550,\"type\":\"movie\"}}";
    for bad in [
        // Compact but unsorted.
        format!(
            "{{\"v\":1,{title},\"at\":{NOW},{}}}",
            rest.trim_end_matches(",\"v\":1")
        ),
        // A duplicate member, the second one canonical.
        format!("{{{title},\"at\":1,\"at\":{NOW},{rest}}}"),
        // Numbers that are not plain integers.
        format!("{{{title},\"at\":{NOW}.0,{rest}}}"),
        format!("{{{title},\"at\":1.79e12,{rest}}}"),
        format!(
            "{{\"args\":{{\"title\":{{\"id\":550.0,\"type\":\"movie\"}}}},\"at\":{NOW},{rest}}}"
        ),
        format!("{{{title},\"at\":-0,{rest}}}"),
        format!("{{{title},\"at\":-1,{rest}}}"),
        // Whitespace.
        format!("{{{title}, \"at\":{NOW},{rest}}}"),
    ] {
        assert_ne!(bad, good);
        assert!(parse_message(bad.as_bytes()).is_none(), "{bad}");
        // And through the whole check, properly signed: malformed.
        let plaintext = [&sign(&SEED, bad.as_bytes())[..], bad.as_bytes()].concat();
        assert_eq!(
            open(&plaintext, &grants_for(key.public())),
            Err(Reject::Malformed),
            "{bad}"
        );
    }
    let plaintext = [&sign(&SEED, good.as_bytes())[..], good.as_bytes()].concat();
    assert!(open(&plaintext, &grants_for(key.public())).is_ok());
}

/// Ed25519's order ℓ, little-endian.
const L: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

#[test]
fn a_non_canonical_signature_is_refused() {
    let key = GrantKey::from_secret(&SEED);
    let args = args();
    let msg = message(&request(key.id(), &args)).unwrap();
    let mut sig = sign(&SEED, &msg);
    // S + ℓ is the same scalar, written non-canonically.
    let mut carry = 0u16;
    for i in 0..32 {
        let sum = u16::from(sig[32 + i]) + u16::from(L[i]) + carry;
        sig[32 + i] = sum as u8;
        carry = sum >> 8;
    }
    assert_eq!(carry, 0);
    let plaintext = [&sig[..], &msg].concat();
    assert_eq!(
        open(&plaintext, &grants_for(key.public())),
        Err(Reject::BadSignature)
    );
}

#[test]
fn a_small_order_grant_key_is_refused() {
    // The identity point as the key, and R = identity, S = 0: a signature on anything for a weak key.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let args = args();
    let id = grant_id(&identity);
    let msg = message(&request(&id, &args)).unwrap();
    let mut sig = [0u8; 64];
    sig[0] = 1;
    let plaintext = [&sig[..], &msg].concat();
    assert_eq!(
        open(&plaintext, &grants_for(identity)),
        Err(Reject::BadSignature)
    );
}

#[test]
fn seals_do_not_cross_infos() {
    let key = GrantKey::from_secret(&SEED);
    let args = args();
    let mcp = [4u8; 32];
    // A claim sealed with the request info, and a request's plaintext sealed with the token info.
    let as_request = b64url(
        &hpke_seal(
            &kem_public(&mcp),
            REQUEST_INFO,
            b"sub",
            &[0u8; 48],
            &[6; 64],
        )
        .unwrap(),
    );
    assert_eq!(
        open_claim(&mcp, "sub", &as_request).err(),
        Some(Error::DoesNotOpen)
    );
    let msg = message(&request(key.id(), &args)).unwrap();
    let plaintext = [&sign(&SEED, &msg)[..], &msg].concat();
    let as_token = b64url(
        &hpke_seal(
            &kem_public(&DROPBOX),
            TOKEN_INFO,
            LIBRARY.as_bytes(),
            &plaintext,
            &[7; 64],
        )
        .unwrap(),
    );
    assert_eq!(
        check(
            &[DROPBOX],
            LIBRARY,
            &as_token,
            &grants_for(key.public()),
            &BTreeMap::new(),
            NOW
        ),
        Err(Reject::DoesNotOpen)
    );
}

#[test]
fn rng_entry_points_round_trip_and_never_repeat() {
    let key = GrantKey::from_secret(&SEED);
    let args = args();
    let mut rng = Counter(0);
    let a = seal_request_with_rng(
        &kem_public(&DROPBOX),
        &key,
        &request(key.id(), &args),
        &mut rng,
    )
    .unwrap();
    let b = seal_request_with_rng(
        &kem_public(&DROPBOX),
        &key,
        &request(key.id(), &args),
        &mut rng,
    )
    .unwrap();
    assert_ne!(a, b);
    let grants = grants_for(key.public());
    assert!(check(&[DROPBOX], LIBRARY, &a, &grants, &BTreeMap::new(), NOW).is_ok());
    let mcp = [4u8; 32];
    let claim = seal_claim_with_rng(&kem_public(&mcp), "sub", &key, &mut rng).unwrap();
    assert_eq!(open_claim(&mcp, "sub", &claim).unwrap().id(), key.id());
    let w1 = wrap_with_rng(&[1; 32], "sid", &key, &mut rng);
    let w2 = wrap_with_rng(&[1; 32], "sid", &key, &mut rng);
    assert_ne!(w1, w2);
    assert_eq!(unwrap(&[1; 32], "sid", &w1).unwrap().id(), key.id());
    let id = request_id_with_rng(&mut rng);
    assert_eq!(id.len(), 32);
}

#[test]
fn expiry_is_checked_after_revocation() {
    let key = GrantKey::from_secret(&SEED);
    let args = args();
    let msg = message(&request(key.id(), &args)).unwrap();
    let plaintext = [&sign(&SEED, &msg)[..], &msg].concat();
    let mut grants = grants_for(key.public());
    grants.values_mut().for_each(|g| g.expires = NOW);
    assert_eq!(open(&plaintext, &grants), Err(Reject::Expired));
    grants.values_mut().for_each(|g| g.revoked = true);
    assert_eq!(open(&plaintext, &grants), Err(Reject::Revoked));
    grants.values_mut().for_each(|g| {
        g.revoked = false;
        g.expires = NOW + 1
    });
    assert!(open(&plaintext, &grants).is_ok());
}
