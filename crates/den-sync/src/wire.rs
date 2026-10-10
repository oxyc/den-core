use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::cmp::Ordering;

pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Wire tuple order is time, counter, then UTF-8 device bytes. No wall clock is read here.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp(pub i64, pub u64, pub String);

impl Stamp {
    pub fn validate(&self) -> Result<(), String> {
        if self.0.unsigned_abs() > MAX_SAFE_INTEGER || self.1 > MAX_SAFE_INTEGER {
            return Err("unsafe_stamp".into());
        }
        Ok(())
    }
    pub fn issue(&self, now: i64, device: String) -> Result<Self, String> {
        self.validate()?;
        let t = now.max(self.0);
        let c = if t == self.0 {
            self.1.checked_add(1).ok_or("counter_overflow")?
        } else {
            0
        };
        let stamp = Self(t, c, device);
        stamp.validate()?;
        Ok(stamp)
    }
}

pub(crate) fn stamp(value: &Value) -> Result<Stamp, String> {
    let at: Stamp = serde_json::from_value(value.clone()).map_err(|_| "invalid_stamp")?;
    at.validate()?;
    Ok(at)
}

fn integer(value: &Value) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|n| *n <= MAX_SAFE_INTEGER)
        .ok_or_else(|| "invalid_integer".into())
}

fn time(value: &Value) -> Result<i64, String> {
    value
        .as_i64()
        .filter(|n| n.unsigned_abs() <= MAX_SAFE_INTEGER)
        .ok_or_else(|| "invalid_time".into())
}

fn object(value: &Value) -> Result<&Map<String, Value>, String> {
    value.as_object().ok_or_else(|| "invalid_row".into())
}

pub(crate) fn name(row: &Value) -> Result<String, String> {
    object(row)?;
    integer(&row["schema"])?;
    let kind = row["kind"].as_str().ok_or("invalid_kind")?;
    if kind == "set" {
        return row["name"]
            .as_str()
            .map(|n| format!("set:{n}"))
            .ok_or_else(|| "invalid_name".into());
    }
    if kind == "snt" {
        let provider = row["provider"].as_str().ok_or("invalid_provider")?;
        let account = row["account"].as_str().ok_or("invalid_account")?;
        if provider.is_empty() || account.is_empty() || provider.contains(':') {
            return Err("invalid_receipt_identity".into());
        }
        if let Some(target) = row["target"].as_str() {
            return Ok(format!("snt:{provider}:{account}:{target}"));
        }
        let shard = row["shard"].as_str().ok_or("invalid_shard")?;
        if shard.len() != 3
            || !shard
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("invalid_shard".into());
        }
        return Ok(format!("snt:{provider}:{account}:t{shard}"));
    }
    let media = row["title"]["type"].as_str().ok_or("invalid_title")?;
    if !matches!(media, "movie" | "tv") {
        return Err("invalid_title".into());
    }
    let id = integer(&row["title"]["id"])?;
    if id == 0 {
        return Err("invalid_title".into());
    }
    match kind {
        "rec" => Ok(format!("rec:{media}:{id}")),
        "ep" if media == "tv" => Ok(format!(
            "ep:{media}:{id}:{}:{}",
            integer(&row["season"])?,
            integer(&row["episode"])?
        )),
        "wat" => {
            let season = integer(&row["season"])?;
            let block = integer(&row["block"])?;
            if media == "movie" && (season != 0 || block != 0) {
                return Err("invalid_film_watch_identity".into());
            }
            Ok(format!("wat:{media}:{id}:{season}:{block}"))
        }
        _ => Err("invalid_kind".into()),
    }
}

pub(crate) fn newest(row: &Value) -> Result<Stamp, String> {
    let stamps: Vec<&Value> = match row["kind"].as_str() {
        Some("rec") => {
            let mut fields: Vec<_> = ["status", "resume", "reaction", "deleted", "dismissed"]
                .iter()
                .map(|k| &row[k]["at"])
                .collect();
            if !row["episodesReset"].is_null() {
                fields.push(&row["episodesReset"]);
            }
            fields
        }
        Some("ep") => vec![&row["progress"]["at"]],
        Some("set") => object(&row["values"])?.values().map(|v| &v["at"]).collect(),
        Some("wat") => {
            let mut fields = Vec::new();
            if !row["seasonReset"].is_null() {
                fields.push(&row["seasonReset"]);
            }
            for register in object(&row["entries"])?.values() {
                if !register["progress"].is_null() {
                    fields.push(&register["progress"]["at"]);
                }
                if !register["cleared"].is_null() {
                    fields.push(&register["cleared"][1]);
                }
            }
            fields
        }
        Some("snt") => {
            let mut fields = Vec::new();
            for entry in object(&row["entries"])?.values() {
                let values = entry.as_array().ok_or("invalid_receipt")?;
                let order = values
                    .iter()
                    .rev()
                    .find(|value| {
                        value.as_array().is_some_and(|parts| {
                            parts.len() == 3
                                && parts[0].is_u64()
                                && parts[1].is_u64()
                                && parts[2].is_string()
                        })
                    })
                    .ok_or("invalid_receipt")?;
                fields.push(order);
            }
            // Settle orders have the stamp shape and deliberately do not use wall clock time.
            fields
        }
        _ => return Err("invalid_kind".into()),
    };
    stamps
        .into_iter()
        .try_fold(Stamp::default(), |latest, at| Ok(latest.max(stamp(at)?)))
}

fn later(a: &Value, b: &Value) -> Result<Value, String> {
    if !object(a)?.contains_key("value") || !object(b)?.contains_key("value") {
        return Err("invalid_value".into());
    }
    let order = stamp(&b["at"])?.cmp(&stamp(&a["at"])?);
    Ok(match order {
        Ordering::Greater => b,
        Ordering::Less => a,
        Ordering::Equal if canonical(b) > canonical(a) => b,
        Ordering::Equal => a,
    }
    .clone())
}

/// The `removals` latch of a `set:deliver` row (library-v4 §9): its `approved` and `held` stamps, either absent.
/// `None` for a value that is not an object of stamps.
pub(crate) fn removals_latch(value: &Value) -> Option<(Option<Stamp>, Option<Stamp>)> {
    let object = value.as_object()?;
    let field = |name: &str| -> Option<Option<Stamp>> {
        match object.get(name) {
            None => Some(None),
            Some(found) => stamp(found).ok().map(Some),
        }
    };
    Some((field("approved")?, field("held")?))
}

/// A `set:deliver` setting merged by its own rule (library-v3 §6, v4 §9): `since` to the earlier stamp it holds,
/// `lease` by epoch, then an empty holder, then the JCS of the stamped value, `unverified` as the union of its epochs
/// at the later stamp, and `removals` by the later `approved` and the later `held`, each on its own. A value not in
/// its setting's form ranks below every value that is, and two of them merge by the later stamp, so each rule stays a
/// join over any mix of values. Any other setting merges by the later stamp.
fn merge_deliver(key: &str, a: &Value, b: &Value) -> Result<Value, String> {
    let pick = |a_wins: bool| if a_wins { a.clone() } else { b.clone() };
    // A well-formed value beats a malformed one; two malformed ones go by the later stamp.
    let malformed = |a_ok: bool, b_ok: bool| -> Option<Result<Value, String>> {
        match (a_ok, b_ok) {
            (true, true) => None,
            (true, false) => Some(Ok(a.clone())),
            (false, true) => Some(Ok(b.clone())),
            (false, false) => Some(later(a, b)),
        }
    };
    let json_string = |v: &Value| {
        v["value"]["string"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
    };
    match key {
        "since" => {
            let held = |v: &Value| json_string(v).and_then(|s| stamp(&s).ok());
            let (sa, sb) = (held(a), held(b));
            if let Some(result) = malformed(sa.is_some(), sb.is_some()) {
                return result;
            }
            let (sa, sb) = (sa.unwrap_or_default(), sb.unwrap_or_default());
            Ok(pick(match sa.cmp(&sb) {
                Ordering::Less => true,
                Ordering::Greater => false,
                Ordering::Equal => canonical(a) >= canonical(b),
            }))
        }
        "lease" => {
            let rank = |v: &Value| {
                let parts = v["value"]["strings"].as_array()?;
                let holder = parts.first()?.as_str()?;
                let epoch = parts.get(1)?.as_str()?.parse::<u64>().ok()?;
                Some((epoch, holder.is_empty(), canonical(v)))
            };
            let (ra, rb) = (rank(a), rank(b));
            if let Some(result) = malformed(ra.is_some(), rb.is_some()) {
                return result;
            }
            Ok(pick(ra >= rb))
        }
        "unverified" => {
            let epochs = |v: &Value| -> Option<Vec<u64>> {
                v["value"]["ints"]
                    .as_array()?
                    .iter()
                    .map(Value::as_u64)
                    .collect()
            };
            let (ea, eb) = (epochs(a), epochs(b));
            if let Some(result) = malformed(ea.is_some(), eb.is_some()) {
                return result;
            }
            let union: std::collections::BTreeSet<u64> = ea
                .into_iter()
                .flatten()
                .chain(eb.into_iter().flatten())
                .collect();
            let at = later(a, b)?["at"].clone();
            Ok(json!({"value": {"ints": union.into_iter().collect::<Vec<_>>()}, "at": at}))
        }
        "removals" => {
            if a == b {
                return Ok(a.clone());
            }
            let (la, lb) = (
                json_string(a).as_ref().and_then(removals_latch),
                json_string(b).as_ref().and_then(removals_latch),
            );
            if let Some(result) = malformed(la.is_some(), lb.is_some()) {
                return result;
            }
            let ((aa, ha), (ab, hb)) = (la.unwrap_or_default(), lb.unwrap_or_default());
            // Always the canonical JSON of the join, at the later of the two stamps, so every grouping of three
            // versions gives the same value.
            let mut latch = Map::new();
            if let Some(approved) = aa.max(ab) {
                latch.insert("approved".into(), json!(approved));
            }
            if let Some(held) = ha.max(hb) {
                latch.insert("held".into(), json!(held));
            }
            let text = serde_json::to_string(&Value::Object(latch))
                .map_err(|_| "invalid_removals".to_string())?;
            let at = if stamp(&a["at"])? >= stamp(&b["at"])? {
                a["at"].clone()
            } else {
                b["at"].clone()
            };
            Ok(json!({"value": {"string": text}, "at": at}))
        }
        _ => later(a, b),
    }
}

pub(crate) fn canonical(value: &Value) -> Vec<u8> {
    // serde_json maps are sorted without preserve_order.  All den wire numbers are integers except
    // progress, whose finite representation is already the shortest JSON form, matching JCS here.
    serde_json::to_vec(value).expect("JSON value always serializes")
}

fn furthest(a: &Value, b: &Value) -> Result<Value, String> {
    fn number(value: &Value) -> Result<f64, String> {
        value
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or_else(|| "invalid_progress".into())
    }
    // Validate both inputs before selecting a winner. Numeric equality includes -0 == +0, as in
    // Swift/JavaScript; a total float ordering would incorrectly outrank the newer stamp on zero.
    let av = number(&a["value"])?;
    let bv = number(&b["value"])?;
    let ordering = integer(&a["viewing"])?
        .cmp(&integer(&b["viewing"])?)
        .then_with(|| av.partial_cmp(&bv).unwrap_or(Ordering::Equal));
    let ordering = ordering.then(stamp(&a["at"])?.cmp(&stamp(&b["at"])?));
    let sa = if a["seconds"].is_null() {
        -1.0
    } else {
        number(&a["seconds"])?
    };
    let sb = if b["seconds"].is_null() {
        -1.0
    } else {
        number(&b["seconds"])?
    };
    Ok(
        if ordering.then(sa.partial_cmp(&sb).unwrap_or(Ordering::Equal)) == Ordering::Less {
            b
        } else {
            a
        }
        .clone(),
    )
}

fn merge_plays(a: &Value, b: &Value) -> Result<Value, String> {
    let mut plays = object(a)?.clone();
    for (key, value) in object(b)? {
        let parsed = key.parse::<i64>().map_err(|_| "invalid_play")?;
        let watched_at = integer(value)?;
        if watched_at == 0 {
            return Err("invalid_play".into());
        }
        if let Some(old) = plays.get(key) {
            if integer(old)? <= watched_at {
                continue;
            }
        }
        let _ = parsed;
        plays.insert(key.clone(), value.clone());
    }
    let mut keys = plays
        .keys()
        .map(|key| key.parse::<i64>().map(|number| (number, key.clone())))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid_play")?;
    keys.sort_unstable();
    if keys.len() > 8 {
        let keep = [keys[..1].to_vec(), keys[keys.len() - 7..].to_vec()].concat();
        plays.retain(|key, _| keep.iter().any(|(_, kept)| kept == key));
    }
    Ok(Value::Object(plays))
}

fn merge_register(a: &Value, b: &Value) -> Result<Value, String> {
    let mut out = object(a)?.clone();
    out.extend(object(b)?.clone());
    match (a.get("progress"), b.get("progress")) {
        (Some(a), Some(b)) => {
            out.insert("progress".into(), furthest(a, b)?);
        }
        (Some(a), None) => {
            out.insert("progress".into(), a.clone());
        }
        _ => {}
    }
    out.insert(
        "imported".into(),
        json!(a["imported"].as_bool().unwrap_or(false) || b["imported"].as_bool().unwrap_or(false)),
    );
    out.insert(
        "plays".into(),
        merge_plays(
            a.get("plays").unwrap_or(&json!({})),
            b.get("plays").unwrap_or(&json!({})),
        )?,
    );
    let cleared = match (
        a.get("cleared").filter(|v| !v.is_null()),
        b.get("cleared").filter(|v| !v.is_null()),
    ) {
        (None, None) => Value::Null,
        (Some(value), None) | (None, Some(value)) => value.clone(),
        (Some(a), Some(b)) => {
            let aa = a
                .as_array()
                .filter(|v| v.len() == 2)
                .ok_or("invalid_cleared")?;
            let bb = b
                .as_array()
                .filter(|v| v.len() == 2)
                .ok_or("invalid_cleared")?;
            let order = integer(&aa[0])?
                .cmp(&integer(&bb[0])?)
                .then(stamp(&aa[1])?.cmp(&stamp(&bb[1])?));
            if order == Ordering::Less {
                b.clone()
            } else {
                a.clone()
            }
        }
    };
    out.insert("cleared".into(), cleared);
    Ok(Value::Object(out))
}

fn receipt_order(entry: &Value) -> Result<(u64, u64, String), String> {
    entry
        .as_array()
        .ok_or("invalid_receipt")?
        .iter()
        .rev()
        .find_map(|value| {
            let parts = value.as_array()?;
            if parts.len() != 3 {
                return None;
            }
            Some((
                parts[0].as_u64()?,
                parts[1].as_u64()?,
                parts[2].as_str()?.to_owned(),
            ))
        })
        .ok_or_else(|| "invalid_receipt".into())
}

fn merge_receipt(a: &Value, b: &Value) -> Result<Value, String> {
    Ok(match receipt_order(a)?.cmp(&receipt_order(b)?) {
        Ordering::Less => b,
        Ordering::Greater => a,
        Ordering::Equal if canonical(b) > canonical(a) => b,
        Ordering::Equal => a,
    }
    .clone())
}

/// The existing den-spec v2 merge. Unknown fields survive; no encryption or projection is performed here.
pub fn merge(a: &Value, b: &Value) -> Result<Value, String> {
    if name(a)? != name(b)? {
        return Err("identity_mismatch".into());
    }
    let newest_a = newest(a)?;
    let newest_b = newest(b)?;
    let (older, newer) =
        if newest_b > newest_a || (newest_b == newest_a && canonical(b) > canonical(a)) {
            (a, b)
        } else {
            (b, a)
        };
    let mut out = object(older)?.clone();
    out.extend(object(newer)?.clone());
    out.insert(
        "schema".into(),
        json!(integer(&a["schema"])?.max(integer(&b["schema"])?)),
    );
    match a["kind"].as_str() {
        Some("rec") => {
            for field in ["status", "reaction", "deleted", "dismissed"] {
                out.insert(field.into(), later(&a[field], &b[field])?);
            }
            out.insert("resume".into(), furthest(&a["resume"], &b["resume"])?);
            let reset = match (a["episodesReset"].is_null(), b["episodesReset"].is_null()) {
                (true, _) => b["episodesReset"].clone(),
                (_, true) => a["episodesReset"].clone(),
                _ => json!(stamp(&a["episodesReset"])?.max(stamp(&b["episodesReset"])?)),
            };
            out.insert("episodesReset".into(), reset);
            out.insert(
                "addedAt".into(),
                json!(time(&a["addedAt"])?.min(time(&b["addedAt"])?)),
            );
            let watches = [&a["watchedAt"], &b["watchedAt"]]
                .into_iter()
                .filter(|v| !v.is_null())
                .map(time)
                .collect::<Result<Vec<_>, _>>()?;
            out.insert("watchedAt".into(), json!(watches.into_iter().min()));
        }
        Some("ep") => {
            out.insert("progress".into(), furthest(&a["progress"], &b["progress"])?);
        }
        Some("set") => {
            let row_name = a["name"].as_str().unwrap_or_default();
            // The download lease row (library-v4 §17) holds one `lease`, merged as `set:deliver`'s is.
            let deliver = row_name.starts_with("deliver:") || row_name == "download-lease";
            // Assistant grants (assistant-v1 §4) merge by grant, a revocation sticky.
            let grants = row_name == crate::assistant::GRANTS_ROW;
            let mut values = object(&a["values"])?.clone();
            for (key, value) in object(&b["values"])? {
                let value = match values.get(key) {
                    Some(prior) if deliver => merge_deliver(key, prior, value)?,
                    Some(prior) if grants => crate::assistant::merge_grant(key, prior, value)?,
                    Some(prior) => later(prior, value)?,
                    None => value.clone(),
                };
                values.insert(key.clone(), value);
            }
            if crate::downloads::is_download(row_name) {
                crate::downloads::clean(&mut values)?;
            }
            out.insert("values".into(), Value::Object(values));
        }
        Some("wat") => {
            let mut entries = object(&a["entries"])?.clone();
            for (key, value) in object(&b["entries"])? {
                let value = match entries.get(key) {
                    Some(prior) => merge_register(prior, value)?,
                    None => value.clone(),
                };
                entries.insert(key.clone(), value);
            }
            out.insert("entries".into(), Value::Object(entries));
            let reset = match (
                a.get("seasonReset").filter(|v| !v.is_null()),
                b.get("seasonReset").filter(|v| !v.is_null()),
            ) {
                (None, None) => Value::Null,
                (Some(value), None) | (None, Some(value)) => value.clone(),
                (Some(a), Some(b)) => match stamp(a)?.cmp(&stamp(b)?) {
                    Ordering::Less => b.clone(),
                    Ordering::Greater => a.clone(),
                    Ordering::Equal if canonical(b) > canonical(a) => b.clone(),
                    Ordering::Equal => a.clone(),
                },
            };
            out.insert("seasonReset".into(), reset);
        }
        Some("snt") => {
            let mut entries = object(&a["entries"])?.clone();
            for (key, value) in object(&b["entries"])? {
                let value = match entries.get(key) {
                    Some(prior) => merge_receipt(prior, value)?,
                    None => value.clone(),
                };
                entries.insert(key.clone(), value);
            }
            out.insert("entries".into(), Value::Object(entries));
        }
        _ => return Err("invalid_kind".into()),
    }
    Ok(Value::Object(out))
}

/// Capture explicit user intent only. Caller supplies the ID/time; imports must never call this operation.
pub fn capture(before: &Value, after: &Value, at: &Stamp, id: &str) -> Result<Value, String> {
    if name(before)? != name(after)? {
        return Err("identity_mismatch".into());
    }
    if integer(&after["schema"])? != 2 {
        return Err("unsupported_write_schema".into());
    }
    at.validate()?;
    if id.is_empty() || id.len() > 128 {
        return Err("invalid_event_id".into());
    }
    newest(before)?;
    newest(after)?;
    let fields: &[&str] = match after["kind"].as_str() {
        Some("rec") => &["status", "reaction", "deleted"],
        Some("ep") => &["progress"],
        _ => return Err("invalid_action_target".into()),
    };
    let mut changes = Map::new();
    for field in fields {
        if stamp(&after[field]["at"])? == *at && before[field] != after[field] {
            changes.insert(
                (*field).into(),
                json!({ "before": before[field], "after": after[field] }),
            );
        }
    }
    if changes.is_empty() {
        return Ok(Value::Null);
    }
    Ok(
        json!({ "schema": 1, "id": id, "at": at, "before": before, "after": after, "changes": changes }),
    )
}
