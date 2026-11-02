//! Assistant reads (den-spec `wire/assistant-v1.md` §15): the projection a device publishes for each live read grant —
//! the watchlist, Continue Watching and Seen history, built from the library-v4 documents the client already decodes,
//! and sealed under each grant's read key. Both clients call this one op, so they publish the same thing.

use crate::index;
use crate::library_v4::{self, doc::Kind, state};
use crate::series::{continue_entry, ContinueInput};
use crate::wire::{canonical, stamp};
use den_assistant::{
    ContinueRow, Media, ProjectionRows, ProjectionTitle, RatedRow, SeenRow, WatchlistRow,
};
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
    pub layouts: &'a BTreeMap<String, &'a RawValue>,
    pub head: u64,
    /// The `set:prefs` row, for the hide rules; absent is none.
    pub prefs: Option<&'a Value>,
    /// The lists already read from the documents (`LibraryColumns::assistant`, `Lists::to_json`), in place of
    /// `documents` and `layouts`.
    pub lists: Option<&'a Value>,
}

fn title_ref((media, id): ProjectionTitle) -> Value {
    json!({"type": media.as_str(), "id": id})
}

fn title_of(media: &str, id: u64) -> ProjectionTitle {
    // An identity is a movie or a series (`doc::identity`).
    (
        if media == "movie" {
            Media::Movie
        } else {
            Media::Tv
        },
        id,
    )
}

fn at_of(field: Option<&Value>) -> Option<i64> {
    field.and_then(|f| stamp(&f["at"]).ok()).map(|s| s.0)
}

fn is_true(doc: &Map<String, Value>, field: &str) -> bool {
    doc.get(field).is_some_and(|f| f["value"] == true)
}

/// The order of a list: most recent first, untimed last, then by title, season and episode, so one library always
/// gives one list.
type Order = (
    std::cmp::Reverse<i64>,
    ProjectionTitle,
    Option<u64>,
    Option<u64>,
);

fn order(
    at: &Value,
    title: ProjectionTitle,
    season: Option<&Value>,
    episode: Option<&Value>,
) -> Order {
    (
        std::cmp::Reverse(at.as_i64().unwrap_or(i64::MIN)),
        title,
        season.and_then(Value::as_u64),
        episode.and_then(Value::as_u64),
    )
}

/// Every list, uncut, as rows, and how many documents could not be read. The rows are a small part of what the lists
/// would be as `Value`s, and a library's Seen list runs to tens of thousands.
pub struct Lists {
    pub watchlist: Vec<WatchlistRow>,
    pub continuing: Vec<ContinueRow>,
    pub seen: Vec<SeenRow>,
    /// Every title the household rated (`reaction` dislike, like or love), with when.
    pub rated: Vec<RatedRow>,
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

/// A document this call has already read, read again: it reads the same.
fn reread(document: &RawValue) -> Result<Map<String, Value>, String> {
    Ok(read(document)?.ok_or("invalid_request")?.1)
}

pub fn lists(lib: &Library, now: i64) -> Result<Lists, String> {
    // Each document is only found here, and read again when it is used, so only one is held at a time: the documents
    // are many times the size of their text once read.
    let mut titles: BTreeMap<(String, u64), &RawValue> = BTreeMap::new();
    let mut seasons: BTreeMap<u64, BTreeMap<u64, &RawValue>> = BTreeMap::new();
    let mut skipped = 0u64;
    for document in lib.documents {
        let Some((identity, _)) = read(document)? else {
            skipped += 1;
            continue;
        };
        match identity.kind {
            Kind::Title => {
                titles.insert((identity.media, identity.id), *document);
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
    let mut watchlist = Vec::new();
    let mut seen = Vec::new();
    let mut films = Vec::new();
    let mut rated = Vec::new();
    for ((media, id), document) in &titles {
        let doc = &reread(document)?;
        // A title `deleted` by a watchlist removal keeps its plays in `seen` and leaves every other list.
        let deleted = is_true(doc, "deleted");
        let title = title_of(media, *id);
        // A rating, not `seen` (a reaction that only says it was seen) and not a cleared one.
        if let Some(reaction) = doc.get("reaction").filter(|_| !deleted) {
            if let Some(value) = reaction["value"]
                .as_str()
                .filter(|v| matches!(*v, "dislike" | "like" | "love"))
            {
                rated.push(RatedRow {
                    title,
                    value: json!(value),
                    at: json!(at_of(Some(reaction)).filter(|t| *t > 0)),
                });
            }
        }
        let status = doc.get("status").and_then(|s| s["value"].as_str());
        if !deleted && status == Some("watchlist") {
            // When it was listed (`TitleFacts::listed_at`): the later of `addedAt`, the first time the title entered the
            // library, and the status stamp, which a move back onto the watchlist sets.
            let added = doc
                .get("addedAt")
                .and_then(Value::as_i64)
                .max(at_of(doc.get("status")));
            watchlist.push(WatchlistRow {
                title,
                added_at: json!(added),
            });
        }
        if media == "movie" {
            let film = state::film_state(doc, now)?;
            push_plays(&mut seen, &film, title, None);
            let resume = doc.get("resume");
            let fraction = resume.and_then(|r| r["value"].as_f64()).unwrap_or(0.0);
            let progress_at = at_of(resume).unwrap_or(0);
            let dismissed = is_true(doc, "dismissed")
                && at_of(doc.get("dismissed")).is_some_and(|d| d >= progress_at);
            if !deleted && status == Some("inProgress") && !dismissed {
                films.push(ContinueRow {
                    title,
                    action: json!("resume"),
                    season: None,
                    episode: None,
                    fraction: json!(fraction),
                    at: json!(progress_at),
                });
            }
        }
    }

    let mut series = Vec::new();
    for (id, by_season) in &seasons {
        let title = match titles.get(&("tv".to_owned(), *id)) {
            Some(document) => Some(reread(document)?),
            None => None,
        };
        let title = title.as_ref();
        let deleted = title.is_some_and(|t| is_true(t, "deleted"));
        // The three summaries `continue_entry` asks for: the mark touched last, the one finished last, the furthest
        // watched with no mark of its own.
        let mut latest: Option<(i64, u64, u64, f64)> = None;
        let mut finished: Option<(i64, u64, u64)> = None;
        let mut flag: Option<(u64, u64)> = None;
        for (season, document) in by_season {
            let doc = &reread(document)?;
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
                    (Media::Tv, *id),
                    Some((*season, episode)),
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
                    if fraction >= WATCHED && finished.is_none_or(|f| (at, *season, episode) > f) {
                        finished = Some((at, *season, episode));
                    }
                }
            }
        }
        if deleted || (latest.is_none() && flag.is_none()) {
            continue;
        }
        // Every layout parses (`projection_op` checked them all).
        let layout: Option<Value> = match lib.layouts.get(&id.to_string()) {
            Some(raw) => Some(serde_json::from_str(raw.get()).map_err(|_| "invalid_request")?),
            None => None,
        };
        let layout = layout.as_ref();
        let dismissed_at = title
            .filter(|t| is_true(t, "dismissed"))
            .and_then(|t| at_of(t.get("dismissed")));
        let coord = |c: Option<(u64, u64)>| c.map(|(s, e)| json!({"season": s, "episode": e}));
        let input: ContinueInput = serde_json::from_value(json!({
            "mark": latest.map(|(at, s, e, f)| json!({"season": s, "episode": e, "fraction": f, "at": at})),
            "finished": finished.map(|(at, s, e)| json!({"season": s, "episode": e, "at": at})),
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
        series.push(ContinueRow {
            title: (Media::Tv, *id),
            action: answer["action"].clone(),
            season: Some(answer["episode"]["season"].clone()),
            episode: Some(answer["episode"]["episode"].clone()),
            fraction: answer["fraction"].clone(),
            at: json!(latest.map(|l| l.0)),
        });
    }
    let mut lists = Lists {
        watchlist,
        continuing: series.into_iter().chain(films).collect(),
        seen,
        rated,
        skipped,
    };
    lists.sort();
    Ok(lists)
}

fn media(title: ProjectionTitle) -> Value {
    json!(title.0.as_str())
}

fn optional(value: &Option<Value>) -> Value {
    value.clone().unwrap_or(Value::Null)
}

impl Lists {
    /// Series and films in one order, latest first: a series by its latest mark, a film by its resume. A series known
    /// only by a flag has no time and comes last.
    pub(crate) fn sort(&mut self) {
        // By positions, so the four lists share one sort (the startup module holds these too).
        fn sorted<T>(rows: Vec<T>, key: impl Fn(&T) -> Order) -> Vec<T> {
            let keys: Vec<Order> = rows.iter().map(key).collect();
            let mut positions = index::indices(rows.len());
            index::sort(&mut positions, &|a, b| keys[a].cmp(&keys[b]));
            index::permute(rows, &positions)
        }
        let at = |at: &Value, title, season: Option<&Value>, episode: Option<&Value>| {
            order(at, title, season, episode)
        };
        self.continuing = sorted(std::mem::take(&mut self.continuing), |r| {
            at(&r.at, r.title, r.season.as_ref(), r.episode.as_ref())
        });
        self.watchlist = sorted(std::mem::take(&mut self.watchlist), |r| {
            at(&r.added_at, r.title, None, None)
        });
        self.seen = sorted(std::mem::take(&mut self.seen), |r| {
            at(&r.at, r.title, r.season.as_ref(), r.episode.as_ref())
        });
        self.rated = sorted(std::mem::take(&mut self.rated), |r| {
            at(&r.at, r.title, None, None)
        });
    }

    /// The lists as `assistant_projection` takes them in place of documents: each row a tuple, title first.
    pub fn to_json(&self) -> Value {
        let watchlist: Vec<Value> = self
            .watchlist
            .iter()
            .map(|r| json!([media(r.title), r.title.1, r.added_at]))
            .collect();
        let continuing: Vec<Value> = self
            .continuing
            .iter()
            .map(|r| {
                json!([
                    media(r.title),
                    r.title.1,
                    r.action,
                    optional(&r.season),
                    optional(&r.episode),
                    r.fraction,
                    r.at
                ])
            })
            .collect();
        let seen: Vec<Value> = self
            .seen
            .iter()
            .map(|r| {
                json!([
                    media(r.title),
                    r.title.1,
                    optional(&r.season),
                    optional(&r.episode),
                    r.at
                ])
            })
            .collect();
        let rated: Vec<Value> = self
            .rated
            .iter()
            .map(|r| json!([media(r.title), r.title.1, r.value, r.at]))
            .collect();
        json!({"watchlist": watchlist, "continue": continuing, "seen": seen, "rated": rated,
            "skipped": self.skipped})
    }

    /// `to_json`'s lists read back. A row that is not one refuses them.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        const INVALID: &str = "invalid_lists";
        fn rows<'a>(value: &'a Value, name: &str, width: usize) -> Result<&'a [Value], String> {
            let rows = value[name].as_array().ok_or(INVALID)?;
            if rows
                .iter()
                .all(|r| r.as_array().is_some_and(|r| r.len() == width))
            {
                Ok(rows)
            } else {
                Err(INVALID.into())
            }
        }
        fn title(r: &Value) -> Result<ProjectionTitle, String> {
            let media = match r[0].as_str() {
                Some("movie") => Media::Movie,
                Some("tv") => Media::Tv,
                _ => return Err(INVALID.into()),
            };
            Ok((media, r[1].as_u64().ok_or(INVALID)?))
        }
        fn some(v: &Value) -> Option<Value> {
            (!v.is_null()).then(|| v.clone())
        }
        Ok(Lists {
            watchlist: rows(value, "watchlist", 3)?
                .iter()
                .map(|r| {
                    Ok(WatchlistRow {
                        title: title(r)?,
                        added_at: r[2].clone(),
                    })
                })
                .collect::<Result<_, String>>()?,
            continuing: rows(value, "continue", 7)?
                .iter()
                .map(|r| {
                    Ok(ContinueRow {
                        title: title(r)?,
                        action: r[2].clone(),
                        season: some(&r[3]),
                        episode: some(&r[4]),
                        fraction: r[5].clone(),
                        at: r[6].clone(),
                    })
                })
                .collect::<Result<_, String>>()?,
            seen: rows(value, "seen", 5)?
                .iter()
                .map(|r| {
                    Ok(SeenRow {
                        title: title(r)?,
                        season: some(&r[2]),
                        episode: some(&r[3]),
                        at: r[4].clone(),
                    })
                })
                .collect::<Result<_, String>>()?,
            rated: rows(value, "rated", 4)?
                .iter()
                .map(|r| {
                    Ok(RatedRow {
                        title: title(r)?,
                        value: r[2].clone(),
                        at: r[3].clone(),
                    })
                })
                .collect::<Result<_, String>>()?,
            skipped: value["skipped"].as_u64().ok_or(INVALID)?,
        })
    }
}

/// The household's hide rules (§15), as Den Web sends them to `/recommend` (`hide`), from the `set:prefs` row:
/// `den.minReleaseYear`, `den.excludedGenreIDs`, `den.excludedLanguages` and `den.hideAnime`. A setting that is
/// absent, cleared or of another type is no rule.
pub fn hide(prefs: Option<&Value>) -> Map<String, Value> {
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
    Map::from_iter([
        ("minYear".to_owned(), json!(year)),
        ("genres".to_owned(), Value::Array(ints)),
        ("languages".to_owned(), Value::Array(strings)),
        ("anime".to_owned(), json!(anime)),
    ])
}

/// One Seen entry per visible play of a film or episode, at its time; one untimed entry for something watched with
/// no play to date it.
fn push_plays(
    seen: &mut Vec<SeenRow>,
    state: &Value,
    title: ProjectionTitle,
    episode: Option<(u64, u64)>,
) {
    let row = |at: Value| SeenRow {
        title,
        season: episode.map(|(s, _)| json!(s)),
        episode: episode.map(|(_, e)| json!(e)),
        at,
    };
    let plays = state["plays"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for play in plays {
        seen.push(row(play[1].clone()));
    }
    if plays.is_empty() && state["watched"] == true {
        seen.push(row(Value::Null));
    }
}

/// An expanded entry (§15) with its `season` and `episode` when it has them.
fn entry(
    title: ProjectionTitle,
    members: Vec<(&str, &Value)>,
    place: [Option<&Value>; 2],
) -> Value {
    let mut object = Map::from_iter(members.into_iter().map(|(k, v)| (k.to_owned(), v.clone())));
    object.insert("title".into(), title_ref(title));
    for (key, value) in ["season", "episode"].into_iter().zip(place) {
        if let Some(value) = value {
            object.insert(key.into(), value.clone());
        }
    }
    Value::Object(object)
}

/// The lists as the projection's rows, and their digest: what changed since the last publish, whoever it is for and
/// whenever. The digest is SHA-256 of the canonical JSON of the expanded lists
/// (`{"continue", "hide", "rated", "seen", "watchlist"}`), written an entry at a time so they are never built whole.
pub fn content(lists: Lists, hide: Map<String, Value>) -> (ProjectionRows, String) {
    let mut hash = Sha256::new();
    let mut list = |name: &str, entries: &mut dyn Iterator<Item = Value>| {
        hash.update(name.as_bytes());
        hash.update(b"[");
        for (i, entry) in entries.enumerate() {
            if i > 0 {
                hash.update(b",");
            }
            hash.update(canonical(&entry));
        }
        hash.update(b"]");
    };
    list(
        "{\"continue\":",
        &mut lists.continuing.iter().map(|r| {
            let members = vec![
                ("action", &r.action),
                ("fraction", &r.fraction),
                ("at", &r.at),
            ];
            entry(r.title, members, [r.season.as_ref(), r.episode.as_ref()])
        }),
    );
    let hide_json = serde_json::to_string(&hide).expect("JSON value always serializes");
    list(
        &format!(",\"hide\":{hide_json},\"rated\":"),
        &mut lists.rated.iter().map(|r| {
            entry(
                r.title,
                vec![("value", &r.value), ("at", &r.at)],
                [None, None],
            )
        }),
    );
    list(
        ",\"seen\":",
        &mut lists.seen.iter().map(|r| {
            entry(
                r.title,
                vec![("at", &r.at)],
                [r.season.as_ref(), r.episode.as_ref()],
            )
        }),
    );
    list(
        ",\"watchlist\":",
        &mut lists
            .watchlist
            .iter()
            .map(|r| entry(r.title, vec![("addedAt", &r.added_at)], [None, None])),
    );
    hash.update(b"}");
    let digest = den_assistant::hex(&hash.finalize());
    let rows = ProjectionRows {
        watchlist: lists.watchlist,
        continuing: lists.continuing,
        seen: lists.seen,
        rated: Some(lists.rated),
        hide: Some(hide),
    };
    (rows, digest)
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
