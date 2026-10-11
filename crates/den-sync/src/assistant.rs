//! Assistant writes (den-spec `wire/assistant-v1.md`) as the clients call them: the library's three settings rows
//! (`set:assistant`, `set:assistant-grants`, `set:assistant-applied`, §5) read into what `den_assistant` checks, and
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
/// Assistant reads (§15): one read record per connection that may read, by grant id.
pub const READ_ROW: &str = "assistant-read";
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

/// A grant value (§5) checked against the grant id it is stored under: `None` when malformed.
fn grant_of(id: &str, value: &Value) -> Option<(Grant, Option<u64>)> {
    grant_in(id, &held(value)?)
}

fn grant_in(id: &str, object: &Map<String, Value>) -> Option<(Grant, Option<u64>)> {
    let keys = [
        "cap",
        "client",
        "createdAt",
        "expiresAt",
        "ops",
        "pk",
        "revokedAt",
        "v",
    ];
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
    let expires = safe(&object["expiresAt"])?;
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
            expires,
        },
        revoked,
    ))
}

/// The `revokedAt` a grant value holds, when it is a time: honoured whatever else the value holds (§5).
fn revoked_in(object: &Map<String, Value>) -> Option<u64> {
    object.get("revokedAt").and_then(safe)
}

/// A read record (§15) checked against the grant id it is stored under: its read key, `expiresAt` and `revokedAt`.
/// `None` when malformed.
fn read_in(id: &str, object: &Map<String, Value>) -> Option<([u8; 32], u64, Option<u64>)> {
    let keys = ["client", "createdAt", "expiresAt", "key", "revokedAt", "v"];
    if object.len() != keys.len()
        || !keys.iter().all(|k| object.contains_key(*k))
        || object["v"] != 1
    {
        return None;
    }
    if id.len() != 32 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let key: [u8; 32] = object["key"]
        .as_str()
        .and_then(den_assistant::b64url_decode)?
        .try_into()
        .ok()?;
    if !object["client"].as_str().is_some_and(clean_client) {
        return None;
    }
    safe(&object["createdAt"])?;
    let expires = safe(&object["expiresAt"])?;
    let revoked = match &object["revokedAt"] {
        Value::Null => None,
        other => Some(safe(other)?),
    };
    Some((key, expires, revoked))
}

/// Which records a setting of the grants or read row holds, for `rank` and the merge.
#[derive(Clone, Copy)]
pub enum Record {
    Grant,
    Read,
}

/// How a grant or read setting ranks in a merge (§5, §15): not a JSON object, a malformed object, a v1 record, a newer
/// version. Read with `revokedAt` set aside, so writing a revocation into a value never changes its rank.
fn rank(record: Record, id: &str, value: &Value) -> (u8, Option<Map<String, Value>>) {
    let Some(object) = held(value) else {
        return (0, None);
    };
    if object
        .get("v")
        .and_then(Value::as_u64)
        .is_some_and(|v| v > 1)
    {
        return (3, Some(object));
    }
    let mut cleared = object.clone();
    cleared.insert("revokedAt".into(), Value::Null);
    let valid = match record {
        Record::Grant => grant_in(id, &cleared).is_some(),
        Record::Read => read_in(id, &cleared).is_some(),
    };
    (if valid { 2 } else { 1 }, Some(object))
}

/// A value's `revokedAt` as the merge joins it: absent, then anything that is not a time (by JCS), then `null`, then
/// a time — the earlier the greater, since an earlier revocation stands.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Revocation {
    Absent,
    Other(Vec<u8>),
    Null,
    At(std::cmp::Reverse<u64>),
}

fn revocation(object: &Map<String, Value>) -> Revocation {
    match object.get("revokedAt") {
        None => Revocation::Absent,
        Some(Value::Null) => Revocation::Null,
        Some(value) => match safe(value) {
            Some(t) => Revocation::At(std::cmp::Reverse(t)),
            None => Revocation::Other(canonical(value)),
        },
    }
}

fn without(object: &Map<String, Value>, keys: &[&str]) -> Vec<u8> {
    let mut object = object.clone();
    for key in keys {
        object.remove(*key);
    }
    canonical(&Value::Object(object))
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
    let expires = now
        .checked_add(den_assistant::GRANT_TTL_MS)
        .filter(|t| *t <= MAX_SAFE_INTEGER)
        .ok_or("invalid_time")?;
    let key = den_assistant::GrantKey::from_secret(&secret);
    let grant = json!({
        "v": 1,
        "pk": den_assistant::b64url(&key.public()),
        "client": client,
        "ops": ops,
        "cap": cap,
        "createdAt": now,
        "expiresAt": expires,
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

/// `assistant_revoke`: a grant value with `revokedAt` set — kept at the earlier time when it already was. Any value
/// that is a JSON object can be revoked, a newer version's or a malformed one's included, so Settings can revoke
/// every grant it lists.
pub fn revoke(id: &str, value: &Value, now: u64) -> Result<Value, String> {
    if now > MAX_SAFE_INTEGER {
        return Err("invalid_time".into());
    }
    let (rank, object) = rank(Record::Grant, id, &json!({ "value": value }));
    let mut object = object.filter(|_| rank > 0).ok_or("invalid_grant")?;
    let at = revoked_in(&object).map_or(now, |r| r.min(now));
    object.insert("revokedAt".into(), json!(at));
    Ok(json!({ "value": jcs_string(&Value::Object(object)) }))
}

/// `assistant_renew`: a v1 grant value, or a v1 read record (§15), whose `expiresAt` is `now` + 30 days, or kept when
/// it is later. A revoked one stays revoked.
pub fn renew(id: &str, value: &Value, now: u64) -> Result<Value, String> {
    let until = now
        .checked_add(den_assistant::GRANT_TTL_MS)
        .filter(|t| *t <= MAX_SAFE_INTEGER)
        .ok_or("invalid_time")?;
    let stamped = json!({ "value": value });
    let mut object = held(&stamped).ok_or("invalid_grant")?;
    let expires = grant_in(id, &object)
        .map(|(grant, _)| grant.expires)
        .or_else(|| read_in(id, &object).map(|(_, expires, _)| expires))
        .ok_or("invalid_grant")?;
    object.insert("expiresAt".into(), json!(expires.max(until)));
    Ok(json!({ "value": jcs_string(&Value::Object(object)) }))
}

/// `assistant_grants`: every grant the row holds, for Settings to list and revoke — with its state at `now`, a
/// device's own record of revocations (§6) counting as revoked — and every read record (§15), whose state also counts
/// a revocation of the grant with its id.
pub fn list(
    row: Option<&Value>,
    read_row: Option<&Value>,
    local_revoked: &[String],
    now: u64,
) -> Result<Value, String> {
    let mut reads = Vec::new();
    for (id, value) in settings(read_row, READ_ROW)?.into_iter().flatten() {
        let (rank, object) = rank(Record::Read, id, value);
        let mut entry = json!({ "grant": id });
        if let Some(object) = &object {
            for key in ["client", "createdAt", "expiresAt", "revokedAt"] {
                if let Some(member) = object.get(key) {
                    entry[key] = member.clone();
                }
            }
        }
        entry["state"] = json!(read_state(id, value, row, local_revoked, now)?.unwrap_or(
            match rank {
                3 => "newer",
                _ => "malformed",
            }
        ));
        reads.push(entry);
    }
    let mut out = Vec::new();
    for (id, value) in settings(row, GRANTS_ROW)?.into_iter().flatten() {
        let (rank, object) = rank(Record::Grant, id, value);
        let mut entry = json!({ "grant": id });
        if let Some(object) = &object {
            for key in [
                "client",
                "ops",
                "cap",
                "createdAt",
                "expiresAt",
                "revokedAt",
            ] {
                if let Some(member) = object.get(key) {
                    entry[key] = member.clone();
                }
            }
        }
        let revoked = local_revoked.contains(id) || object.as_ref().and_then(revoked_in).is_some();
        entry["state"] = json!(match (rank, grant_of(id, value)) {
            _ if revoked => "revoked",
            (3, _) => "newer",
            (_, Some((grant, _))) if now >= grant.expires => "expired",
            (_, Some(_)) => "active",
            _ => "malformed",
        });
        out.push(entry);
    }
    Ok(json!({ "grants": out, "reads": reads }))
}

/// A read record's state (§15): `revoked` — in the record, in the grant with its id, or in the device's own set —
/// `expired`, or `active`; `None` when it is no v1 read record.
fn read_state(
    id: &str,
    value: &Value,
    grants_row: Option<&Value>,
    local_revoked: &[String],
    now: u64,
) -> Result<Option<&'static str>, String> {
    let object = held(value);
    let grant_revoked = settings(grants_row, GRANTS_ROW)?
        .and_then(|values| values.get(id))
        .and_then(held)
        .and_then(|o| revoked_in(&o))
        .is_some();
    let revoked = local_revoked.iter().any(|r| r == id)
        || grant_revoked
        || object.as_ref().and_then(revoked_in).is_some();
    Ok(match object.as_ref().and_then(|o| read_in(id, o)) {
        _ if revoked && object.is_some() => Some("revoked"),
        Some((_, expires, _)) if now >= expires => Some("expired"),
        Some(_) => Some("active"),
        None => None,
    })
}

/// `assistant_keygen_read`: 32 random bytes → a connection's read key and its `set:assistant-read` record, under the
/// connection's grant id (§15).
pub fn keygen_read(grant: &str, random: &str, client: &str, now: u64) -> Result<Value, String> {
    let key = seed(random)?;
    if grant.len() != 32
        || !grant
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("invalid_grant".into());
    }
    if !clean_client(client) {
        return Err("invalid_client".into());
    }
    let expires = now
        .checked_add(den_assistant::GRANT_TTL_MS)
        .filter(|t| *t <= MAX_SAFE_INTEGER)
        .ok_or("invalid_time")?;
    let record = json!({
        "v": 1,
        "key": den_assistant::b64url(&key),
        "client": client,
        "createdAt": now,
        "expiresAt": expires,
        "revokedAt": null,
    });
    Ok(json!({
        "key": den_assistant::b64url(&key),
        "setting": grant,
        "value": jcs_string(&record),
    }))
}

/// `assistant_grant_key`: 32 random bytes → a grant key and its id, with no grants row setting — for a connection
/// that may only read (§15), which still needs a grant id and a key for den-edge to carry.
pub fn grant_key(random: &str) -> Result<Value, String> {
    let key = den_assistant::GrantKey::from_secret(&seed(random)?);
    Ok(json!({
        "grant": key.id(),
        "public": den_assistant::b64url(&key.public()),
        "secret": den_assistant::b64url(key.secret()),
    }))
}

/// `assistant_projection` (§15): the projection for every live read grant, sealed under its read key, and the read
/// grants whose projection den-edge should drop.
pub fn projection(
    lib: &crate::projection::Library,
    read_row: Option<&Value>,
    grants_row: Option<&Value>,
    local_revoked: &[String],
    random: &str,
    now: u64,
) -> Result<Value, String> {
    if lib.library.len() != 32
        || !lib
            .library
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("invalid_library".into());
    }
    if now > MAX_SAFE_INTEGER {
        return Err("invalid_time".into());
    }
    let random = seed(random)?;
    let (watchlist, continuing, seen, skipped) = crate::projection::lists(lib, now as i64)?;
    let lists = (watchlist, continuing, seen);
    let mut publish = Vec::new();
    let mut delete = Vec::new();
    let mut digest = String::new();
    let mut counts = Value::Null;
    for (id, value) in settings(read_row, READ_ROW)?.into_iter().flatten() {
        let live = read_state(id, value, grants_row, local_revoked, now)? == Some("active");
        let record = held(value).and_then(|o| read_in(id, &o));
        let Some((key, _, _)) = record.filter(|_| live) else {
            delete.push(json!(id));
            continue;
        };
        let (plain, hash) = crate::projection::plaintext(lib, id, &lists, now);
        digest = hash;
        counts = json!({
            "watchlist": plain["watchlist"].as_array().map_or(0, Vec::len),
            "continue": plain["continue"].as_array().map_or(0, Vec::len),
            "seen": plain["seen"].as_array().map_or(0, Vec::len),
        });
        let sealed = den_assistant::seal_projection(
            &key,
            lib.library,
            id,
            &canonical(&plain),
            &crate::projection::nonce(&random, id),
        )
        .map_err(|_| "too_large")?;
        publish.push(json!({"grant": id, "sealed": sealed}));
    }
    Ok(json!({
        "publish": publish,
        "delete": delete,
        "digest": if digest.is_empty() { Value::Null } else { json!(digest) },
        "counts": counts,
        "skipped": skipped,
    }))
}

/// One grant setting merged (§5). The higher rank wins (`rank`): a newer version over a v1 grant over a malformed
/// value over one that is not an object. Two v1 grants keep the byte-greater JCS of every member but `revokedAt` and
/// `expiresAt` (a grant is written once, so they agree), the later `expiresAt` (a renewal), and the later stamp. Two
/// values of another rank: the later stamp, then the byte-greater JCS without `revokedAt`. Then, whatever won, its
/// `revokedAt` is the join of both sides' (`Revocation`): a revocation either side holds is kept, even one in a value
/// that lost, and an earlier one stands. Each part is a join, so the merge is commutative, associative and
/// idempotent.
pub fn merge_grant(id: &str, a: &Value, b: &Value) -> Result<Value, String> {
    merge_record(Record::Grant, id, a, b)
}

/// One read record merged (§15), by the grants' rule.
pub fn merge_read(id: &str, a: &Value, b: &Value) -> Result<Value, String> {
    merge_record(Record::Read, id, a, b)
}

fn merge_record(record: Record, id: &str, a: &Value, b: &Value) -> Result<Value, String> {
    if a == b {
        return Ok(a.clone());
    }
    let (sa, sb) = (stamp(&a["at"])?, stamp(&b["at"])?);
    let ((ra, oa), (rb, ob)) = (rank(record, id, a), rank(record, id, b));
    let revoked = [&oa, &ob]
        .into_iter()
        .flatten()
        .map(revocation)
        .max()
        .unwrap_or(Revocation::Absent);
    let key = |v: &Value, o: &Option<Map<String, Value>>| match o {
        Some(object) => without(object, &["revokedAt"]),
        None => canonical(&v["value"]),
    };
    let (mut value, at) = match ra.cmp(&rb) {
        Ordering::Greater => (a.clone(), sa),
        Ordering::Less => (b.clone(), sb),
        Ordering::Equal if ra == 2 => {
            let (oa, ob) = (oa.clone().expect("ranked"), ob.clone().expect("ranked"));
            let fixed = |o: &Map<String, Value>| without(o, &["revokedAt", "expiresAt"]);
            let mut object = if fixed(&ob) > fixed(&oa) {
                ob.clone()
            } else {
                oa.clone()
            };
            let expires = safe(&oa["expiresAt"]).max(safe(&ob["expiresAt"]));
            object.insert("expiresAt".into(), json!(expires));
            (
                json!({"value": jcs_string(&Value::Object(object))}),
                sa.clone().max(sb.clone()),
            )
        }
        Ordering::Equal => {
            if (sb.clone(), key(b, &ob)) > (sa.clone(), key(a, &oa)) {
                (b.clone(), sb)
            } else {
                (a.clone(), sa)
            }
        }
    };
    // Rewritten as JCS whenever it is an object, so both orders give the same bytes.
    if let Some(mut object) = held(&value) {
        match revoked {
            Revocation::Absent => {
                object.remove("revokedAt");
            }
            Revocation::Other(bytes) => {
                let other = serde_json::from_slice(&bytes).expect("canonical JSON");
                object.insert("revokedAt".into(), other);
            }
            Revocation::Null => {
                object.insert("revokedAt".into(), Value::Null);
            }
            Revocation::At(std::cmp::Reverse(t)) => {
                object.insert("revokedAt".into(), json!(t));
            }
        }
        value["value"] = jcs_string(&Value::Object(object));
    }
    value["at"] = json!(at);
    Ok(value)
}

/// Every v1 grant the row holds, a grant in the device's own record of revocations (§6) counting as revoked.
fn grants(
    row: Option<&Value>,
    local_revoked: &[String],
) -> Result<BTreeMap<String, Grant>, String> {
    Ok(settings(row, GRANTS_ROW)?
        .into_iter()
        .flatten()
        .filter_map(|(id, value)| {
            let (mut grant, _) = grant_of(id, value)?;
            grant.revoked |= local_revoked.contains(id);
            Some((id.clone(), grant))
        })
        .collect())
}

// ---- set:assistant-applied

/// One applied entry from its tagged value. A malformed value still names an id already applied (so it still refuses
/// a replay), counts for no grant's cap, and is pruned at once. `applied` is read as at least `at`.
fn applied_entry(value: &Value) -> Applied {
    held(&json!({ "value": value }))
        .filter(|o| o.len() == 3)
        .and_then(|o| {
            let at = safe(o.get("at")?)?;
            Some(Applied {
                grant: o.get("grant")?.as_str()?.to_owned(),
                at,
                applied: safe(o.get("applied")?)?.max(at),
            })
        })
        .unwrap_or(Applied {
            grant: String::new(),
            at: 0,
            applied: 0,
        })
}

/// Every applied entry: the row's, and the device's own record (§6) — id → the tagged value it recorded. An id in
/// both counts once, at the later `applied`.
fn applied(
    row: Option<&Value>,
    local: &Map<String, Value>,
) -> Result<BTreeMap<String, Applied>, String> {
    let mut out: BTreeMap<String, Applied> = BTreeMap::new();
    let row_entries = settings(row, APPLIED_ROW)?
        .into_iter()
        .flatten()
        .map(|(id, setting)| (id, &setting["value"]));
    for (id, value) in row_entries.chain(local.iter()) {
        if value.is_null() {
            continue;
        }
        let entry = applied_entry(value);
        match out.get(id) {
            Some(prior) if prior.applied >= entry.applied => {}
            _ => {
                out.insert(id.clone(), entry);
            }
        }
    }
    Ok(out)
}

fn device(text: &str) -> Result<&str, String> {
    (text.len() == 16 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then_some(text)
        .ok_or_else(|| "invalid_device".into())
}

/// What `assistant_open` reads besides the request: the three rows and the device's own records.
pub struct Library<'a> {
    pub library: &'a str,
    pub device: &'a str,
    pub dropbox: Option<&'a Value>,
    pub grants: Option<&'a Value>,
    pub applied: Option<&'a Value>,
    pub local_revoked: &'a [String],
    pub local_applied: &'a Map<String, Value>,
}

/// `assistant_open` (§6): the request, accepted with the stamp to write it at and the applied entry to record, or the
/// reason it is refused.
pub fn open(sealed: &str, lib: &Library, now: u64) -> Result<Value, String> {
    let library = lib.library;
    if library.len() != 32
        || !library
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("invalid_library".into());
    }
    let device = device(lib.device)?;
    if now > MAX_SAFE_INTEGER {
        return Err("invalid_time".into());
    }
    let keys: Vec<[u8; 32]> = dropbox_keys(lib.dropbox)?
        .into_iter()
        .map(|k| k.1)
        .collect();
    let grants = grants(lib.grants, lib.local_revoked)?;
    let applied = applied(lib.applied, lib.local_applied)?;
    Ok(
        match den_assistant::check(&keys, library, sealed, &grants, &applied, now) {
            Ok(accepted) => json!({"accept": {
                "grant": accepted.grant,
                "id": accepted.id,
                "at": accepted.at,
                "op": accepted.op,
                "args": accepted.args,
                // §6 *Performing*: the write is stamped at the request's own time, so `apply_write`'s replay rule
                // keeps any change the library already holds from later.
                "stamp": [accepted.at, 0, device],
                "setting": accepted.id,
                "value": jcs_string(&json!({"grant": accepted.grant, "at": accepted.at, "applied": now})),
            }}),
            Err(reason) => json!({ "reject": reason.as_str() }),
        },
    )
}

/// `assistant_prune` (§5): the applied entries to drop, from the row and from the device's own record alike.
pub fn prune(
    applied_row: Option<&Value>,
    local: &Map<String, Value>,
    now: u64,
) -> Result<Value, String> {
    Ok(json!({ "remove": den_assistant::prune(&applied(applied_row, local)?, now) }))
}
