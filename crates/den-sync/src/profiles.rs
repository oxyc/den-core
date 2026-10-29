//! Household profiles v1 (`den-spec wire/profiles-v1.md`). This is only the pure directory, content-policy, and
//! lifecycle-decision boundary: it has no clock, storage, network, split, reset, or wire-minimum effects.

use crate::library_v4::{codec, jcs};
use crate::wire::{Stamp, MAX_SAFE_INTEGER};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::char::{canonical_combining_class, compose, decompose_canonical};

const ROOT_MEMBERS: &[&str] = &["format", "kind", "schema", "owner", "migration", "profiles"];
const ENTRY_MEMBERS: &[&str] = &["id", "role", "display", "library", "policy", "deleted"];
const APPROVAL_ROOT_MEMBERS: &[&str] = &["format", "kind", "schema", "kid", "approvals"];
/// The one legacy preference that is not a preference of a profile: decoded into Owner policy, then dropped.
const LEGACY_MATURITY_KEY: &str = "den.maturityCeiling";

fn object<'a>(value: &'a Value, error: &str) -> Result<&'a Map<String, Value>, String> {
    value.as_object().ok_or_else(|| error.into())
}

fn exact_members(map: &Map<String, Value>, members: &[&str]) -> bool {
    map.len() == members.len() && members.iter().all(|member| map.contains_key(*member))
}

fn profile_id(value: &Value) -> Option<&str> {
    value.as_str().filter(|id| valid_profile_id(id))
}

fn valid_profile_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_digest(value: &Value) -> bool {
    value.as_str().is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn stamp(value: &Value) -> Result<Stamp, String> {
    let parts = value
        .as_array()
        .filter(|parts| parts.len() == 3)
        .ok_or("invalid_stamp")?;
    let time = parts[0]
        .as_u64()
        .filter(|time| *time <= MAX_SAFE_INTEGER)
        .ok_or("invalid_stamp")?;
    let counter = parts[1]
        .as_u64()
        .filter(|counter| *counter <= MAX_SAFE_INTEGER)
        .ok_or("invalid_stamp")?;
    let device = parts[2]
        .as_str()
        .filter(|device| {
            device.is_empty()
                || (device.len() == 16
                    && device
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        })
        .ok_or("invalid_stamp")?;
    Ok(Stamp(time as i64, counter, device.into()))
}

fn stamped<'a>(value: &'a Value, error: &str) -> Result<&'a Map<String, Value>, String> {
    let field = object(value, error)?;
    if !field.contains_key("value") || !field.contains_key("at") || stamp(&field["at"]).is_err() {
        return Err(error.into());
    }
    Ok(field)
}

fn display(value: &Value) -> Result<(), String> {
    let display = stamped(value, "invalid_display")?;
    let value = object(&display["value"], "invalid_display")?;
    let name = value["name"].as_str().ok_or("invalid_display")?;
    if name.is_empty() || name.chars().count() > 64 {
        return Err("invalid_display".into());
    }
    if !is_nfc(name) {
        return Err("display_not_nfc".into());
    }
    if let Some(avatar) = value.get("avatar") {
        let avatar = avatar.as_str().filter(|avatar| {
            (1..=64).contains(&avatar.len())
                && avatar
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
        if avatar.is_none() {
            return Err("invalid_avatar".into());
        }
    }
    Ok(())
}

/// Whether `text` is in Normalization Form C: `text.nfc().eq(text.chars())`, by the same algorithm (UAX #15 canonical
/// decomposition, canonical ordering, then composition) over unicode-normalization's canonical-only functions.
/// Its `nfc()` chooses canonical or compatibility decomposition at run time, so it also links the compatibility
/// tables NFC never reads: about 65 KB of the web module.
fn is_nfc(text: &str) -> bool {
    let mut chars = Vec::new();
    for c in text.chars() {
        decompose_canonical(c, |d| chars.push((canonical_combining_class(d), d)));
    }
    // Every run of non-starters in canonical order; stable, so equal classes keep their order.
    for run in chars.split_mut(|&(class, _)| class == 0) {
        run.sort_by_key(|&(class, _)| class);
    }
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    // The starter a character may compose with, and the class of the last character kept after it.
    let mut starter = None;
    let mut last = None;
    for (class, c) in chars {
        if let Some(s) = starter {
            // Blocked when a character between them is a starter or has at least this class.
            if last.is_none_or(|l| l < class) {
                if let Some(composed) = compose(out[s], c) {
                    out[s] = composed;
                    continue;
                }
            }
        }
        if class == 0 {
            starter = Some(out.len());
            last = None;
        } else {
            last = Some(class);
        }
        out.push(c);
    }
    out.into_iter().eq(text.chars())
}

fn standard_base64(bytes: &[u8]) -> String {
    let mut text = codec::base64(bytes).replace('-', "+").replace('_', "/");
    while !text.len().is_multiple_of(4) {
        text.push('=');
    }
    text
}

fn library(value: &Value) -> Result<(), String> {
    let library = stamped(value, "invalid_library")?;
    let value = object(&library["value"], "invalid_library")?;
    if !value.contains_key("keyVersion")
        || !value.contains_key("key")
        || value["keyVersion"]
            .as_u64()
            .filter(|version| (1..=u64::from(u32::MAX)).contains(version))
            .is_none()
    {
        return Err("invalid_library".into());
    }
    let key = value["key"].as_str().ok_or("invalid_library_key")?;
    let bytes = codec::unbase64(key).map_err(|_| "invalid_library_key")?;
    if bytes.len() != 32 || key != standard_base64(&bytes) {
        return Err("invalid_library_key".into());
    }
    Ok(())
}

fn policy_value(value: &Value) -> Result<(), String> {
    let value = object(value, "invalid_policy")?;
    if !value.contains_key("maximumAge")
        || !value.contains_key("unknownRatings")
        || (!value["maximumAge"].is_null()
            && value["maximumAge"]
                .as_u64()
                .filter(|age| *age <= 18)
                .is_none())
        || !matches!(value["unknownRatings"].as_str(), Some("allow" | "block"))
    {
        return Err("invalid_policy".into());
    }
    Ok(())
}

fn policy(value: &Value) -> Result<(), String> {
    let policy = stamped(value, "invalid_policy")?;
    policy_value(&policy["value"])
}

fn deleted(value: &Value) -> Result<(), String> {
    let deleted = stamped(value, "invalid_deleted")?;
    if !deleted["value"].is_boolean() {
        return Err("invalid_deleted".into());
    }
    Ok(())
}

fn entry(value: &Value) -> Result<(), String> {
    let entry = object(value, "invalid_profile")?;
    if !ENTRY_MEMBERS
        .iter()
        .all(|member| entry.contains_key(*member))
        || profile_id(&entry["id"]).is_none()
        || !matches!(entry["role"].as_str(), Some("owner" | "adult" | "kid"))
    {
        return Err("invalid_profile".into());
    }
    display(&entry["display"])?;
    library(&entry["library"])?;
    policy(&entry["policy"])?;
    deleted(&entry["deleted"])
}

fn migration(value: &Value, owner: &str) -> Result<(), String> {
    let migration = object(value, "invalid_migration")?;
    if !exact_members(
        migration,
        &[
            "version",
            "attempt",
            "source",
            "backup",
            "ownerProfile",
            "ownerHead",
        ],
    ) || migration["version"].as_u64() != Some(1)
        || profile_id(&migration["attempt"]).is_none()
        || migration["ownerProfile"].as_str() != Some(owner)
        || !valid_digest(&migration["ownerHead"])
    {
        return Err("invalid_migration".into());
    }
    let source = object(&migration["source"], "invalid_migration")?;
    if !exact_members(source, &["generation", "head"])
        || source["generation"]
            .as_u64()
            .filter(|generation| *generation <= MAX_SAFE_INTEGER)
            .is_none()
        || !valid_digest(&source["head"])
    {
        return Err("invalid_migration".into());
    }
    let backup = object(&migration["backup"], "invalid_migration")?;
    if !exact_members(backup, &["id", "digest"])
        || backup["id"]
            .as_str()
            .filter(|id| (1..=256).contains(&id.chars().count()))
            .is_none()
        || !valid_digest(&backup["digest"])
    {
        return Err("invalid_migration".into());
    }
    Ok(())
}

fn validate_directory(document: &Value, require_migration: bool) -> Result<Value, String> {
    let root = object(document, "invalid_profiles")?;
    if root.get("format").and_then(Value::as_u64) != Some(4)
        || root.get("kind").and_then(Value::as_str) != Some("profiles")
        || root.get("schema").and_then(Value::as_u64) != Some(1)
    {
        return Err("invalid_profiles".into());
    }
    let owner = root
        .get("owner")
        .and_then(profile_id)
        .ok_or("invalid_owner")?;
    let profiles = root
        .get("profiles")
        .and_then(Value::as_object)
        .filter(|profiles| !profiles.is_empty())
        .ok_or("invalid_profiles")?;
    if require_migration && !root.contains_key("migration") {
        return Err("migration_missing".into());
    }
    let mut owners = 0;
    let mut live_keys = BTreeSet::new();
    for (map_id, value) in profiles {
        if profile_id(&Value::String(map_id.clone())).is_none() {
            return Err("invalid_profile_id".into());
        }
        entry(value)?;
        let profile = value.as_object().expect("entry checked");
        if profile["id"].as_str() != Some(map_id) {
            return Err("profile_id_mismatch".into());
        }
        if profile["role"] == "owner" {
            owners += 1;
        }
        if !profile["deleted"]["value"].as_bool().unwrap_or(true)
            && !live_keys.insert(
                profile["library"]["value"]["key"]
                    .as_str()
                    .unwrap_or_default(),
            )
        {
            return Err("duplicate_library_key".into());
        }
    }
    let owner_entry = profiles.get(owner).ok_or("owner_missing")?;
    if owner_entry["role"] != "owner" {
        return Err("owner_role".into());
    }
    if owner_entry["deleted"]["value"] != false {
        return Err("owner_deleted".into());
    }
    if owners != 1 {
        return Err("multiple_owners".into());
    }
    if let Some(value) = root.get("migration") {
        migration(value, owner)?;
    }
    Ok(document.clone())
}

/// Validate and return the unchanged logical document. Serialization at the boundary supplies its canonical JSON.
pub fn validate(document: &Value) -> Result<Value, String> {
    validate_directory(document, false)
}

/// Validate constructor input or, with `stored`, require immutable migration evidence suitable for publication.
pub fn validate_input(document: &Value, stored: bool) -> Result<Value, String> {
    validate_directory(document, stored)
}

fn depth_within(value: &Value, maximum: usize) -> bool {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, parent_depth)) = pending.pop() {
        let depth = parent_depth + 1;
        let children: Vec<&Value> = match value {
            Value::Array(items) => items.iter().collect(),
            Value::Object(map) => map.values().collect(),
            _ => continue,
        };
        if depth > maximum {
            return false;
        }
        pending.extend(children.into_iter().map(|child| (child, depth)));
    }
    true
}

/// Validate one stored profile directory and return the only seal-ready library-v4 document bytes.
pub fn materialize(document: &Value) -> Result<Value, String> {
    let root = document
        .as_object()
        .ok_or_else(|| "invalid_profiles_materialize".to_string())?;
    if !root.contains_key("migration") {
        return Err("profiles_migration_required".into());
    }
    // Bound arbitrary stored extensions before validation clones the directory or
    // any recursive serializer sees it.
    if !depth_within(document, codec::MAX_DEPTH) {
        return Err("invalid_profiles_materialize".into());
    }
    let directory = validate_directory(document, true)
        .map_err(|_| "invalid_profiles_materialize".to_string())?;
    let canonical = jcs::jcs(&directory);
    if canonical.len() > codec::MAX_JCS {
        return Err("profiles_materialize_too_large".into());
    }
    let plaintext =
        codec::compress(&directory).map_err(|_| "profiles_materialize_too_large".to_string())?;
    let inflated = plaintext
        .strip_prefix(&[codec::COMPRESSED])
        .ok_or("profiles_materialize_roundtrip")
        .and_then(|compressed| {
            codec::inflate(compressed).map_err(|_| "profiles_materialize_roundtrip")
        })?;
    let decoded: Value = serde_json::from_slice(&inflated)
        .map_err(|_| "profiles_materialize_roundtrip".to_string())?;
    if !depth_within(&decoded, codec::MAX_DEPTH)
        || validate_directory(&decoded, true).is_err()
        || inflated != canonical
        || jcs::jcs(&decoded) != canonical
    {
        return Err("profiles_materialize_roundtrip".into());
    }
    Ok(json!({
        "directory": directory,
        "jcs": codec::base64(&canonical),
        "plaintext": codec::base64(&plaintext),
    }))
}

fn rank(set: &Map<String, Value>) -> (bool, Vec<u8>) {
    (!set.is_empty(), jcs::jcs(&Value::Object(set.clone())))
}

fn later(a: &Value, b: &Value) -> Value {
    let order = stamp(&a["at"])
        .expect("validated stamp")
        .cmp(&stamp(&b["at"]).expect("validated stamp"))
        .then_with(|| jcs::cmp(a, b));
    if order == Ordering::Less { b } else { a }.clone()
}

fn library_later(a: &Value, b: &Value) -> Value {
    let order = a["value"]["keyVersion"]
        .as_u64()
        .cmp(&b["value"]["keyVersion"].as_u64())
        .then_with(|| {
            stamp(&a["at"])
                .expect("validated stamp")
                .cmp(&stamp(&b["at"]).expect("validated stamp"))
        })
        .then_with(|| jcs::cmp(a, b));
    if order == Ordering::Less { b } else { a }.clone()
}

fn deleted_later(a: &Value, b: &Value) -> Value {
    let order = a["value"]
        .as_bool()
        .cmp(&b["value"].as_bool())
        .then_with(|| {
            stamp(&a["at"])
                .expect("validated stamp")
                .cmp(&stamp(&b["at"]).expect("validated stamp"))
        })
        .then_with(|| jcs::cmp(a, b));
    if order == Ordering::Less { b } else { a }.clone()
}

fn monotone_entry(entry: &Map<String, Value>) -> Stamp {
    ["display", "policy"]
        .iter()
        .map(|field| stamp(&entry[*field]["at"]).expect("validated stamp"))
        .max()
        .unwrap_or_default()
}

fn unknown(entry: &Map<String, Value>, known: &[&str]) -> Map<String, Value> {
    entry
        .iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn merge_entry(a: &Value, b: &Value) -> Result<Value, String> {
    let (a, b) = (a.as_object().unwrap(), b.as_object().unwrap());
    if a["id"] != b["id"] || a["role"] != b["role"] {
        return Err("identity_conflict".into());
    }
    let (ua, ub) = (unknown(a, ENTRY_MEMBERS), unknown(b, ENTRY_MEMBERS));
    let mut out = if (monotone_entry(a), rank(&ua)) < (monotone_entry(b), rank(&ub)) {
        ub
    } else {
        ua
    };
    out.insert("id".into(), a["id"].clone());
    out.insert("role".into(), a["role"].clone());
    out.insert("display".into(), later(&a["display"], &b["display"]));
    out.insert(
        "library".into(),
        library_later(&a["library"], &b["library"]),
    );
    out.insert("policy".into(), later(&a["policy"], &b["policy"]));
    out.insert(
        "deleted".into(),
        deleted_later(&a["deleted"], &b["deleted"]),
    );
    Ok(Value::Object(out))
}

fn monotone_document(root: &Map<String, Value>) -> Stamp {
    root["profiles"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, entry)| entry.as_object())
        .map(monotone_entry)
        .max()
        .unwrap_or_default()
}

/// Validate both versions, merge the profile-id union, and validate the converged result.
pub fn merge(a: &Value, b: &Value) -> Result<Value, String> {
    validate_directory(a, true)?;
    validate_directory(b, true)?;
    let (a, b) = (a.as_object().unwrap(), b.as_object().unwrap());
    if a["owner"] != b["owner"] || a["migration"] != b["migration"] {
        return Err("identity_conflict".into());
    }
    let (ua, ub) = (unknown(a, ROOT_MEMBERS), unknown(b, ROOT_MEMBERS));
    let mut out = if (monotone_document(a), rank(&ua)) < (monotone_document(b), rank(&ub)) {
        ub
    } else {
        ua
    };
    out.insert("format".into(), json!(4));
    out.insert("kind".into(), json!("profiles"));
    out.insert("schema".into(), json!(1));
    out.insert("owner".into(), a["owner"].clone());
    out.insert("migration".into(), a["migration"].clone());
    let (ap, bp) = (
        a["profiles"].as_object().unwrap(),
        b["profiles"].as_object().unwrap(),
    );
    let mut profiles = Map::new();
    for id in ap.keys().chain(bp.keys()).collect::<BTreeSet<_>>() {
        let value = match (ap.get(id), bp.get(id)) {
            (Some(a), Some(b)) => merge_entry(a, b)?,
            (Some(value), None) | (None, Some(value)) => value.clone(),
            (None, None) => unreachable!(),
        };
        profiles.insert(id.clone(), value);
    }
    out.insert("profiles".into(), Value::Object(profiles));
    validate_directory(&Value::Object(out), true)
}

/// Apply one validated owner-authorized directory operation without inventing clocks or keys.
pub fn apply(
    document: &Value,
    operation: &str,
    profile: &str,
    entry_value: Option<&Value>,
    value: Option<&Value>,
    at: Option<&Value>,
) -> Result<Value, String> {
    let mut result = validate_directory(document, true)?;
    if !valid_profile_id(profile) {
        return Err("invalid_profile_id".into());
    }
    let root = result.as_object_mut().expect("validated directory");
    let owner = root["owner"].as_str().expect("validated owner").to_owned();
    let profiles = root["profiles"]
        .as_object_mut()
        .expect("validated profiles");
    if operation == "create" {
        if profiles.contains_key(profile) {
            return Err("profile_exists".into());
        }
        let mut candidate = entry_value.ok_or("invalid_operation_input")?.clone();
        let candidate_object = candidate.as_object().ok_or("invalid_create_profile")?;
        if candidate_object.get("id").and_then(Value::as_str) != Some(profile)
            || candidate_object.get("role").and_then(Value::as_str) != Some("kid")
            || candidate["library"]["value"]["keyVersion"].as_u64() != Some(1)
            || candidate["deleted"]["value"].as_bool() != Some(false)
        {
            return Err("invalid_create_profile".into());
        }
        let maximum_age = candidate_object
            .get("policy")
            .and_then(Value::as_object)
            .and_then(|policy| policy.get("value"))
            .and_then(Value::as_object)
            .and_then(|policy| policy.get("maximumAge"))
            .and_then(Value::as_u64)
            .filter(|age| *age <= 18)
            .ok_or("invalid_create_policy")?;
        candidate["policy"]["value"]["maximumAge"] = json!(maximum_age);
        candidate["policy"]["value"]["unknownRatings"] = json!("block");
        profiles.insert(profile.into(), candidate);
        return validate_directory(&result, true);
    }

    let profile_entry = profiles.get_mut(profile).ok_or("profile_not_live")?;
    if profile_entry["deleted"]["value"] != false {
        return Err("profile_not_live".into());
    }
    let profile_entry = profile_entry
        .as_object_mut()
        .expect("validated profile entry");
    match operation {
        "rename" => {
            let next =
                replace_stamped_object(&profile_entry["display"], value, at, &["name", "avatar"])?;
            display(&next)?;
            profile_entry.insert("display".into(), next);
        }
        "policy" => {
            let next = replace_stamped_object(
                &profile_entry["policy"],
                value,
                at,
                &["maximumAge", "unknownRatings"],
            )?;
            policy(&next)?;
            profile_entry.insert("policy".into(), next);
        }
        "library" => {
            let next = replace_stamped_object(
                &profile_entry["library"],
                value,
                at,
                &["keyVersion", "key"],
            )?;
            library(&next)?;
            let previous = profile_entry["library"]["value"]["keyVersion"]
                .as_u64()
                .expect("validated key version");
            if next["value"]["keyVersion"].as_u64() != previous.checked_add(1) {
                return Err("invalid_key_version".into());
            }
            profile_entry.insert("library".into(), next);
        }
        "delete" => {
            if profile == owner {
                return Err("owner_deleted".into());
            }
            let mut next = profile_entry["deleted"].clone();
            next["value"] = json!(true);
            next["at"] = at.ok_or("invalid_operation_input")?.clone();
            deleted(&next)?;
            profile_entry.insert("deleted".into(), next);
        }
        _ => return Err("invalid_operation".into()),
    }
    validate_directory(&result, true)
}

fn create_journal_entry(value: &Value) -> Result<(String, String, Value), String> {
    let journal = object(value, "invalid_create_journal")?;
    if !exact_members(
        journal,
        &[
            "version",
            "profile",
            "key",
            "display",
            "libraryAt",
            "maximumAge",
            "policyAt",
            "deletedAt",
        ],
    ) || journal["version"].as_u64() != Some(1)
    {
        return Err("invalid_create_journal".into());
    }
    let profile = profile_id(&journal["profile"])
        .ok_or("invalid_create_journal")?
        .to_owned();
    let key = journal["key"]
        .as_str()
        .ok_or("invalid_create_journal")?
        .to_owned();
    let key_bytes = codec::unbase64(&key).map_err(|_| "invalid_create_journal")?;
    if key_bytes.len() != 32 || key != standard_base64(&key_bytes) {
        return Err("invalid_create_journal".into());
    }
    let maximum_age = journal["maximumAge"]
        .as_u64()
        .filter(|age| *age <= 18)
        .ok_or("invalid_create_journal")?;
    let display_value = object(&journal["display"], "invalid_create_journal")?;
    if !exact_members(display_value, &["value", "at"])
        || stamp(&display_value["at"]).is_err()
        || display(&journal["display"]).is_err()
        || stamp(&journal["libraryAt"]).is_err()
        || stamp(&journal["policyAt"]).is_err()
        || stamp(&journal["deletedAt"]).is_err()
    {
        return Err("invalid_create_journal".into());
    }
    let profile_entry = json!({
        "id": profile,
        "role": "kid",
        "display": journal["display"].clone(),
        "library": {
            "value": {"keyVersion": 1, "key": key},
            "at": journal["libraryAt"].clone(),
        },
        "policy": {
            "value": {"maximumAge": maximum_age, "unknownRatings": "block"},
            "at": journal["policyAt"].clone(),
        },
        "deleted": {"value": false, "at": journal["deletedAt"].clone()},
    });
    entry(&profile_entry).map_err(|_| "invalid_create_journal")?;
    Ok((profile, key, profile_entry))
}

fn create_authority(value: &Value) -> Result<(u64, String), String> {
    let authority = object(value, "invalid_create_authority")?;
    if !exact_members(authority, &["base", "generation"])
        || authority["base"]
            .as_u64()
            .filter(|base| *base <= MAX_SAFE_INTEGER)
            .is_none()
    {
        return Err("invalid_create_authority".into());
    }
    let generation = authority["generation"]
        .as_str()
        .filter(|generation| {
            !generation.is_empty()
                && generation.trim() == *generation
                && generation.chars().count() <= 256
        })
        .ok_or("invalid_create_authority")?;
    Ok((authority["base"].as_u64().unwrap(), generation.into()))
}

fn create_bool(input: &Map<String, Value>, key: &str) -> Result<bool, String> {
    input.get(key).map_or(Ok(false), |value| {
        value.as_bool().ok_or("invalid_create_evidence".into())
    })
}

/// Decide the next pure crash-safe Kid creation effect from one durable journal and authenticated observations.
pub fn create_next(input: &Value) -> Result<Value, String> {
    let input = object(input, "invalid_create_evidence")?;
    let state = input
        .get("state")
        .and_then(Value::as_str)
        .ok_or("invalid_create_state")?;
    if !matches!(
        state,
        "idle"
            | "journaled"
            | "library-created"
            | "ready"
            | "committed"
            | "conflict"
            | "cas-unknown"
            | "cancelling"
            | "active"
    ) {
        return Err("invalid_create_state".into());
    }
    for key in ["explicitIntent", "householdFresh", "candidateRetired"] {
        create_bool(input, key)?;
    }
    if state == "idle" && !create_bool(input, "explicitIntent")? {
        return Ok(json!({"effect":"no-op"}));
    }

    let (profile, key, entry) = create_journal_entry(input.get("journal").unwrap_or(&Value::Null))?;
    match state {
        "idle" => return Ok(json!({"effect":"persist-journal"})),
        "journaled" => {
            return Ok(json!({"effect":"create-profile-library", "profile":profile, "key":key}));
        }
        "library-created" => {
            return Ok(json!({"effect":"apply-create", "profile":profile, "entry":entry}));
        }
        _ => {}
    }

    let candidate = validate_directory(input.get("candidate").unwrap_or(&Value::Null), true)
        .map_err(|_| "invalid_create_candidate".to_string())?;
    let candidate_entry = candidate["profiles"]
        .as_object()
        .and_then(|profiles| profiles.get(&profile))
        .ok_or("create_candidate_mismatch")?;
    if jcs::jcs(candidate_entry) != jcs::jcs(&entry) {
        return Err("create_candidate_mismatch".into());
    }

    let cas = |directory: Value, authority: &Value| -> Result<Value, String> {
        let (base, generation) = create_authority(authority)?;
        Ok(json!({
            "effect":"cas-directory",
            "base":base,
            "generation":generation,
            "directory":directory,
        }))
    };
    if state == "ready" {
        return cas(candidate, input.get("authority").unwrap_or(&Value::Null));
    }
    if !create_bool(input, "householdFresh")? {
        return Ok(json!({"effect":"read-directory"}));
    }
    let current = validate_directory(input.get("current").unwrap_or(&Value::Null), true)
        .map_err(|_| "invalid_create_current".to_string())?;
    let authority = input.get("authority").unwrap_or(&Value::Null);
    create_authority(authority)?;
    let merged = merge(&candidate, &current).map_err(|_| "create_directory_conflict")?;
    let candidate_won = jcs::jcs(&merged) == jcs::jcs(&current);
    let current_profiles = current["profiles"]
        .as_object()
        .expect("validated current profiles");
    let current_keys: BTreeSet<&str> = current_profiles
        .values()
        .filter(|entry| entry["deleted"]["value"] == false)
        .filter_map(|entry| entry["library"]["value"]["key"].as_str())
        .collect();
    let retired = create_bool(input, "candidateRetired")?;
    if retired && current_keys.contains(key.as_str()) {
        return Err("create_key_referenced".into());
    }
    let authoritative = current_profiles.get(&profile);
    let displaced = authoritative.is_some_and(|entry| {
        entry["deleted"]["value"] == true
            || entry["library"]["value"]["key"].as_str() != Some(key.as_str())
    });
    let retire_displaced = || {
        (displaced && !current_keys.contains(key.as_str()) && !retired)
            .then(|| json!({"effect":"retire-profile-library", "profile":profile, "key":key}))
    };

    if state == "cancelling" {
        if let Some(authoritative) = authoritative {
            if !candidate_won {
                return Err("create_directory_conflict".into());
            }
            if let Some(effect) = retire_displaced() {
                return Ok(effect);
            }
            if authoritative["deleted"]["value"] == true {
                return Ok(json!({"effect":"discard-journal"}));
            }
            return Ok(json!({"effect":"adopt", "directory":current}));
        }
        if current_keys.contains(key.as_str()) {
            return Err("create_key_referenced".into());
        }
        if !retired {
            return Ok(json!({"effect":"retire-profile-library", "profile":profile, "key":key}));
        }
        return Ok(json!({"effect":"discard-journal"}));
    }

    if candidate_won {
        if let Some(effect) = retire_displaced() {
            return Ok(effect);
        }
        if authoritative.is_some_and(|entry| entry["deleted"]["value"] == true) || state == "active"
        {
            return Ok(json!({"effect":"discard-journal"}));
        }
        return Ok(json!({"effect":"adopt", "directory":current}));
    }
    if matches!(state, "committed" | "active") {
        return Err("create_not_committed".into());
    }
    cas(merged, authority)
}

fn replace_stamped_object(
    current: &Value,
    replacement: Option<&Value>,
    at: Option<&Value>,
    known: &[&str],
) -> Result<Value, String> {
    let mut next = current.clone();
    let mut value = object(&current["value"], "invalid_operation_input")?.clone();
    let replacement = object(
        replacement.ok_or("invalid_operation_input")?,
        "invalid_operation_input",
    )?;
    for key in known {
        if let Some(replacement) = replacement.get(*key) {
            value.insert((*key).into(), replacement.clone());
        } else {
            value.remove(*key);
        }
    }
    next["value"] = Value::Object(value);
    next["at"] = at.ok_or("invalid_operation_input")?.clone();
    Ok(next)
}

/// Partition preferences for the supported browser-local household conversion.
///
/// Every complete stamped preference moves to Owner without interpretation. The legacy maturity value is returned
/// separately for policy normalization, and the older household keeps no preference row.
pub fn partition_preferences(values: &Value) -> Result<Value, String> {
    let values = values.as_object().ok_or("invalid_preferences")?;
    let mut owner = values.clone();
    let owner_maturity = owner.remove(LEGACY_MATURITY_KEY).unwrap_or(Value::Null);
    Ok(json!({
        "household": {},
        "owner": owner,
        "ownerMaturity": owner_maturity,
    }))
}

/// Decide the next pure compound-reset effect. The caller remains responsible for every returned effect.
pub fn reset_next(input: &Value) -> Result<Value, String> {
    let input = object(input, "invalid_evidence")?;
    let directory = input.get("directory").ok_or("invalid_evidence")?;
    validate_directory(directory, true)?;
    let directory_profiles = directory["profiles"]
        .as_object()
        .expect("validated profiles");
    let live_ids = directory_profiles
        .iter()
        .filter(|(_, entry)| entry["deleted"]["value"] == false)
        .map(|(id, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    let fence = input
        .get("fence")
        .and_then(Value::as_object)
        .ok_or("invalid_reset_fence")?;
    if !exact_members(fence, &["generation", "head"])
        || fence["generation"]
            .as_u64()
            .filter(|generation| *generation <= MAX_SAFE_INTEGER)
            .is_none()
        || !valid_digest(&fence["head"])
    {
        return Err("invalid_reset_fence".into());
    }
    let profiles = input["profiles"]
        .as_array()
        .ok_or("invalid_reset_profiles")?;
    let mut evidence = BTreeMap::new();
    for profile in profiles {
        let profile = object(profile, "invalid_reset_profiles")?;
        let id = profile
            .get("profile")
            .and_then(profile_id)
            .ok_or("invalid_reset_profiles")?;
        if evidence.insert(id, profile).is_some() {
            return Err("invalid_reset_profiles".into());
        }
    }
    if evidence.keys().copied().collect::<BTreeSet<_>>() != live_ids {
        return Err("invalid_reset_profiles".into());
    }

    let mut old_keys = BTreeSet::new();
    let mut new_keys = BTreeSet::new();
    let mut pending = Vec::new();
    let mut unretired = Vec::new();
    for id in &live_ids {
        let profile = evidence[id];
        let current = &directory_profiles[*id]["library"]["value"];
        let old_version = profile["oldVersion"]
            .as_u64()
            .ok_or("invalid_reset_profiles")?;
        let new_version = profile["newVersion"]
            .as_u64()
            .ok_or("invalid_reset_profiles")?;
        let old_key = profile["oldKey"].as_str().ok_or("invalid_reset_profiles")?;
        if current["keyVersion"].as_u64() != Some(old_version)
            || current["key"].as_str() != Some(old_key)
        {
            return Err("profile_state_mismatch".into());
        }
        if old_version.checked_add(1) != Some(new_version) {
            return Err("invalid_key_version".into());
        }
        let new_key = profile["newKey"].as_str().ok_or("invalid_library_key")?;
        let new_key_bytes = codec::unbase64(new_key).map_err(|_| "invalid_library_key")?;
        if new_key_bytes.len() != 32 || new_key != standard_base64(&new_key_bytes) {
            return Err("invalid_library_key".into());
        }
        old_keys.insert(old_key);
        if !new_keys.insert(new_key) {
            return Err("reused_key".into());
        }
        let verified = profile["verified"]
            .as_bool()
            .ok_or("invalid_reset_profiles")?;
        let retired = profile["oldRetired"]
            .as_bool()
            .ok_or("invalid_reset_profiles")?;
        if verified {
            if !valid_digest(&profile["copiedHead"]) {
                return Err("invalid_copy_head".into());
            }
        } else if !profile["copiedHead"].is_null() {
            return Err("invalid_copy_head".into());
        }
        if !verified {
            pending.push(*id);
        }
        if !retired {
            unretired.push(*id);
        }
    }
    if old_keys.iter().any(|key| new_keys.contains(key)) {
        return Err("reused_key".into());
    }
    let household_committed = input["householdCommitted"]
        .as_bool()
        .ok_or("invalid_reset_profiles")?;
    let old_household_retired = input["oldHouseholdRetired"]
        .as_bool()
        .ok_or("invalid_reset_profiles")?;
    if !household_committed && (old_household_retired || unretired.len() != live_ids.len()) {
        return Err("premature_retirement".into());
    }
    if household_committed && !pending.is_empty() {
        return Err("commit_without_verification".into());
    }
    let answer = if !pending.is_empty() {
        json!({"effect":"move-live-profiles", "profiles":pending})
    } else if !household_committed {
        json!({"effect":"commit-successor-household"})
    } else if !unretired.is_empty() || !old_household_retired {
        json!({
            "effect":"retire-old-libraries",
            "profiles":unretired,
            "household":!old_household_retired,
        })
    } else {
        json!({"effect":"complete"})
    };
    Ok(answer)
}

/// The `set:prefs` values a profile library is given from an older household's (profiles-v1 §6.2): every
/// household key the profile's own `values` lacks, stamped values untouched. A member the profile already holds,
/// even a cleared one, is a decision and is never overwritten.
pub fn seed_preferences(input: &Value) -> Result<Value, String> {
    const INVALID: &str = "invalid_preferences";
    let input = object(input, INVALID)?;
    if !exact_members(input, &["household", "profile"]) {
        return Err(INVALID.into());
    }
    let household = object(&input["household"], INVALID)?;
    let held = match &input["profile"] {
        Value::Null => None,
        profile => Some(object(profile, INVALID)?),
    };
    let values: Map<String, Value> = household
        .iter()
        .filter(|(key, _)| {
            key.as_str() != LEGACY_MATURITY_KEY && !held.is_some_and(|held| held.contains_key(*key))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(json!({"values": values}))
}

/// Construct the initial Owner-only directory from explicit verified inputs, then validate it with the stored
/// directory rules. Identity bindings are derived from one Owner id; no clock, randomness, sealing, or I/O occurs.
pub fn construct_directory(input: &Value) -> Result<Value, String> {
    let input = object(input, "invalid_directory_constructor")?;
    if !exact_members(
        input,
        &["attempt", "source", "backup", "owner", "ownerHead"],
    ) {
        return Err("invalid_directory_constructor".into());
    }
    let owner = object(&input["owner"], "invalid_directory_constructor")?;
    if !exact_members(
        owner,
        &[
            "id",
            "display",
            "libraryKey",
            "libraryAt",
            "policy",
            "deletedAt",
        ],
    ) {
        return Err("invalid_directory_constructor".into());
    }
    let owner_id = profile_id(&owner["id"]).ok_or("invalid_profile")?;
    for field in ["display", "policy"] {
        let stamped = object(&owner[field], "invalid_directory_constructor")?;
        if !exact_members(stamped, &["value", "at"]) {
            return Err("invalid_directory_constructor".into());
        }
        stamp(&stamped["at"])?;
    }
    stamp(&owner["libraryAt"])?;
    stamp(&owner["deletedAt"])?;

    let document = json!({
        "format": 4,
        "kind": "profiles",
        "schema": 1,
        "owner": owner_id,
        "migration": {
            "version": 1,
            "attempt": input["attempt"].clone(),
            "source": input["source"].clone(),
            "backup": input["backup"].clone(),
            "ownerProfile": owner_id,
            "ownerHead": input["ownerHead"].clone(),
        },
        "profiles": {
            (owner_id): {
                "id": owner_id,
                "role": "owner",
                "display": owner["display"].clone(),
                "library": {
                    "value": {"keyVersion": 1, "key": owner["libraryKey"].clone()},
                    "at": owner["libraryAt"].clone(),
                },
                "policy": owner["policy"].clone(),
                "deleted": {"value": false, "at": owner["deletedAt"].clone()},
            }
        },
    });
    let directory = validate_directory(&document, true)?;
    let canonical = jcs::jcs(&directory);
    let plaintext = codec::compress(&directory).map_err(|_| "invalid_directory_constructor")?;
    let inflated = plaintext
        .strip_prefix(&[codec::COMPRESSED])
        .ok_or("invalid_directory_constructor")
        .and_then(|compressed| {
            codec::inflate(compressed).map_err(|_| "invalid_directory_constructor")
        })?;
    if inflated != canonical {
        return Err("invalid_directory_constructor".into());
    }
    Ok(json!({
        "directory": directory,
        "jcs": codec::base64(&canonical),
        "plaintext": codec::base64(&plaintext),
    }))
}

fn profile_scoped_name(name: &str) -> bool {
    name.starts_with("title:")
        || name.starts_with("season:")
        || name.starts_with("dlv:")
        || name.starts_with("set:deliver:")
        || name.starts_with("set:handoff:")
        || matches!(
            name,
            "set:prefs" | "set:keys" | "set:trackers" | "set:assistant-applied"
        )
}

fn legacy_profile_name(name: &str) -> bool {
    name.starts_with("rec:")
        || name.starts_with("wat:")
        || name.starts_with("snt:")
        || name.starts_with("ep:")
        || name.starts_with("set:tracker-event:")
}

fn household_values(row: &Value) -> Result<(String, Option<Map<String, Value>>), String> {
    const INVALID: &str = "invalid_household_write";
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or(INVALID)?;
    if !matches!(name, "set:prefs" | "set:keys") {
        let withheld = profile_scoped_name(name) || legacy_profile_name(name);
        return Ok((name.into(), withheld.then(Map::new)));
    }
    let values = row
        .get("values")
        .and_then(Value::as_object)
        .ok_or(INVALID)?;
    if name == "set:prefs" {
        // Every preference is profile-scoped (profiles-v1 §6): the household holds no `set:prefs` at all.
        return Ok((name.into(), Some(Map::new())));
    }
    let mut kept = values.clone();
    kept.remove("simkl");
    Ok((name.into(), (kept.len() != values.len()).then_some(kept)))
}

/// Whether one row may be written to a profile-aware household: `{effect: allow}`, `{effect: refuse}`, or
/// `{effect: restrict, values}` carrying the household-only values to write instead.
pub fn household_write(row: &Value) -> Result<Value, String> {
    Ok(match household_values(row)?.1 {
        None => json!({"effect":"allow"}),
        Some(values) if values.is_empty() => json!({"effect":"refuse"}),
        Some(values) => json!({"effect":"restrict", "values":values}),
    })
}

/// The same rule over rows already stored in a household: which to remove and which to rewrite, in input order.
pub fn household_audit(rows: &Value) -> Result<Value, String> {
    let rows = rows.as_array().ok_or("invalid_household_write")?;
    let mut seen = std::collections::HashSet::new();
    let mut remove = Vec::new();
    let mut rewrite = Vec::new();
    for row in rows {
        let (name, kept) = household_values(row)?;
        if !seen.insert(name.clone()) {
            return Err("invalid_household_write".into());
        }
        match kept {
            None => {}
            Some(values) if values.is_empty() => remove.push(json!(name)),
            Some(values) => rewrite.push(json!({"name":name, "values":values})),
        }
    }
    Ok(json!({"remove":remove, "rewrite":rewrite}))
}

/// Canonical SHA-256 for the source and Owner snapshots retained by profile topology and compound reset.
/// The operation is pure: it validates and normalizes the typed input, sorts rows by raw UTF-8 key bytes, then
/// hashes RFC 8785 JCS.
pub fn snapshot_digest(kind: &str, input: &Value) -> Result<Value, String> {
    let input = object(input, "invalid_migration_digest")?;
    let normalized = match kind {
        "source" => snapshot_digest_input(input, true)?,
        "owner" => snapshot_digest_input(input, false)?,
        _ => return Err("invalid_migration_digest_kind".into()),
    };
    let digest = Sha256::digest(jcs::jcs(&normalized));
    Ok(json!(digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()))
}

fn snapshot_digest_input(input: &Map<String, Value>, source: bool) -> Result<Value, String> {
    let members = if source {
        &["libraryId", "generation", "base", "wireMin", "rows"][..]
    } else {
        &["libraryId", "base", "wireMin", "rows"][..]
    };
    if !exact_members(input, members)
        || !valid_profile_id(input["libraryId"].as_str().unwrap_or(""))
    {
        return Err("invalid_migration_digest".into());
    }
    let base = digest_integer(&input["base"], false)?;
    let mut answer = Map::from_iter([
        ("libraryId".into(), input["libraryId"].clone()),
        ("base".into(), json!(base)),
        (
            "wireMin".into(),
            json!(digest_integer(&input["wireMin"], true)?),
        ),
        (
            "rows".into(),
            Value::Array(digest_rows(&input["rows"], true, Some(base))?),
        ),
    ]);
    if source {
        answer.insert(
            "generation".into(),
            json!(digest_integer(&input["generation"], false)?),
        );
    }
    Ok(Value::Object(answer))
}

fn digest_rows(value: &Value, sequenced: bool, base: Option<u64>) -> Result<Vec<Value>, String> {
    let rows = value.as_array().ok_or("invalid_migration_digest")?;
    let mut keys = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    let mut answer = Vec::with_capacity(rows.len());
    for value in rows {
        let row = object(value, "invalid_migration_digest")?;
        let members = if sequenced {
            &["k", "seq", "v"][..]
        } else {
            &["k", "v"][..]
        };
        let key = row
            .get("k")
            .and_then(Value::as_str)
            .ok_or("invalid_migration_digest")?;
        if !exact_members(row, members)
            || !valid_lower_hex(key, 64)
            || !keys.insert(key.to_owned())
            || row.get("v").and_then(Value::as_str).is_none()
        {
            return Err("invalid_migration_digest".into());
        }
        if sequenced {
            let sequence = digest_integer(&row["seq"], true)?;
            if !sequences.insert(sequence) || sequence > base.ok_or("invalid_migration_digest")? {
                return Err("invalid_migration_digest".into());
            }
        }
        answer.push(value.clone());
    }
    answer.sort_by(|a, b| {
        a["k"]
            .as_str()
            .unwrap()
            .as_bytes()
            .cmp(b["k"].as_str().unwrap().as_bytes())
    });
    Ok(answer)
}

fn digest_integer(value: &Value, positive: bool) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|number| *number <= MAX_SAFE_INTEGER && (!positive || *number != 0))
        .ok_or_else(|| "invalid_migration_digest".into())
}

fn valid_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Public typed equivalents for Rust callers; JSON callers use the versioned `evaluate` envelope.
pub fn profiles_validate(document: &Value) -> Result<Value, String> {
    validate(document)
}

pub fn profiles_merge(a: &Value, b: &Value) -> Result<Value, String> {
    merge(a, b)
}

pub fn normalize_legacy(legacy: Option<&str>) -> Result<Value, String> {
    let maximum_age = match legacy {
        None => Value::Null,
        Some("pg13") => json!(13),
        Some("r") => json!(17),
        Some(_) => return Err("invalid_legacy_policy".into()),
    };
    Ok(json!({"maximumAge": maximum_age, "unknownRatings": "allow"}))
}

fn maximum_age(policy: &Value) -> Option<u64> {
    policy["maximumAge"].as_u64()
}

pub fn policy_allows(
    policy: &Value,
    minimum_age: Option<&Value>,
    explicit_approval: bool,
) -> Result<Value, String> {
    policy_value(policy)?;
    if explicit_approval {
        return Ok(Value::Bool(true));
    }
    let minimum_age = match minimum_age {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|age| *age <= 18)
                .ok_or("invalid_minimum_age")?,
        ),
    };
    let allowed = match minimum_age {
        Some(minimum) => maximum_age(policy).is_none_or(|maximum| minimum <= maximum),
        None => policy["unknownRatings"] == "allow",
    };
    Ok(Value::Bool(allowed))
}

fn age_strictness(policy: &Value) -> u64 {
    maximum_age(policy).unwrap_or(19)
}

/// Compare normalized policies in the product order used by profile-selection gating.
pub fn policy_relation(a: &Value, b: &Value) -> Result<Value, String> {
    policy_value(a)?;
    policy_value(b)?;
    let age = age_strictness(a).cmp(&age_strictness(b));
    let unknown = (a["unknownRatings"] == "allow").cmp(&(b["unknownRatings"] == "allow"));
    let relation = match (age, unknown) {
        (Ordering::Equal, Ordering::Equal) => "equal",
        (Ordering::Less | Ordering::Equal, Ordering::Less | Ordering::Equal) => "stricter",
        (Ordering::Greater | Ordering::Equal, Ordering::Greater | Ordering::Equal) => "looser",
        _ => "incomparable",
    };
    Ok(json!(relation))
}

fn title_key(value: &str) -> bool {
    let Some((media, id)) = value.split_once(':') else {
        return false;
    };
    if !matches!(media, "movie" | "tv")
        || id.is_empty()
        || (id.len() > 1 && id.starts_with('0'))
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    id.parse::<u32>().is_ok_and(|id| id > 0)
}

fn approval_entry(value: &Value) -> Result<(), String> {
    let entry = object(value, "invalid_approval")?;
    if !entry.contains_key("value")
        || !entry.contains_key("at")
        || !matches!(entry["value"].as_str(), Some("allow" | "tombstone"))
        || stamp(&entry["at"]).is_err()
    {
        return Err("invalid_approval".into());
    }
    Ok(())
}

fn approval_kid<'a>(logical_name: &str, document: &'a Value) -> Result<&'a str, String> {
    let named_kid = logical_name
        .strip_prefix("kid-title-approvals:")
        .filter(|id| valid_profile_id(id))
        .ok_or("approval_name_mismatch")?;
    let kid = document
        .get("kid")
        .and_then(profile_id)
        .ok_or("invalid_approvals")?;
    if kid != named_kid {
        return Err("approval_name_mismatch".into());
    }
    Ok(kid)
}

/// Validate one approval document in its logical-name and current directory context.
pub fn approvals_validate(
    logical_name: &str,
    document: &Value,
    directory: &Value,
) -> Result<Value, String> {
    validate(directory)?;
    let root = object(document, "invalid_approvals")?;
    if root.get("format").and_then(Value::as_u64) != Some(4)
        || root.get("kind").and_then(Value::as_str) != Some("kid-title-approvals")
        || root.get("schema").and_then(Value::as_u64) != Some(1)
    {
        return Err("invalid_approvals".into());
    }
    let kid = approval_kid(logical_name, document)?;
    directory["profiles"]
        .get(kid)
        .filter(|profile| profile["role"] == "kid" && profile["deleted"]["value"] == false)
        .ok_or("approval_not_kid")?;
    let approvals = root
        .get("approvals")
        .and_then(Value::as_object)
        .ok_or("invalid_approvals")?;
    for (key, entry) in approvals {
        if !title_key(key) {
            return Err("invalid_title_key".into());
        }
        approval_entry(entry)?;
    }
    Ok(document.clone())
}

fn latest_approval(root: &Map<String, Value>) -> Option<Stamp> {
    root["approvals"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, entry)| stamp(&entry["at"]).expect("validated approval stamp"))
        .max()
}

fn approval_later(a: &Value, b: &Value) -> Value {
    let order = stamp(&a["at"])
        .expect("validated approval stamp")
        .cmp(&stamp(&b["at"]).expect("validated approval stamp"))
        .then_with(|| (a["value"] == "tombstone").cmp(&(b["value"] == "tombstone")))
        .then_with(|| jcs::cmp(a, b));
    if order == Ordering::Less { b } else { a }.clone()
}

/// Converge two valid versions of one Kid approval document.
pub fn approvals_merge(
    logical_name: &str,
    a: &Value,
    b: &Value,
    directory: &Value,
) -> Result<Value, String> {
    approvals_validate(logical_name, a, directory)?;
    approvals_validate(logical_name, b, directory)?;
    let (a, b) = (a.as_object().unwrap(), b.as_object().unwrap());
    if a["kid"] != b["kid"] {
        return Err("identity_conflict".into());
    }
    let (ua, ub) = (
        unknown(a, APPROVAL_ROOT_MEMBERS),
        unknown(b, APPROVAL_ROOT_MEMBERS),
    );
    let mut out = if (latest_approval(a), rank(&ua)) < (latest_approval(b), rank(&ub)) {
        ub
    } else {
        ua
    };
    out.insert("format".into(), json!(4));
    out.insert("kind".into(), json!("kid-title-approvals"));
    out.insert("schema".into(), json!(1));
    out.insert("kid".into(), a["kid"].clone());
    let (aa, ba) = (
        a["approvals"].as_object().unwrap(),
        b["approvals"].as_object().unwrap(),
    );
    let mut approvals = Map::new();
    for key in aa.keys().chain(ba.keys()).collect::<BTreeSet<_>>() {
        let value = match (aa.get(key), ba.get(key)) {
            (Some(a), Some(b)) => approval_later(a, b),
            (Some(value), None) | (None, Some(value)) => value.clone(),
            (None, None) => unreachable!(),
        };
        approvals.insert(key.clone(), value);
    }
    out.insert("approvals".into(), Value::Object(approvals));
    approvals_validate(logical_name, &Value::Object(out), directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_base64_rejects_other_spellings() {
        let bytes = [17u8; 32];
        let key = standard_base64(&bytes);
        assert_eq!(key, "ERERERERERERERERERERERERERERERERERERERERERE=");
        assert_eq!(codec::unbase64(&key).unwrap(), bytes);
    }

    #[test]
    fn is_nfc_answers_as_unicode_normalization() {
        use unicode_normalization::UnicodeNormalization;
        let oracle = |text: &str| text.nfc().eq(text.chars());
        assert!(is_nfc("é") && !is_nfc("e\u{301}"));
        assert!(!is_nfc("\u{1100}\u{1161}") && !is_nfc("가\u{11a8}"));
        // A mark composes past one of a lower class; marks sort by class.
        assert!(is_nfc("á\u{316}") && !is_nfc("a\u{316}\u{301}") && !is_nfc("a\u{301}\u{316}"));
        let scalars = || (0..=0x10ffff).filter_map(char::from_u32);
        for c in scalars() {
            let text = c.to_string();
            assert_eq!(is_nfc(&text), oracle(&text), "{:04x}", c as u32);
        }
        // Every pair that composes, with two marks of every combining class before or after its second character:
        // reordering, and composition blocked or not by a mark between them.
        let mut pairs = Vec::new();
        let mut marks: BTreeMap<u8, Vec<char>> = BTreeMap::new();
        for c in scalars() {
            let class = canonical_combining_class(c);
            if class != 0 && marks.entry(class).or_default().len() < 2 {
                marks.entry(class).or_default().push(c);
            }
            let mut parts = Vec::new();
            decompose_canonical(c, |d| parts.push(d));
            if let [b, y] = parts[..] {
                pairs.push((b, y));
            }
        }
        let marks: Vec<char> = marks.into_values().flatten().collect();
        for &(b, y) in &pairs {
            for &x in &marks {
                for text in [[b, x, y], [b, y, x]] {
                    let text = String::from_iter(text);
                    assert_eq!(is_nfc(&text), oracle(&text), "{text:?}");
                }
            }
        }
        // Hangul: leading and vowel jamo compose to a syllable, which a trailing jamo extends.
        for l in '\u{1100}'..='\u{1112}' {
            for v in '\u{1161}'..='\u{1175}' {
                for t in ['\u{11a7}'].into_iter().chain('\u{11a8}'..='\u{11c2}') {
                    for text in [
                        vec![l, v],
                        vec![l, v, t],
                        vec![l, '\u{301}', v],
                        compose(l, v).into_iter().chain([t]).collect(),
                    ] {
                        let text = String::from_iter(text);
                        assert_eq!(is_nfc(&text), oracle(&text), "{text:?}");
                    }
                }
            }
        }
    }
}
