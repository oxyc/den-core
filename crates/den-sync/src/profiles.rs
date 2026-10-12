//! Household profiles v1 (`den-spec wire/profiles-v1.md`). This is only the pure directory and content-policy
//! boundary: it has no clock, storage, network, migration, split, reset, or wire-minimum effects.

use crate::library_v4::{codec, jcs};
use crate::wire::{Stamp, MAX_SAFE_INTEGER};
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

const ROOT_MEMBERS: &[&str] = &["format", "kind", "schema", "owner", "profiles"];
const ENTRY_MEMBERS: &[&str] = &["id", "role", "display", "library", "policy", "deleted"];

fn object<'a>(value: &'a Value, error: &str) -> Result<&'a Map<String, Value>, String> {
    value.as_object().ok_or_else(|| error.into())
}

fn exact_members(map: &Map<String, Value>, members: &[&str]) -> bool {
    map.len() == members.len() && members.iter().all(|member| map.contains_key(*member))
}

fn profile_id(value: &Value) -> Option<&str> {
    value.as_str().filter(|id| {
        id.len() == 32
            && id
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
    if !exact_members(field, &["value", "at"]) || stamp(&field["at"]).is_err() {
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
    if !exact_members(value, &["keyVersion", "key"])
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
    if !exact_members(value, &["maxBand", "unknownRatings"])
        || !matches!(
            value["maxBand"].as_str(),
            None | Some("littleKids" | "family" | "teen" | "mature")
        )
        || (!value["maxBand"].is_null() && !value["maxBand"].is_string())
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

/// Validate and return the unchanged logical document. Serialization at the boundary supplies its canonical JSON.
pub fn validate(document: &Value) -> Result<Value, String> {
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
    Ok(document.clone())
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
    validate(a)?;
    validate(b)?;
    let (a, b) = (a.as_object().unwrap(), b.as_object().unwrap());
    if a["owner"] != b["owner"] {
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
    validate(&Value::Object(out))
}

/// Public typed equivalents for Rust callers; JSON callers use the versioned `evaluate` envelope.
pub fn profiles_validate(document: &Value) -> Result<Value, String> {
    validate(document)
}

pub fn profiles_merge(a: &Value, b: &Value) -> Result<Value, String> {
    merge(a, b)
}

pub fn normalize_legacy(legacy: Option<&str>) -> Result<Value, String> {
    let max_band = match legacy {
        None => Value::Null,
        Some("pg13") => json!("teen"),
        Some("r") => json!("mature"),
        Some(_) => return Err("invalid_legacy_policy".into()),
    };
    Ok(json!({"maxBand": max_band, "unknownRatings": "allow"}))
}

fn band(value: Option<&str>) -> Result<Option<u8>, String> {
    match value {
        None => Ok(None),
        Some("littleKids") => Ok(Some(0)),
        Some("family") => Ok(Some(1)),
        Some("teen") => Ok(Some(2)),
        Some("mature") => Ok(Some(3)),
        Some(_) => Err("invalid_title_band".into()),
    }
}

pub fn policy_allows(policy: &Value, title_band: Option<&str>) -> Result<Value, String> {
    policy_value(policy)?;
    let allowed = match band(title_band)? {
        None => policy["unknownRatings"] == "allow",
        Some(title) => match band(policy["maxBand"].as_str())? {
            None => true,
            Some(maximum) => title <= maximum,
        },
    };
    Ok(Value::Bool(allowed))
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
