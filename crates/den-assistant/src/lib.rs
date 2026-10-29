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
/// HPKE `info` of a managed-playback completion receipt sealed to the same household drop-box key. A distinct
/// domain means an assistant request can never be replayed as playback evidence, or the reverse.
pub const PLAYBACK_RECEIPT_INFO: &[u8] = b"den/playback-receipt/v1";
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
/// A playback receipt is deliberately smaller than an assistant request: it carries one fixed progress operation.
pub const MAX_PLAYBACK_RECEIPT_MESSAGE: usize = 768;
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

/// The ops a request may carry, in byte order: the library ops, and the TV's (§16).
pub const OPS: [&str; 6] = [
    "play",
    "play_trailer",
    "rate",
    "seen",
    "watchlist_add",
    "watchlist_remove",
];
/// The ops a v1 grant's `ops` may list, in byte order: the library ops. The TV's are allowed by a play record (§16),
/// never by a grant, so a grant stays readable by every client.
pub const GRANT_OPS: [&str; 4] = ["rate", "seen", "watchlist_add", "watchlist_remove"];
/// The TV's ops (§16).
pub const PLAY_OPS: [&str; 2] = ["play", "play_trailer"];
/// A TV op older than this is `stale` (§16).
pub const PLAY_STALE_MS: u64 = 120_000;

/// The queue a request belongs in, which its signed op decides (§16): the library's, which every device drains, or
/// the TV's, which only a TV drains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    Library,
    Tv,
}

impl Queue {
    /// The queue of a request with this op.
    pub fn of(op: &str) -> Queue {
        if PLAY_OPS.contains(&op) {
            Queue::Tv
        } else {
            Queue::Library
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Queue::Library => "library",
            Queue::Tv => "tv",
        }
    }
}

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
    WrongQueue,
    Revoked,
    Expired,
    FromFuture,
    Stale,
    Replay,
    OpNotAllowed,
    OverDailyCap,
}

/// Why a device refuses a managed-playback receipt. Unlike an assistant request, a receipt has no grant: its
/// authority is the authenticated, session-bound Hearth callback that was allowed to append it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackReceiptReject {
    DoesNotOpen,
    Malformed,
    WrongLibrary,
    WrongKey,
    FromFuture,
    Stale,
    Expired,
    Replay,
}

impl PlaybackReceiptReject {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DoesNotOpen => "does_not_open",
            Self::Malformed => "malformed",
            Self::WrongLibrary => "wrong_library",
            Self::WrongKey => "wrong_key",
            Self::FromFuture => "from_future",
            Self::Stale => "stale",
            Self::Expired => "expired",
            Self::Replay => "replay",
        }
    }
}

impl Reject {
    pub fn as_str(self) -> &'static str {
        match self {
            Reject::DoesNotOpen => "does_not_open",
            Reject::Malformed => "malformed",
            Reject::UnknownGrant => "unknown_grant",
            Reject::BadSignature => "bad_signature",
            Reject::WrongLibrary => "wrong_library",
            Reject::WrongQueue => "wrong_queue",
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

/// A grant's private key with its id, as den-edge and den-mcp hold it, and the connection's read key when it may read
/// (§15). Wiped when dropped.
pub struct GrantKey {
    id: String,
    secret: Zeroizing<[u8; 32]>,
    read: Option<Zeroizing<[u8; 32]>>,
}

impl GrantKey {
    pub fn from_secret(secret: &[u8; 32]) -> GrantKey {
        GrantKey {
            id: grant_id(&grant_public(secret)),
            secret: Zeroizing::new(*secret),
            read: None,
        }
    }

    /// The same key, carrying a connection's read key (§15).
    pub fn with_read_key(mut self, read: &[u8; 32]) -> GrantKey {
        self.read = Some(Zeroizing::new(*read));
        self
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

    /// The connection's read key, when it may read.
    pub fn read_key(&self) -> Option<&[u8; 32]> {
        self.read.as_deref()
    }

    /// The 48 bytes a `dw` claim holds, whatever the connection may do: the id's 16 bytes, then the seed.
    fn to_blob(&self) -> Zeroizing<[u8; GRANT_BLOB_LEN]> {
        let mut out = Zeroizing::new([0u8; GRANT_BLOB_LEN]);
        let id = unhex16(&self.id).expect("a grant id is 16 bytes of hex");
        out[..16].copy_from_slice(&id);
        out[16..].copy_from_slice(&*self.secret);
        out
    }

    /// What a wrap holds (§8, §15): the 48-byte blob, then the read key when there is one (80 bytes).
    fn to_wrap_blob(&self) -> Zeroizing<Vec<u8>> {
        let mut out = Zeroizing::new(self.to_blob().to_vec());
        if let Some(read) = &self.read {
            out.extend_from_slice(&**read);
        }
        out
    }

    /// A blob of 48 bytes, or 80 with a read key. One whose id is not its seed's is no grant key.
    fn from_blob(blob: &[u8]) -> Option<GrantKey> {
        if blob.len() != GRANT_BLOB_LEN && blob.len() != GRANT_BLOB_LEN + 32 {
            return None;
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(&blob[16..GRANT_BLOB_LEN]);
        let mut key = GrantKey::from_secret(&seed);
        if blob.len() > GRANT_BLOB_LEN {
            let mut read = Zeroizing::new([0u8; 32]);
            read.copy_from_slice(&blob[GRANT_BLOB_LEN..]);
            key.read = Some(read);
        }
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
        "play" => {
            if !keys_are(object, &["title"], &["season", "episode"]) {
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
        "play_trailer" => keys_are(object, &["title"], &[]),
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

// ---- managed-playback completion receipts

/// The fixed progress evidence Hearth seals when an authenticated managed playback session ends.
pub struct PlaybackReceipt<'a> {
    pub library: &'a str,
    /// The drop-box key id Edge attached to the session when it was authorized.
    pub kid: &'a str,
    /// Stable for this session's terminal sequence, so a callback retry is the same operation.
    pub id: &'a str,
    pub title: &'a Value,
    pub season: Option<u64>,
    pub episode: Option<u64>,
    pub position_seconds: u64,
    pub duration_seconds: u64,
    pub completed: bool,
    /// The time of the last player evidence, not the later time an idle supervisor happened to reap the session.
    pub ended_at: u64,
    pub expires_at: u64,
}

/// A checked receipt a client may turn into the ordinary progress write.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackReceiptMessage {
    pub library: String,
    pub kid: String,
    pub id: String,
    pub title: Value,
    pub season: Option<u64>,
    pub episode: Option<u64>,
    pub position_seconds: u64,
    pub duration_seconds: u64,
    pub completed: bool,
    pub ended_at: u64,
    pub expires_at: u64,
}

/// The canonical bytes of a receipt. Keeping seconds integral avoids two runtimes giving one media clock different
/// floating-point spellings.
pub fn playback_receipt_message(receipt: &PlaybackReceipt<'_>) -> Result<Vec<u8>, Error> {
    let mut value = json!({
        "v": 1,
        "library": receipt.library,
        "kid": receipt.kid,
        "id": receipt.id,
        "title": receipt.title,
        "positionSeconds": receipt.position_seconds,
        "durationSeconds": receipt.duration_seconds,
        "completed": receipt.completed,
        "endedAt": receipt.ended_at,
        "expiresAt": receipt.expires_at,
    });
    if let Some(season) = receipt.season {
        value["season"] = json!(season);
    }
    if let Some(episode) = receipt.episode {
        value["episode"] = json!(episode);
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| Error::InvalidRequest)?;
    parse_playback_receipt(&bytes).ok_or(Error::InvalidRequest)?;
    Ok(bytes)
}

/// Seal one receipt under the household drop-box public key. `eseed` is exposed for vectors; production calls
/// [`seal_playback_receipt_with_rng`].
#[doc(hidden)]
pub fn seal_playback_receipt(
    public: &[u8],
    receipt: &PlaybackReceipt<'_>,
    eseed: &[u8],
) -> Result<String, Error> {
    if key_id(public) != receipt.kid {
        return Err(Error::InvalidKey);
    }
    let message = playback_receipt_message(receipt)?;
    let sealed = b64url(&hpke_seal(
        public,
        PLAYBACK_RECEIPT_INFO,
        receipt.library.as_bytes(),
        &message,
        eseed,
    )?);
    (sealed.len() <= MAX_SEALED)
        .then_some(sealed)
        .ok_or(Error::InvalidRequest)
}

/// [`seal_playback_receipt`] with fresh HPKE randomness.
pub fn seal_playback_receipt_with_rng(
    public: &[u8],
    receipt: &PlaybackReceipt<'_>,
    rng: &mut impl rand_core::CryptoRng,
) -> Result<String, Error> {
    let mut eseed = Zeroizing::new([0u8; ESEED_LEN]);
    rng.fill_bytes(&mut *eseed);
    seal_playback_receipt(public, receipt, &*eseed)
}

/// Parse and validate a receipt's canonical plaintext, without judging its delivery time or replay state.
pub fn parse_playback_receipt(bytes: &[u8]) -> Option<PlaybackReceiptMessage> {
    if bytes.len() > MAX_PLAYBACK_RECEIPT_MESSAGE {
        return None;
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if serde_json::to_vec(&value).ok()? != bytes {
        return None;
    }
    let object = value.as_object()?;
    if !keys_are(
        object,
        &[
            "v",
            "library",
            "kid",
            "id",
            "title",
            "positionSeconds",
            "durationSeconds",
            "completed",
            "endedAt",
            "expiresAt",
        ],
        &["season", "episode"],
    ) || object["v"] != json!(1)
    {
        return None;
    }
    let text = |key: &str| object[key].as_str().filter(|s| lower_hex_id(s));
    let (library, kid, id) = (text("library")?, text("kid")?, text("id")?);
    let media = title(&object["title"])?;
    let season = match object.get("season") {
        Some(value) => Some(safe(value)?),
        None => None,
    };
    let episode = match object.get("episode") {
        Some(value) => Some(safe(value)?),
        None => None,
    };
    if season.is_some() != episode.is_some()
        || season.is_some() && media != "tv"
        || episode.is_some_and(|episode| episode == 0 || episode > MAX_EPISODE)
    {
        return None;
    }
    let position_seconds = safe(&object["positionSeconds"])?;
    let duration_seconds = safe(&object["durationSeconds"]).filter(|duration| *duration > 0)?;
    if position_seconds > duration_seconds {
        return None;
    }
    let ended_at = safe(&object["endedAt"])?;
    let expires_at = safe(&object["expiresAt"])?;
    if expires_at < ended_at || expires_at > ended_at.saturating_add(STALE_MS) {
        return None;
    }
    Some(PlaybackReceiptMessage {
        library: library.to_owned(),
        kid: kid.to_owned(),
        id: id.to_owned(),
        title: object["title"].clone(),
        season,
        episode,
        position_seconds,
        duration_seconds,
        completed: object["completed"].as_bool()?,
        ended_at,
        expires_at,
    })
}

/// Open a receipt with the library's drop-box keys and enforce its library, key epoch, freshness and replay fences.
pub fn open_playback_receipt(
    dropbox: &[(String, [u8; KEM_SECRET_LEN])],
    library: &str,
    sealed: &str,
    applied: &std::collections::BTreeSet<String>,
    now: u64,
) -> Result<PlaybackReceiptMessage, PlaybackReceiptReject> {
    let bytes = (sealed.len() <= MAX_SEALED)
        .then(|| b64url_decode(sealed))
        .flatten()
        .ok_or(PlaybackReceiptReject::DoesNotOpen)?;
    let (opened_kid, plaintext) = dropbox
        .iter()
        .find_map(|(kid, secret)| {
            hpke_open(secret, PLAYBACK_RECEIPT_INFO, library.as_bytes(), &bytes)
                .map(|plaintext| (kid, plaintext))
        })
        .ok_or(PlaybackReceiptReject::DoesNotOpen)?;
    let message = parse_playback_receipt(&plaintext).ok_or(PlaybackReceiptReject::Malformed)?;
    if message.library != library {
        return Err(PlaybackReceiptReject::WrongLibrary);
    }
    if &message.kid != opened_kid {
        return Err(PlaybackReceiptReject::WrongKey);
    }
    if message.ended_at > now.saturating_add(FUTURE_MS) {
        return Err(PlaybackReceiptReject::FromFuture);
    }
    if message.ended_at < now.saturating_sub(STALE_MS) {
        return Err(PlaybackReceiptReject::Stale);
    }
    if message.expires_at <= now {
        return Err(PlaybackReceiptReject::Expired);
    }
    if applied.contains(&message.id) {
        return Err(PlaybackReceiptReject::Replay);
    }
    Ok(message)
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
    /// Whether a live play record (§16) lets it play on the TV.
    pub play: bool,
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
/// requests, at `now` — as drained from the library's queue.
pub fn check(
    dropbox: &[[u8; KEM_SECRET_LEN]],
    library: &str,
    sealed: &str,
    grants: &BTreeMap<String, Grant>,
    applied: &BTreeMap<String, Applied>,
    now: u64,
) -> Result<Accepted, Reject> {
    check_queue(
        dropbox,
        library,
        sealed,
        grants,
        applied,
        now,
        Queue::Library,
        None,
    )
}

/// §6, §16: [`check`] for a request drained from `queue`. A request whose op belongs in the other queue is
/// `wrong_queue`. A TV op is `stale` past [`PLAY_STALE_MS`], or when it was made before `awake_since`, the time the TV
/// last woke (which the TV queue requires).
#[allow(clippy::too_many_arguments)]
pub fn check_queue(
    dropbox: &[[u8; KEM_SECRET_LEN]],
    library: &str,
    sealed: &str,
    grants: &BTreeMap<String, Grant>,
    applied: &BTreeMap<String, Applied>,
    now: u64,
    queue: Queue,
    awake_since: Option<u64>,
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
    // The queue is the signed op's, so a request moved into the other queue is refused, not applied there.
    let own = Queue::of(&message.op);
    if own != queue {
        return Err(Reject::WrongQueue);
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
    let stale = match own {
        Queue::Library => message.at < now.saturating_sub(STALE_MS),
        Queue::Tv => {
            message.at < now.saturating_sub(PLAY_STALE_MS)
                || awake_since.is_none_or(|since| message.at < since)
        }
    };
    if stale {
        return Err(Reject::Stale);
    }
    if applied.contains_key(&message.id) {
        return Err(Reject::Replay);
    }
    let allowed = match own {
        Queue::Library => grant.ops.contains(&message.op),
        Queue::Tv => grant.play,
    };
    if !allowed {
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
                msg: &grant.to_wrap_blob(),
                aad: session.as_bytes(),
            },
        )
        .expect("AES-GCM seals 48 or 80 bytes");
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
    // v1's 48-byte blob, or 80 with a read key (§15): a wrap made before reads keeps unwrapping.
    if bytes.len() != 12 + GRANT_BLOB_LEN + TAG_LEN
        && bytes.len() != 12 + GRANT_BLOB_LEN + 32 + TAG_LEN
    {
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

// ---- reads (§15)

/// HPKE `info` of a connection's read key sealed into an access token for den-mcp (`dr`, §15).
pub const READ_INFO: &[u8] = b"den/assistant/read/v1";
/// What a projection part's additional data starts with (§15).
pub const PROJECTION_CONTEXT: &[u8] = b"den/assistant/projection/v1";
/// The longest sealed part, in characters of base64url: 256 KiB.
pub const MAX_PART: usize = 262_144;
/// Compressed bytes per part: what fills [`MAX_PART`] once a nonce and a tag are added (196,608 − 28).
pub const PART_BYTES: usize = 196_580;
/// The most parts one projection may have.
pub const MAX_PARTS: usize = 64;
/// The largest projection plaintext, before compression and after inflating: 64 MiB.
pub const MAX_PROJECTION_PLAINTEXT: usize = 64 << 20;

/// A connection's read key, as den-mcp holds it from a `dr` claim. Wiped when dropped.
pub struct ReadKey {
    grant: String,
    key: Zeroizing<[u8; 32]>,
}

impl ReadKey {
    pub fn grant(&self) -> &str {
        &self.grant
    }

    pub fn key(&self) -> &[u8; 32] {
        &self.key
    }
}

/// §15: a grant's read key sealed to den-mcp's X-Wing public key for the access token whose `sub` this is: unpadded
/// base64url of `enc ‖ ct`, the plaintext the grant id's 16 bytes and the read key. `InvalidRequest` for a grant key
/// with no read key. For the vectors; production code calls [`seal_read_claim_with_rng`].
#[doc(hidden)]
pub fn seal_read_claim(
    public: &[u8],
    sub: &str,
    grant: &GrantKey,
    eseed: &[u8],
) -> Result<String, Error> {
    let read = grant.read_key().ok_or(Error::InvalidRequest)?;
    let id = unhex16(grant.id()).expect("a grant id is 16 bytes of hex");
    let plaintext = Zeroizing::new([&id[..], &read[..]].concat());
    Ok(b64url(&hpke_seal(
        public,
        READ_INFO,
        sub.as_bytes(),
        &plaintext,
        eseed,
    )?))
}

/// [`seal_read_claim`] with a fresh `eseed` from `rng`. What den-edge calls for every access token of a read session.
pub fn seal_read_claim_with_rng(
    public: &[u8],
    sub: &str,
    grant: &GrantKey,
    rng: &mut impl rand_core::CryptoRng,
) -> Result<String, Error> {
    let mut eseed = Zeroizing::new([0u8; ESEED_LEN]);
    rng.fill_bytes(&mut *eseed);
    seal_read_claim(public, sub, grant, &*eseed)
}

/// §15: den-mcp's side of [`seal_read_claim`].
pub fn open_read_claim(
    secret: &[u8; KEM_SECRET_LEN],
    sub: &str,
    claim: &str,
) -> Result<ReadKey, Error> {
    let sealed = b64url_decode(claim).ok_or(Error::DoesNotOpen)?;
    let plain = Zeroizing::new(
        hpke_open(secret, READ_INFO, sub.as_bytes(), &sealed).ok_or(Error::DoesNotOpen)?,
    );
    if plain.len() != 48 {
        return Err(Error::DoesNotOpen);
    }
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&plain[16..]);
    Ok(ReadKey {
        grant: hex(&plain[..16]),
        key,
    })
}

/// One part's additional data (§15): the context, the library id, the grant id, the publish's set id, the part's index
/// and the number of parts, each after a zero byte, numbers in decimal. A part therefore opens only in its own place
/// in its own publish.
pub fn projection_aad(
    library: &str,
    grant: &str,
    set: &str,
    index: usize,
    count: usize,
) -> Vec<u8> {
    let mut out = PROJECTION_CONTEXT.to_vec();
    for field in [library, grant, set, &index.to_string(), &count.to_string()] {
        out.push(0);
        out.extend_from_slice(field.as_bytes());
    }
    out
}

/// A sealed projection: the publish's set id (32 hex) and its parts, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct SealedProjection {
    pub set: String,
    pub parts: Vec<String>,
}

fn derive(random: &[u8; 32], info: &[&[u8]], out: &mut [u8]) {
    Hkdf::<Sha256>::new(None, random)
        .expand(&info.concat(), out)
        .expect("a short HKDF-SHA256 output");
}

/// §15: a projection (the expanded object: `v`, `library`, `grant`, `at`, `head`, `watchlist`, `continue`, `seen`)
/// written compact, compressed with raw DEFLATE, split into parts of [`PART_BYTES`] and each sealed under the read
/// key. The set id and every part's nonce derive from `random`, which MUST be fresh for every call under one key.
/// `InvalidRequest` for a projection that is not one, or past [`MAX_PROJECTION_PLAINTEXT`] or [`MAX_PARTS`].
pub fn seal_projection(
    key: &[u8; 32],
    projection: &Value,
    random: &[u8; 32],
) -> Result<SealedProjection, Error> {
    let library = projection["library"]
        .as_str()
        .ok_or(Error::InvalidRequest)?;
    let grant = projection["grant"].as_str().ok_or(Error::InvalidRequest)?;
    seal_plaintext(
        key,
        library,
        grant,
        &projection_plaintext(projection)?,
        random,
    )
}

/// [`seal_projection`] of the projection for `grant` made of `rows`, without the expanded object: what a client with
/// a large library calls, since the expanded lists are many times the size of their rows.
pub fn seal_projection_rows(
    key: &[u8; 32],
    library: &str,
    grant: &str,
    at: u64,
    head: u64,
    rows: &ProjectionRows,
    random: &[u8; 32],
) -> Result<SealedProjection, Error> {
    let meta = [json!(library), json!(grant), json!(at), json!(head)];
    let plain = write_compact(&meta, rows)?;
    seal_plaintext(key, library, grant, &plain, random)
}

fn seal_plaintext(
    key: &[u8; 32],
    library: &str,
    grant: &str,
    plain: &[u8],
    random: &[u8; 32],
) -> Result<SealedProjection, Error> {
    let deflated = miniz_oxide::deflate::compress_to_vec(plain, 9);
    let count = deflated.len().div_ceil(PART_BYTES);
    if count > MAX_PARTS {
        return Err(Error::InvalidRequest);
    }
    let mut set = [0u8; 16];
    derive(random, &[b"den/assistant/projection/set/v1"], &mut set);
    let set = hex(&set);
    let cipher = Aes256Gcm::new(key.into());
    let mut parts = Vec::with_capacity(count);
    for (index, chunk) in deflated.chunks(PART_BYTES).enumerate() {
        let mut nonce = [0u8; 12];
        derive(
            random,
            &[
                b"den/assistant/projection/nonce/v1\0",
                index.to_string().as_bytes(),
            ],
            &mut nonce,
        );
        let ct = cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: chunk,
                    aad: &projection_aad(library, grant, &set, index, count),
                },
            )
            .map_err(|_| Error::InvalidRequest)?;
        parts.push(b64url(&[&nonce[..], &ct].concat()));
    }
    Ok(SealedProjection { set, parts })
}

/// §15: den-mcp's side of [`seal_projection`]: every part opened in its place, the whole inflated (at most
/// [`MAX_PROJECTION_PLAINTEXT`]) and read back to the expanded projection, which must name this library and grant.
/// `DoesNotOpen` for anything else — a missing, extra, reordered or foreign part included.
pub fn open_projection(
    key: &[u8; 32],
    library: &str,
    grant: &str,
    set: &str,
    parts: &[String],
) -> Result<Value, Error> {
    let plain = open_projection_compact(key, library, grant, set, parts)?;
    let compact: Value = serde_json::from_slice(&plain).map_err(|_| Error::DoesNotOpen)?;
    expand(&compact).ok_or(Error::DoesNotOpen)
}

/// The members of a compact projection [`open_projection_compact`] checks; everything else is skipped unparsed.
#[derive(serde::Deserialize)]
struct Names {
    v: u64,
    library: String,
    grant: String,
}

/// Inflated output, grown by hand so every buffer it outgrows is wiped before it is freed.
struct Output(Zeroizing<Vec<u8>>);

impl Output {
    fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let need = self.0.len() + bytes.len();
        if need > MAX_PROJECTION_PLAINTEXT {
            return Err(Error::DoesNotOpen);
        }
        // Growing copies into a new buffer and wipes the old one (a `Vec` reallocation would free it unwiped), so the
        // moment of growth holds both: by half again, that is at most 2.5 times the output.
        if need > self.0.capacity() {
            let capacity = need
                .max(self.0.capacity() + self.0.capacity() / 2)
                .clamp(1 << 16, MAX_PROJECTION_PLAINTEXT);
            let mut grown = Zeroizing::new(Vec::with_capacity(capacity));
            grown.extend_from_slice(&self.0);
            self.0 = grown;
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

/// Like [`open_projection`], but returns the inflated compact plaintext (§15's JCS compact form) with every part
/// opened in place and `v`, `library` and `grant` checked — no `serde_json::Value` of the lists. Parts are decoded,
/// opened and inflated one at a time, so the deflated set is never held whole; the output grows with what inflates,
/// up to [`MAX_PROJECTION_PLAINTEXT`]. Every intermediate buffer is wiped. Fails as `open_projection` does.
pub fn open_projection_compact(
    key: &[u8; 32],
    library: &str,
    grant: &str,
    set: &str,
    parts: &[String],
) -> Result<Zeroizing<Vec<u8>>, Error> {
    use miniz_oxide::inflate::stream::{inflate, InflateState};
    use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
    let count = parts.len();
    if count == 0 || count > MAX_PARTS || !lower_hex_id(set) {
        return Err(Error::DoesNotOpen);
    }
    let cipher = Aes256Gcm::new(key.into());
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut scratch = Zeroizing::new(vec![0u8; 1 << 16]);
    let mut out = Output(Zeroizing::new(Vec::new()));
    let mut ended = false;
    // Inflates `input` into `out`; `true` once the stream has ended. Input past the end is refused.
    let mut feed = |input: &[u8], out: &mut Output, ended: &mut bool| -> Result<(), Error> {
        let mut at = 0;
        loop {
            if *ended {
                return if at < input.len() {
                    Err(Error::DoesNotOpen)
                } else {
                    Ok(())
                };
            }
            let result = inflate(&mut state, &input[at..], &mut scratch, MZFlush::None);
            // `Buf` is the inflater waiting for input it was not given: the next part, or — after the last — a
            // stream cut short, which the caller refuses as not ended.
            let status = match result.status {
                Ok(status) => status,
                Err(MZError::Buf) => MZStatus::Ok,
                Err(_) => return Err(Error::DoesNotOpen),
            };
            out.push(&scratch[..result.bytes_written])?;
            at += result.bytes_consumed;
            *ended = status == MZStatus::StreamEnd;
            if !*ended && result.bytes_consumed == 0 && result.bytes_written == 0 {
                return Ok(());
            }
        }
    };
    for (index, part) in parts.iter().enumerate() {
        if part.len() > MAX_PART {
            return Err(Error::DoesNotOpen);
        }
        let bytes = Zeroizing::new(b64url_decode(part).ok_or(Error::DoesNotOpen)?);
        if bytes.len() <= 12 + TAG_LEN {
            return Err(Error::DoesNotOpen);
        }
        let nonce: &[u8; 12] = bytes[..12].try_into().expect("12 bytes");
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    nonce.into(),
                    Payload {
                        msg: &bytes[12..],
                        aad: &projection_aad(library, grant, set, index, count),
                    },
                )
                .map_err(|_| Error::DoesNotOpen)?,
        );
        drop(bytes);
        feed(&plain, &mut out, &mut ended)?;
    }
    // Whatever the inflater still holds, then the stream must have ended: a set cut short does not open.
    feed(&[], &mut out, &mut ended)?;
    if !ended {
        return Err(Error::DoesNotOpen);
    }
    let names: Names = serde_json::from_slice(&out.0).map_err(|_| Error::DoesNotOpen)?;
    if names.v != 1 || names.library != library || names.grant != grant {
        return Err(Error::DoesNotOpen);
    }
    Ok(out.0)
}

/// §15: the plaintext a projection compresses: the JCS of its compact form. `InvalidRequest` for a projection that is
/// not one, or past [`MAX_PROJECTION_PLAINTEXT`].
pub fn projection_plaintext(projection: &Value) -> Result<Vec<u8>, Error> {
    let rows = rows_of(projection).ok_or(Error::InvalidRequest)?;
    let meta = ["library", "grant", "at", "head"].map(|k| projection[k].clone());
    write_compact(&meta, &rows)
}

/// A title in a projection (§15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Media {
    Movie,
    Tv,
}

impl Media {
    pub fn as_str(self) -> &'static str {
        match self {
            Media::Movie => "movie",
            Media::Tv => "tv",
        }
    }
}

pub type ProjectionTitle = (Media, u64);

/// A projection's lists as rows (§15): each entry's members, `None` where the expanded entry has no such member.
#[derive(Debug, Default)]
pub struct ProjectionRows {
    pub watchlist: Vec<WatchlistRow>,
    pub continuing: Vec<ContinueRow>,
    pub seen: Vec<SeenRow>,
    /// `rated` and `hide` came after the first projections: carried only when the projection has them.
    pub rated: Option<Vec<RatedRow>>,
    pub hide: Option<Map<String, Value>>,
}

#[derive(Debug)]
pub struct WatchlistRow {
    pub title: ProjectionTitle,
    pub added_at: Value,
}

#[derive(Debug)]
pub struct ContinueRow {
    pub title: ProjectionTitle,
    pub action: Value,
    pub season: Option<Value>,
    pub episode: Option<Value>,
    pub fraction: Value,
    pub at: Value,
}

#[derive(Debug)]
pub struct SeenRow {
    pub title: ProjectionTitle,
    pub season: Option<Value>,
    pub episode: Option<Value>,
    pub at: Value,
}

#[derive(Debug)]
pub struct RatedRow {
    pub title: ProjectionTitle,
    pub value: Value,
    pub at: Value,
}

fn title_of(value: &Value) -> Option<ProjectionTitle> {
    let media = match value["type"].as_str()? {
        "movie" => Media::Movie,
        "tv" => Media::Tv,
        _ => return None,
    };
    Some((media, value["id"].as_u64()?))
}

/// The rows of an expanded projection; `None` when it is not one.
fn rows_of(view: &Value) -> Option<ProjectionRows> {
    let list = |name: &str| view[name].as_array();
    let watchlist = list("watchlist")?
        .iter()
        .map(|e| {
            Some(WatchlistRow {
                title: title_of(&e["title"])?,
                added_at: e["addedAt"].clone(),
            })
        })
        .collect::<Option<_>>()?;
    let continuing = list("continue")?
        .iter()
        .map(|e| {
            Some(ContinueRow {
                title: title_of(&e["title"])?,
                action: e["action"].clone(),
                season: e.get("season").cloned(),
                episode: e.get("episode").cloned(),
                fraction: e["fraction"].clone(),
                at: e["at"].clone(),
            })
        })
        .collect::<Option<_>>()?;
    let seen = list("seen")?
        .iter()
        .map(|e| {
            Some(SeenRow {
                title: title_of(&e["title"])?,
                season: e.get("season").cloned(),
                episode: e.get("episode").cloned(),
                at: e["at"].clone(),
            })
        })
        .collect::<Option<_>>()?;
    let rated = match view.get("rated") {
        None => None,
        Some(list) => Some(
            list.as_array()?
                .iter()
                .map(|e| {
                    Some(RatedRow {
                        title: title_of(&e["title"])?,
                        value: e["value"].clone(),
                        at: e["at"].clone(),
                    })
                })
                .collect::<Option<_>>()?,
        ),
    };
    let hide = match view.get("hide") {
        None => None,
        Some(hide) => Some(hide.as_object()?.clone()),
    };
    Some(ProjectionRows {
        watchlist,
        continuing,
        seen,
        rated,
        hide,
    })
}

fn put(out: &mut Vec<u8>, value: &impl serde::Serialize) {
    serde_json::to_writer(&mut *out, value).expect("JSON value always serializes");
}

/// Writes `rows` as each item of a JSON array, after `name`.
fn put_list<T>(out: &mut Vec<u8>, name: &str, rows: &[T], mut row: impl FnMut(&mut Vec<u8>, &T)) {
    out.extend_from_slice(name.as_bytes());
    out.push(b'[');
    for (i, item) in rows.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        row(out, item);
    }
    out.push(b']');
}

/// The compact form (§15) of `rows`, written as `serde_json` writes it (members sorted, nothing between tokens):
/// titles once, in a table sorted by type and id; each list's entries as arrays that index it. `meta` is `library`,
/// `grant`, `at`, `head`. Written straight from the rows, so no `Value` of the whole is built.
fn write_compact(meta: &[Value; 4], rows: &ProjectionRows) -> Result<Vec<u8>, Error> {
    let [library, grant, at, head] = meta;
    let mut titles: Vec<ProjectionTitle> = rows
        .watchlist
        .iter()
        .map(|r| r.title)
        .chain(rows.continuing.iter().map(|r| r.title))
        .chain(rows.seen.iter().map(|r| r.title))
        .chain(rows.rated.iter().flatten().map(|r| r.title))
        .collect();
    titles.sort();
    titles.dedup();
    let index = |title: &ProjectionTitle| titles.binary_search(title).expect("a listed title");
    let null = Value::Null;
    let mut out = Vec::new();
    out.extend_from_slice(b"{\"at\":");
    put(&mut out, at);
    // One entry: `[index, values...]`.
    let entry = |out: &mut Vec<u8>, title: &ProjectionTitle, values: &[&Value]| {
        out.push(b'[');
        put(out, &index(title));
        for value in values {
            out.push(b',');
            put(out, value);
        }
        out.push(b']');
    };
    put_list(&mut out, ",\"continue\":", &rows.continuing, |out, r| {
        let season = r.season.as_ref().unwrap_or(&null);
        let episode = r.episode.as_ref().unwrap_or(&null);
        entry(
            out,
            &r.title,
            &[&r.action, season, episode, &r.fraction, &r.at],
        );
    });
    out.extend_from_slice(b",\"grant\":");
    put(&mut out, grant);
    out.extend_from_slice(b",\"head\":");
    put(&mut out, head);
    if let Some(hide) = &rows.hide {
        out.extend_from_slice(b",\"hide\":");
        put(&mut out, hide);
    }
    out.extend_from_slice(b",\"library\":");
    put(&mut out, library);
    if let Some(rated) = &rows.rated {
        put_list(&mut out, ",\"rated\":", rated, |out, r| {
            entry(out, &r.title, &[&r.value, &r.at]);
        });
    }
    put_list(&mut out, ",\"seen\":", &rows.seen, |out, r| {
        let season = r.season.as_ref().unwrap_or(&null);
        let episode = r.episode.as_ref().unwrap_or(&null);
        entry(out, &r.title, &[season, episode, &r.at]);
    });
    put_list(&mut out, ",\"titles\":", &titles, |out, (media, id)| {
        put(out, &(media.as_str(), id));
    });
    out.extend_from_slice(b",\"v\":1");
    put_list(&mut out, ",\"watchlist\":", &rows.watchlist, |out, r| {
        entry(out, &r.title, &[&r.added_at]);
    });
    out.push(b'}');
    if out.len() > MAX_PROJECTION_PLAINTEXT {
        return Err(Error::InvalidRequest);
    }
    Ok(out)
}

/// The expanded form den-mcp reads, from the compact one.
fn expand(compact: &Value) -> Option<Value> {
    if compact["v"] != 1 {
        return None;
    }
    let titles = compact["titles"]
        .as_array()?
        .iter()
        .map(|t| {
            let media = t[0].as_str().filter(|m| matches!(*m, "movie" | "tv"))?;
            Some(json!({"type": media, "id": t[1].as_u64()?}))
        })
        .collect::<Option<Vec<_>>>()?;
    let title = |row: &Value| titles.get(usize::try_from(row[0].as_u64()?).ok()?).cloned();
    let place = |entry: &mut Value, season: &Value, episode: &Value| {
        if !season.is_null() {
            entry["season"] = season.clone();
        }
        if !episode.is_null() {
            entry["episode"] = episode.clone();
        }
    };
    let rows = |list: &str| compact[list].as_array().cloned();
    let watchlist = rows("watchlist")?
        .iter()
        .map(|r| Some(json!({"title": title(r)?, "addedAt": r[1]})))
        .collect::<Option<Vec<_>>>()?;
    let continuing = rows("continue")?
        .iter()
        .map(|r| {
            let mut entry =
                json!({"title": title(r)?, "action": r[1], "fraction": r[4], "at": r[5]});
            place(&mut entry, &r[2], &r[3]);
            Some(entry)
        })
        .collect::<Option<Vec<_>>>()?;
    let seen = rows("seen")?
        .iter()
        .map(|r| {
            let mut entry = json!({"title": title(r)?, "at": r[3]});
            place(&mut entry, &r[1], &r[2]);
            Some(entry)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut out = json!({
        "v": 1, "library": compact["library"], "grant": compact["grant"], "at": compact["at"],
        "head": compact["head"], "watchlist": watchlist, "continue": continuing, "seen": seen,
    });
    if compact.get("rated").is_some() {
        out["rated"] = Value::Array(
            rows("rated")?
                .iter()
                .map(|r| Some(json!({"title": title(r)?, "value": r[1], "at": r[2]})))
                .collect::<Option<Vec<_>>>()?,
        );
    }
    if let Some(hide) = compact.get("hide") {
        hide.as_object()?;
        out["hide"] = hide.clone();
    }
    Some(out)
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

    #[test]
    fn a_playback_receipt_is_domain_separated_fresh_and_replay_safe() {
        let secret = [9; 32];
        let public = kem_public(&secret);
        let kid = key_id(&public);
        let title = json!({"type":"tv","id":1399});
        let receipt = PlaybackReceipt {
            library: "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e",
            kid: &kid,
            id: "00112233445566778899aabbccddeeff",
            title: &title,
            season: Some(1),
            episode: Some(2),
            position_seconds: 3120,
            duration_seconds: 3200,
            completed: true,
            ended_at: 1_789_999_940_000,
            expires_at: 1_790_604_740_000,
        };
        let sealed = seal_playback_receipt(&public, &receipt, &[7; ESEED_LEN]).unwrap();
        let keys = vec![(kid.clone(), secret)];
        let now = receipt.ended_at;
        let opened =
            open_playback_receipt(&keys, receipt.library, &sealed, &Default::default(), now)
                .unwrap();
        assert_eq!(opened.id, receipt.id);
        assert_eq!(opened.position_seconds, 3120);
        assert!(opened.completed);

        let mut applied = std::collections::BTreeSet::new();
        applied.insert(receipt.id.to_owned());
        assert_eq!(
            open_playback_receipt(&keys, receipt.library, &sealed, &applied, now),
            Err(PlaybackReceiptReject::Replay)
        );
        let request_open = hpke_open(
            &secret,
            REQUEST_INFO,
            receipt.library.as_bytes(),
            &b64url_decode(&sealed).unwrap(),
        );
        assert!(
            request_open.is_none(),
            "a receipt must not open in the assistant-request domain"
        );
    }

    #[test]
    fn playback_receipts_reject_bad_coordinates_and_time_bounds() {
        let title = json!({"type":"movie","id":550});
        let make = || PlaybackReceipt {
            library: "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e",
            kid: "00112233445566778899aabbccddeeff",
            id: "ffeeddccbbaa99887766554433221100",
            title: &title,
            season: None,
            episode: None,
            position_seconds: 10,
            duration_seconds: 100,
            completed: false,
            ended_at: 1000,
            expires_at: 2000,
        };
        assert!(playback_receipt_message(&make()).is_ok());
        assert!(playback_receipt_message(&PlaybackReceipt {
            position_seconds: 101,
            ..make()
        })
        .is_err());
        assert!(playback_receipt_message(&PlaybackReceipt {
            season: Some(1),
            episode: Some(1),
            ..make()
        })
        .is_err());
        assert!(playback_receipt_message(&PlaybackReceipt {
            expires_at: STALE_MS + 1001,
            ..make()
        })
        .is_err());
    }
}
