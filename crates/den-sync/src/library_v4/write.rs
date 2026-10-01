//! §8 Writing: every write is a set-to-value on the documents it touches, decided on the stored state as v3 §7
//! decides it on rows. Returns only the documents that change.

use super::doc::{play_key, safe_u64, Identity, Kind};
use super::jcs;
use super::merge::{later, select};
use super::state;
use crate::library_v3;
use crate::wire::{stamp, Stamp};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
const IMPORT_OFFSET: i64 = 1 << 53;

fn effective_t(at: &Stamp, now: i64) -> i64 {
    if at.0 > now.saturating_add(DAY) {
        0
    } else {
        at.0
    }
}

fn floor(resets: &[Stamp], now: i64) -> i64 {
    resets
        .iter()
        .map(|r| effective_t(r, now))
        .max()
        .unwrap_or(0)
}

fn writer_stamp(value: &Value) -> Result<Stamp, String> {
    let at = stamp(value)?;
    let device = &at.2;
    let valid = device.is_empty()
        || device == "local"
        || (device.len() == 16
            && device
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    if !valid {
        return Err("invalid_device".into());
    }
    if at.0 == 0 {
        return Err("timeless_write".into());
    }
    Ok(at)
}

/// Whether a write stamped `at` is not later than a stamp the state it writes already holds: a kept write replayed
/// after a newer change, or a resend. Applying it would overwrite that change, so it writes nothing (§2.2, §2.7). A
/// stored stamp more than a day ahead counts as none, as everywhere a stamp is read (§5).
fn stale(stored: &[Option<Stamp>], at: &Stamp, now: i64) -> bool {
    stored
        .iter()
        .flatten()
        .any(|s| effective_t(s, now) > 0 && s >= at)
}

fn at_of(value: Option<&Value>) -> Option<Stamp> {
    stamp(&value?["at"]).ok()
}

/// The stamps an episode register's own writes leave: `progress.at` and the `cleared` stamp.
fn register_stamps(register: &Map<String, Value>) -> [Option<Stamp>; 2] {
    [
        at_of(register.get("progress")),
        cleared_of(register).map(|(_, at)| at),
    ]
}

/// The stamps a film's own writes leave: `resume.at`, `status.at` and its `watch` register's `cleared` stamp.
fn film_stamps(title: &Map<String, Value>) -> [Option<Stamp>; 3] {
    [
        at_of(title.get("resume")),
        at_of(title.get("status")),
        title
            .get("watch")
            .and_then(Value::as_object)
            .and_then(cleared_of)
            .map(|(_, at)| at),
    ]
}

fn default_register() -> Map<String, Value> {
    Map::from_iter([
        ("imported".into(), json!(false)),
        ("plays".into(), json!({})),
        ("cleared".into(), Value::Null),
    ])
}

fn plays_of(register: &Map<String, Value>) -> Vec<(i64, u64)> {
    register
        .get("plays")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((play_key(k)?, safe_u64(v)?)))
        .collect()
}

fn cleared_of(register: &Map<String, Value>) -> Option<(u64, Stamp)> {
    let c = register.get("cleared").filter(|v| !v.is_null())?;
    Some((safe_u64(&c[0])?, stamp(&c[1]).ok()?))
}

fn add_play(register: &mut Map<String, Value>, viewing: u64, at: u64) {
    let mut all = plays_of(register);
    if all.iter().any(|(key, _)| *key == viewing as i64) {
        return;
    }
    all.push((viewing as i64, at));
    register.insert("plays".into(), Value::Object(select(&mut all)));
}

/// Whether the play of `viewing` exists and is hidden by a covering reset or by `cleared` (v3 §7).
fn hidden_play(register: &Map<String, Value>, viewing: u64, reset: i64) -> bool {
    let cleared = cleared_of(register).map(|(v, _)| v as i64).unwrap_or(-1);
    plays_of(register)
        .iter()
        .any(|(key, at)| *key == viewing as i64 && (*at as i64 <= reset || *key <= cleared))
}

fn progress_value(write: &Value, at: &Stamp, viewing: u64) -> Result<Value, String> {
    let value = write["value"].as_f64().ok_or("invalid_progress")?;
    if !(0.0..=1.0).contains(&value) {
        return Err("invalid_progress".into());
    }
    let mut progress = json!({"value": write["value"], "at": at, "viewing": viewing});
    if let Some(seconds) = write.get("seconds").filter(|s| !s.is_null()) {
        if !seconds.as_f64().is_some_and(|s| (0.0..=1e7).contains(&s)) {
            return Err("invalid_seconds".into());
        }
        progress["seconds"] = seconds.clone();
    }
    Ok(progress)
}

/// v3 §7 *Playback* on an episode register.
fn episode_progress(
    register: &mut Map<String, Value>,
    write: &Value,
    at: &Stamp,
    resets: &[Stamp],
    now: i64,
) -> Result<(), String> {
    let reset = floor(resets, now);
    let stored = register.get("progress").cloned();
    let viewing = stored
        .as_ref()
        .and_then(|p| safe_u64(&p["viewing"]))
        .unwrap_or(0);
    let state = library_v3::episode_state(&Value::Object(register.clone()), resets, now)?;
    let finished = stored
        .as_ref()
        .is_some_and(|p| p["value"].as_f64().unwrap_or(0.0) >= 0.95);
    let hidden = stored
        .as_ref()
        .is_some_and(|p| stamp(&p["at"]).is_ok_and(|s| effective_t(&s, now) <= reset));
    let imported = stored.is_none()
        && register.get("imported").and_then(Value::as_bool) == Some(true)
        && state["watched"] == json!(true);
    let next = if finished || hidden || imported || hidden_play(register, viewing, reset) {
        viewing.checked_add(1).ok_or("viewing_overflow")?
    } else {
        viewing
    };
    let value = write["value"].as_f64().ok_or("invalid_progress")?;
    let new_viewing = stored.is_none() || next != viewing;
    if new_viewing && value <= 0.0 {
        return Ok(());
    }
    register.insert("progress".into(), progress_value(write, at, next)?);
    if value >= 0.95 {
        add_play(register, next, at.0 as u64);
    }
    Ok(())
}

fn episode_mark_watched(
    register: &mut Map<String, Value>,
    write: &Value,
    at: &Stamp,
    resets: &[Stamp],
    now: i64,
) -> Result<(), String> {
    let state = library_v3::episode_state(&Value::Object(register.clone()), resets, now)?;
    if state["watched"] == json!(true) {
        return Ok(());
    }
    let reset = floor(resets, now);
    let viewing = state["viewing"].as_u64().unwrap_or(0);
    let finished_hidden = register.get("progress").is_some_and(|p| {
        p["value"].as_f64().unwrap_or(0.0) >= 0.95
            && stamp(&p["at"]).is_ok_and(|s| effective_t(&s, now) <= reset)
    });
    let next = if finished_hidden || hidden_play(register, viewing, reset) {
        viewing + 1
    } else {
        viewing
    };
    register.insert(
        "progress".into(),
        json!({"value": 1, "at": at, "viewing": next}),
    );
    let watched_at = write
        .get("watched_at")
        .and_then(safe_u64)
        .unwrap_or(at.0 as u64);
    add_play(register, next, watched_at);
    Ok(())
}

fn episode_unwatch(
    register: &mut Map<String, Value>,
    at: &Stamp,
    resets: &[Stamp],
    now: i64,
) -> Result<(), String> {
    let state = library_v3::episode_state(&Value::Object(register.clone()), resets, now)?;
    // v3 §7 un-watches a watched or an in-progress episode (clearing its resume point); one with neither has
    // nothing to clear. A replay is caught by the stamp check before this.
    if state["watched"] != json!(true) && state["resume"].is_null() {
        return Ok(());
    }
    let viewing = state["viewing"].as_u64().unwrap_or(0);
    register.insert("cleared".into(), json!([viewing, at]));
    register.insert(
        "progress".into(),
        json!({"value": 0, "at": at, "viewing": viewing + 1}),
    );
    Ok(())
}

/// The plays a tracker or file import may write into `register` (v3 §7 *Import*, less v4's dropped windows).
/// `receipt` is that account's entry for the register's target, when it has one.
fn import_plays(register: &Map<String, Value>, item: &Value, receipt: Option<&Value>) -> Vec<u64> {
    let second = |ms: u64| ms / 1000 * 1000;
    let mut den: Vec<u64> = plays_of(register)
        .iter()
        .filter(|(key, _)| *key >= 0)
        .map(|(_, at)| *at)
        .collect();
    den.sort_unstable();
    let mut aside: Vec<u64> = den.iter().map(|w| second(*w)).collect();
    if let Some(entry) = receipt.and_then(Value::as_array) {
        if entry
            .first()
            .and_then(Value::as_str)
            .is_some_and(|c| matches!(c, "w" | "u" | "n"))
        {
            if let Some(w) = entry.get(2).and_then(safe_u64) {
                aside.push(second(w));
            }
            for element in entry.get(5).and_then(Value::as_array).into_iter().flatten() {
                if element[0].as_i64() == Some(-1) {
                    if let Some(i) = safe_u64(&element[1]) {
                        aside.push(i);
                    }
                }
            }
        }
    }
    let mut out: Vec<u64> = item["plays"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(safe_u64)
        .map(second)
        .filter(|p| !aside.contains(p))
        .collect();
    // Day-only plays: the start of the UTC day, matched first exactly against a Den play at local noon, then by
    // range with dates ascending, each Den play excusing at most one day.
    let mut days: Vec<(u64, u64)> = item["days"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| Some((safe_u64(&d["day"])?, safe_u64(&d["noon"]).unwrap_or(0))))
        .filter(|(day, _)| !aside.contains(day))
        .collect();
    days.sort_unstable();
    let mut taken = vec![false; den.len()];
    let mut excused = vec![false; days.len()];
    for (index, (_, noon)) in days.iter().enumerate() {
        if let Some(i) = (0..den.len()).find(|i| !taken[*i] && den[*i] == *noon) {
            taken[i] = true;
            excused[index] = true;
        }
    }
    for (index, (day, _)) in days.iter().enumerate() {
        if excused[index] {
            continue;
        }
        let (low, high) = (*day as i64 - 2 * HOUR, *day as i64 + DAY);
        if let Some(i) = (0..den.len()).find(|i| {
            let w = den[*i] as i64;
            !taken[*i] && w % 900_000 == 0 && (low..=high).contains(&w)
        }) {
            taken[i] = true;
            excused[index] = true;
        }
    }
    out.extend(
        days.iter()
            .zip(excused)
            .filter(|(_, excused)| !excused)
            .map(|((day, _), _)| *day),
    );
    out.retain(|p| *p >= 1000);
    out.sort_unstable();
    out.dedup();
    out
}

fn episode_import(
    register: &mut Map<String, Value>,
    item: &Value,
    receipt: Option<&Value>,
    resets: &[Stamp],
    now: i64,
) -> Result<(), String> {
    let plays = import_plays(register, item, receipt);
    let reset_t = resets
        .iter()
        .map(|r| effective_t(r, now))
        .filter(|t| *t > 0)
        .max();
    let unhidden_progress = register.get("progress").is_some_and(|p| {
        stamp(&p["at"]).is_ok_and(|s| effective_t(&s, now) > reset_t.unwrap_or(0))
    });
    let blocked_by_reset = match (reset_t, plays.last()) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(reset), Some(newest)) => reset >= *newest as i64,
    };
    let mut all = plays_of(register);
    all.extend(plays.iter().map(|at| (*at as i64 - IMPORT_OFFSET, *at)));
    register.insert("plays".into(), Value::Object(select(&mut all)));
    if !unhidden_progress && !blocked_by_reset {
        register.insert("imported".into(), json!(true));
    }
    Ok(())
}

/// v3 §7 *Title imports*: written only where the field is import-owned and earlier than the import's stamp,
/// and never over the same value from an import.
fn title_import(title: &mut Map<String, Value>, field: &str, value: Value, at: Stamp) {
    let current = title.get(field);
    let current_at = current
        .and_then(|c| stamp(&c["at"]).ok())
        .unwrap_or_default();
    if current_at.0 != 0 || current_at >= at {
        return;
    }
    if current.is_some_and(|c| jcs::same(&c["value"], &value)) {
        return;
    }
    title.insert(field.into(), json!({"value": value, "at": at}));
}

fn deleted(title: &Map<String, Value>) -> bool {
    title.get("deleted").and_then(|d| d["value"].as_bool()) == Some(true)
}

fn film_progress(
    title: &mut Map<String, Value>,
    write: &Value,
    at: &Stamp,
    now: i64,
) -> Result<(), String> {
    // The status is this op's to decide, below: a client that left a film `watched` while playing it again would
    // start another viewing on every progress write.
    if write.get("status").is_some() {
        return Err("invalid_write:status".into());
    }
    if stale(&film_stamps(title), at, now) {
        return Ok(());
    }
    let resume = title.get("resume").cloned();
    let viewing = resume
        .as_ref()
        .and_then(|r| safe_u64(&r["viewing"]))
        .unwrap_or(0);
    let value = write["value"].as_f64().ok_or("invalid_progress")?;
    let resumed = resume
        .as_ref()
        .and_then(|r| r["value"].as_f64())
        .unwrap_or(0.0);
    let watched = title.get("status").and_then(|s| s["value"].as_str()) == Some("watched");
    let watch = title
        .get("watch")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let next = if resumed >= 0.95 || (watched && resumed < 0.95) || hidden_play(&watch, viewing, 0)
    {
        viewing + 1
    } else {
        viewing
    };
    if (resume.is_none() || next != viewing) && value <= 0.0 {
        return Ok(());
    }
    title.insert("resume".into(), progress_value(write, at, next)?);
    // v2's status machine, as the shipped clients run it: finishing makes the film `watched`, any other position
    // above 0 makes it `inProgress`, and 0 leaves it. So a watched film played again is in progress in its new
    // viewing, and the next tick stays there. Written when the value changes or a viewing starts, so a rewatch
    // finished in one write carries its own stamp.
    let status = if value >= 0.95 {
        Some("watched")
    } else if value > 0.0 {
        Some("inProgress")
    } else {
        None
    };
    let stored = title.get("status").and_then(|s| s["value"].as_str());
    if let Some(status) = status.filter(|s| stored != Some(*s) || next != viewing) {
        title.insert("status".into(), json!({"value": status, "at": at}));
    }
    if value >= 0.95 {
        let mut watch = watch;
        if watch.is_empty() {
            watch = default_register();
        }
        add_play(&mut watch, next, at.0 as u64);
        title.insert("watch".into(), Value::Object(watch));
    }
    Ok(())
}

fn film_mark_watched(title: &mut Map<String, Value>, write: &Value, at: &Stamp, now: i64) {
    if title.get("status").and_then(|s| s["value"].as_str()) == Some("watched")
        || stale(&film_stamps(title), at, now)
    {
        return;
    }
    let resume = title.get("resume").cloned().unwrap_or(Value::Null);
    let viewing = safe_u64(&resume["viewing"]).unwrap_or(0);
    let mut watch = title
        .get("watch")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(default_register);
    let next = if hidden_play(&watch, viewing, 0) {
        viewing + 1
    } else {
        viewing
    };
    title.insert("status".into(), json!({"value": "watched", "at": at}));
    title.insert(
        "resume".into(),
        json!({"value": 1, "at": at, "viewing": next}),
    );
    let watched_at = write
        .get("watched_at")
        .and_then(safe_u64)
        .unwrap_or(at.0 as u64);
    add_play(&mut watch, next, watched_at);
    title.insert("watch".into(), Value::Object(watch));
}

fn film_unwatch(title: &mut Map<String, Value>, at: &Stamp, now: i64) {
    if title.get("status").and_then(|s| s["value"].as_str()) != Some("watched")
        || stale(&film_stamps(title), at, now)
    {
        return;
    }
    let viewing = title
        .get("resume")
        .and_then(|r| safe_u64(&r["viewing"]))
        .unwrap_or(0);
    let mut watch = title
        .get("watch")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(default_register);
    // `cleared` before the bump, in the same document write (§8).
    watch.insert("cleared".into(), json!([viewing, at]));
    title.insert("watch".into(), Value::Object(watch));
    title.insert(
        "resume".into(),
        json!({"value": 0, "at": at, "viewing": viewing + 1}),
    );
    title.insert("status".into(), json!({"value": "none", "at": at}));
}

/// The receipt entry of `account` for `key` in the delivery documents given, for an import's set-aside.
fn receipt<'a>(receipts: &'a [Value], name: &str, key: &str) -> Option<&'a Value> {
    receipts
        .iter()
        .find(|d| super::name(d).is_ok_and(|n| n == name))
        .and_then(|d| d["entries"].get(key))
}

/// `apply_write` (§8, §14). `title` and `seasons` are the stored documents the write touches (absent ones are
/// created by their first write); `receipts` the importing account's delivery documents. Returns the documents
/// to write, each only when it changed.
pub fn apply_write(
    write: &Value,
    target: &Value,
    title: Option<&Value>,
    seasons: &[Value],
    receipts: &[Value],
    now: i64,
) -> Result<Value, String> {
    let kind = write["kind"].as_str().ok_or("invalid_write")?;
    let base = Identity {
        kind: Kind::Title,
        media: target["type"]
            .as_str()
            .filter(|t| matches!(*t, "movie" | "tv"))
            .ok_or("invalid_target")?
            .into(),
        id: safe_u64(&target["id"])
            .filter(|id| *id > 0)
            .ok_or("invalid_target")?,
        season: None,
        provider: None,
        account: None,
    };
    let stored_title = match title {
        Some(doc) => {
            let (identity, doc) = super::checked(doc)?;
            if identity != base {
                return Err("identity_mismatch".into());
            }
            Some(doc)
        }
        None => None,
    };
    let mut stored_seasons: BTreeMap<u64, Map<String, Value>> = BTreeMap::new();
    for doc in seasons {
        let (identity, doc) = super::checked(doc)?;
        if identity.kind != Kind::Season || identity.media != base.media || identity.id != base.id {
            return Err("identity_mismatch".into());
        }
        stored_seasons.insert(identity.season.unwrap_or(0), doc);
    }
    let mut new_title = stored_title.clone().unwrap_or_else(|| base.members());
    let mut new_seasons = stored_seasons.clone();
    let season_doc = |seasons: &mut BTreeMap<u64, Map<String, Value>>, season: u64| {
        seasons
            .entry(season)
            .or_insert_with(|| {
                let mut doc = Identity {
                    kind: Kind::Season,
                    season: Some(season),
                    ..base.clone()
                }
                .members();
                doc.insert("seasonReset".into(), Value::Null);
                doc.insert("episodes".into(), json!({}));
                doc
            })
            .clone()
    };
    let coordinates = |write: &Value| -> Result<Vec<(u64, u64)>, String> {
        let list = write["episodes"].as_array().ok_or("invalid_episodes")?;
        list.iter()
            .map(|c| {
                safe_u64(&c[0])
                    .zip(safe_u64(&c[1]).filter(|e| *e <= 99_999))
                    .ok_or_else(|| "invalid_episodes".to_string())
            })
            .collect()
    };
    let episode_kinds = matches!(
        kind,
        "progress" | "mark_watched" | "unwatch" | "import_episodes"
    );
    let film = base.is_film();
    if episode_kinds && !film && !(kind == "progress" && write.get("episode").is_none()) {
        let items: Vec<(u64, u64, Value)> = if kind == "import_episodes" {
            write["items"]
                .as_array()
                .ok_or("invalid_items")?
                .iter()
                .map(|item| {
                    safe_u64(&item["season"])
                        .zip(safe_u64(&item["episode"]).filter(|e| *e <= 99_999))
                        .map(|(s, e)| (s, e, item.clone()))
                        .ok_or_else(|| "invalid_items".to_string())
                })
                .collect::<Result<_, _>>()?
        } else if kind == "progress" {
            let c = &write["episode"];
            vec![(
                safe_u64(&c[0]).ok_or("invalid_episode")?,
                safe_u64(&c[1])
                    .filter(|e| *e <= 99_999)
                    .ok_or("invalid_episode")?,
                Value::Null,
            )]
        } else {
            coordinates(write)?
                .into_iter()
                .map(|(s, e)| (s, e, Value::Null))
                .collect()
        };
        // v3 §7: imports skip a title that is `deleted`, episodes of a deleted series included.
        if kind == "import_episodes" && deleted(&new_title) {
            return Ok(json!({"documents": []}));
        }
        let at = if kind == "import_episodes" {
            Stamp::default()
        } else {
            writer_stamp(&write["at"])?
        };
        let dlv = write["provider"]
            .as_str()
            .zip(write["account"].as_str())
            .map(|(p, a)| format!("dlv:{p}:{a}:tv:{}", base.id));
        for (season, episode, item) in items {
            let mut doc = season_doc(&mut new_seasons, season);
            if !doc.get("episodes").is_some_and(Value::is_object) {
                doc.insert("episodes".into(), json!({}));
            }
            let resets = state::resets(Some(&new_title), Some(&doc));
            let key = episode.to_string();
            let stored = doc["episodes"]
                .get(&key)
                .and_then(Value::as_object)
                .cloned();
            let mut register = stored.clone().unwrap_or_else(default_register);
            if kind != "import_episodes" && stale(&register_stamps(&register), &at, now) {
                continue;
            }
            match kind {
                "progress" => episode_progress(&mut register, write, &at, &resets, now)?,
                "mark_watched" => episode_mark_watched(&mut register, write, &at, &resets, now)?,
                "unwatch" => episode_unwatch(&mut register, &at, &resets, now)?,
                _ => {
                    let name = dlv.as_ref().map(|d| format!("{d}:{season}"));
                    let entry = name.and_then(|n| receipt(receipts, &n, &key));
                    episode_import(&mut register, &item, entry, &resets, now)?
                }
            }
            let unchanged = match &stored {
                Some(stored) => stored == &register,
                None => register == default_register(),
            };
            if !unchanged {
                doc["episodes"][&key] = Value::Object(register);
            }
            new_seasons.insert(season, doc);
        }
    } else {
        match kind {
            "season_reset" => {
                let at = writer_stamp(&write["at"])?;
                let season = safe_u64(&write["season"]).ok_or("invalid_season")?;
                if film {
                    return Err("invalid_target".into());
                }
                let mut doc = season_doc(&mut new_seasons, season);
                if doc
                    .get("seasonReset")
                    .is_some_and(|old| stamp(old).is_ok_and(|old| old >= at))
                {
                    return Ok(json!({"documents": []}));
                }
                doc.insert("seasonReset".into(), json!(at));
                new_seasons.insert(season, doc);
            }
            "series_reset" => {
                let at = writer_stamp(&write["at"])?;
                if film {
                    return Err("invalid_target".into());
                }
                if !new_title
                    .get("episodesReset")
                    .is_some_and(|old| stamp(old).is_ok_and(|old| old >= at))
                {
                    new_title.insert("episodesReset".into(), json!(at));
                }
            }
            "title" => {
                let at = writer_stamp(&write["at"])?;
                let fields = write["fields"].as_object().cloned().unwrap_or_default();
                for (field, value) in fields {
                    let ok = match field.as_str() {
                        "status" => value.is_string(),
                        "reaction" => value.is_string() || value.is_null(),
                        "deleted" | "dismissed" => value.is_boolean(),
                        _ => false,
                    };
                    if !ok {
                        return Err(format!("invalid_field:{field}"));
                    }
                    // Each field by its own merge rule, the later stamp: a kept write replayed, or a resend, never
                    // overwrites a newer change (§2.2, §2.7).
                    let written = json!({"value": value, "at": at});
                    let field_value = match new_title.get(&field) {
                        Some(stored) => later(stored, &written),
                        None => written,
                    };
                    new_title.insert(field, field_value);
                }
                if let Some(added) = write.get("added_at").and_then(Value::as_i64) {
                    let old = new_title.get("addedAt").and_then(Value::as_i64);
                    new_title.insert("addedAt".into(), json!(old.map_or(added, |o| o.min(added))));
                }
                if let Some(watched) = write.get("watched_at").and_then(Value::as_i64) {
                    let old = new_title.get("watchedAt").and_then(Value::as_i64);
                    new_title.insert(
                        "watchedAt".into(),
                        json!(old.map_or(watched, |o| o.min(watched))),
                    );
                }
            }
            "progress" if film => {
                let at = writer_stamp(&write["at"])?;
                film_progress(&mut new_title, write, &at, now)?;
            }
            "mark_watched" if film => {
                let at = writer_stamp(&write["at"])?;
                film_mark_watched(&mut new_title, write, &at, now);
            }
            "unwatch" if film => {
                let at = writer_stamp(&write["at"])?;
                film_unwatch(&mut new_title, &at, now);
            }
            "import_film" if film && !deleted(&new_title) => {
                let name = write["provider"]
                    .as_str()
                    .zip(write["account"].as_str())
                    .map(|(p, a)| format!("dlv:{p}:{a}:movie:{}", base.id));
                let entry = name.and_then(|n| receipt(receipts, &n, "watch"));
                let mut watch = new_title
                    .get("watch")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_else(default_register);
                let plays = import_plays(&watch, write, entry);
                let latest = write["plays"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(safe_u64)
                    .chain(
                        write["days"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|d| safe_u64(&d["day"])),
                    )
                    .max()
                    .unwrap_or(1);
                if !plays.is_empty() {
                    let mut all = plays_of(&watch);
                    all.extend(plays.iter().map(|at| (*at as i64 - IMPORT_OFFSET, *at)));
                    watch.insert("plays".into(), Value::Object(select(&mut all)));
                    new_title.insert("watch".into(), Value::Object(watch));
                }
                title_import(
                    &mut new_title,
                    "status",
                    json!("watched"),
                    Stamp(0, latest, String::new()),
                );
            }
            "import_rating" if !deleted(&new_title) => {
                if !matches!(
                    write["reaction"].as_str(),
                    Some("dislike" | "like" | "love")
                ) {
                    return Err("invalid_reaction".into());
                }
                let c = write["rated_at"].as_u64().filter(|c| *c > 0).unwrap_or(1);
                title_import(
                    &mut new_title,
                    "reaction",
                    write["reaction"].clone(),
                    Stamp(0, c, String::new()),
                );
            }
            "import_watchlist_add" if !deleted(&new_title) => {
                let c = write["listed_at"].as_u64().filter(|c| *c > 0).unwrap_or(1);
                title_import(
                    &mut new_title,
                    "status",
                    json!("watchlist"),
                    Stamp(0, c, String::new()),
                );
            }
            "import_watchlist_remove" if !deleted(&new_title) => {
                let status = new_title.get("status").cloned().unwrap_or(Value::Null);
                if status["value"] == "watchlist" {
                    let current_c = stamp(&status["at"]).map(|s| s.1).unwrap_or(0);
                    let previous = write["previous_listed_at"].as_u64().unwrap_or(0);
                    let watch = new_title
                        .get("watch")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let cleared_t = cleared_of(&watch).map(|(_, s)| s.0).unwrap_or(i64::MIN);
                    let imported_play = plays_of(&watch)
                        .iter()
                        .any(|(key, at)| *key < 0 && *at as i64 > cleared_t);
                    let value = if film && imported_play {
                        "watched"
                    } else {
                        "none"
                    };
                    title_import(
                        &mut new_title,
                        "status",
                        json!(value),
                        Stamp(0, previous.max(current_c) + 1, String::new()),
                    );
                }
            }
            "import_film"
            | "import_rating"
            | "import_watchlist_add"
            | "import_watchlist_remove" => {}
            _ => return Err("invalid_write".into()),
        }
    }
    let mut out = Vec::new();
    let changed = |before: Option<&Map<String, Value>>, after: &Map<String, Value>| {
        before.is_none_or(|b| !jcs::same(&Value::Object(b.clone()), &Value::Object(after.clone())))
    };
    if changed(stored_title.as_ref(), &new_title)
        && (stored_title.is_some() || new_title.len() > base.members().len())
    {
        out.push(Value::Object(new_title));
    }
    for (season, doc) in new_seasons {
        let before = stored_seasons.get(&season);
        let empty = doc
            .get("episodes")
            .and_then(Value::as_object)
            .is_none_or(Map::is_empty)
            && doc.get("seasonReset").is_none_or(Value::is_null);
        if changed(before, &doc) && (before.is_some() || !empty) {
            out.push(Value::Object(doc));
        }
    }
    Ok(json!({"documents": out}))
}
