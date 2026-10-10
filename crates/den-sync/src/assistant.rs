//! Assistant writes (den-spec `wire/assistant-v1.md`) as the clients call them: the library's three settings rows
//! (`set:assistant`, `set:assistant-grants`, `set:assistant-applied`, §4) read into what `den_assistant` checks, and
//! the values a client writes back. The crypto and the accept rules are `den_assistant`'s; this module only reads and
//! writes rows.

use crate::wire::{canonical, stamp, Stamp};
use den_assistant::{Applied, Grant, MAX_CAP, MAX_SAFE_INTEGER, OPS};
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;

pub const DROPBOX_ROW: &str = "assistant";
pub const GRANTS_ROW: &str = "assistant-grants";
pub const APPLIED_ROW: &str = "assistant-applied";
const DROPBOX_PREFIX: &str = "dropbox.";
const CLIENT_MAX: usize = 80;

fn seed(text: &str) -> Result<[u8; 32], String> {
    let bad = || "invalid_seed".to_string();
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
    }
    Ok(out)
}

fn jcs_string(value: &Value) -> Value {
    json!({ "string": String::from_utf8(canonical(value)).expect("JSON is UTF-8") })
}

/// The settings of a row, checked to be the named settings row. Absent (`None` or `null`) is an empty row.
fn settings<'a>(
    row: Option<&'a Value>,
    name: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    let Some(row) = row.filter(|r| !r.is_null()) else {
        return Ok(None);
    };
    if row["kind"] != "set" || row["name"] != name {
        return Err("invalid_row".into());
    }
    row["values"]
        .as_object()
        .map(Some)
        .ok_or_else(|| "invalid_row".into())
}

/// The JSON object a `{"string": <JCS>}` setting value holds.
fn held(value: &Value) -> Option<Map<String, Value>> {
    let text = value["value"]["string"].as_str()?;
    match serde_json::from_str::<Value>(text).ok()? {
        Value::Object(object) => Some(object),
        _ => None,
    }
}

fn safe(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= MAX_SAFE_INTEGER)
}

// ---- set:assistant — the drop-box keys

/// Every drop-box private key the row holds, by setting name.
fn dropbox_keys(row: Option<&Value>) -> Result<Vec<(String, [u8; 32], Stamp)>, String> {
    let mut keys = Vec::new();
    for (name, value) in settings(row, DROPBOX_ROW)?.into_iter().flatten() {
        if !name.starts_with(DROPBOX_PREFIX) {
            continue;
        }
        let secret = value["value"]["string"]
            .as_str()
            .and_then(den_assistant::b64url_decode)
            .and_then(|b| <[u8; 32]>::try_from(b).ok());
        if let (Some(secret), Ok(at)) = (secret, stamp(&value["at"])) {
            keys.push((name.clone(), secret, at));
        }
    }
    Ok(keys)
}

/// `assistant_keygen_dropbox`: 32 random bytes → a drop-box key, the setting that holds it, and its public half.
pub fn keygen_dropbox(random: &str) -> Result<Value, String> {
    let secret = seed(random)?;
    let public = den_assistant::kem_public(&secret);
    let kid = den_assistant::key_id(&public);
    Ok(json!({
        "kid": kid,
        "public": den_assistant::b64url(&public),
        "setting": format!("{DROPBOX_PREFIX}{kid}"),
        "value": {"string": den_assistant::b64url(&secret)},
    }))
}

/// `assistant_dropbox`: the drop-box key den-edge should hold — the row's key with the latest stamp — or null when the
/// row holds none.
pub fn dropbox(row: Option<&Value>) -> Result<Value, String> {
    let latest = dropbox_keys(row)?
        .into_iter()
        .max_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    Ok(match latest {
        Some((_, secret, _)) => {
            let public = den_assistant::kem_public(&secret);
            json!({"kid": den_assistant::key_id(&public), "public": den_assistant::b64url(&public)})
        }
        None => Value::Null,
    })
}

// ---- set:assistant-grants

fn clean_client(name: &str) -> bool {
    let bad = |c: char| {
        c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    };
    name.trim() == name
        && (1..=CLIENT_MAX).contains(&name.chars().count())
        && !name.chars().any(bad)
}

fn valid_ops(ops: &Value) -> Option<Vec<String>> {
    let list: Vec<String> = ops
        .as_array()?
        .iter()
        .map(|op| op.as_str().filter(|op| OPS.contains(op)).map(str::to_owned))
        .collect::<Option<_>>()?;
    // Sorted and distinct, so one set of ops has one spelling.
    (!list.is_empty() && list.windows(2).all(|w| w[0] < w[1])).then_some(list)
}

/// A grant value (§4) checked against the grant id it is stored under: `None` when malformed.
fn grant_of(id: &str, value: &Value) -> Option<(Grant, Option<u64>)> {
    let object = held(value)?;
    let keys = ["cap", "client", "createdAt", "ops", "pk", "revokedAt", "v"];
    if object.len() != keys.len()
        || !keys.iter().all(|k| object.contains_key(*k))
        || object["v"] != 1
    {
        return None;
    }
    let public: [u8; 32] = object["pk"]
        .as_str()
        .and_then(den_assistant::b64url_decode)?
        .try_into()
        .ok()?;
    if den_assistant::grant_id(&public) != id {
        return None;
    }
    let client = object["client"].as_str()?;
    let cap = safe(&object["cap"]).filter(|c| (1..=MAX_CAP).contains(c))?;
    let revoked = match &object["revokedAt"] {
        Value::Null => None,
        other => Some(safe(other)?),
    };
    safe(&object["createdAt"])?;
    if !clean_client(client) {
        return None;
    }
    let ops = valid_ops(&object["ops"])?;
    Some((
        Grant {
            public,
            ops,
            cap,
            revoked: revoked.is_some(),
        },
        revoked,
    ))
}

/// `assistant_keygen_grant`: 32 random bytes and what the person consented to → the grant key (its secret goes to
/// den-edge at approval, never into the library) and the grants row setting that records it.
pub fn keygen_grant(
    random: &str,
    client: &str,
    ops: &[String],
    cap: u64,
    now: u64,
) -> Result<Value, String> {
    let secret = seed(random)?;
    if !clean_client(client) {
        return Err("invalid_client".into());
    }
    let mut ops = ops.to_vec();
    ops.sort();
    ops.dedup();
    if ops.is_empty() || !ops.iter().all(|op| OPS.contains(&op.as_str())) {
        return Err("invalid_ops".into());
    }
    if !(1..=MAX_CAP).contains(&cap) {
        return Err("invalid_cap".into());
    }
    if now > MAX_SAFE_INTEGER {
        return Err("invalid_time".into());
    }
    let key = den_assistant::GrantKey::from_secret(&secret);
    let grant = json!({
        "v": 1,
        "pk": den_assistant::b64url(&key.public()),
        "client": client,
        "ops": ops,
        "cap": cap,
        "createdAt": now,
        "revokedAt": null,
    });
    Ok(json!({
        "grant": key.id(),
        "public": den_assistant::b64url(&key.public()),
        "secret": den_assistant::b64url(key.secret()),
        "setting": key.id(),
        "value": jcs_string(&grant),
    }))
}

/// `assistant_revoke`: a grant value with `revokedAt` set — kept at the earlier time when it already was.
pub fn revoke(id: &str, value: &Value, now: u64) -> Result<Value, String> {
    let stamped = json!({ "value": value, "at": [0, 0, ""] });
    let (_, revoked) = grant_of(id, &stamped).ok_or("invalid_grant")?;
    if now > MAX_SAFE_INTEGER {
        return Err("invalid_time".into());
    }
    let mut object = held(&stamped).expect("checked");
    object.insert(
        "revokedAt".into(),
        json!(revoked.map_or(now, |r| r.min(now))),
    );
    Ok(json!({ "value": jcs_string(&Value::Object(object)) }))
}

/// One grant setting merged (§4): a well-formed version beats a malformed one, and two malformed ones go by the later
/// stamp. Two well-formed ones keep the byte-greater JCS of everything but `revokedAt` (the versions agree on it,
/// since a grant is written once), the earlier non-null `revokedAt` (a revocation is never undone), and the later
/// stamp. Each part is a join, so the merge is commutative, associative and idempotent.
pub fn merge_grant(id: &str, a: &Value, b: &Value) -> Result<Value, String> {
    let later = |a: &Value, b: &Value| -> Result<Value, String> {
        Ok(match stamp(&b["at"])?.cmp(&stamp(&a["at"])?) {
            Ordering::Greater => b.clone(),
            Ordering::Less => a.clone(),
            Ordering::Equal if canonical(b) > canonical(a) => b.clone(),
            Ordering::Equal => a.clone(),
        })
    };
    let (ga, gb) = (grant_of(id, a), grant_of(id, b));
    let ((_, ra), (_, rb)) = match (ga, gb) {
        (Some(ga), Some(gb)) => (ga, gb),
        (Some(_), None) => return Ok(a.clone()),
        (None, Some(_)) => return Ok(b.clone()),
        (None, None) => return later(a, b),
    };
    let fixed = |v: &Value| {
        let mut object = held(v).expect("checked");
        object.remove("revokedAt");
        object
    };
    let (fa, fb) = (fixed(a), fixed(b));
    let mut object =
        if canonical(&Value::Object(fb.clone())) > canonical(&Value::Object(fa.clone())) {
            fb
        } else {
            fa
        };
    let revoked = match (ra, rb) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    };
    object.insert("revokedAt".into(), json!(revoked));
    let at = if stamp(&a["at"])? >= stamp(&b["at"])? {
        a["at"].clone()
    } else {
        b["at"].clone()
    };
    Ok(json!({ "value": jcs_string(&Value::Object(object)), "at": at }))
}

fn grants(row: Option<&Value>) -> Result<BTreeMap<String, Grant>, String> {
    Ok(settings(row, GRANTS_ROW)?
        .into_iter()
        .flatten()
        .filter_map(|(id, value)| Some((id.clone(), grant_of(id, value)?.0)))
        .collect())
}

// ---- set:assistant-applied

/// Every applied entry. A malformed value still names an id already applied (so it still refuses a replay), counts
/// for no grant's cap, and is pruned at once.
fn applied(row: Option<&Value>) -> Result<BTreeMap<String, Applied>, String> {
    let mut out = BTreeMap::new();
    for (id, value) in settings(row, APPLIED_ROW)?.into_iter().flatten() {
        if value["value"].is_null() {
            continue;
        }
        let entry = held(value)
            .filter(|o| o.len() == 3)
            .and_then(|o| {
                Some(Applied {
                    grant: o.get("grant")?.as_str()?.to_owned(),
                    at: safe(o.get("at")?)?,
                    applied: safe(o.get("applied")?)?,
                })
            })
            .unwrap_or(Applied {
                grant: String::new(),
                at: 0,
                applied: 0,
            });
        out.insert(id.clone(), entry);
    }
    Ok(out)
}

/// `assistant_open` (§5): the request, accepted with the applied entry to record, or the reason it is refused.
pub fn open(
    library: &str,
    sealed: &str,
    dropbox_row: Option<&Value>,
    grants_row: Option<&Value>,
    applied_row: Option<&Value>,
    now: u64,
) -> Result<Value, String> {
    if library.len() != 32
        || !library
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("invalid_library".into());
    }
    if now > MAX_SAFE_INTEGER {
        return Err("invalid_time".into());
    }
    let keys: Vec<[u8; 32]> = dropbox_keys(dropbox_row)?
        .into_iter()
        .map(|k| k.1)
        .collect();
    let grants = grants(grants_row)?;
    let applied = applied(applied_row)?;
    Ok(
        match den_assistant::check(&keys, library, sealed, &grants, &applied, now) {
            Ok(accepted) => json!({"accept": {
                "grant": accepted.grant,
                "id": accepted.id,
                "at": accepted.at,
                "op": accepted.op,
                "args": accepted.args,
                "setting": accepted.id,
                "value": jcs_string(&json!({"grant": accepted.grant, "at": accepted.at, "applied": now})),
            }}),
            Err(reason) => json!({ "reject": reason.as_str() }),
        },
    )
}

/// `assistant_prune` (§4): the applied entries to drop.
pub fn prune(applied_row: Option<&Value>, now: u64) -> Result<Value, String> {
    Ok(json!({ "remove": den_assistant::prune(&applied(applied_row)?, now) }))
}
