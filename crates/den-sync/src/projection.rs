//! Assistant reads (den-spec `wire/assistant-v1.md` §15): the projection a device publishes for each live read grant —
//! the watchlist, Continue Watching and Seen history, built from the library-v4 documents the client already decodes,
//! and sealed under each grant's read key. Both clients call this one op, so they publish the same thing.

use crate::library_v4::{self, doc::Kind, state};
use crate::series::{continue_entry, ContinueInput};
use crate::wire::{canonical, stamp};
use hkdf::Hkdf;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MAX_WATCHLIST: usize = 500;
pub const MAX_CONTINUE: usize = 100;
pub const MAX_SEEN: usize = 1000;
const WATCHED: f64 = 0.95;
const NONCE_INFO: &[u8] = b"den/assistant/projection/nonce/v1";

/// What a projection is built from: the library's documents, the series layouts the client knows, and the log head
/// it read them at.
pub struct Library<'a> {
    pub library: &'a str,
    pub documents: &'a [Value],
    /// By TMDB id (`"1399"`): `{"seasons": [{season, episodes}], "last_aired": {season, episode} | null}`.
    pub layouts: &'a Map<String, Value>,
    pub head: u64,
}

fn title_ref(media: &str, id: u64) -> Value {
    json!({"type": media, "id": id})
}

fn at_of(field: Option<&Value>) -> Option<i64> {
    field.and_then(|f| stamp(&f["at"]).ok()).map(|s| s.0)
}

fn is_true(doc: &Map<String, Value>, field: &str) -> bool {
    doc.get(field).is_some_and(|f| f["value"] == true)
}

/// Most recent first, untimed last, then by title, season and episode, so one library always gives one list.
fn newest_first(entries: &mut [Value], key: &str) {
    let order = |e: &Value| {
        (
            std::cmp::Reverse(e[key].as_i64().map_or(i64::MIN, |t| t)),
            e["title"]["type"].as_str().unwrap_or("").to_owned(),
            e["title"]["id"].as_u64(),
            e["season"].as_u64(),
            e["episode"].as_u64(),
        )
    };
    entries.sort_by_key(order);
}

/// Every list, uncut: (watchlist, continue, seen), and how many documents could not be read.
pub fn lists(lib: &Library, now: i64) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>, u64), String> {
    let mut titles: BTreeMap<(String, u64), Map<String, Value>> = BTreeMap::new();
    let mut seasons: BTreeMap<u64, BTreeMap<u64, Map<String, Value>>> = BTreeMap::new();
    let mut skipped = 0u64;
    for document in lib.documents {
        let Ok(doc) = library_v4::readable(document) else {
            skipped += 1;
            continue;
        };
        let Ok(identity) = library_v4::doc::identity(&doc) else {
            skipped += 1;
            continue;
        };
        match identity.kind {
            Kind::Title => {
                titles.insert((identity.media.clone(), identity.id), doc);
            }
            Kind::Season => {
                seasons
                    .entry(identity.id)
                    .or_default()
                    .insert(identity.season.unwrap_or(0), doc);
            }
            Kind::Delivery => {}
        }
    }
    let deleted = |media: &str, id: u64| {
        titles
            .get(&(media.to_owned(), id))
            .is_some_and(|t| is_true(t, "deleted"))
    };

    let mut watchlist = Vec::new();
    let mut seen = Vec::new();
    let mut films = Vec::new();
    for ((media, id), doc) in &titles {
        if is_true(doc, "deleted") {
            continue;
        }
        let status = doc.get("status").and_then(|s| s["value"].as_str());
        if status == Some("watchlist") {
            let added = doc
                .get("addedAt")
                .and_then(Value::as_i64)
                .or_else(|| at_of(doc.get("status")));
            watchlist.push(json!({"title": title_ref(media, *id), "addedAt": added}));
        }
        if media == "movie" {
            let film = state::film_state(doc, now)?;
            push_plays(&mut seen, &film, json!({"title": title_ref(media, *id)}));
            let resume = doc.get("resume");
            let fraction = resume.and_then(|r| r["value"].as_f64()).unwrap_or(0.0);
            let progress_at = at_of(resume).unwrap_or(0);
            let dismissed = is_true(doc, "dismissed")
                && at_of(doc.get("dismissed")).is_some_and(|d| d >= progress_at);
            if status == Some("inProgress") && !dismissed {
                films.push(
                    json!({"title": title_ref(media, *id), "action": "resume", "fraction": fraction,
                    "at": progress_at}),
                );
            }
        }
    }

    let mut series = Vec::new();
    for (id, by_season) in &seasons {
        if deleted("tv", *id) {
            continue;
        }
        let title = titles.get(&("tv".to_owned(), *id));
        // The three summaries `continue_entry` asks for: the mark touched last, the furthest finished, the furthest
        // watched with no mark of its own.
        let mut latest: Option<(i64, u64, u64, f64)> = None;
        let mut finished: Option<(u64, u64)> = None;
        let mut flag: Option<(u64, u64)> = None;
        for (season, doc) in by_season {
            let Some(episodes) = doc.get("episodes").and_then(Value::as_object) else {
                continue;
            };
            for (key, register) in episodes {
                let Some(episode) = key
                    .parse::<u64>()
                    .ok()
                    .filter(|e| *e <= 99_999 && e.to_string() == *key)
                else {
                    continue;
                };
                let state = state::episode_state(title, Some(doc), key, now)?;
                push_plays(
                    &mut seen,
                    &state,
                    json!({"title": title_ref("tv", *id), "season": season, "episode": episode}),
                );
                let mark = if let Some(resume) = state["resume"].as_object() {
                    Some((
                        stamp(&resume["at"]).map(|s| s.0).unwrap_or(0),
                        resume["value"].as_f64().unwrap_or(0.0),
                    ))
                } else if state["watched"] == true {
                    match register.get("progress").filter(|p| !p.is_null()) {
                        Some(progress) => {
                            Some((stamp(&progress["at"]).map(|s| s.0).unwrap_or(0), 1.0))
                        }
                        None => {
                            if flag.is_none_or(|f| (*season, episode) > f) {
                                flag = Some((*season, episode));
                            }
                            None
                        }
                    }
                } else {
                    None
                };
                if let Some((at, fraction)) = mark {
                    if latest.is_none_or(|l| at > l.0) {
                        latest = Some((at, *season, episode, fraction));
                    }
                    if fraction >= WATCHED && finished.is_none_or(|f| (*season, episode) > f) {
                        finished = Some((*season, episode));
                    }
                }
            }
        }
        if latest.is_none() && flag.is_none() {
            continue;
        }
        let layout = lib.layouts.get(&id.to_string());
        let dismissed_at = title
            .filter(|t| is_true(t, "dismissed"))
            .and_then(|t| at_of(t.get("dismissed")));
        let coord = |c: Option<(u64, u64)>| c.map(|(s, e)| json!({"season": s, "episode": e}));
        let input: ContinueInput = serde_json::from_value(json!({
            "mark": latest.map(|(at, s, e, f)| json!({"season": s, "episode": e, "fraction": f, "at": at})),
            "finished": coord(finished),
            "flag": coord(flag),
            "seasons": layout.map_or(json!([]), |l| l["seasons"].clone()),
            "last_aired": layout.map_or(Value::Null, |l| l["last_aired"].clone()),
            "dismissed_at": dismissed_at,
            "title_watched": title.and_then(|t| t.get("status")).is_some_and(|s| s["value"] == "watched"),
        }))
        .map_err(|_| "invalid_layout")?;
        let answer = continue_entry(&input);
        if answer["action"] == "none" || answer["episode"].is_null() {
            continue;
        }
        series.push((
            latest.map_or(i64::MIN, |l| l.0),
            *id,
            json!({"title": title_ref("tv", *id), "action": answer["action"],
                "season": answer["episode"]["season"], "episode": answer["episode"]["episode"],
                "fraction": answer["fraction"], "at": latest.map(|l| l.0)}),
        ));
    }
    // Series by their latest mark, a series known only by a flag last; then films in progress, latest first.
    series.sort_by_key(|(at, id, _)| (std::cmp::Reverse(*at), *id));
    newest_first(&mut films, "at");
    let continuing: Vec<Value> = series.into_iter().map(|(_, _, e)| e).chain(films).collect();
    newest_first(&mut watchlist, "addedAt");
    newest_first(&mut seen, "at");
    Ok((watchlist, continuing, seen, skipped))
}

/// One Seen entry per visible play of a film or episode, at its time; one untimed entry for something watched with
/// no play to date it.
fn push_plays(seen: &mut Vec<Value>, state: &Value, base: Value) {
    let plays = state["plays"].as_array().cloned().unwrap_or_default();
    for play in &plays {
        let mut entry = base.clone();
        entry["at"] = play[1].clone();
        seen.push(entry);
    }
    if plays.is_empty() && state["watched"] == true {
        let mut entry = base;
        entry["at"] = Value::Null;
        seen.push(entry);
    }
}

/// The projection object for one grant (§15), and the digest of everything in it but who it is for and when.
pub fn plaintext(
    lib: &Library,
    grant: &str,
    lists: &(Vec<Value>, Vec<Value>, Vec<Value>),
    now: u64,
) -> (Value, String) {
    let cut = |list: &Vec<Value>, max: usize| {
        (
            list.iter().take(max).cloned().collect::<Vec<_>>(),
            list.len().saturating_sub(max),
        )
    };
    let (watchlist, w) = cut(&lists.0, MAX_WATCHLIST);
    let (continuing, c) = cut(&lists.1, MAX_CONTINUE);
    let (seen, s) = cut(&lists.2, MAX_SEEN);
    let content = json!({
        "watchlist": watchlist, "continue": continuing, "seen": seen,
        "omitted": {"watchlist": w, "continue": c, "seen": s},
    });
    let digest = den_assistant::hex(&Sha256::digest(canonical(&content)));
    let mut object = content.as_object().cloned().expect("an object");
    object.insert("v".into(), json!(1));
    object.insert("library".into(), json!(lib.library));
    object.insert("grant".into(), json!(grant));
    object.insert("at".into(), json!(now));
    object.insert("head".into(), json!(lib.head));
    (Value::Object(object), digest)
}

/// A seal's nonce for one grant from the call's 32 random bytes: HKDF-SHA256(ikm = random, info = context ‖ grant).
pub fn nonce(random: &[u8; 32], grant: &str) -> [u8; 12] {
    let mut out = [0u8; 12];
    Hkdf::<Sha256>::new(None, random)
        .expand(&[NONCE_INFO, grant.as_bytes()].concat(), &mut out)
        .expect("12 bytes is a valid HKDF-SHA256 length");
    out
}
