//! Profile-routed playback receipts v2 (`den-spec/wire/playback-receipts-v2.md`).
//!
//! This module is deliberately pure. Persistent-profile and Edge stores own transactions; these helpers pin the
//! bytes and the bounded state transitions which those transactions must commit.

use super::{
    b64url, b64url_decode, hex, hpke_open, hpke_seal, key_id, Error, ESEED_LEN, FUTURE_MS,
    KEM_PUBLIC_LEN, KEM_SECRET_LEN, MAX_PLAYBACK_RECEIPT_MESSAGE, MAX_SAFE_INTEGER, STALE_MS,
};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use zeroize::Zeroizing;

pub const PLAYBACK_RECEIPT_V2_INFO: &[u8] = b"den/playback-receipt/v2";
pub const PLAYBACK_RECEIPT_ROUTE_V2_DOMAIN: &[u8] = b"den/playback-receipt-route/v2";
pub const PLAYBACK_RECEIPT_PROOF_V2_DOMAIN: &[u8] = b"den/playback-receipt-proof/v2";
pub const PLAYBACK_RECEIPT_RETIRED_V2_DOMAIN: &[u8] = b"den/playback-receipt-retired/v2";
pub const PLAYBACK_RECEIPT_V2_MAX_SEALED: usize = 2_539;
pub const PLAYBACK_RECEIPT_V2_HISTORY_CAPACITY: usize = 512;
pub const PLAYBACK_RECEIPT_V2_MAX_OPERATION: usize = 4 * 1024;
pub const PLAYBACK_RECEIPT_V2_MAX_ROUTE_QUEUE: usize = 25;
const PLAYBACK_RECEIPT_V2_MAX_DRAIN: usize = 70 * 1024;

fn lower_hex(text: &str, bytes: usize) -> bool {
    text.len() == bytes * 2
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if !lower_hex(text, N) {
        return None;
    }
    let mut out = [0; N];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Reject every non-integer JSON number spelling before `serde_json` can coerce it. The JSON parser still owns
/// structural validation; this pass only enforces v2's lexical integer and safe-range rule.
pub fn playback_receipt_v2_exact_integers(input: &[u8]) -> bool {
    let mut index = 0;
    let mut string = false;
    let mut escaped = false;
    while index < input.len() {
        let byte = input[index];
        if string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            string = true;
            index += 1;
            continue;
        }
        if byte == b'-' {
            return false;
        }
        if byte.is_ascii_digit() {
            let start = index;
            if byte == b'0' {
                index += 1;
                if input.get(index).is_some_and(u8::is_ascii_digit) {
                    return false;
                }
            } else {
                while input.get(index).is_some_and(u8::is_ascii_digit) {
                    index += 1;
                }
            }
            if input
                .get(index)
                .is_some_and(|next| matches!(next, b'.' | b'e' | b'E'))
            {
                return false;
            }
            let Ok(token) = std::str::from_utf8(&input[start..index]) else {
                return false;
            };
            if token
                .parse::<u64>()
                .ok()
                .is_none_or(|number| number > MAX_SAFE_INTEGER)
            {
                return false;
            }
            continue;
        }
        index += 1;
    }
    !string && serde_json::from_slice::<Value>(input).is_ok()
}

/// Whether one token is exactly a safe, non-negative JSON integer. Field-level validators separately require
/// positive values where the contract says so.
pub fn playback_receipt_v2_exact_integer_token(input: &[u8]) -> bool {
    if input.is_empty()
        || input == b"-0"
        || input[0] == b'-'
        || input.len() > 1 && input[0] == b'0'
        || !input.iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    std::str::from_utf8(input)
        .ok()
        .and_then(|token| token.parse::<u64>().ok())
        .is_some_and(|number| number <= MAX_SAFE_INTEGER)
}

fn canonical_object(input: &[u8]) -> Option<Value> {
    if !playback_receipt_v2_exact_integers(input) {
        return None;
    }
    let value: Value = serde_json::from_slice(input).ok()?;
    value.is_object().then_some(())?;
    (serde_json::to_vec(&value).ok()? == input).then_some(value)
}

fn exact_keys(object: &Map<String, Value>, required: &[&str]) -> bool {
    object.len() == required.len() && required.iter().all(|key| object.contains_key(*key))
}

fn operation_text<'a>(object: &'a Map<String, Value>, key: &str, bytes: usize) -> Option<&'a str> {
    object.get(key)?.as_str().filter(|text| lower_hex(text, bytes))
}

/// The exact proof-bearing operation shapes. Clock and membership checks remain Edge inputs; all byte-level and
/// relation checks which do not need them are shared here.
pub fn validate_playback_receipt_v2_operation(input: &[u8]) -> Option<Value> {
    let value = canonical_object(input)?;
    let object = value.as_object()?;
    if object.get("v")?.as_u64()? != 2 {
        return None;
    }
    let op = object.get("op")?.as_str()?;
    let common = |keys: &[&str]| {
        exact_keys(object, keys)
            && operation_text(object, "binding", 16).is_some()
            && operation_text(object, "household", 16).is_some()
            && operation_text(object, "nonce", 16).is_some()
            && operation_text(object, "route", 16).is_some()
            && object
                .get("generation")
                .and_then(Value::as_u64)
                .is_some_and(|generation| generation > 0)
            && object.get("issuedAt").and_then(Value::as_u64).is_some()
    };
    let base = [
        "binding",
        "generation",
        "household",
        "issuedAt",
        "nonce",
        "op",
        "route",
        "v",
    ];
    let valid = match op {
        "register" => {
            let keys = [
                "activity",
                "binding",
                "generation",
                "household",
                "issuedAt",
                "kid",
                "nonce",
                "op",
                "public",
                "route",
                "v",
            ];
            common(&keys)
                && object["generation"] == json!(1)
                && operation_text(object, "activity", 16).is_some()
                && object["activity"] != object["household"]
                && object["public"]
                    .as_str()
                    .zip(object["kid"].as_str())
                    .is_some_and(|(public, kid)| playback_receipt_v2_public(public, kid).is_some())
        }
        "rebind" => {
            let mut keys = base.to_vec();
            keys.extend(["activity", "previousActivity"]);
            common(&keys)
                && object["generation"].as_u64().is_some_and(|value| value > 1)
                && operation_text(object, "activity", 16).is_some()
                && operation_text(object, "previousActivity", 16).is_some()
                && object["activity"] != object["household"]
        }
        "rotate" => {
            let mut keys = base.to_vec();
            keys.extend(["kid", "public"]);
            common(&keys)
                && object["generation"].as_u64().is_some_and(|value| value > 1)
                && object["public"]
                    .as_str()
                    .zip(object["kid"].as_str())
                    .is_some_and(|(public, kid)| playback_receipt_v2_public(public, kid).is_some())
        }
        "playback" => {
            let mut keys = base.to_vec();
            keys.push("request");
            common(&keys) && operation_text(object, "request", 32).is_some()
        }
        "ack" => {
            let mut keys = base.to_vec();
            keys.push("upTo");
            common(&keys)
                && object["upTo"].as_u64().is_some_and(|value| value > 0)
        }
        "drain" | "activate" | "revoke" => common(&base),
        _ => false,
    };
    valid.then_some(value)
}

/// Profile-owned secret state. Callers persist the canonical encodings only inside that profile's encrypted state.
pub struct PlaybackReceiptV2Authority {
    authority: Zeroizing<[u8; 32]>,
    binding: [u8; 16],
}

impl PlaybackReceiptV2Authority {
    /// Exactly 48 CSPRNG bytes: 32 for authority followed by 16 for the stable binding.
    pub fn from_random(random: &[u8]) -> Result<Self, Error> {
        if random.len() != 48 {
            return Err(Error::InvalidRandomness);
        }
        let mut authority = Zeroizing::new([0; 32]);
        authority.copy_from_slice(&random[..32]);
        let mut binding = [0; 16];
        binding.copy_from_slice(&random[32..]);
        Ok(Self { authority, binding })
    }

    pub fn from_parts(authority: &str, binding: &str) -> Option<Self> {
        let authority = b64url_decode(authority)?.try_into().ok()?;
        let binding = unhex(binding)?;
        Some(Self {
            authority: Zeroizing::new(authority),
            binding,
        })
    }

    pub fn authority(&self) -> String {
        b64url(&*self.authority)
    }

    pub fn binding(&self) -> String {
        hex(&self.binding)
    }

    pub fn route(&self) -> String {
        playback_receipt_v2_route(&self.authority)
    }

    pub fn proof(&self, canonical_operation: &[u8]) -> Result<String, Error> {
        playback_receipt_v2_proof(&self.authority, canonical_operation)
    }
}

pub fn playback_receipt_v2_route(authority: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(PLAYBACK_RECEIPT_ROUTE_V2_DOMAIN);
    hash.update([0]);
    hash.update(authority);
    hex(&hash.finalize()[..16])
}

/// Proof over exact JCS operation bytes. This refuses non-canonical input so a caller cannot sign one spelling and
/// send another.
pub fn playback_receipt_v2_proof(
    authority: &[u8; 32],
    canonical_operation: &[u8],
) -> Result<String, Error> {
    if canonical_operation.len() > PLAYBACK_RECEIPT_V2_MAX_OPERATION {
        return Err(Error::InvalidRequest);
    }
    let operation = validate_playback_receipt_v2_operation(canonical_operation)
        .ok_or(Error::InvalidRequest)?;
    if operation["route"] != playback_receipt_v2_route(authority) {
        return Err(Error::InvalidRequest);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(authority).expect("HMAC accepts a 32-byte key");
    mac.update(PLAYBACK_RECEIPT_PROOF_V2_DOMAIN);
    mac.update(&[0]);
    mac.update(canonical_operation);
    Ok(b64url(&mac.finalize().into_bytes()))
}

pub fn verify_playback_receipt_v2_proof(
    authority: &[u8; 32],
    canonical_operation: &[u8],
    proof: &str,
) -> bool {
    let Some(proof) = b64url_decode(proof).filter(|proof| proof.len() == 32) else {
        return false;
    };
    if canonical_operation.len() > PLAYBACK_RECEIPT_V2_MAX_OPERATION
        || validate_playback_receipt_v2_operation(canonical_operation).is_none()
    {
        return false;
    }
    let operation: Value = serde_json::from_slice(canonical_operation).expect("validated operation");
    if operation["route"] != playback_receipt_v2_route(authority) {
        return false;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(authority).expect("HMAC accepts a 32-byte key");
    mac.update(PLAYBACK_RECEIPT_PROOF_V2_DOMAIN);
    mac.update(&[0]);
    mac.update(canonical_operation);
    mac.verify_slice(&proof).is_ok()
}

/// Canonical 1,216-byte X-Wing public key and its mandated SHA-256 key id.
pub fn playback_receipt_v2_public(public: &str, kid: &str) -> Option<Vec<u8>> {
    let public = b64url_decode(public)?;
    (public.len() == KEM_PUBLIC_LEN && lower_hex(kid, 16) && key_id(&public) == kid)
        .then_some(public)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackReceiptV2Aad {
    pub binding: String,
    pub generation: u64,
    pub household: String,
    pub id: String,
    pub kid: String,
    pub route: String,
}

impl PlaybackReceiptV2Aad {
    pub fn canonical(&self) -> Option<Vec<u8>> {
        (self.generation > 0
            && self.generation <= MAX_SAFE_INTEGER
            && lower_hex(&self.binding, 16)
            && lower_hex(&self.household, 16)
            && lower_hex(&self.id, 16)
            && lower_hex(&self.kid, 16)
            && lower_hex(&self.route, 16))
        .then(|| {
            serde_json::to_vec(&json!({
                "binding": self.binding,
                "generation": self.generation,
                "household": self.household,
                "id": self.id,
                "kid": self.kid,
                "route": self.route,
                "v": 2,
            }))
            .expect("serializing fixed JSON")
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackReceiptV2<'a> {
    pub aad: &'a PlaybackReceiptV2Aad,
    pub title: &'a Value,
    pub season: Option<u64>,
    pub episode: Option<u64>,
    pub position_seconds: u64,
    pub duration_seconds: u64,
    pub completed: bool,
    pub ended_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackReceiptV2Message {
    pub aad: PlaybackReceiptV2Aad,
    pub title: Value,
    pub season: Option<u64>,
    pub episode: Option<u64>,
    pub position_seconds: u64,
    pub duration_seconds: u64,
    pub completed: bool,
    pub ended_at: u64,
    pub expires_at: u64,
}

fn title(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.len() != 2 || !object.contains_key("type") || !object.contains_key("id") {
        return None;
    }
    let kind = object["type"].as_str()?;
    let id = object["id"].as_u64()?;
    (matches!(kind, "movie" | "tv") && id > 0 && id <= MAX_SAFE_INTEGER).then_some(kind)
}

pub fn playback_receipt_v2_message(receipt: &PlaybackReceiptV2<'_>) -> Result<Vec<u8>, Error> {
    let aad = &receipt.aad;
    aad.canonical().ok_or(Error::InvalidRequest)?;
    let mut message = json!({
        "binding": aad.binding,
        "completed": receipt.completed,
        "durationSeconds": receipt.duration_seconds,
        "endedAt": receipt.ended_at,
        "expiresAt": receipt.expires_at,
        "generation": aad.generation,
        "household": aad.household,
        "id": aad.id,
        "kid": aad.kid,
        "positionSeconds": receipt.position_seconds,
        "route": aad.route,
        "title": receipt.title,
        "v": 2,
    });
    if let Some(season) = receipt.season {
        message["season"] = json!(season);
    }
    if let Some(episode) = receipt.episode {
        message["episode"] = json!(episode);
    }
    let bytes = serde_json::to_vec(&message).map_err(|_| Error::InvalidRequest)?;
    parse_playback_receipt_v2(&bytes).ok_or(Error::InvalidRequest)?;
    Ok(bytes)
}

pub fn parse_playback_receipt_v2(bytes: &[u8]) -> Option<PlaybackReceiptV2Message> {
    if bytes.len() > MAX_PLAYBACK_RECEIPT_MESSAGE {
        return None;
    }
    let value = canonical_object(bytes)?;
    let object = value.as_object()?;
    let required = [
        "binding", "completed", "durationSeconds", "endedAt", "expiresAt", "generation",
        "household", "id", "kid", "positionSeconds", "route", "title", "v",
    ];
    if object.len() < required.len()
        || object.len() > required.len() + 2
        || required.iter().any(|key| !object.contains_key(*key))
        || object.keys().any(|key| {
            !required.contains(&key.as_str()) && !matches!(key.as_str(), "season" | "episode")
        })
        || object["v"] != json!(2)
    {
        return None;
    }
    let generation = object["generation"].as_u64().filter(|value| *value > 0)?;
    let text = |key: &str| object[key].as_str().filter(|text| lower_hex(text, 16));
    let aad = PlaybackReceiptV2Aad {
        binding: text("binding")?.to_owned(),
        generation,
        household: text("household")?.to_owned(),
        id: text("id")?.to_owned(),
        kid: text("kid")?.to_owned(),
        route: text("route")?.to_owned(),
    };
    let kind = title(&object["title"])?;
    let season = match object.get("season") {
        Some(value) => Some(value.as_u64()?),
        None => None,
    };
    let episode = match object.get("episode") {
        Some(value) => Some(value.as_u64()?),
        None => None,
    };
    if season.is_some() != episode.is_some()
        || season.is_some() && kind != "tv"
        || episode.is_some_and(|number| number == 0 || number > super::MAX_EPISODE)
    {
        return None;
    }
    let position_seconds = object["positionSeconds"].as_u64()?;
    let duration_seconds = object["durationSeconds"].as_u64().filter(|value| *value > 0)?;
    if position_seconds > duration_seconds {
        return None;
    }
    let ended_at = object["endedAt"].as_u64()?;
    let expires_at = object["expiresAt"].as_u64()?;
    if ended_at.checked_add(STALE_MS) != Some(expires_at) {
        return None;
    }
    Some(PlaybackReceiptV2Message {
        aad,
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

#[doc(hidden)]
pub fn seal_playback_receipt_v2(
    public: &[u8],
    receipt: &PlaybackReceiptV2<'_>,
    eseed: &[u8],
) -> Result<String, Error> {
    if public.len() != KEM_PUBLIC_LEN || key_id(public) != receipt.aad.kid {
        return Err(Error::InvalidKey);
    }
    let aad = receipt.aad.canonical().ok_or(Error::InvalidRequest)?;
    let message = playback_receipt_v2_message(receipt)?;
    let sealed = b64url(&hpke_seal(
        public,
        PLAYBACK_RECEIPT_V2_INFO,
        &aad,
        &message,
        eseed,
    )?);
    (sealed.len() <= PLAYBACK_RECEIPT_V2_MAX_SEALED)
        .then_some(sealed)
        .ok_or(Error::InvalidRequest)
}

pub fn seal_playback_receipt_v2_with_rng(
    public: &[u8],
    receipt: &PlaybackReceiptV2<'_>,
    rng: &mut impl rand_core::CryptoRng,
) -> Result<String, Error> {
    let mut eseed = Zeroizing::new([0u8; ESEED_LEN]);
    rng.fill_bytes(&mut *eseed);
    seal_playback_receipt_v2(public, receipt, &*eseed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackReceiptV2Reject {
    DoesNotOpen,
    Malformed,
    WrongHousehold,
    WrongRoute,
    WrongBinding,
    WrongGeneration,
    WrongKey,
    WrongId,
    FromFuture,
    Stale,
    Expired,
    Replay,
}

impl PlaybackReceiptV2Reject {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DoesNotOpen => "does_not_open",
            Self::Malformed => "malformed",
            Self::WrongHousehold => "wrong_household",
            Self::WrongRoute => "wrong_route",
            Self::WrongBinding => "wrong_binding",
            Self::WrongGeneration => "wrong_generation",
            Self::WrongKey => "wrong_key",
            Self::WrongId => "wrong_id",
            Self::FromFuture => "from_future",
            Self::Stale => "stale",
            Self::Expired => "expired",
            Self::Replay => "replay",
        }
    }
}

/// Open with exactly the key named by the drain entry. There is intentionally no collection/key-probing overload.
pub fn open_playback_receipt_v2(
    secret: &[u8; KEM_SECRET_LEN],
    expected: &PlaybackReceiptV2Aad,
    sealed: &str,
    applied: &BTreeSet<String>,
    now: u64,
) -> Result<PlaybackReceiptV2Message, PlaybackReceiptV2Reject> {
    let aad = expected
        .canonical()
        .ok_or(PlaybackReceiptV2Reject::Malformed)?;
    let bytes = (sealed.len() <= PLAYBACK_RECEIPT_V2_MAX_SEALED)
        .then(|| b64url_decode(sealed))
        .flatten()
        .ok_or(PlaybackReceiptV2Reject::DoesNotOpen)?;
    let plaintext = hpke_open(secret, PLAYBACK_RECEIPT_V2_INFO, &aad, &bytes)
        .ok_or(PlaybackReceiptV2Reject::DoesNotOpen)?;
    let message = parse_playback_receipt_v2(&plaintext).ok_or(PlaybackReceiptV2Reject::Malformed)?;
    if message.aad.household != expected.household {
        return Err(PlaybackReceiptV2Reject::WrongHousehold);
    }
    if message.aad.route != expected.route {
        return Err(PlaybackReceiptV2Reject::WrongRoute);
    }
    if message.aad.binding != expected.binding {
        return Err(PlaybackReceiptV2Reject::WrongBinding);
    }
    if message.aad.generation != expected.generation {
        return Err(PlaybackReceiptV2Reject::WrongGeneration);
    }
    if message.aad.kid != expected.kid {
        return Err(PlaybackReceiptV2Reject::WrongKey);
    }
    if message.aad.id != expected.id {
        return Err(PlaybackReceiptV2Reject::WrongId);
    }
    if message.ended_at > now.saturating_add(FUTURE_MS) {
        return Err(PlaybackReceiptV2Reject::FromFuture);
    }
    if message.ended_at < now.saturating_sub(STALE_MS) {
        return Err(PlaybackReceiptV2Reject::Stale);
    }
    if message.expires_at <= now {
        return Err(PlaybackReceiptV2Reject::Expired);
    }
    if applied.contains(&message.aad.id) {
        return Err(PlaybackReceiptV2Reject::Replay);
    }
    Ok(message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackReceiptV2DrainEntry {
    pub generation: u64,
    pub id: String,
    pub kid: String,
    pub sealed: String,
    pub seq: u64,
}

impl PlaybackReceiptV2DrainEntry {
    pub fn aad(&self, household: &str, route: &str, binding: &str) -> PlaybackReceiptV2Aad {
        PlaybackReceiptV2Aad {
            binding: binding.to_owned(),
            generation: self.generation,
            household: household.to_owned(),
            id: self.id.clone(),
            kid: self.kid.clone(),
            route: route.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackReceiptV2Drain {
    pub binding: String,
    pub messages: Vec<PlaybackReceiptV2DrainEntry>,
    pub route: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDrain {
    binding: String,
    messages: Vec<RawDrainEntry>,
    route: String,
    v: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDrainEntry {
    generation: u64,
    id: String,
    kid: String,
    sealed: String,
    seq: u64,
}

/// Strict response decoding preserves each entry's complete AAD metadata and increasing sequence order.
pub fn parse_playback_receipt_v2_drain(bytes: &[u8]) -> Option<PlaybackReceiptV2Drain> {
    if bytes.len() > PLAYBACK_RECEIPT_V2_MAX_DRAIN
        || !playback_receipt_v2_exact_integers(bytes)
    {
        return None;
    }
    let raw: RawDrain = serde_json::from_slice(bytes).ok()?;
    if raw.v != 2
        || raw.messages.len() > PLAYBACK_RECEIPT_V2_MAX_ROUTE_QUEUE
        || !lower_hex(&raw.binding, 16)
        || !lower_hex(&raw.route, 16)
    {
        return None;
    }
    let mut previous = 0;
    let mut messages = Vec::with_capacity(raw.messages.len());
    for entry in raw.messages {
        if entry.generation == 0
            || entry.seq == 0
            || entry.seq <= previous
            || !lower_hex(&entry.id, 16)
            || !lower_hex(&entry.kid, 16)
            || entry.sealed.len() > PLAYBACK_RECEIPT_V2_MAX_SEALED
            || b64url_decode(&entry.sealed).is_none()
        {
            return None;
        }
        previous = entry.seq;
        messages.push(PlaybackReceiptV2DrainEntry {
            generation: entry.generation,
            id: entry.id,
            kid: entry.kid,
            sealed: entry.sealed,
            seq: entry.seq,
        });
    }
    Some(PlaybackReceiptV2Drain {
        binding: raw.binding,
        messages,
        route: raw.route,
    })
}

pub fn playback_receipt_v2_retired_tag(
    key: &[u8; 32],
    household: &str,
    route: &str,
) -> Option<[u8; 32]> {
    let household = unhex::<16>(household)?;
    let route = unhex::<16>(route)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    mac.update(PLAYBACK_RECEIPT_RETIRED_V2_DOMAIN);
    mac.update(&[0]);
    mac.update(&household);
    mac.update(&route);
    Some(mac.finalize().into_bytes().into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackReceiptV2RetirementKey {
    pub version: u64,
    pub key: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackReceiptV2Admission {
    Admitted,
    GenerationConflict,
    Retired,
    HistoryFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackReceiptV2HistoryError {
    Invalid,
    NotLive,
    NotTombstoned,
    UnknownKeyVersion,
    PreviousKeyInUse,
}

/// Edge's bounded reservation model. Store adapters must commit each successful method atomically with their own
/// durable metadata; failed methods do not mutate this value.
#[derive(Debug, Clone)]
pub struct PlaybackReceiptV2History {
    live: BTreeSet<String>,
    tombstones: BTreeSet<String>,
    tags: BTreeSet<(u64, [u8; 32])>,
    current: PlaybackReceiptV2RetirementKey,
    previous: Option<PlaybackReceiptV2RetirementKey>,
}

impl PlaybackReceiptV2History {
    pub fn new(current: PlaybackReceiptV2RetirementKey) -> Result<Self, PlaybackReceiptV2HistoryError> {
        Self::restore(current, None, [], [], [])
    }

    /// Rebuild the pure admission state from one durable household transaction. Unknown key versions, overlapping
    /// route states and over-cap snapshots fail closed rather than dropping non-resurrection evidence.
    pub fn restore(
        current: PlaybackReceiptV2RetirementKey,
        previous: Option<PlaybackReceiptV2RetirementKey>,
        live: impl IntoIterator<Item = String>,
        tombstones: impl IntoIterator<Item = String>,
        tags: impl IntoIterator<Item = (u64, [u8; 32])>,
    ) -> Result<Self, PlaybackReceiptV2HistoryError> {
        if current.version == 0
            || current.version > MAX_SAFE_INTEGER
            || previous.as_ref().is_some_and(|key| {
                key.version == 0
                    || key.version > MAX_SAFE_INTEGER
                    || key.version == current.version
            })
        {
            return Err(PlaybackReceiptV2HistoryError::Invalid);
        }
        let live: BTreeSet<_> = live.into_iter().collect();
        let tombstones: BTreeSet<_> = tombstones.into_iter().collect();
        let tags: BTreeSet<_> = tags.into_iter().collect();
        if live.iter().chain(&tombstones).any(|route| !lower_hex(route, 16))
            || live.iter().any(|route| tombstones.contains(route))
            || tags.iter().any(|(version, _)| {
                *version != current.version
                    && previous.as_ref().is_none_or(|key| key.version != *version)
            })
            || live.len() + tombstones.len() + tags.len()
                > PLAYBACK_RECEIPT_V2_HISTORY_CAPACITY
        {
            return Err(PlaybackReceiptV2HistoryError::Invalid);
        }
        Ok(Self {
            live,
            tombstones,
            tags,
            current,
            previous,
        })
    }

    pub fn reserved(&self) -> usize {
        self.live.len() + self.tombstones.len() + self.tags.len()
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        (self.live.len(), self.tombstones.len(), self.tags.len())
    }

    pub fn register(&mut self, household: &str, route: &str) -> PlaybackReceiptV2Admission {
        if !lower_hex(household, 16) || !lower_hex(route, 16) {
            return PlaybackReceiptV2Admission::GenerationConflict;
        }
        if self.live.contains(route) {
            return PlaybackReceiptV2Admission::GenerationConflict;
        }
        if self.tombstones.contains(route)
            || self.retained_keys().any(|key| {
                playback_receipt_v2_retired_tag(&key.key, household, route)
                    .is_some_and(|tag| self.tags.contains(&(key.version, tag)))
            })
        {
            return PlaybackReceiptV2Admission::Retired;
        }
        if self.reserved() == PLAYBACK_RECEIPT_V2_HISTORY_CAPACITY {
            return PlaybackReceiptV2Admission::HistoryFull;
        }
        self.live.insert(route.to_owned());
        PlaybackReceiptV2Admission::Admitted
    }

    pub fn retire(&mut self, route: &str) -> Result<(), PlaybackReceiptV2HistoryError> {
        if !self.live.remove(route) {
            return Err(PlaybackReceiptV2HistoryError::NotLive);
        }
        self.tombstones.insert(route.to_owned());
        debug_assert!(self.reserved() <= PLAYBACK_RECEIPT_V2_HISTORY_CAPACITY);
        Ok(())
    }

    pub fn compact(
        &mut self,
        household: &str,
        route: &str,
        key_version: u64,
    ) -> Result<(), PlaybackReceiptV2HistoryError> {
        if !self.tombstones.contains(route) {
            return Err(PlaybackReceiptV2HistoryError::NotTombstoned);
        }
        let key = self
            .retained_keys()
            .find(|key| key.version == key_version)
            .ok_or(PlaybackReceiptV2HistoryError::UnknownKeyVersion)?;
        let tag = playback_receipt_v2_retired_tag(&key.key, household, route)
            .ok_or(PlaybackReceiptV2HistoryError::Invalid)?;
        self.tombstones.remove(route);
        self.tags.insert((key_version, tag));
        debug_assert!(self.reserved() <= PLAYBACK_RECEIPT_V2_HISTORY_CAPACITY);
        Ok(())
    }

    pub fn rotate_retirement_key(
        &mut self,
        next: PlaybackReceiptV2RetirementKey,
    ) -> Result<(), PlaybackReceiptV2HistoryError> {
        if next.version == 0
            || next.version > MAX_SAFE_INTEGER
            || next.version == self.current.version
            || self.previous.as_ref().is_some_and(|key| key.version == next.version)
        {
            return Err(PlaybackReceiptV2HistoryError::Invalid);
        }
        if self.previous.as_ref().is_some_and(|previous| {
            self.tags.iter().any(|(version, _)| *version == previous.version)
        }) {
            return Err(PlaybackReceiptV2HistoryError::PreviousKeyInUse);
        }
        self.previous = Some(std::mem::replace(&mut self.current, next));
        Ok(())
    }

    pub fn retained_versions(&self) -> Vec<u64> {
        self.retained_keys().map(|key| key.version).collect()
    }

    fn retained_keys(&self) -> impl Iterator<Item = &PlaybackReceiptV2RetirementKey> {
        std::iter::once(&self.current).chain(self.previous.iter())
    }
}
