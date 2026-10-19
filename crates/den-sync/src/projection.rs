//! Assistant reads (den-spec `wire/assistant-v1.md` §15): the projection a device publishes for each live read grant —
//! the watchlist, Continue Watching and Seen history, built from the library-v4 documents the client already decodes,
//! and sealed under each grant's read key. Both clients call this one op, so they publish the same thing.

use crate::library_v4::{self, doc::Kind, state};
use crate::series::{continue_entry, ContinueInput};
use crate::wire::{canonical, stamp};
use hkdf::Hkdf;
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const WATCHED: f64 = 0.95;
const RANDOM_INFO: &[u8] = b"den/assistant/projection/grant/v1\0";

/// What a projection is built from: the library's documents, the series layouts the client knows, and the log head
/// it read them at.
pub struct Library<'a> {
    pub library: &'a str,
    /// As the request spells them: each is parsed when it is read.
    pub documents: &'a [&'a RawValue],
    /// By TMDB id (`"1399"`): `{"seasons": [{season, episodes}], "last_aired": {season, episode} | null}`.
    pub layouts: &'a Map<String, Value>,
    pub head: u64,
    /// The `set:prefs` row, for the hide rules; absent is none.
    pub prefs: Option<&'a Value>,
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

/// Every list, uncut, and how many documents could not be read.
pub struct Lists {
    pub watchlist: Vec<Value>,
    pub continuing: Vec<Value>,
    pub seen: Vec<Value>,
    /// Every title the household rated (`reaction` dislike, like or love), with when.
    pub rated: Vec<Value>,
    pub skipped: u64,
}

/// A document read as state is read from it, with its identity; `None` when it cannot be. One that is no JSON value
/// refuses the request (`projection_op` has already checked that none is).
type Read = Option<(library_v4::doc::Identity, Map<String, Value>)>;

fn read(document: &RawValue) -> Result<Read, String> {
    let value = serde_json::from_str(document.get()).map_err(|_| "invalid_request")?;
    Ok(library_v4::into_readable(value).ok().and_then(|doc| {
        let identity = library_v4::doc::identity(&doc).ok()?;
        Some((identity, doc))
    }))
}

pub fn lists(lib: &Library, now: i64) -> Result<Lists, String> {
    let mut titles: BTreeMap<(String, u64), Map<String, Value>> = BTreeMap::new();
    // A season document is only found here, and read again when its series is: the seasons hold most of a library, and
    // only one of them is held at a time.
    let mut seasons: BTreeMap<u64, BTreeMap<u64, &RawValue>> = BTreeMap::new();
    let mut skipped = 0u64;
    for document in lib.documents {
        let Some((identity, doc)) = read(document)? else {
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
                    .insert(identity.season.unwrap_or(0), *document);
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
    let mut rated = Vec::new();
    for ((media, id), doc) in &titles {
        if is_true(doc, "deleted") {
            continue;
        }
        // A rating, not `seen` (a reaction that only says it was seen) and not a cleared one.
        if let Some(reaction) = doc.get("reaction") {
            if let Some(value) = reaction["value"]
                .as_str()
                .filter(|v| matches!(*v, "dislike" | "like" | "love"))
            {
                rated.push(json!({"title": title_ref(media, *id), "value": value,
                    "at": at_of(Some(reaction)).filter(|t| *t > 0)}));
            }
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
        for (season, document) in by_season {
            // It was read once already, so it reads the same again.
            let Some((_, doc)) = read(document)? else {
                continue;
            };
            let doc = &doc;
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
        series.push(
            json!({"title": title_ref("tv", *id), "action": answer["action"],
                "season": answer["episode"]["season"], "episode": answer["episode"]["episode"],
                "fraction": answer["fraction"], "at": latest.map(|l| l.0)}),
        );
    }
    // Series and films in one order, latest first: a series by its latest mark, a film by its resume. A series known
    // only by a flag has no time and comes last.
    let mut continuing: Vec<Value> = series.into_iter().chain(films).collect();
    newest_first(&mut continuing, "at");
    newest_first(&mut watchlist, "addedAt");
    newest_first(&mut seen, "at");
    newest_first(&mut rated, "at");
    Ok(Lists {
        watchlist,
        continuing,
        seen,
        rated,
        skipped,
    })
}

/// The household's hide rules (§15), as Den Web sends them to `/recommend` (`hide`), from the `set:prefs` row:
/// `den.minReleaseYear`, `den.excludedGenreIDs`, `den.excludedLanguages` and `den.hideAnime`. A setting that is
/// absent, cleared or of another type is no rule.
pub fn hide(prefs: Option<&Value>) -> Value {
    let value = |key: &str| prefs.and_then(|row| row["values"][key]["value"].as_object().cloned());
    let year = value("den.minReleaseYear").and_then(|v| v.get("int").and_then(Value::as_i64));
    let ints = value("den.excludedGenreIDs")
        .and_then(|v| v.get("ints").and_then(Value::as_array).cloned())
        .map(|list| list.into_iter().filter(Value::is_i64).collect::<Vec<_>>())
        .unwrap_or_default();
    let strings = value("den.excludedLanguages")
        .and_then(|v| v.get("strings").and_then(Value::as_array).cloned())
        .map(|list| {
            list.into_iter()
                .filter(Value::is_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let anime = value("den.hideAnime").is_some_and(|v| v.get("bool") == Some(&json!(true)));
    json!({"minYear": year, "genres": ints, "languages": strings, "anime": anime})
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

/// Makes `content` the projection object for one grant (§15): the lists, with who it is for, when, and the log head
/// they were read at. In place, since the lists are most of a projection: each grant's call replaces the members the
/// last one set.
pub fn view(lib: &Library, grant: &str, content: &mut Value, now: u64) {
    let object = content.as_object_mut().expect("an object");
    object.insert("v".into(), json!(1));
    object.insert("library".into(), json!(lib.library));
    object.insert("grant".into(), json!(grant));
    object.insert("at".into(), json!(now));
    object.insert("head".into(), json!(lib.head));
}

/// The lists as one object, and its digest: what changed since the last publish, whoever it is for and whenever.
pub fn content(lists: Lists, hide: Value) -> (Value, String) {
    // Moved in, not `json!`, which would serialize a copy of every list.
    let content = Value::Object(Map::from_iter([
        ("watchlist".to_owned(), Value::Array(lists.watchlist)),
        ("continue".to_owned(), Value::Array(lists.continuing)),
        ("seen".to_owned(), Value::Array(lists.seen)),
        ("rated".to_owned(), Value::Array(lists.rated)),
        ("hide".to_owned(), hide),
    ]));
    let digest = den_assistant::hex(&Sha256::digest(canonical(&content)));
    (content, digest)
}

/// One grant's 32 random bytes for its seal, from the call's: HKDF-SHA256(ikm = random, info = context ‖ grant), so
/// one call's grants never share a set id or a nonce.
pub fn random_for(random: &[u8; 32], grant: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(None, random)
        .expand(&[RANDOM_INFO, grant.as_bytes()].concat(), &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 length");
    out
}
