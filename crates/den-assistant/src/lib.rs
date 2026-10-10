//! Assistant writes (den-spec `wire/assistant-v1.md`): a request from an assistant to change a library, signed by the
//! grant the household gave that assistant and sealed so that only a device holding the library opens it, and the
//! rules a device checks before acting on one. Also the two forms den-edge keeps a grant's private key in: sealed into
//! an access token for den-mcp, and wrapped at rest under the session's refresh secret.
//!
//! Pure, like den-sync: no clock, storage, network or randomness. Every random byte (key seeds, request ids, the
//! encapsulation randomness, nonces) and the time arrive as inputs, so one set of inputs always gives one output and
//! the vectors pin every byte.
//!
//! Primitives, none of them implemented here: HPKE (RFC 9180, base mode) with the X-Wing KEM (0x647a,
//! draft-connolly-cfrg-xwing-kem), HKDF-SHA256 and AES-256-GCM, from `hpke`; Ed25519 from `ed25519-dalek`
//! (`verify_strict`); AES-256-GCM from `aes-gcm`; HKDF from `hkdf`.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::XWing;
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::convert::Infallible;
use zeroize::Zeroizing;

/// HPKE `info` of a request sealed to a library's drop-box key (§4).
pub const REQUEST_INFO: &[u8] = b"den/assistant/v1";
/// HPKE `info` of the grant key sealed into an access token for den-mcp (§7).
pub const TOKEN_INFO: &[u8] = b"den/assistant/token/v1";
/// HKDF `info` of the key a grant key is wrapped under at rest (§8).
pub const WRAP_INFO: &[u8] = b"den/assistant/wrap/v1";
/// What a grant signs before a request's message bytes (§4): this, then a zero byte.
pub const SIGN_CONTEXT: &[u8] = b"den/assistant/sig/v1\0";

/// An X-Wing private key (a drop-box key, den-mcp's token key): 32 bytes, X-Wing's seed.
pub const KEM_SECRET_LEN: usize = 32;
/// An X-Wing public key.
pub const KEM_PUBLIC_LEN: usize = 1216;
/// HPKE's encapsulated key for X-Wing, the first bytes of every sealed request and claim.
pub const ENC_LEN: usize = 1120;
/// The randomness one X-Wing encapsulation takes: X-Wing's `eseed`.
pub const ESEED_LEN: usize = 64;
/// AES-256-GCM's tag.
const TAG_LEN: usize = 16;
/// An Ed25519 signature.
pub const SIG_LEN: usize = 64;
/// The longest message a request may carry, in bytes.
pub const MAX_MESSAGE: usize = 1024;
/// den-edge's cap on one queued entry, in characters of base64url.
pub const MAX_SEALED: usize = 4096;
/// A grant key as a claim or a wrap holds it: the grant id's 16 bytes, then the Ed25519 seed.
const GRANT_BLOB_LEN: usize = 48;

const DAY_MS: u64 = 86_400_000;
/// A request older than this is `stale` (§6).
pub const STALE_MS: u64 = 7 * DAY_MS;
/// A request further ahead than this is `from_future` (§6).
pub const FUTURE_MS: u64 = 5 * 60_000;
/// The window the daily cap counts over (§6).
pub const CAP_WINDOW_MS: u64 = DAY_MS;
/// An applied entry may be pruned once both its request's `at` and its `applied` are older than this (§5): twice the
/// stale window, so a skewed clock on either side never drops an entry that still guards a request.
pub const PRUNE_MS: u64 = 14 * DAY_MS;
/// How long a grant lasts from consent or its last renewal (§5): den-edge's idle limit for a session.
pub const GRANT_TTL_MS: u64 = 30 * DAY_MS;
/// The largest daily cap a grant may carry.
pub const MAX_CAP: u64 = 1000;
/// JavaScript's largest safe integer, the bound on every number on the wire.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// The highest episode number (library v4 §3).
pub const MAX_EPISODE: u64 = 99_999;

/// The ops a request may carry, in byte order.
pub const OPS: [&str; 4] = ["rate", "seen", "watchlist_add", "watchlist_remove"];

/// Why building, sealing or opening something failed. Never a reason a device gives for a request: that is
/// [`Reject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A key of the wrong length, or an X-Wing public key that does not decode.
    InvalidKey,
    /// A request that would not pass §4's rules, or that names another grant than the key signing it.
    InvalidRequest,
    /// Randomness of the wrong length.
    InvalidRandomness,
    /// A claim or a wrap that is not base64url, does not open, or holds no grant key.
    DoesNotOpen,
}

impl Error {
    pub fn as_str(self) -> &'static str {
        match self {
            Error::InvalidKey => "invalid_key",
            Error::InvalidRequest => "invalid_request",
            Error::InvalidRandomness => "invalid_randomness",
            Error::DoesNotOpen => "does_not_open",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for Error {}

/// Why a device does not act on a request (§6), in the order the checks run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    DoesNotOpen,
    Malformed,
    UnknownGrant,
    BadSignature,
    WrongLibrary,
    Revoked,
    Expired,
    FromFuture,
    Stale,
    Replay,
    OpNotAllowed,
    OverDailyCap,
}

impl Reject {
    pub fn as_str(self) -> &'static str {
        match self {
            Reject::DoesNotOpen => "does_not_open",
            Reject::Malformed => "malformed",
            Reject::UnknownGrant => "unknown_grant",
            Reject::BadSignature => "bad_signature",
            Reject::WrongLibrary => "wrong_library",
            Reject::Revoked => "revoked",
            Reject::Expired => "expired",
            Reject::FromFuture => "from_future",
            Reject::Stale => "stale",
            Reject::Replay => "replay",
            Reject::OpNotAllowed => "op_not_allowed",
            Reject::OverDailyCap => "over_daily_cap",
        }
    }
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---- encodings

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Unpadded base64url (RFC 4648 §5).
pub fn b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Strict unpadded base64url: no padding, no other alphabet, and no bits set past the last byte, so every byte string
/// has exactly one spelling.
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for b in text.bytes() {
        acc = acc << 6 | B64.iter().position(|c| *c == b)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (acc == 0).then_some(out)
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn lower_hex_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ---- randomness as input

/// Hands HPKE exactly the bytes it was given, and notes whether it was asked for a different amount.
struct Fixed<'a> {
    bytes: &'a [u8],
    used: usize,
    short: bool,
}

impl rand_core::TryRng for Fixed<'_> {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        rand_core::utils::next_word_via_fill(self)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        rand_core::utils::next_word_via_fill(self)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Infallible> {
        let end = self.used + dest.len();
        if end > self.bytes.len() {
            self.short = true;
            dest.fill(0);
        } else {
            dest.copy_from_slice(&self.bytes[self.used..end]);
        }
        self.used = end;
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Fixed<'_> {}

// ---- keys

/// The X-Wing public key of a private key (a drop-box key, den-mcp's token key). The private key is X-Wing's 32-byte
/// seed, so a key made from 32 random bytes is the bytes themselves.
pub fn kem_public(secret: &[u8; KEM_SECRET_LEN]) -> Vec<u8> {
    let sk = <XWing as Kem>::PrivateKey::from_bytes(secret).expect("32 bytes are an X-Wing key");
    XWing::sk_to_pk(&sk).to_bytes().to_vec()
}

/// A drop-box key's id: lowercase hex of the first 16 bytes of SHA-256 of its public key.
pub fn key_id(public: &[u8]) -> String {
    hex(&Sha256::digest(public)[..16])
}

/// A grant's Ed25519 public key from its private key (the 32-byte seed).
pub fn grant_public(secret: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(secret).verifying_key().to_bytes()
}

/// A grant's id: lowercase hex of the first 16 bytes of SHA-256 of its public key.
pub fn grant_id(public: &[u8; 32]) -> String {
    hex(&Sha256::digest(public)[..16])
}

/// A grant's private key with its id, as den-edge and den-mcp hold it. Wiped when dropped.
pub struct GrantKey {
    id: String,
    secret: Zeroizing<[u8; 32]>,
}

impl GrantKey {
    pub fn from_secret(secret: &[u8; 32]) -> GrantKey {
        GrantKey {
            id: grant_id(&grant_public(secret)),
            secret: Zeroizing::new(*secret),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    pub fn public(&self) -> [u8; 32] {
        grant_public(&self.secret)
    }

    /// The 48 bytes a claim or a wrap holds: the id's 16 bytes, then the seed.
    fn to_blob(&self) -> Zeroizing<[u8; GRANT_BLOB_LEN]> {
        let mut out = Zeroizing::new([0u8; GRANT_BLOB_LEN]);
        let id = unhex16(&self.id).expect("a grant id is 16 bytes of hex");
        out[..16].copy_from_slice(&id);
        out[16..].copy_from_slice(&*self.secret);
        out
    }

    /// A blob whose id is not its seed's is no grant key.
    fn from_blob(blob: &[u8]) -> Option<GrantKey> {
        if blob.len() != GRANT_BLOB_LEN {
            return None;
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(&blob[16..]);
        let key = GrantKey::from_secret(&seed);
        (hex(&blob[..16]) == key.id).then_some(key)
    }
}

fn unhex16(text: &str) -> Option<[u8; 16]> {
    if !lower_hex_id(text) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

// ---- HPKE

/// HPKE base mode, X-Wing / HKDF-SHA256 / AES-256-GCM: `enc ‖ ct`.
pub fn hpke_seal(
    public: &[u8],
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
    eseed: &[u8],
) -> Result<Vec<u8>, Error> {
    if eseed.len() != ESEED_LEN {
        return Err(Error::InvalidRandomness);
    }
    let pk = <XWing as Kem>::PublicKey::from_bytes(public).map_err(|_| Error::InvalidKey)?;
    let mut rng = Fixed {
        bytes: eseed,
        used: 0,
        short: false,
    };
    let (enc, ct) = hpke::single_shot_seal_with_rng::<AesGcm256, HkdfSha256, XWing>(
        &OpModeS::Base,
        &pk,
        info,
        plaintext,
        aad,
        &mut rng,
    )
    .map_err(|_| Error::InvalidKey)?;
    // X-Wing takes its 64 bytes in one draw; anything else means the randomness was not what the vectors pin.
    if rng.short || rng.used != ESEED_LEN {
        return Err(Error::InvalidRandomness);
    }
    let mut out = enc.to_bytes().to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Opens `enc ‖ ct` under one X-Wing private key, or `None`.
pub fn hpke_open(
    secret: &[u8; KEM_SECRET_LEN],
    info: &[u8],
    aad: &[u8],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() < ENC_LEN + TAG_LEN {
        return None;
    }
    let sk = <XWing as Kem>::PrivateKey::from_bytes(secret).ok()?;
    let enc = <XWing as Kem>::EncappedKey::from_bytes(&sealed[..ENC_LEN]).ok()?;
    hpke::single_shot_open::<AesGcm256, HkdfSha256, XWing>(
        &OpModeR::Base,
        &sk,
        &enc,
        info,
        &sealed[ENC_LEN..],
        aad,
    )
    .ok()
}

// ---- requests

/// A request's fields (§4). `args` is the op's own object.
pub struct Request<'a> {
    pub library: &'a str,
    pub grant: &'a str,
    pub id: &'a str,
    pub at: u64,
    pub op: &'a str,
    pub args: &'a Value,
}

/// A request's message as §4 checks it.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub library: String,
    pub grant: String,
    pub id: String,
    pub at: u64,
    pub op: String,
    pub args: Value,
}

/// The message bytes of a request: the JCS (RFC 8785) of its object, which every member of a valid message keeps
/// ASCII and integral, so it is serde_json's sorted, compact output. Refused when the request breaks §4.
pub fn message(request: &Request) -> Result<Vec<u8>, Error> {
    let bytes = serde_json::to_vec(&json!({
        "v": 1,
        "library": request.library,
        "grant": request.grant,
        "id": request.id,
        "at": request.at,
        "op": request.op,
        "args": request.args,
    }))
    .map_err(|_| Error::InvalidRequest)?;
    parse_message(&bytes).ok_or(Error::InvalidRequest)?;
    Ok(bytes)
}

/// The bytes a grant signs: [`SIGN_CONTEXT`], then the message.
pub fn signed_bytes(message: &[u8]) -> Vec<u8> {
    [SIGN_CONTEXT, message].concat()
}

/// Ed25519 over [`signed_bytes`].
pub fn sign(secret: &[u8; 32], message: &[u8]) -> [u8; SIG_LEN] {
    SigningKey::from_bytes(secret)
        .sign(&signed_bytes(message))
        .to_bytes()
}

/// A request, signed with `grant` and sealed to the drop-box `public` key with the library id as the additional
/// data: unpadded base64url of `enc ‖ ct`, where the plaintext is `signature ‖ message`. The `eseed` is an argument
/// for the vectors; production code calls [`seal_request_with_rng`].
#[doc(hidden)]
pub fn seal_request(
    public: &[u8],
    grant: &GrantKey,
    request: &Request,
    eseed: &[u8],
) -> Result<String, Error> {
    if request.grant != grant.id() {
        return Err(Error::InvalidRequest);
    }
    let message = message(request)?;
    let plaintext = [&sign(grant.secret(), &message)[..], &message].concat();
    let sealed = b64url(&hpke_seal(
        public,
        REQUEST_INFO,
        request.library.as_bytes(),
        &plaintext,
        eseed,
    )?);
    // Cannot happen under MAX_MESSAGE (at most 2,966 characters), kept so a change to either bound fails here.
    if sealed.len() > MAX_SEALED {
        return Err(Error::InvalidRequest);
    }
    Ok(sealed)
}

fn keys_are(object: &Map<String, Value>, required: &[&str], optional: &[&str]) -> bool {
    required.iter().all(|k| object.contains_key(*k))
        && object
            .keys()
            .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
}

fn safe(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= MAX_SAFE_INTEGER)
}

/// `{"type": "movie" | "tv", "id": <TMDB id>}` (library v2 §3) → its type.
fn title(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    if !keys_are(object, &["type", "id"], &[]) || safe(&object["id"]).is_none_or(|id| id == 0) {
        return None;
    }
    object["type"]
        .as_str()
        .filter(|t| matches!(*t, "movie" | "tv"))
}

/// §4 *Message*: whether `args` is a valid argument object for `op`.
fn valid_args(op: &str, args: &Value) -> bool {
    let Some(object) = args.as_object() else {
        return false;
    };
    let Some(media) = object.get("title").and_then(title) else {
        return false;
    };
    match op {
        "watchlist_add" | "watchlist_remove" => keys_are(object, &["title"], &[]),
        "rate" => {
            keys_are(object, &["title", "value"], &[])
                && (object["value"].is_null()
                    || matches!(object["value"].as_str(), Some("dislike" | "like" | "love")))
        }
        "seen" => {
            if !keys_are(object, &["title", "value"], &["season", "episode"])
                || !object["value"].is_boolean()
            {
                return false;
            }
            let season = object.get("season");
            let episode = object.get("episode");
            if (season.is_some() || episode.is_some()) && media != "tv" {
                return false;
            }
            if season.is_some_and(|s| safe(s).is_none()) {
                return false;
            }
            match episode {
                None => true,
                Some(e) => season.is_some() && safe(e).is_some_and(|e| e <= MAX_EPISODE),
            }
        }
        _ => false,
    }
}

/// §4: the message bytes, checked. `None` for anything that is not exactly a valid message's JCS.
pub fn parse_message(bytes: &[u8]) -> Option<Message> {
    if bytes.len() > MAX_MESSAGE {
        return None;
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    // Equal to its own canonical form: sorted keys, no whitespace, no duplicate members, integers as integers.
    if serde_json::to_vec(&value).ok()? != bytes {
        return None;
    }
    let object = value.as_object()?;
    if !keys_are(
        object,
        &["v", "library", "grant", "id", "at", "op", "args"],
        &[],
    ) || object["v"] != json!(1)
    {
        return None;
    }
    let text = |key: &str| object[key].as_str().filter(|s| lower_hex_id(s));
    let (library, grant, id) = (text("library")?, text("grant")?, text("id")?);
    let at = safe(&object["at"])?;
    let op = object["op"].as_str().filter(|op| OPS.contains(op))?;
    if !valid_args(op, &object["args"]) {
        return None;
    }
    Some(Message {
        library: library.to_owned(),
        grant: grant.to_owned(),
        id: id.to_owned(),
        at,
        op: op.to_owned(),
        args: object["args"].clone(),
    })
}

// ---- the accept rules

/// A grant as the library's grants row holds it (§5), read for the checks. `revoked` is the row's `revokedAt` or the
/// device's own record of a revocation (§6).
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub public: [u8; 32],
    pub ops: Vec<String>,
    pub cap: u64,
    pub revoked: bool,
    /// `expiresAt`: from this time on, the grant accepts nothing.
    pub expires: u64,
}

/// An applied request as the library's applied row holds it (§5): its grant, its own `at`, and when the device that
/// applied it did. A reader keeps `applied` at least `at` (§5), so neither clock alone can age an entry early.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub grant: String,
    pub at: u64,
    pub applied: u64,
}

/// A request a device acts on.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub grant: String,
    pub id: String,
    pub at: u64,
    pub op: String,
    pub args: Value,
}

/// §6: open a sealed request with any of the library's drop-box keys and check it against the grants and the applied
/// requests, at `now`.
pub fn check(
    dropbox: &[[u8; KEM_SECRET_LEN]],
    library: &str,
    sealed: &str,
    grants: &BTreeMap<String, Grant>,
    applied: &BTreeMap<String, Applied>,
    now: u64,
) -> Result<Accepted, Reject> {
    let bytes = (sealed.len() <= MAX_SEALED)
        .then(|| b64url_decode(sealed))
        .flatten()
        .ok_or(Reject::DoesNotOpen)?;
    let plaintext = dropbox
        .iter()
        .find_map(|secret| hpke_open(secret, REQUEST_INFO, library.as_bytes(), &bytes))
        .ok_or(Reject::DoesNotOpen)?;
    if plaintext.len() <= SIG_LEN {
        return Err(Reject::Malformed);
    }
    let (signature, bytes) = plaintext.split_at(SIG_LEN);
    // The grant named is read before the signature only to find the key that checks it; nothing else is trusted until
    // that key has.
    let named: Value = serde_json::from_slice(bytes).map_err(|_| Reject::Malformed)?;
    let grant_id = named["grant"].as_str().ok_or(Reject::Malformed)?;
    let grant = grants.get(grant_id).ok_or(Reject::UnknownGrant)?;
    let key = VerifyingKey::from_bytes(&grant.public).map_err(|_| Reject::BadSignature)?;
    let signature = Signature::from_bytes(signature.try_into().expect("64 bytes"));
    key.verify_strict(&signed_bytes(bytes), &signature)
        .map_err(|_| Reject::BadSignature)?;
    let message = parse_message(bytes).ok_or(Reject::Malformed)?;
    if message.library != library {
        return Err(Reject::WrongLibrary);
    }
    if grant.revoked {
        return Err(Reject::Revoked);
    }
    if now >= grant.expires {
        return Err(Reject::Expired);
    }
    if message.at > now.saturating_add(FUTURE_MS) {
        return Err(Reject::FromFuture);
    }
    if message.at < now.saturating_sub(STALE_MS) {
        return Err(Reject::Stale);
    }
    if applied.contains_key(&message.id) {
        return Err(Reject::Replay);
    }
    if !grant.ops.iter().any(|op| *op == message.op) {
        return Err(Reject::OpNotAllowed);
    }
    if applied_today(applied, &message.grant, now) >= grant.cap {
        return Err(Reject::OverDailyCap);
    }
    Ok(Accepted {
        grant: message.grant,
        id: message.id,
        at: message.at,
        op: message.op,
        args: message.args,
    })
}

/// The requests of `grant` applied in the 24 hours up to `now` (by when they were applied, not by their own `at`,
/// which the signer chooses). An entry applied "later" than `now` (another device's clock ahead) counts: the cap
/// fails closed.
pub fn applied_today(applied: &BTreeMap<String, Applied>, grant: &str, now: u64) -> u64 {
    applied
        .values()
        .filter(|a| a.grant == grant && a.applied.max(a.at) > now.saturating_sub(CAP_WINDOW_MS))
        .count() as u64
}

/// §5: the applied entries a device may drop at `now` — those whose request's `at` and whose `applied` are both
/// more than 14 days before `now`, twice the window in which §6 accepts a request at all — by id, in order.
pub fn prune(applied: &BTreeMap<String, Applied>, now: u64) -> Vec<String> {
    let cutoff = now.saturating_sub(PRUNE_MS);
    applied
        .iter()
        .filter(|(_, a)| a.at < cutoff && a.applied.max(a.at) < cutoff)
        .map(|(id, _)| id.clone())
        .collect()
}

// ---- the token claim

/// §7: a grant key sealed to den-mcp's X-Wing public key, bound to the access token's `sub`: unpadded base64url of
/// `enc ‖ ct`, the plaintext the grant id's 16 bytes and the grant's seed. For the vectors; production code calls
/// [`seal_claim_with_rng`].
#[doc(hidden)]
pub fn seal_claim(
    public: &[u8],
    sub: &str,
    grant: &GrantKey,
    eseed: &[u8],
) -> Result<String, Error> {
    Ok(b64url(&hpke_seal(
        public,
        TOKEN_INFO,
        sub.as_bytes(),
        &*grant.to_blob(),
        eseed,
    )?))
}

/// §7: den-mcp's side of [`seal_claim`].
pub fn open_claim(
    secret: &[u8; KEM_SECRET_LEN],
    sub: &str,
    claim: &str,
) -> Result<GrantKey, Error> {
    let sealed = b64url_decode(claim).ok_or(Error::DoesNotOpen)?;
    let blob = Zeroizing::new(
        hpke_open(secret, TOKEN_INFO, sub.as_bytes(), &sealed).ok_or(Error::DoesNotOpen)?,
    );
    GrantKey::from_blob(&blob).ok_or(Error::DoesNotOpen)
}

// ---- the wrap at rest

/// §8: K = HKDF-SHA256(ikm = the refresh secret's bytes, salt = the session id, info = [`WRAP_INFO`]), 32 bytes.
pub fn wrap_key(refresh_secret: &[u8], session: &str) -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(session.as_bytes()), refresh_secret)
        .expand(WRAP_INFO, &mut *key)
        .expect("32 bytes is a valid HKDF-SHA256 length");
    key
}

/// §8: the grant key under [`wrap_key`], AES-256-GCM with the session id as additional data: unpadded base64url of
/// `nonce ‖ ct ‖ tag`. For the vectors; production code calls [`wrap_with_rng`].
#[doc(hidden)]
pub fn wrap(refresh_secret: &[u8], session: &str, grant: &GrantKey, nonce: &[u8; 12]) -> String {
    let key = wrap_key(refresh_secret, session);
    let cipher = Aes256Gcm::new((&*key).into());
    let ct = cipher
        .encrypt(
            nonce.into(),
            Payload {
                msg: &*grant.to_blob(),
                aad: session.as_bytes(),
            },
        )
        .expect("AES-GCM seals 48 bytes");
    b64url(&[&nonce[..], &ct].concat())
}

// ---- the same, drawing their randomness from a CSPRNG

/// [`seal_request`] with a fresh `eseed` from `rng`. What den-mcp calls.
pub fn seal_request_with_rng(
    public: &[u8],
    grant: &GrantKey,
    request: &Request,
    rng: &mut impl rand_core::CryptoRng,
) -> Result<String, Error> {
    let mut eseed = Zeroizing::new([0u8; ESEED_LEN]);
    rng.fill_bytes(&mut *eseed);
    seal_request(public, grant, request, &*eseed)
}

/// [`seal_claim`] with a fresh `eseed` from `rng`. What den-edge calls for every access token.
pub fn seal_claim_with_rng(
    public: &[u8],
    sub: &str,
    grant: &GrantKey,
    rng: &mut impl rand_core::CryptoRng,
) -> Result<String, Error> {
    let mut eseed = Zeroizing::new([0u8; ESEED_LEN]);
    rng.fill_bytes(&mut *eseed);
    seal_claim(public, sub, grant, &*eseed)
}

/// [`wrap`] with a fresh nonce from `rng`. What den-edge calls at every exchange and refresh.
pub fn wrap_with_rng(
    refresh_secret: &[u8],
    session: &str,
    grant: &GrantKey,
    rng: &mut impl rand_core::CryptoRng,
) -> String {
    let mut nonce = [0u8; 12];
    rng.fill_bytes(&mut nonce);
    wrap(refresh_secret, session, grant, &nonce)
}

/// A request id (§4): 16 bytes from `rng`, as hex.
pub fn request_id_with_rng(rng: &mut impl rand_core::CryptoRng) -> String {
    let mut id = [0u8; 16];
    rng.fill_bytes(&mut id);
    hex(&id)
}

/// §8: den-edge's side of [`wrap`].
pub fn unwrap(refresh_secret: &[u8], session: &str, wrapped: &str) -> Result<GrantKey, Error> {
    let bytes = b64url_decode(wrapped).ok_or(Error::DoesNotOpen)?;
    if bytes.len() != 12 + GRANT_BLOB_LEN + TAG_LEN {
        return Err(Error::DoesNotOpen);
    }
    let key = wrap_key(refresh_secret, session);
    let cipher = Aes256Gcm::new((&*key).into());
    let nonce: &[u8; 12] = bytes[..12].try_into().expect("12 bytes");
    let blob = Zeroizing::new(
        cipher
            .decrypt(
                nonce.into(),
                Payload {
                    msg: &bytes[12..],
                    aad: session.as_bytes(),
                },
            )
            .map_err(|_| Error::DoesNotOpen)?,
    );
    GrantKey::from_blob(&blob).ok_or(Error::DoesNotOpen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_is_strict() {
        for bytes in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            &[0xff, 0xfe, 0xfd][..],
        ] {
            assert_eq!(b64url_decode(&b64url(bytes)).unwrap(), bytes);
        }
        // Padding, the standard alphabet, a lone character, and set bits past the last byte.
        for bad in ["Zg==", "+/8", "Z", "Zh"] {
            assert!(b64url_decode(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_grant_blob_must_name_its_own_key() {
        let key = GrantKey::from_secret(&[7; 32]);
        let mut blob = *key.to_blob();
        assert_eq!(GrantKey::from_blob(&blob).unwrap().id(), key.id());
        blob[0] ^= 1;
        assert!(GrantKey::from_blob(&blob).is_none());
    }

    #[test]
    fn randomness_must_be_exactly_an_eseed() {
        let public = kem_public(&[1; 32]);
        for len in [0, 63, 65] {
            assert_eq!(
                hpke_seal(&public, REQUEST_INFO, b"", b"x", &vec![0; len]).unwrap_err(),
                Error::InvalidRandomness
            );
        }
    }
}
