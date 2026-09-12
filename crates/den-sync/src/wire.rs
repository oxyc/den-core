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

fn stamp(value: &Value) -> Result<Stamp, String> {
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
        _ => Err("invalid_kind".into()),
    }
}

fn newest(row: &Value) -> Result<Stamp, String> {
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
    Ok(if stamp(&b["at"])? > stamp(&a["at"])? {
        b
    } else {
        a
    }
    .clone())
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

/// The existing den-spec v2 merge. Unknown fields survive; no encryption or projection is performed here.
pub fn merge(a: &Value, b: &Value) -> Result<Value, String> {
    if name(a)? != name(b)? {
        return Err("identity_mismatch".into());
    }
    let (older, newer) = if newest(b)? > newest(a)? {
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
            let mut values = object(&a["values"])?.clone();
            for (key, value) in object(&b["values"])? {
                let value = match values.get(key) {
                    Some(prior) => later(prior, value)?,
                    None => value.clone(),
                };
                values.insert(key.clone(), value);
            }
            out.insert("values".into(), Value::Object(values));
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
