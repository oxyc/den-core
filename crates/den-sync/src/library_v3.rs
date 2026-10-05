//! Library-v3 policy.  Inputs contain every clock and provider fact: this module performs no I/O.

use crate::wire::{merge, name, stamp, Stamp, MAX_SAFE_INTEGER};
use serde_json::{json, Map, Value};

const DAY: i64 = 86_400_000;

fn object(value: &Value) -> Result<&Map<String, Value>, String> {
    value.as_object().ok_or_else(|| "invalid_object".into())
}

fn safe_u64(value: &Value) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|n| *n <= MAX_SAFE_INTEGER)
        .ok_or_else(|| "invalid_integer".into())
}

fn valid_device(device: &str) -> bool {
    device.is_empty()
        || device == "local"
        || (device.len() == 16
            && device
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn validate_writer_stamp(value: &Value) -> Result<Stamp, String> {
    let value = stamp(value)?;
    if !valid_device(&value.2) {
        return Err("invalid_device".into());
    }
    Ok(value)
}

fn effective_t(value: &Stamp, now: i64) -> i64 {
    if value.0 > now.saturating_add(DAY) {
        0
    } else {
        value.0
    }
}

fn floor(resets: &[Stamp], now: i64) -> i64 {
    resets
        .iter()
        .map(|r| effective_t(r, now))
        .filter(|t| *t > 0)
        .max()
        .unwrap_or(0)
}

fn plays(register: &Value, resets: &[Stamp], now: i64) -> Result<Vec<(i64, u64)>, String> {
    let reset = floor(resets, now);
    let cleared = register
        .get("cleared")
        .filter(|v| !v.is_null())
        .map(|v| {
            let parts = v
                .as_array()
                .filter(|v| v.len() == 2)
                .ok_or("invalid_cleared")?;
            Ok::<_, String>((safe_u64(&parts[0])? as i64, stamp(&parts[1])?.0))
        })
        .transpose()?
        .unwrap_or((-1, 0));
    let mut result = Vec::new();
    for (key, value) in register
        .get("plays")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let key = key.parse::<i64>().map_err(|_| "invalid_play")?;
        let at = safe_u64(value)?;
        if at > now.saturating_add(DAY).max(0) as u64 || at <= reset.max(0) as u64 {
            continue;
        }
        if (key >= 0 && key <= cleared.0) || (key < 0 && at <= cleared.1.max(0) as u64) {
            continue;
        }
        result.push((key, at));
    }
    result.sort_unstable_by_key(|(_, at)| *at);
    Ok(result)
}

pub fn episode_state(register: &Value, resets: &[Stamp], now: i64) -> Result<Value, String> {
    let register = object(register)?;
    let floor = floor(resets, now);
    let progress = register.get("progress").filter(|value| !value.is_null());
    let viewing = progress
        .map(|p| safe_u64(&p["viewing"]))
        .transpose()?
        .unwrap_or(0);
    let visible_progress =
        progress.filter(|p| stamp(&p["at"]).is_ok_and(|at| effective_t(&at, now) > floor));
    let visible_plays = plays(&Value::Object(register.clone()), resets, now)?;
    let imported_play = visible_plays.iter().any(|(key, _)| *key < 0);
    let imported = register
        .get("imported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && (floor == 0 || imported_play);
    let value = visible_progress.and_then(|p| p["value"].as_f64());
    let watched = value.is_some_and(|v| v >= 0.95) || (visible_progress.is_none() && imported);
    let resume = visible_progress
        .filter(|p| {
            p["value"]
                .as_f64()
                .is_some_and(|v| (0.02..0.95).contains(&v))
        })
        .cloned();
    let watched_at = visible_plays
        .iter()
        .find(|(key, _)| *key == viewing as i64)
        .or_else(|| visible_plays.last())
        .map(|(_, at)| *at);
    Ok(json!({
        "watched": watched,
        "resume": resume,
        "viewing": viewing,
        "plays": visible_plays.iter().map(|(p, at)| json!([p, at])).collect::<Vec<_>>(),
        "first_play": visible_plays.first().map(|(_, at)| *at),
        "watched_at": watched_at
    }))
}

pub fn film_state(
    rec: &Value,
    register: &Value,
    resets: &[Stamp],
    now: i64,
) -> Result<Value, String> {
    let mut state = episode_state(register, resets, now)?;
    let status = rec["status"]["value"].as_str().ok_or("invalid_status")?;
    let viewing = safe_u64(&rec["resume"]["viewing"])?;
    let visible = if matches!(status, "none" | "watchlist") {
        Vec::new()
    } else {
        plays(register, resets, now)?
    };
    let status_at = stamp(&rec["status"]["at"])?;
    let watched_at = visible
        .iter()
        .find(|(key, _)| *key == viewing as i64)
        .map(|(_, at)| *at)
        .or_else(|| {
            (status_at.0 > 0 && effective_t(&status_at, now) > 0).then_some(status_at.0 as u64)
        })
        .or_else(|| visible.last().map(|(_, at)| *at));
    state["watched"] = json!(status == "watched");
    state["resume"] = rec["resume"].clone();
    state["viewing"] = json!(viewing);
    state["plays"] = json!(visible
        .iter()
        .map(|(p, at)| json!([p, at]))
        .collect::<Vec<_>>());
    state["first_play"] = json!(visible.first().map(|(_, at)| *at));
    state["watched_at"] = json!(watched_at);
    Ok(state)
}

fn selected_plays(mut plays: Vec<(i64, u64)>) -> Map<String, Value> {
    plays.sort_unstable_by_key(|(key, _)| *key);
    plays.dedup_by(|a, b| {
        if a.0 == b.0 {
            b.1 = b.1.min(a.1);
            true
        } else {
            false
        }
    });
    if plays.len() > 8 {
        let first = plays[0];
        plays = std::iter::once(first)
            .chain(plays[plays.len() - 7..].iter().copied())
            .collect();
    }
    plays
        .into_iter()
        .map(|(key, at)| (key.to_string(), json!(at)))
        .collect()
}

pub fn register_write(
    action: &Value,
    current: Option<&Value>,
    resets: &[Stamp],
    now: i64,
) -> Result<Value, String> {
    let kind = action["kind"].as_str().ok_or("invalid_action")?;
    let at = validate_writer_stamp(&action["at"])?;
    if at.0 == 0 {
        return Err("timeless_progress".into());
    }
    let mut register = current
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| {
            Map::from_iter([
                ("imported".into(), json!(false)),
                ("plays".into(), json!({})),
                ("cleared".into(), Value::Null),
            ])
        });
    let old = Value::Object(register.clone());
    let old_state = episode_state(&old, resets, now)?;
    let viewing = old_state["viewing"].as_u64().unwrap_or(0);
    if kind == "mark_watched" && old_state["watched"] == json!(true) {
        return Ok(Value::Null);
    }
    let next_viewing = match kind {
        "replay" => viewing.checked_add(1).ok_or("viewing_overflow")?,
        "mark_watched"
            if old.get("progress").is_some()
                && stamp(&old["progress"]["at"])
                    .is_ok_and(|progress| effective_t(&progress, now) <= floor(resets, now)) =>
        {
            viewing.checked_add(1).ok_or("viewing_overflow")?
        }
        "mark_watched" if old_state["watched"] == json!(false) && old.get("progress").is_some() => {
            viewing
        }
        "mark_watched" => viewing,
        "progress" => action
            .get("viewing")
            .map(safe_u64)
            .transpose()?
            .unwrap_or(viewing),
        "unwatch" => viewing.checked_add(1).ok_or("viewing_overflow")?,
        _ => return Err("invalid_action".into()),
    };
    let value = match kind {
        "mark_watched" => 1.0,
        "replay" | "unwatch" => 0.0,
        _ => action["value"].as_f64().ok_or("invalid_progress")?,
    };
    if !(0.0..=1.0).contains(&value) {
        return Err("invalid_progress".into());
    }
    let mut progress = json!({"value":value,"at":at,"viewing":next_viewing});
    if let Some(seconds) = action.get("seconds") {
        if !seconds.is_null() {
            progress["seconds"] = seconds.clone();
        }
    }
    register.insert("progress".into(), progress);
    if matches!(kind, "mark_watched") {
        let watched_at = action
            .get("watched_at")
            .and_then(Value::as_u64)
            .unwrap_or(at.0 as u64);
        let mut all = register
            .get("plays")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_u64()?)))
            .collect::<Vec<_>>();
        all.push((next_viewing as i64, watched_at));
        register.insert("plays".into(), Value::Object(selected_plays(all)));
    }
    if kind == "unwatch" {
        register.insert("cleared".into(), json!([viewing, at]));
    }
    Ok(Value::Object(register))
}

pub fn import_write(
    register: Option<&Value>,
    item: &Value,
    resets: &[Stamp],
    now: i64,
) -> Result<Value, String> {
    let mut out = register
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| {
            Map::from_iter([
                ("imported".into(), json!(false)),
                ("plays".into(), json!({})),
                ("cleared".into(), Value::Null),
            ])
        });
    if register.is_some_and(|r| r.get("progress").is_some()) {
        return Ok(Value::Null);
    }
    let imported = item
        .get("plays")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let reset = floor(resets, now);
    let cleared_t = out
        .get("cleared")
        .filter(|v| !v.is_null())
        .and_then(|v| stamp(&v[1]).ok())
        .map(|s| s.0)
        .unwrap_or(0);
    let mut all = out
        .get("plays")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_u64()?)))
        .collect::<Vec<_>>();
    for value in imported {
        let at = safe_u64(&value)? / 1000 * 1000;
        if at < 1000 || at as i64 <= reset || at as i64 <= cleared_t {
            continue;
        }
        all.push((at as i64 - (1i64 << 53), at));
    }
    let selected = selected_plays(all);
    if out.get("plays").and_then(Value::as_object) == Some(&selected) {
        return Ok(Value::Null);
    }
    out.insert("plays".into(), Value::Object(selected));
    out.insert("imported".into(), json!(true));
    Ok(Value::Object(out))
}

fn value_stamp(value: &Value) -> Result<Stamp, String> {
    stamp(
        value
            .get("stamp")
            .or_else(|| value.get("at"))
            .ok_or("missing_stamp")?,
    )
}

fn receipt_value(entry: &Value) -> Option<Value> {
    let parts = entry.as_array()?;
    match parts.first()?.as_str()? {
        "b" => parts.get(1).cloned(),
        "w" => Some(json!("watched")),
        "u" => Some(json!("unwatched")),
        "n" => Some(json!("unwatched")),
        value => Some(json!(value)),
    }
}

/// Targets are normalized client inputs: `{key, kind, value, stamp, p?, watched_at?, episode?}`.
pub fn pending_targets(
    targets: &[Value],
    receipts: &Value,
    since: &Stamp,
    now: i64,
) -> Result<Value, String> {
    let receipts = object(receipts)?;
    let mut commands = Vec::new();
    for target in targets {
        let key = target["key"].as_str().ok_or("invalid_target")?;
        let current = target["value"].clone();
        let at = value_stamp(target)?;
        if effective_t(&at, now) == 0 && at.0 != 0 {
            continue;
        }
        let receipt = receipts.get(key);
        if receipt.and_then(receipt_value).as_ref() == Some(&current) {
            continue;
        }
        // With no settlement history, only additive catch-up is safe. An un-watch, removal,
        // or rating clear could erase provider state that Den never observed; it becomes eligible
        // after a receipt exists.
        if receipt.is_none() && matches!(current.as_str(), Some("unwatched" | "gone" | "none")) {
            continue;
        }
        let baseline = at.0 == 0 || receipt.is_none() && at <= *since;
        let kind = target["kind"].as_str().ok_or("invalid_target")?;
        let command = match kind {
            "episode" | "film" => match current.as_str() {
                Some("watched") => Some(json!({"kind":"watched"})),
                Some("unwatched") => Some(json!({"kind":"unwatched"})),
                _ => None,
            },
            "list" => match current.as_str() {
                Some("in") => Some(json!({"kind":"list","added":true})),
                Some("gone") => Some(json!({"kind":"list","added":false})),
                _ => None,
            },
            "rating" => Some(
                json!({"kind":"rating","rating":match current.as_str() {Some("love")=>Some(10),Some("like")=>Some(7),Some("dislike")=>Some(2),_=>None}}),
            ),
            _ => return Err("invalid_target".into()),
        };
        if command.is_none() {
            continue;
        }
        let mut command = command.unwrap();
        command["key"] = json!(key);
        command["at"] = json!(at.0.max(0));
        command["current"] = json!(true);
        command["baseline"] = json!(baseline);
        command["episode"] = json!(kind == "episode");
        if command.get("added").is_none() {
            command["added"] = json!(false);
        }
        if command.get("rating").is_none() {
            command["rating"] = Value::Null;
        }
        command["built_from"] = target.clone();
        if let Some(value) = target.get("p") {
            command["p"] = value.clone();
        }
        if let Some(value) = target.get("watched_at") {
            command["watched_at"] = value.clone();
        }
        commands.push(command);
    }
    commands.sort_by(|a, b| {
        a["at"]
            .as_i64()
            .cmp(&b["at"].as_i64())
            .then(a["key"].as_str().cmp(&b["key"].as_str()))
    });
    Ok(Value::Array(commands))
}

pub fn settle(outcome: &Value, built: &Value, order: &Value) -> Result<Value, String> {
    let action = outcome["action"].as_str().ok_or("invalid_outcome")?;
    if matches!(action, "hold" | "superseded")
        || (built["value"] == json!("none") && action != "resettle")
    {
        return Ok(Value::Null);
    }
    let order = order
        .as_array()
        .filter(|v| v.len() == 3)
        .ok_or("invalid_settle_order")?;
    safe_u64(&order[0])?;
    safe_u64(&order[1])?;
    let stamp = built.get("stamp").ok_or("missing_stamp")?;
    validate_writer_stamp(stamp)?;
    let value = built["value"].clone();
    let receipt = match built["kind"].as_str() {
        Some("episode" | "film") => json!([
            value
                .as_str()
                .and_then(|v| v.chars().next())
                .unwrap_or('n')
                .to_string(),
            built.get("p").and_then(Value::as_i64).unwrap_or(-1),
            built.get("watched_at").cloned().unwrap_or(Value::Null),
            stamp,
            order
        ]),
        Some("list") => json!([value, stamp, order]),
        Some("rating") => {
            let mut values = vec![value, stamp.clone(), Value::Array(order.clone())];
            if action == "acknowledge" {
                if let Some(remote) = outcome.get("remote_rating").filter(|v| !v.is_null()) {
                    values.push(remote.clone());
                }
            }
            Value::Array(values)
        }
        _ => return Err("invalid_target".into()),
    };
    Ok(receipt)
}

/// Pure lease state machine. Durations are supplied as monotonic elapsed milliseconds by the caller.
pub fn lease(input: &Value) -> Result<Value, String> {
    let device = input["device"].as_str().ok_or("invalid_device")?;
    if !valid_device(device) || device.is_empty() || device == "local" {
        return Err("invalid_device".into());
    }
    if input["generation_changed"].as_bool().unwrap_or(false) {
        return Ok(json!({"action":"stop","reason":"generation_changed"}));
    }
    let holder = input["holder"].as_str().unwrap_or("");
    let epoch = input["epoch"].as_u64().unwrap_or(0);
    let elapsed = input["elapsed"].as_u64().unwrap_or(u64::MAX);
    if holder == device {
        if elapsed >= 120_000 || input["clock_backwards"].as_bool().unwrap_or(false) {
            return Ok(json!({"action":"stop","reason":"expired"}));
        }
        return Ok(if elapsed >= 60_000 {
            json!({"action":"renew","epoch":epoch})
        } else {
            json!({"action":"send","epoch":epoch})
        });
    }
    let observed = input["observed"].as_u64().unwrap_or(0);
    let fresh_generation = input["fresh_generation"].as_bool().unwrap_or(false);
    if holder.is_empty() && !fresh_generation || observed >= 600_000 {
        let greatest = input["greatest_epoch"].as_u64().unwrap_or(epoch).max(epoch);
        return Ok(json!({"action":"take","epoch":greatest + 1}));
    }
    Ok(json!({"action":"wait","reason":"observation"}))
}

/// A row that holds a v1 tracker event as shipped: the settings row `set:tracker-event:<id>`.
fn is_event_row(row: &Value) -> bool {
    row["kind"] == json!("set")
        && row["name"]
            .as_str()
            .is_some_and(|row_name| row_name.starts_with("tracker-event:"))
}

/// The event a `set:tracker-event:<id>` row holds (library v3 Appendix A), or `None` when a reader ignores it:
/// `schema` 1, the row named for its `id`, `values.event.at` equal to its `at`, and `before` and `after` naming
/// the same non-settings row.
fn stored_event(row: &Value) -> Option<Value> {
    let id = row["name"].as_str()?.strip_prefix("tracker-event:")?;
    let stored = &row["values"]["event"];
    let event: Value = serde_json::from_str(stored["value"]["string"].as_str()?).ok()?;
    let target = name(&event["after"]).ok()?;
    let valid = event["schema"] == json!(1)
        && event["id"].as_str() == Some(id)
        && stamp(&stored["at"]).ok()? == stamp(&event["at"]).ok()?
        && event["after"]["kind"] != json!("set")
        && name(&event["before"]).ok()? == target;
    valid.then_some(event)
}

/// What a log row claims for §8: a v1 event's `after` (stored as `set:tracker-event:<id>`, or already decoded),
/// otherwise the row itself. `None` for an event row a reader ignores.
fn claim(row: &Value) -> Option<Value> {
    if row["schema"] == json!(1) && row.get("after").is_some() {
        return Some(row["after"].clone());
    }
    if is_event_row(row) {
        return stored_event(row).map(|event| event["after"].clone());
    }
    Some(row.clone())
}

pub fn v2_reading(rows: &[Value], now: i64) -> Result<Value, String> {
    let mut folded: Map<String, Value> = Map::new();
    for candidate in rows.iter().filter_map(claim) {
        let row_name = name(&candidate).unwrap_or_else(|_| format!("unknown:{}", folded.len()));
        let merged = match folded.get(&row_name) {
            Some(old) => {
                merge(old, &candidate).map_err(|error| format!("v2_reading:{row_name}:{error}"))?
            }
            None => candidate,
        };
        folded.insert(row_name, merged);
    }
    Ok(json!({"rows":folded,"now":now}))
}

/// Insert a row, merging it (§3) with a row of the same name already there, so a register the fold derives joins
/// the one the log holds instead of replacing it.
fn put(output: &mut Map<String, Value>, row_name: String, row: Value) -> Result<(), String> {
    let row = match output.get(&row_name) {
        Some(old) => merge(old, &row).map_err(|error| format!("v3_form:{row_name}:{error}"))?,
        None => row,
    };
    output.insert(row_name, row);
    Ok(())
}

/// Conversion deliberately returns only v3 watch/receipt rows plus unchanged v2 rec/set and unknown rows.
pub fn v3_form(rows: &[Value], now: i64) -> Result<Value, String> {
    v3_form_with_context(rows, now, None)
}

/// Fold the v2 log and, when the performer supplies provider facts, seed the v3 connection and delivery rows.
/// Provider lookups and the rewrite base are inputs so this remains deterministic and free of I/O.
pub fn v3_form_with_context(
    rows: &[Value],
    now: i64,
    context: Option<&Value>,
) -> Result<Value, String> {
    let reading = v2_reading(rows, now).map_err(|error| format!("v3_form:reading:{error}"))?;
    let mut output: Map<String, Value> = Map::new();
    let mut claims: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    for candidate in rows.iter().filter_map(claim) {
        // Shipped Simkl data can contain episode-shaped rows for anime films. v2 clients ignore
        // them, so preserve them as unknown state rather than deriving a TV watch or losing them.
        if candidate["kind"] == json!("ep") && candidate["title"]["type"] == json!("tv") {
            claims
                .entry(name(&candidate).map_err(|error| format!("v3_form:claim:{error}"))?)
                .or_default()
                .push(candidate);
        }
    }
    let read_rows = reading["rows"].as_object().ok_or("invalid_reading")?;
    for row in read_rows.values() {
        match row["kind"].as_str() {
            Some("ep") if row["title"]["type"] == json!("tv") => {
                let logical_name =
                    name(row).map_err(|error| format!("v3_form:episode_name:{error}"))?;
                let season = safe_u64(&row["season"])?;
                let episode = safe_u64(&row["episode"])?;
                let block = episode / 32;
                let watch_name =
                    format!("wat:tv:{}:{season}:{block}", safe_u64(&row["title"]["id"])?);
                let row_claims = claims.get(&logical_name).ok_or("missing_claims")?;
                let merged = row_claims
                    .iter()
                    .skip(1)
                    .try_fold(row_claims[0].clone(), |old, next| merge(&old, next))
                    .map_err(|error| format!("v3_form:{logical_name}:{error}"))?;
                let progress_at = stamp(&merged["progress"]["at"])?;
                let value = merged["progress"]["value"]
                    .as_f64()
                    .ok_or("invalid_progress")?;
                let mut register =
                    json!({"imported":progress_at.0==0 && value>=0.95,"plays":{},"cleared":null});
                if progress_at.0 != 0 {
                    register["progress"] = merged["progress"].clone();
                }
                let mut completed: std::collections::BTreeMap<u64, i64> = Default::default();
                let mut cleared: Option<(u64, Stamp)> = None;
                for claim in row_claims {
                    let progress = &claim["progress"];
                    let claim_at = stamp(&progress["at"])?;
                    let claim_viewing = safe_u64(&progress["viewing"])?;
                    let claim_value = progress["value"].as_f64().ok_or("invalid_progress")?;
                    if claim_at.0 > 0 && claim_value >= 0.95 {
                        completed
                            .entry(claim_viewing)
                            .and_modify(|at| *at = (*at).min(claim_at.0))
                            .or_insert(claim_at.0);
                    }
                    if claim_value == 0.0 && claim_viewing >= 1 {
                        let next = (claim_viewing - 1, claim_at);
                        if cleared
                            .as_ref()
                            .is_none_or(|old| next.0 > old.0 || (next.0 == old.0 && next.1 > old.1))
                        {
                            cleared = Some(next);
                        }
                    }
                }
                register["plays"] = Value::Object(selected_plays(
                    completed
                        .into_iter()
                        .map(|(viewing, at)| (viewing as i64, at as u64))
                        .collect(),
                ));
                register["cleared"] = cleared
                    .map(|(viewing, at)| json!([viewing, at]))
                    .unwrap_or(Value::Null);
                let wat = json!({"kind":"wat","schema":3,"title":row["title"],"season":season,"block":block,"seasonReset":null,"entries":{episode.to_string():register}});
                put(&mut output, watch_name, wat)?;
            }
            Some("rec") => {
                output.insert(
                    name(row).map_err(|error| format!("v3_form:title_name:{error}"))?,
                    row.clone(),
                );
                if row["title"]["type"] == json!("movie") {
                    let id = safe_u64(&row["title"]["id"])?;
                    let status = row["status"]["value"].as_str().ok_or("invalid_status")?;
                    let status_at = stamp(&row["status"]["at"])?;
                    let viewing = safe_u64(&row["resume"]["viewing"])?;
                    let mut register = json!({"imported":false,"plays":{},"cleared":null});
                    if status == "watched" && status_at.0 > 0 {
                        register["plays"][viewing.to_string()] = json!(status_at.0);
                    }
                    if status == "none"
                        && viewing >= 1
                        && row["resume"]["at"] == row["status"]["at"]
                    {
                        register["cleared"] = json!([viewing - 1, status_at]);
                    }
                    if register["plays"]
                        .as_object()
                        .is_some_and(|plays| !plays.is_empty())
                        || !register["cleared"].is_null()
                    {
                        put(
                            &mut output,
                            format!("wat:movie:{id}:0:0"),
                            json!({"kind":"wat","schema":3,"title":row["title"],"season":0,"block":0,"seasonReset":null,"entries":{"0":register}}),
                        )?;
                    }
                }
            }
            // §9: no `set:tracker-event:*` row arrives here; `claim` gave the reading its `after` instead.
            Some("set" | "wat" | "snt") => {
                put(
                    &mut output,
                    name(row).map_err(|error| format!("v3_form:preserved_name:{error}"))?,
                    row.clone(),
                )?;
            }
            Some("ep") => {
                output.insert(format!("unknown:{}", output.len()), row.clone());
            }
            _ => {
                output.insert(format!("unknown:{}", output.len()), row.clone());
            }
        }
    }
    if let Some(context) = context {
        seed_switch_rows(&mut output, context, now)
            .map_err(|error| format!("v3_form:seed:{error}"))?;
    }
    Ok(Value::Array(output.into_values().collect()))
}

/// §9's compaction of a library already in v3: its rows, with every stray series `ep` row and v1 tracker event
/// folded through §8 and dropped, and rows of one name merged (§3). Only the stray rows are folded: a `rec` no
/// stray event touches is kept as it is and yields no film play, since its plays already live in its `wat` row.
/// Seeds nothing; the caller commits it with an unchanged wire minimum.
pub fn v3_compact(rows: &[Value], now: i64) -> Result<Value, String> {
    let stray = |row: &Value| {
        is_event_row(row)
            || row["schema"] == json!(1) && row.get("after").is_some()
            || row["kind"] == json!("ep") && row["title"]["type"] == json!("tv")
    };
    let touched = rows
        .iter()
        .filter(|row| stray(row))
        .filter_map(claim)
        .filter(|after| after["kind"] == json!("rec"))
        .filter_map(|after| name(&after).ok())
        .collect::<std::collections::BTreeSet<_>>();
    let to_fold = rows
        .iter()
        .filter(|row| {
            stray(row)
                || row["kind"] == json!("rec") && name(row).is_ok_and(|n| touched.contains(&n))
        })
        .cloned()
        .collect::<Vec<_>>();
    let folded = v3_form(&to_fold, now).map_err(|error| format!("v3_compact:{error}"))?;
    let mut output: Map<String, Value> = Map::new();
    let mut unknown = Vec::new();
    for row in rows
        .iter()
        .filter(|row| !stray(row))
        .chain(folded.as_array().into_iter().flatten())
    {
        match name(row) {
            Ok(row_name) if matches!(row["kind"].as_str(), Some("rec" | "set" | "wat" | "snt")) => {
                put(&mut output, row_name, row.clone())
                    .map_err(|error| format!("v3_compact:{error}"))?
            }
            _ => unknown.push(row.clone()),
        }
    }
    Ok(Value::Array(output.into_values().chain(unknown).collect()))
}

fn switch_setting(value: Value, at: &Stamp) -> Value {
    json!({"value": value, "at": at})
}

fn upsert_settings(output: &mut Map<String, Value>, group: &str, values: Map<String, Value>) {
    let name = format!("set:{group}");
    let row = output
        .entry(name)
        .or_insert_with(|| json!({"kind":"set","schema":2,"name":group,"values":{}}));
    let settings = row["values"].as_object_mut().expect("settings row values");
    settings.extend(values);
}

fn seed_switch_rows(
    output: &mut Map<String, Value>,
    context: &Value,
    now: i64,
) -> Result<(), String> {
    let context = object(context)?;
    let accounts = context
        .get("accounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if accounts.is_empty() {
        return Ok(());
    }
    let at = context
        .get("stamp")
        .map(validate_writer_stamp)
        .transpose()?
        .unwrap_or(Stamp(
            now,
            0,
            context
                .get("performer")
                .and_then(Value::as_str)
                .unwrap_or("local")
                .into(),
        ));
    let base = context.get("base").and_then(Value::as_u64).unwrap_or(0);
    let seed_bound = context
        .get("seed_bound")
        .and_then(Value::as_i64)
        .unwrap_or(now);

    let mut tracker_values = Map::new();
    let mut clear_keys = Map::new();
    for account in &accounts {
        let provider = account["provider"].as_str().ok_or("invalid_provider")?;
        let id = account["account"].as_str().ok_or("invalid_account")?;
        if provider.is_empty() || provider.contains(':') || id.is_empty() || id.contains(':') {
            return Err("invalid_account".into());
        }
        let connected_at = account
            .get("connected_at")
            .map(validate_writer_stamp)
            .transpose()?
            .unwrap_or_else(|| at.clone());
        let connection = if let Some(connection) = account.get("connection").and_then(Value::as_str)
        {
            connection.to_owned()
        } else {
            let credential = account["credential"].as_str().ok_or("missing_credential")?;
            serde_json::to_string(&json!({"access_token":credential,"connectedAt":connected_at}))
                .map_err(|_| "invalid_credential")?
        };
        tracker_values.insert(
            format!("{provider}:{id}"),
            switch_setting(json!({"string":connection}), &connected_at),
        );
        if provider == "simkl" {
            clear_keys.insert("simkl".into(), switch_setting(Value::Null, &at));
        }
        output.entry(format!("set:deliver:{provider}:{id}")).or_insert_with(|| {
            json!({"kind":"set","schema":2,"name":format!("deliver:{provider}:{id}"),"values":{}})
        });
    }
    upsert_settings(output, "trackers", tracker_values);
    if !clear_keys.is_empty() {
        upsert_settings(output, "keys", clear_keys);
    }

    let seeded_through = base.saturating_add(output.len() as u64);
    for account in accounts {
        let provider = account["provider"].as_str().ok_or("invalid_provider")?;
        let id = account["account"].as_str().ok_or("invalid_account")?;
        let connected_at = account
            .get("connected_at")
            .map(validate_writer_stamp)
            .transpose()?
            .unwrap_or_else(|| at.clone());
        let row = output
            .get_mut(&format!("set:deliver:{provider}:{id}"))
            .ok_or("missing_delivery_row")?;
        let values = row["values"]
            .as_object_mut()
            .ok_or("invalid_delivery_row")?;
        values.insert(
            "since".into(),
            switch_setting(
                json!({"string":serde_json::to_string(&connected_at).unwrap()}),
                &at,
            ),
        );
        values.insert(
            "lease".into(),
            switch_setting(json!({"strings":["","1"]}), &at),
        );
        values.insert(
            "seedBound".into(),
            switch_setting(json!({"int":seed_bound}), &at),
        );
        values.insert(
            "seededThrough".into(),
            switch_setting(json!({"int":seeded_through}), &at),
        );
    }
    Ok(())
}

pub fn write_back(held: &[Value], log: &[Value], now: i64) -> Result<Value, String> {
    let mut known = Map::new();
    for row in log {
        if let Ok(row_name) = name(row) {
            known.insert(row_name, row.clone());
        }
    }
    let converted = v3_form(held, now)?;
    let mut writes = Vec::new();
    for row in converted.as_array().unwrap() {
        if matches!(row["kind"].as_str(), Some("ep") | None) || row["name"] == json!("lease") {
            continue;
        }
        let row_name = name(row)?;
        let candidate = match known.get(&row_name) {
            Some(current) => merge(current, row)?,
            None => row.clone(),
        };
        if known.get(&row_name) != Some(&candidate) {
            writes.push(candidate);
        }
    }
    Ok(Value::Array(writes))
}

pub fn switch_ready(input: &Value) -> Result<Value, String> {
    let now = input["now"].as_i64().ok_or("invalid_time")?;
    let performer = input["performer"].as_str().ok_or("invalid_device")?;
    let devices = input["devices"].as_array().ok_or("invalid_devices")?;
    let connected = input["connected"].as_array().cloned().unwrap_or_default();
    let mut blockers = Vec::new();
    let mut active = 0;
    for device in devices {
        if device["removed"].as_bool().unwrap_or(false) {
            continue;
        }
        let seen = device["seen"].as_i64();
        let tv = device["kind"] == json!("tv");
        let relevant = tv || seen.is_none() || seen.is_some_and(|seen| now - seen <= 180 * DAY);
        if !relevant {
            continue;
        }
        active += 1;
        if device["format"].as_u64().unwrap_or(0) < 3 {
            blockers.push(json!({"device":device["id"],"reason":"format"}));
        }
        if device["id"] != json!(performer)
            && !device["delivers"]
                .as_array()
                .unwrap_or(&Vec::new())
                .is_empty()
            && !device["handoff"].as_bool().unwrap_or(false)
        {
            blockers.push(json!({"device":device["id"],"reason":"handoff"}));
        }
    }
    if input["waiting"].as_bool().unwrap_or(false) {
        blockers.push(json!({"reason":"waiting_commands"}));
    }
    if input["facade"].as_bool() == Some(false) && !connected.is_empty() {
        blockers.push(json!({"reason":"facade"}));
    }
    let offered = blockers.iter().all(|b| b["reason"] != json!("format")) && active > 0;
    Ok(json!({"offered":offered,"performable":blockers.is_empty() && active>0,"blockers":blockers}))
}

pub fn receipt_row_name(provider: &str, account: &str, target: &str) -> Result<String, String> {
    if provider.is_empty() || provider.contains(':') || account.is_empty() || account.contains(':')
    {
        return Err("invalid_receipt_identity".into());
    }
    if target.starts_with("wat:") {
        return Ok(format!("snt:{provider}:{account}:{target}"));
    }
    if target.starts_with("rec:") {
        let digest = sha256(target.as_bytes());
        return Ok(format!(
            "snt:{provider}:{account}:t{:02x}{:01x}",
            digest[0],
            digest[1] >> 4
        ));
    }
    Err("invalid_receipt_target".into())
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut bytes = input.to_vec();
    let bit_len = (bytes.len() as u64) * 8;
    bytes.push(0x80);
    while bytes.len() % 64 != 56 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for block in bytes.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            words[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = words[i - 15].rotate_right(7)
                ^ words[i - 15].rotate_right(18)
                ^ (words[i - 15] >> 3);
            let s1 = words[i - 2].rotate_right(17)
                ^ words[i - 2].rotate_right(19)
                ^ (words[i - 2] >> 10);
            words[i] = words[i - 16]
                .wrapping_add(s0)
                .wrapping_add(words[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let first = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[i])
                .wrapping_add(words[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut digest = [0u8; 32];
    for (out, value) in digest.chunks_exact_mut(4).zip(state) {
        out.copy_from_slice(&value.to_be_bytes());
    }
    digest
}

pub fn watch_row_name(media: &str, id: u64, season: u64, episode: u64) -> Result<String, String> {
    if !matches!(media, "tv" | "movie")
        || id == 0
        || (media == "movie" && (season != 0 || episode != 0))
    {
        return Err("invalid_watch_coordinate".into());
    }
    Ok(format!("wat:{media}:{id}:{season}:{}", episode / 32))
}
