//! §7 Deriving state from the registers and resets held by documents.

use crate::watch;
use crate::wire::{stamp, Stamp};
use serde_json::{json, Map, Value};

const DAY: i64 = 86_400_000;

/// An absent stamped field counts as `[0, 0, ""]` (v3 §7); these are the defaults a film needs to derive.
fn default_status() -> Value {
    json!({"value": "none", "at": [0, 0, ""]})
}

fn default_resume() -> Value {
    json!({"value": 0, "viewing": 0, "at": [0, 0, ""]})
}

pub fn empty_register() -> Value {
    json!({})
}

/// The covering resets of an episode: the series' `episodesReset` and the season's `seasonReset`.
pub fn resets(
    title: Option<&Map<String, Value>>,
    season: Option<&Map<String, Value>>,
) -> Vec<Stamp> {
    [
        title.and_then(|t| t.get("episodesReset")),
        season.and_then(|s| s.get("seasonReset")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|v| stamp(v).ok())
    .collect()
}

pub fn register<'a>(season: Option<&'a Map<String, Value>>, episode: &str) -> Option<&'a Value> {
    season?.get("episodes")?.get(episode)
}

pub fn episode_state(
    title: Option<&Map<String, Value>>,
    season: Option<&Map<String, Value>>,
    episode: &str,
    now: i64,
) -> Result<Value, String> {
    episode_state_with_resets(season, episode, &resets(title, season), now)
}

fn episode_state_with_resets(
    season: Option<&Map<String, Value>>,
    episode: &str,
    resets: &[Stamp],
    now: i64,
) -> Result<Value, String> {
    let empty = empty_register();
    watch::episode_state(register(season, episode).unwrap_or(&empty), resets, now)
}

/// Every canonical episode register in one season, derived with one fixed time and one set of covering resets.
/// Unknown extension keys in `episodes` remain part of the document but are not episode registers (§3).
pub fn season_state(
    title: Option<&Map<String, Value>>,
    season: Option<&Map<String, Value>>,
    now: i64,
) -> Result<Value, String> {
    let Some(episodes) = season
        .and_then(|season| season.get("episodes"))
        .and_then(Value::as_object)
    else {
        return Ok(json!({}));
    };
    let resets = resets(title, season);
    let states = episodes
        .keys()
        .filter(|episode| super::doc::valid_episode_key(episode))
        .map(|episode| {
            episode_state_with_resets(season, episode, &resets, now)
                .map(|state| (episode.clone(), state))
        })
        .collect::<Result<Map<_, _>, _>>()?;
    Ok(Value::Object(states))
}

/// The `rec` fields a film derives from, with v3 §7's defaults for absent ones.
pub fn film_rec(title: &Map<String, Value>) -> Value {
    json!({
        "status": title.get("status").cloned().unwrap_or_else(default_status),
        "resume": title.get("resume").cloned().unwrap_or_else(default_resume),
    })
}

pub fn film_state(title: &Map<String, Value>, now: i64) -> Result<Value, String> {
    let empty = empty_register();
    watch::film_state(
        &film_rec(title),
        title.get("watch").unwrap_or(&empty),
        &[],
        now,
    )
}

/// A title's v2 `rec` fields as stored, a stamp more than a day ahead read as timeless (v2 §4).
pub fn title_state(title: &Map<String, Value>, now: i64) -> Value {
    let mut out = Map::new();
    for field in [
        "status",
        "resume",
        "reaction",
        "deleted",
        "dismissed",
        "episodesReset",
        "addedAt",
        "watchedAt",
    ] {
        let mut value = title.get(field).cloned().unwrap_or(Value::Null);
        let future = |v: &Value| stamp(v).is_ok_and(|s| s.0 > now.saturating_add(DAY));
        if value.is_object() && future(&value["at"]) {
            value["at"] = json!([0, 0, ""]);
        } else if field == "episodesReset" && future(&value) {
            value = Value::Null;
        }
        out.insert(field.into(), value);
    }
    Value::Object(out)
}
