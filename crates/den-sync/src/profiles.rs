//! Household profiles v1 (`den-spec wire/profiles-v1.md`). This is only the pure directory, content-policy, and
//! lifecycle-decision boundary: it has no clock, storage, network, split, reset, or wire-minimum effects.

use crate::library_v4::{codec, jcs};
use crate::wire::{Stamp, MAX_SAFE_INTEGER};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

const ROOT_MEMBERS: &[&str] = &["format", "kind", "schema", "owner", "migration", "profiles"];
const ENTRY_MEMBERS: &[&str] = &["id", "role", "display", "library", "policy", "deleted"];
const APPROVAL_ROOT_MEMBERS: &[&str] = &["format", "kind", "schema", "kid", "approvals"];
const PROFILE_PREFERENCE_KEYS: &[&str] = &[
    "den.excludedGenreIDs",
    "den.excludedLanguages",
    "den.preferredSubtitleLanguage",
    "den.hasExplicitSubtitlePreference",
    "den.shownSubtitleLanguages",
    "den.maxSubtitlesPerLanguage",
    "den.preferredAudioLanguage",
    "den.autoSkipSegments",
    "den.autoplayTrailers",
    "den.hideAnime",
    "den.hideWatched",
    "den.minReleaseYear",
    "den.enabledRatingSources",
    "den.shownWarningCategories",
];

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
    if name.nfc().ne(name.chars()) {
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
        let candidate = entry_value.ok_or("invalid_operation_input")?;
        entry(candidate)?;
        if candidate["id"].as_str() != Some(profile) || candidate["deleted"]["value"] != false {
            return Err("invalid_profile".into());
        }
        profiles.insert(profile.into(), candidate.clone());
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

fn optional_bool(input: &Map<String, Value>, key: &str) -> Result<bool, String> {
    input.get(key).map_or(Ok(false), |value| {
        value.as_bool().ok_or("invalid_evidence".into())
    })
}

/// Decide the next pure initial-split effect from durable observations. This operation performs no effect itself.
pub fn migration_next(input: &Value) -> Result<Value, String> {
    let input = object(input, "invalid_evidence")?;
    let state = input
        .get("state")
        .and_then(Value::as_str)
        .ok_or("invalid_state")?;
    if !matches!(
        state,
        "idle"
            | "fenced"
            | "backed-up"
            | "owner-copied"
            | "ready-to-commit"
            | "commit-unknown"
            | "adopting"
            | "cleaning-loser"
            | "active"
            | "restore-required"
    ) {
        return Err("invalid_state".into());
    }
    let observed = input
        .get("observed")
        .and_then(Value::as_str)
        .ok_or("invalid_observation")?;
    if !matches!(observed, "legacy" | "committed" | "corrupt") {
        return Err("invalid_observation".into());
    }
    for name in [
        "explicitIntent",
        "backupVerified",
        "ownerVerified",
        "winnerVerified",
        "candidateRetired",
    ] {
        optional_bool(input, name)?;
    }
    let candidate = input
        .get("candidateKey")
        .map(validate_candidate_key)
        .transpose()?;
    let candidate_attempt = match input.get("candidateAttempt") {
        Some(value) => Some(profile_id(value).ok_or("invalid_candidate_attempt")?),
        None => None,
    };
    if candidate_attempt.is_some() && candidate.is_none() {
        return Err("invalid_candidate_attempt".into());
    }
    let candidate_retired = optional_bool(input, "candidateRetired")?;
    if candidate_retired && candidate.is_none() {
        return Err("invalid_evidence".into());
    }
    let owner_verified = optional_bool(input, "ownerVerified")?;
    let candidate_head = input.get("candidateHead");
    if owner_verified {
        if candidate.is_none() {
            return Err("invalid_evidence".into());
        }
        if candidate_head.is_none_or(|head| !valid_digest(head)) {
            return Err("invalid_copy_head".into());
        }
    } else if candidate_head.is_some() {
        return Err("invalid_copy_head".into());
    }
    let fence_changed = if observed == "legacy" && state != "idle" {
        let fence = migration_fence(input.get("fence"))?;
        let observed_fence = migration_fence(input.get("observedFence"))?;
        fence != observed_fence
    } else {
        false
    };

    let answer = if observed == "committed" {
        let directory = input
            .get("directory")
            .ok_or("committed_directory_missing")?;
        validate_directory(directory, true)?;
        if candidate.is_some() && candidate_attempt.is_none() {
            return Err("invalid_candidate_attempt".into());
        }
        let live_keys = directory["profiles"]
            .as_object()
            .expect("validated profiles")
            .values()
            .filter(|profile| profile["deleted"]["value"] == false)
            .filter_map(|profile| profile["library"]["value"]["key"].as_str())
            .collect::<BTreeSet<_>>();
        if candidate.is_some_and(|key| live_keys.contains(key)) {
            if candidate_retired {
                return Err("committed_key_retired".into());
            }
            let owner = directory["owner"].as_str().expect("validated owner");
            let migration = &directory["migration"];
            let owns_winner = candidate
                == directory["profiles"][owner]["library"]["value"]["key"].as_str()
                && candidate_attempt == migration["attempt"].as_str()
                && owner_verified
                && candidate_head == Some(&migration["ownerHead"]);
            if owns_winner {
                if !optional_bool(input, "winnerVerified")? {
                    "verify-winner"
                } else {
                    "activate"
                }
            } else if state == "cleaning-loser" {
                "cleanup_targets_winner"
            } else {
                return Err("winner_evidence_mismatch".into());
            }
        } else if candidate.is_some() && !candidate_retired {
            "retire-losing-candidate"
        } else if !optional_bool(input, "winnerVerified")? {
            "verify-winner"
        } else {
            "activate"
        }
    } else if observed == "corrupt" {
        if input.contains_key("directory") {
            return Err("invalid_evidence".into());
        }
        if optional_bool(input, "backupVerified")? {
            "restore-backup"
        } else {
            "recovery-blocked"
        }
    } else if input.contains_key("directory") {
        return Err("invalid_evidence".into());
    } else if state == "idle" {
        if !optional_bool(input, "explicitIntent")? {
            return Err("explicit_intent_required".into());
        }
        "acquire-fence"
    } else if state == "commit-unknown" {
        "read-household"
    } else if fence_changed {
        if candidate.is_some() && !candidate_retired {
            "retire-losing-candidate"
        } else {
            "restart"
        }
    } else if (matches!(state, "backed-up" | "owner-copied" | "ready-to-commit")
        && !optional_bool(input, "backupVerified")?)
        || (matches!(state, "owner-copied" | "ready-to-commit") && candidate.is_none())
    {
        return Err("invalid_evidence".into());
    } else if state == "ready-to-commit" && !owner_verified {
        "verify-owner-library"
    } else {
        match state {
            "fenced" => "create-backup",
            "backed-up" => "create-owner-library",
            "owner-copied" => "verify-owner-library",
            "ready-to-commit" => "commit-fenced-household",
            _ => return Err("invalid_state".into()),
        }
    };
    Ok(json!(answer))
}

fn migration_fence(value: Option<&Value>) -> Result<&Map<String, Value>, String> {
    let fence = value
        .and_then(Value::as_object)
        .ok_or("invalid_migration_fence")?;
    if !exact_members(fence, &["generation", "head"])
        || fence["generation"]
            .as_u64()
            .filter(|generation| *generation <= MAX_SAFE_INTEGER)
            .is_none()
        || !valid_digest(&fence["head"])
    {
        return Err("invalid_migration_fence".into());
    }
    Ok(fence)
}

fn validate_candidate_key(value: &Value) -> Result<&str, String> {
    let key = value.as_str().ok_or("invalid_library_key")?;
    let bytes = codec::unbase64(key).map_err(|_| "invalid_library_key")?;
    if bytes.len() != 32 || key != standard_base64(&bytes) {
        return Err("invalid_library_key".into());
    }
    Ok(key)
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

/// Split one legacy `set:prefs.values` object without interpreting its stamped values.
///
/// Exact assigned personal keys move to Owner. The legacy maturity input is returned separately for policy
/// normalization. Every other key, including future keys, stays in the household so an older client cannot leak
/// an unassigned setting into one profile.
pub fn partition_preferences(values: &Value) -> Result<Value, String> {
    let values = values.as_object().ok_or("invalid_preferences")?;
    let mut household = Map::new();
    let mut owner = Map::new();
    let mut owner_maturity = Value::Null;
    for (key, value) in values {
        if PROFILE_PREFERENCE_KEYS.contains(&key.as_str()) {
            owner.insert(key.clone(), value.clone());
        } else if key == "den.maturityCeiling" {
            owner_maturity = value.clone();
        } else {
            household.insert(key.clone(), value.clone());
        }
    }
    Ok(json!({
        "household": household,
        "owner": owner,
        "ownerMaturity": owner_maturity,
    }))
}

/// Canonical SHA-256 evidence for the four profiles-v1 §8.1 migration structures. This operation is pure: it
/// validates and normalizes the typed input, sorts rows by raw UTF-8 key bytes, then hashes RFC 8785 JCS.
pub fn migration_digest(kind: &str, input: &Value) -> Result<Value, String> {
    let input = object(input, "invalid_migration_digest")?;
    let normalized = match kind {
        "source" => snapshot_digest_input(input, true)?,
        "owner" => snapshot_digest_input(input, false)?,
        "staged" => {
            if !exact_members(input, &["rows"]) {
                return Err("invalid_migration_digest".into());
            }
            let rows = digest_rows(&input["rows"], false, None)?;
            if rows.is_empty() {
                return Err("invalid_migration_digest".into());
            }
            Value::Array(rows)
        }
        "backup" => backup_digest_input(input)?,
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

fn backup_digest_input(input: &Map<String, Value>) -> Result<Value, String> {
    const MEMBERS: &[&str] = &[
        "id",
        "householdId",
        "attempt",
        "source",
        "wireMin",
        "authorization",
        "rows",
    ];
    if !exact_members(input, MEMBERS)
        || !input["id"]
            .as_str()
            .is_some_and(|id| (1..=256).contains(&id.chars().count()))
        || !valid_profile_id(input["householdId"].as_str().unwrap_or(""))
        || !valid_profile_id(input["attempt"].as_str().unwrap_or(""))
    {
        return Err("invalid_migration_digest".into());
    }
    let source = object(&input["source"], "invalid_migration_digest")?;
    let authorization = object(&input["authorization"], "invalid_migration_digest")?;
    if !exact_members(source, &["generation", "base", "head", "rewrite"])
        || !source["head"].as_str().is_some_and(valid_digest_text)
        || !source["rewrite"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        || !exact_members(authorization, &["tokenHash", "memberHash"])
        || !authorization["tokenHash"]
            .as_str()
            .is_some_and(valid_digest_text)
        || !(authorization["memberHash"].is_null()
            || authorization["memberHash"]
                .as_str()
                .is_some_and(valid_digest_text))
    {
        return Err("invalid_migration_digest".into());
    }
    let base = digest_integer(&source["base"], false)?;
    Ok(json!({
        "id": input["id"],
        "householdId": input["householdId"],
        "attempt": input["attempt"],
        "source": {
            "generation": digest_integer(&source["generation"], false)?,
            "base": base,
            "head": source["head"],
            "rewrite": source["rewrite"],
        },
        "wireMin": digest_integer(&input["wireMin"], true)?,
        "authorization": authorization,
        "rows": digest_rows(&input["rows"], true, Some(base))?,
    }))
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

fn valid_digest_text(value: &str) -> bool {
    valid_lower_hex(value, 64)
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
}
