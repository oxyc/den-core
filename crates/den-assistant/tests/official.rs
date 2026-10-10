//! The primitives against their own published vectors, before any Den vector is trusted:
//!
//! - `official/xwing-kem.json`: draft-connolly-cfrg-xwing-kem's test vectors
//!   (github.com/dconnolly/draft-connolly-cfrg-xwing-kem, `spec/test-vectors.json`, as the `x-wing` crate ships them).
//! - `official/hpke-pq-xwing-hkdfsha256.json`: the HPKE-PQ reference vectors' X-Wing / HKDF-SHA256 suite
//!   (github.com/hpkewg/hpke-pq at 6433c8f, as `hpke` 0.14.1 ships them in `test-vectors/pq-6433c8f.json`). Its only
//!   published AEAD is ChaCha20-Poly1305 (0x0003); the KEM, the key schedule and the KDF are the ones Den's suite uses.

use den_assistant::{hpke_open, kem_public, ENC_LEN};
use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::XWing;
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
use serde_json::Value;
use std::convert::Infallible;
use x_wing::TryKeyInit;

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn field(value: &Value, key: &str) -> Vec<u8> {
    unhex(value[key].as_str().unwrap())
}

struct Bytes(Vec<u8>);

impl rand_core::TryRng for Bytes {
    type Error = Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        rand_core::utils::next_word_via_fill(self)
    }
    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        rand_core::utils::next_word_via_fill(self)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Infallible> {
        dest.copy_from_slice(&self.0[..dest.len()]);
        self.0.drain(..dest.len());
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Bytes {}

/// A Den key from a seed is X-Wing's key from that seed, and HPKE's X-Wing encapsulates and decapsulates as the draft
/// says.
#[test]
fn xwing_draft_vectors() {
    let vectors: Vec<Value> =
        serde_json::from_str(include_str!("official/xwing-kem.json")).unwrap();
    assert!(!vectors.is_empty());
    for v in &vectors {
        let seed: [u8; 32] = field(v, "seed").try_into().unwrap();
        assert_eq!(field(v, "sk"), seed, "X-Wing's private key is its seed");
        assert_eq!(kem_public(&seed), field(v, "pk"));
        let pk: [u8; 1216] = field(v, "pk").try_into().unwrap();
        let eseed: [u8; 64] = field(v, "eseed").try_into().unwrap();
        let (ct, ss) = x_wing::EncapsulationKey::new((&pk).into())
            .unwrap()
            .encapsulate_deterministic((&eseed).into());
        assert_eq!(&ct[..], field(v, "ct"));
        assert_eq!(&ss[..], field(v, "ss"));
        let sk = <XWing as Kem>::PrivateKey::from_bytes(&seed).unwrap();
        let enc = <XWing as Kem>::EncappedKey::from_bytes(&field(v, "ct")).unwrap();
        assert_eq!(enc.to_bytes().as_slice(), field(v, "ct"));
        assert_eq!(XWing::sk_to_pk(&sk).to_bytes().as_slice(), field(v, "pk"));
    }
}

/// HPKE base mode with X-Wing and HKDF-SHA256: the encapsulation from `ikmE`, every sealed message in sequence, and
/// opening each one.
#[test]
fn hpke_pq_xwing_hkdf_sha256_vectors() {
    let v: Value =
        serde_json::from_str(include_str!("official/hpke-pq-xwing-hkdfsha256.json")).unwrap();
    assert_eq!(
        (&v["mode"], &v["kem_id"], &v["kdf_id"], &v["aead_id"]),
        (
            &Value::from(0),
            &Value::from(0x647a),
            &Value::from(1),
            &Value::from(3)
        )
    );
    let sk_bytes: [u8; 32] = field(&v, "skRm").try_into().unwrap();
    assert_eq!(kem_public(&sk_bytes), field(&v, "pkRm"));
    let pk = <XWing as Kem>::PublicKey::from_bytes(&field(&v, "pkRm")).unwrap();
    let info = field(&v, "info");
    let mut rng = Bytes(field(&v, "ikmE"));
    let (enc, mut sender) = hpke::setup_sender_with_rng::<ChaCha20Poly1305, HkdfSha256, XWing>(
        &OpModeS::Base,
        &pk,
        &info,
        &mut rng,
    )
    .unwrap();
    assert!(rng.0.is_empty(), "X-Wing takes exactly its 64-byte eseed");
    assert_eq!(enc.to_bytes().as_slice(), field(&v, "enc"));
    let sk = <XWing as Kem>::PrivateKey::from_bytes(&sk_bytes).unwrap();
    let mut receiver = hpke::setup_receiver::<ChaCha20Poly1305, HkdfSha256, XWing>(
        &OpModeR::Base,
        &sk,
        &enc,
        &info,
    )
    .unwrap();
    for e in v["encryptions"].as_array().unwrap() {
        let ct = sender.seal(&field(e, "pt"), &field(e, "aad")).unwrap();
        assert_eq!(ct, field(e, "ct"));
        assert_eq!(
            receiver.open(&ct, &field(e, "aad")).unwrap(),
            field(e, "pt")
        );
    }
    for x in v["exports"].as_array().unwrap() {
        let mut out = vec![0u8; x["L"].as_u64().unwrap() as usize];
        sender
            .export(&field(x, "exporter_context"), &mut out)
            .unwrap();
        assert_eq!(out, field(x, "exported_value"));
    }
    // The single-shot open den-assistant uses reads the same `enc ‖ ct` framing (with its own AEAD, so a ChaCha
    // ciphertext must not open under it).
    let first = &v["encryptions"][0];
    let framed = [field(&v, "enc"), field(first, "ct")].concat();
    assert_eq!(framed.len(), ENC_LEN + field(first, "ct").len());
    assert!(hpke_open(&sk_bytes, &info, &field(first, "aad"), &framed).is_none());
}
