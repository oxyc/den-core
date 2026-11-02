//! A whole-library index over the library's documents: each document is read once, held by name in the order it was
//! first added, and whole-library questions (Home's inputs, Seen history) are answered from it in one call rather than
//! one boundary crossing per document. The documents stay the canonical form; this is derived state, rebuilt or
//! updated a document at a time (`upsert`, `remove`).
//!
//! A document is read into typed registers when it is added; what depends on the time (a stamp more than a day ahead
//! reads as timeless, §7) is applied when a list is asked for. The per-document rules are `state::title_state`'s and
//! `watch::episode_state`'s, held to them by the parity tests below; what this adds is the whole-library fold the
//! clients did on their side of the boundary.

use super::doc::{identity, valid_episode_key, Kind};
use super::{decode, into_readable};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

const WATCHED: f64 = 0.95;
const DAY: i64 = 86_400_000;

/// A stamp's time and device, as a register holds it.
#[derive(Clone)]
struct At {
    t: i64,
    device: Option<String>,
}

fn at_of(value: &Value) -> Option<At> {
    let stamp = crate::wire::stamp(value).ok()?;
    Some(At {
        t: stamp.0,
        device: (!stamp.2.is_empty()).then_some(stamp.2),
    })
}

struct Resume {
    value: f64,
    at: Option<At>,
    seconds: Option<f64>,
    playing: Option<bool>,
}

/// A title document's registers.
struct TitleDoc {
    tv: bool,
    id: u64,
    status: Option<(String, Option<At>)>,
    resume: Option<Resume>,
    reaction: Option<(Option<String>, Option<At>)>,
    deleted: Option<(bool, Option<At>)>,
    dismissed: Option<(bool, Option<At>)>,
    reset: Option<i64>,
    added_at: Option<f64>,
    watched_at: Option<f64>,
}

impl TitleDoc {
    fn read(tv: bool, id: u64, doc: &Map<String, Value>) -> Self {
        let field = |name: &str| doc.get(name).filter(|v| v.is_object());
        let at = |v: &Value| at_of(&v["at"]);
        TitleDoc {
            tv,
            id,
            status: field("status")
                .map(|v| (v["value"].as_str().unwrap_or("none").to_owned(), at(v))),
            resume: field("resume").map(|v| Resume {
                value: v["value"].as_f64().unwrap_or(0.0),
                at: at(v),
                seconds: v["seconds"].as_f64(),
                playing: v["playing"].as_bool(),
            }),
            reaction: field("reaction").map(|v| (v["value"].as_str().map(str::to_owned), at(v))),
            deleted: field("deleted").map(|v| (v["value"] == true, at(v))),
            dismissed: field("dismissed").map(|v| (v["value"] == true, at(v))),
            reset: doc.get("episodesReset").and_then(at_of).map(|a| a.t),
            added_at: doc.get("addedAt").and_then(Value::as_f64),
            watched_at: doc.get("watchedAt").and_then(Value::as_f64),
        }
    }
}

/// One episode register of a season document.
struct Register {
    episode: u64,
    /// `progress`: value, stamp (absent when it is no valid stamp), viewing, seconds, playing.
    progress: Option<(Option<f64>, Option<At>, u64, Option<f64>, Option<bool>)>,
    viewing: u64,
    imported: bool,
    /// In the document's key order, as `watch::plays` reads them.
    plays: Vec<(i64, u64)>,
    cleared: (i64, i64),
}

struct SeasonDoc {
    id: u64,
    season: u64,
    reset: Option<i64>,
    registers: Vec<Register>,
}

impl SeasonDoc {
    fn read(id: u64, season: u64, doc: &Map<String, Value>) -> Self {
        let mut registers: Vec<Register> = doc
            .get("episodes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(key, _)| valid_episode_key(key))
            .filter_map(|(key, register)| {
                let register = register.as_object()?;
                let progress = register.get("progress").filter(|p| !p.is_null()).map(|p| {
                    (
                        p["value"].as_f64(),
                        at_of(&p["at"]),
                        p["viewing"].as_u64().unwrap_or(0),
                        p["seconds"].as_f64(),
                        p["playing"].as_bool(),
                    )
                });
                let cleared = register
                    .get("cleared")
                    .and_then(Value::as_array)
                    .filter(|c| c.len() == 2)
                    .and_then(|c| Some((c[0].as_u64()? as i64, at_of(&c[1])?.t)))
                    .unwrap_or((-1, 0));
                Some(Register {
                    episode: key.parse().ok()?,
                    viewing: progress.as_ref().map_or(0, |p| p.2),
                    progress,
                    imported: register
                        .get("imported")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    plays: register
                        .get("plays")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flatten()
                        .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_u64()?)))
                        .collect(),
                    cleared,
                })
            })
            .collect();
        registers.sort_unstable_by_key(|r| r.episode);
        SeasonDoc {
            id,
            season,
            reset: doc.get("seasonReset").and_then(at_of).map(|a| a.t),
            registers,
        }
    }
}

/// `watch::effective_t`: a stamp more than a day ahead counts as timeless.
fn effective(t: i64, now: i64) -> i64 {
    if t > now.saturating_add(DAY) {
        0
    } else {
        t
    }
}

/// `watch::episode_state` of one register under `floor` (the covering resets' latest effective time): whether it is
/// watched, its resume point when it has one, and when it was watched.
fn episode(r: &Register, floor: i64, now: i64) -> (bool, Option<&Register>, Option<u64>) {
    let visible = r
        .progress
        .as_ref()
        .filter(|p| p.1.as_ref().is_some_and(|at| effective(at.t, now) > floor));
    let mut plays: Vec<(i64, u64)> = r
        .plays
        .iter()
        .copied()
        .filter(|&(key, at)| {
            !(at > now.saturating_add(DAY).max(0) as u64 || at <= floor.max(0) as u64)
                && !((key >= 0 && key <= r.cleared.0)
                    || (key < 0 && at <= r.cleared.1.max(0) as u64))
        })
        .collect();
    plays.sort_unstable_by_key(|(_, at)| *at);
    let imported = r.imported && (floor == 0 || plays.iter().any(|(key, _)| *key < 0));
    let value = visible.and_then(|p| p.0);
    let watched = value.is_some_and(|v| v >= WATCHED) || (visible.is_none() && imported);
    let resume = value
        .is_some_and(|v| (0.02..WATCHED).contains(&v))
        .then_some(r);
    let watched_at = plays
        .iter()
        .find(|(key, _)| *key == r.viewing as i64)
        .or_else(|| plays.last())
        .map(|(_, at)| *at);
    (watched, resume, watched_at)
}

enum Slot {
    Title(Map<String, Value>, TitleDoc),
    Season(Map<String, Value>, SeasonDoc),
    /// Delivery documents are held so the index can be the one copy of the library, but no list reads them yet.
    Other(Map<String, Value>),
}

#[derive(Default)]
pub struct Columns {
    slots: Vec<Option<(String, Slot)>>,
    by_name: HashMap<String, usize>,
}

/// A title's `rec` fields as `title_state` derives them, with the defaults an absent field reads as.
struct TitleFacts {
    key: String,
    tv: bool,
    status: &'static str,
    status_at: i64,
    progress: f64,
    progress_at: i64,
    seconds: Option<f64>,
    playing: Option<bool>,
    by: Option<String>,
    added_at: f64,
    deleted: bool,
    deleted_at: i64,
    dismissed: Option<i64>,
    reaction: Option<String>,
    reaction_at: i64,
    reset: Option<i64>,
    watched_at: Option<f64>,
}

/// One episode's projected row (`v2 ep`): what Continue and Seen history read.
struct EpisodeFacts {
    season: u64,
    episode: u64,
    value: f64,
    at: i64,
    by: Option<String>,
    seconds: Option<f64>,
    playing: Option<bool>,
}

#[derive(Clone)]
struct Mark {
    key: String,
    season: u64,
    episode: u64,
    fraction: f64,
    at: i64,
    seconds: Option<f64>,
    playing: Option<bool>,
    by: Option<String>,
}

fn key(tv: bool, id: u64) -> String {
    format!("{}:{id}", if tv { "tv" } else { "movie" })
}

fn ahead(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 > b.0 || (a.0 == b.0 && a.1 > b.1)
}

impl Columns {
    pub fn new() -> Self {
        Self::default()
    }

    /// Documents held.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Add or replace one document, read as state is read from it (`readable`). A replaced document keeps its place.
    pub fn upsert(&mut self, document: Value) -> Result<String, String> {
        let doc = into_readable(document)?;
        let identity = identity(&doc)?;
        let name = identity.name();
        let slot = match identity.kind {
            Kind::Title => {
                let facts = TitleDoc::read(identity.media == "tv", identity.id, &doc);
                Slot::Title(doc, facts)
            }
            Kind::Season => {
                let facts = SeasonDoc::read(identity.id, identity.season.unwrap_or(0), &doc);
                Slot::Season(doc, facts)
            }
            Kind::Delivery => Slot::Other(doc),
        };
        match self.by_name.get(&name) {
            Some(&index) => self.slots[index] = Some((name.clone(), slot)),
            None => {
                self.by_name.insert(name.clone(), self.slots.len());
                self.slots.push(Some((name.clone(), slot)));
            }
        }
        Ok(name)
    }

    /// `doc_decode` of one row's plaintext, the document held here rather than handed back: the answer is the
    /// decode's status, name and dropped parts. A JSON row or an unreadable one is answered as `doc_decode` answers it.
    pub fn decode(&mut self, plaintext: &[u8], expected: Option<&str>) -> Value {
        let mut decoded = decode(plaintext, expected);
        if let Some(document) = decoded.as_object_mut().and_then(|d| d.remove("document")) {
            if let Err(reason) = self.upsert(document) {
                return json!({"status": "unreadable", "reason": reason});
            }
        }
        decoded
    }

    /// One held document, as it was read.
    pub fn document(&self, name: &str) -> Option<Value> {
        match &self.slots[*self.by_name.get(name)?].as_ref()?.1 {
            Slot::Title(doc, _) | Slot::Season(doc, _) | Slot::Other(doc) => {
                Some(Value::Object(doc.clone()))
            }
        }
    }

    pub fn remove(&mut self, name: &str) -> bool {
        match self.by_name.remove(name) {
            Some(index) => {
                self.slots[index] = None;
                true
            }
            None => false,
        }
    }

    fn title_doc(&self, tv: bool, id: u64) -> Option<&TitleDoc> {
        let name = format!("title:{}", key(tv, id));
        match self.slots[*self.by_name.get(&name)?].as_ref()? {
            (_, Slot::Title(_, facts)) => Some(facts),
            _ => None,
        }
    }

    /// Each title's `rec` fields as `title_state` derives them at `now`, in document order.
    fn titles(&self, now: i64) -> Vec<TitleFacts> {
        let future = |at: &Option<At>| at.as_ref().is_some_and(|a| a.t > now.saturating_add(DAY));
        // `title_state` reads a stamp more than a day ahead as the zero stamp, device and all.
        let at = |at: &Option<At>| if future(at) { None } else { at.clone() };
        let t = |a: &Option<At>| at(a).map_or(0, |a| a.t);
        let mut out = Vec::new();
        for (_, slot) in self.slots.iter().flatten() {
            let Slot::Title(_, d) = slot else {
                continue;
            };
            let status = match d.status.as_ref().map(|s| s.0.as_str()) {
                Some("watchlist") => "watchlist",
                Some("inProgress") => "inProgress",
                Some("watched") => "watched",
                _ => "none",
            };
            let resume_at = d.resume.as_ref().and_then(|r| at(&r.at));
            out.push(TitleFacts {
                key: key(d.tv, d.id),
                tv: d.tv,
                status,
                status_at: d.status.as_ref().map_or(0, |s| t(&s.1)),
                progress: d.resume.as_ref().map_or(0.0, |r| r.value),
                progress_at: resume_at.as_ref().map_or(0, |a| a.t),
                seconds: d.resume.as_ref().and_then(|r| r.seconds),
                playing: d.resume.as_ref().and_then(|r| r.playing),
                by: resume_at.and_then(|a| a.device),
                added_at: d.added_at.unwrap_or(0.0),
                deleted: d.deleted.as_ref().is_some_and(|x| x.0),
                deleted_at: d.deleted.as_ref().map_or(0, |x| t(&x.1)),
                dismissed: d.dismissed.as_ref().filter(|x| x.0).map(|x| t(&x.1)),
                reaction: d.reaction.as_ref().and_then(|x| x.0.clone()),
                reaction_at: d.reaction.as_ref().map_or(0, |x| t(&x.1)),
                reset: d.reset.filter(|r| *r <= now.saturating_add(DAY)),
                watched_at: d.watched_at,
            });
        }
        out
    }

    /// Every season document's episodes as `v2 ep` rows, season documents in order and episodes in number order.
    fn episodes(&self, now: i64, mut visit: impl FnMut(&str, EpisodeFacts)) -> Result<(), String> {
        for (_, slot) in self.slots.iter().flatten() {
            let Slot::Season(_, s) = slot else {
                continue;
            };
            let series = key(true, s.id);
            // `state::resets` then `watch::floor`: the latest covering reset that is not more than a day ahead.
            let floor = [self.title_doc(true, s.id).and_then(|t| t.reset), s.reset]
                .into_iter()
                .flatten()
                .map(|t| effective(t, now))
                .filter(|t| *t > 0)
                .max()
                .unwrap_or(0);
            for r in &s.registers {
                let (watched, resume, watched_at) = episode(r, floor, now);
                let facts = match resume.and_then(|r| r.progress.as_ref()) {
                    Some(p) => EpisodeFacts {
                        season: s.season,
                        episode: r.episode,
                        value: p.0.unwrap_or(0.0),
                        at: p.1.as_ref().map_or(0, |a| a.t),
                        by: p.1.as_ref().and_then(|a| a.device.clone()),
                        seconds: p.3,
                        playing: p.4,
                    },
                    None => EpisodeFacts {
                        season: s.season,
                        episode: r.episode,
                        value: if watched { 1.0 } else { 0.0 },
                        at: watched_at.map_or(0, |t| t as i64),
                        by: None,
                        seconds: None,
                        playing: None,
                    },
                };
                visit(&series, facts);
            }
        }
        Ok(())
    }

    /// Home's library inputs (den-edge `HomeLibraryView` without downloads), every title by `type:id`.
    pub fn home(&self, now: i64) -> Result<Value, String> {
        let titles = self.titles(now);
        let resets: HashMap<&str, i64> = titles
            .iter()
            .filter_map(|f| Some((f.key.as_str(), f.reset?)))
            .collect();
        // Continue's three summaries per series, in the order series first appear.
        let mut order: Vec<String> = Vec::new();
        let mut latest: HashMap<String, Mark> = HashMap::new();
        let mut finished: HashMap<String, Mark> = HashMap::new();
        let mut flag_order: Vec<String> = Vec::new();
        let mut flagged: HashMap<String, (u64, u64)> = HashMap::new();
        self.episodes(now, |series, e| {
            let reset = resets.get(series).copied();
            // `episode_mark` of an authoritative row with nothing held: un-watched clears, a timeless watched bit flags,
            // anything else replaces.
            if e.value == 0.0 {
                return;
            }
            if e.at == 0 {
                if e.value >= WATCHED && reset.is_none() {
                    let place = (e.season, e.episode);
                    match flagged.get(series) {
                        None => {
                            flag_order.push(series.to_owned());
                            flagged.insert(series.to_owned(), place);
                        }
                        Some(front) if ahead(place, *front) => {
                            flagged.insert(series.to_owned(), place);
                        }
                        _ => {}
                    }
                }
                return;
            }
            if reset.is_some_and(|r| e.at <= r) {
                return;
            }
            let mark = Mark {
                key: series.to_owned(),
                season: e.season,
                episode: e.episode,
                fraction: e.value,
                at: e.at,
                seconds: e.seconds,
                playing: e.playing,
                by: e.by,
            };
            match latest.get(series) {
                None => {
                    order.push(series.to_owned());
                    latest.insert(series.to_owned(), mark.clone());
                }
                Some(current) if mark.at > current.at => {
                    latest.insert(series.to_owned(), mark.clone());
                }
                _ => {}
            }
            if mark.fraction >= WATCHED {
                let better = finished.get(series).is_none_or(|front| {
                    mark.at > front.at
                        || (mark.at == front.at
                            && ahead((mark.season, mark.episode), (front.season, front.episode)))
                });
                if better {
                    finished.insert(series.to_owned(), mark);
                }
            }
        })?;
        let mut marks: Vec<&Mark> = Vec::new();
        for series in &order {
            let newest = &latest[series];
            marks.push(newest);
            if let Some(front) = finished.get(series) {
                if (front.season, front.episode) != (newest.season, newest.episode) {
                    marks.push(front);
                }
            }
        }

        let records: Vec<&TitleFacts> = titles.iter().collect();
        let active: Vec<&TitleFacts> = records.iter().copied().filter(|f| !f.deleted).collect();
        let mut owned = Vec::new();
        let mut seen_owned = HashSet::new();
        for f in &active {
            if seen_owned.insert(f.key.as_str()) {
                owned.push(f.key.as_str());
            }
        }
        let watched: Vec<&str> = active
            .iter()
            .filter(|f| f.status == "watched")
            .map(|f| f.key.as_str())
            .collect();
        let mut watchlist: Vec<&TitleFacts> = active
            .iter()
            .copied()
            .filter(|f| f.status == "watchlist")
            .collect();
        watchlist.sort_by(|a, b| b.added_at.total_cmp(&a.added_at));

        let mut standings: Vec<(&str, &str)> = Vec::new();
        let mut standing_at: HashMap<&str, usize> = HashMap::new();
        for m in &marks {
            match standing_at.get(m.key.as_str()) {
                Some(&i) => standings[i].1 = "inProgress",
                None => {
                    standing_at.insert(m.key.as_str(), standings.len());
                    standings.push((m.key.as_str(), "inProgress"));
                }
            }
        }
        for f in &records {
            if f.deleted || f.status == "none" {
                continue;
            }
            match standing_at.get(f.key.as_str()) {
                Some(_) if f.status == "watchlist" => {}
                Some(&i) => standings[i].1 = f.status,
                None => {
                    standing_at.insert(f.key.as_str(), standings.len());
                    standings.push((f.key.as_str(), f.status));
                }
            }
        }
        standings.sort_by(|a, b| a.0.cmp(b.0));

        let weight = |f: &TitleFacts| -> f64 {
            let reaction = f.reaction.as_deref();
            if reaction == Some("dislike") {
                return -1.5;
            }
            let seen = match f.status {
                "watched" | "inProgress" => 1.0,
                "watchlist" => 0.6,
                _ => 0.0,
            };
            seen + match reaction {
                Some("love") => 1.0,
                Some("like") => 0.5,
                _ => 0.0,
            }
        };
        let weighted: Vec<Value> = active
            .iter()
            .filter_map(|f| {
                let w = weight(f);
                (w != 0.0).then(|| json!([f.key, w, (f.progress_at as f64).max(f.added_at)]))
            })
            .collect();

        // The two most recent watched-or-liked and watchlisted titles.
        let recency = |f: &TitleFacts| {
            f.watched_at
                .unwrap_or(0.0)
                .max(f.reaction_at as f64)
                .max(f.added_at)
        };
        let before = |a: &TitleFacts, b: &TitleFacts| {
            let (ra, rb) = (recency(a), recency(b));
            rb < ra || (rb == ra && a.key < b.key)
        };
        let offer = |selected: &mut Vec<usize>, i: usize| match selected
            .iter()
            .position(|&held| before(active[i], active[held]))
        {
            None => {
                if selected.len() < 2 {
                    selected.push(i);
                }
            }
            Some(at) => {
                selected.insert(at, i);
                selected.truncate(2);
            }
        };
        let (mut seed_watched, mut seed_listed) = (Vec::new(), Vec::new());
        for (i, f) in active.iter().enumerate() {
            let reaction = f.reaction.as_deref();
            if f.status == "watched" || matches!(reaction, Some("like" | "love")) {
                offer(&mut seed_watched, i);
            }
            if f.status == "watchlist" {
                offer(&mut seed_listed, i);
            }
        }
        let seeds_watched: Vec<&str> = seed_watched
            .iter()
            .map(|&i| active[i].key.as_str())
            .collect();
        let seeds_listed: Vec<&str> = seed_listed
            .iter()
            .map(|&i| active[i].key.as_str())
            .collect();

        let mut shelf: Vec<&str> = Vec::new();
        let mut on_shelf = HashSet::new();
        let refs = seeds_watched
            .iter()
            .chain(&seeds_listed)
            .copied()
            .chain(
                records
                    .iter()
                    .filter(|f| !f.deleted && matches!(f.status, "watchlist" | "inProgress"))
                    .map(|f| f.key.as_str()),
            )
            .chain(marks.iter().map(|m| m.key.as_str()));
        for k in refs {
            if on_shelf.insert(k) {
                shelf.push(k);
            }
        }
        let shaped: HashSet<&str> = marks
            .iter()
            .map(|m| m.key.as_str())
            .chain(flag_order.iter().map(String::as_str))
            .collect();
        let mut watched_at: HashMap<&str, i64> = HashMap::new();
        for m in &marks {
            let at = watched_at.entry(m.key.as_str()).or_insert(0);
            *at = (*at).max(m.at);
        }
        let mut required: Vec<&str> = shelf
            .iter()
            .copied()
            .filter(|k| k.starts_with("tv:") && shaped.contains(k))
            .collect();
        required.sort_by_key(|k| std::cmp::Reverse(watched_at.get(k).copied().unwrap_or(0)));

        // Continue's library: the records and dismissals its policy can still use.
        let live_series: HashSet<&str> = order
            .iter()
            .map(String::as_str)
            .chain(flag_order.iter().map(String::as_str))
            .collect();
        let kept: Vec<&TitleFacts> = records
            .iter()
            .copied()
            .filter(|f| {
                !f.deleted
                    && if f.tv {
                        live_series.contains(f.key.as_str())
                            && (f.status == "watched" || flagged.contains_key(&f.key))
                    } else {
                        f.status == "inProgress"
                    }
            })
            .collect();
        let live: HashSet<&str> = kept
            .iter()
            .map(|f| f.key.as_str())
            .chain(live_series.iter().copied())
            .collect();
        let dismissed: Vec<Value> = records
            .iter()
            .filter_map(|f| Some((f, f.dismissed?)))
            .filter(|(f, _)| live.contains(f.key.as_str()))
            .map(|(f, at)| json!([f.key, at]))
            .collect();

        Ok(json!({
            "owned": owned,
            "watched": watched,
            "watchlist": watchlist.iter().map(|f| &f.key).collect::<Vec<_>>(),
            "standings": standings.iter().map(|(k, s)| json!([k, s])).collect::<Vec<_>>(),
            "weighted": weighted,
            "seeds": {"watched": seeds_watched, "watchlisted": seeds_listed},
            "shelfRefs": shelf,
            "requiredShapeRefs": required,
            "continue": {
                // [key, status, progress, progressAt, seconds, playing, progressBy, addedAt]
                "records": kept.iter().map(|f| json!([
                    f.key, f.status, f.progress, f.progress_at, f.seconds, f.playing, f.by, f.added_at
                ])).collect::<Vec<_>>(),
                // [key, season, episode, fraction, updatedAt, seconds, playing, updatedBy]
                "marks": marks.iter().map(|m| json!([
                    m.key, m.season, m.episode, m.fraction, m.at, m.seconds, m.playing, m.by
                ])).collect::<Vec<_>>(),
                "flags": flag_order.iter().map(|k| {
                    let (s, e) = flagged[k];
                    json!([k, s, e])
                }).collect::<Vec<_>>(),
                "dismissed": dismissed,
            },
        }))
    }

    /// Seen history (den-edge `watchedHistory`) before titles are named: `[key, at, season, episode, episodes]`, newest
    /// first, then by key.
    pub fn history(&self, now: i64) -> Result<Value, String> {
        struct Seen {
            latest_at: i64,
            latest: Option<(u64, u64, i64)>,
            count: u64,
        }
        let titles = self.titles(now);
        let by_key: HashMap<&str, &TitleFacts> =
            titles.iter().map(|f| (f.key.as_str(), f)).collect();
        let mut order: Vec<String> = Vec::new();
        let mut series: HashMap<String, Seen> = HashMap::new();
        self.episodes(now, |key, e| {
            if e.value < WATCHED {
                return;
            }
            if by_key
                .get(key)
                .and_then(|f| f.reset)
                .is_some_and(|reset| e.at <= reset)
            {
                return;
            }
            let found = series.entry(key.to_owned()).or_insert_with(|| {
                order.push(key.to_owned());
                Seen {
                    latest_at: e.at,
                    latest: None,
                    count: 0,
                }
            });
            if e.at > found.latest_at {
                found.latest_at = e.at;
            }
            if e.season == 0 {
                return;
            }
            found.count += 1;
            if found.latest.is_none_or(|l| e.at > l.2) {
                found.latest = Some((e.season, e.episode, e.at));
            }
        })?;
        let mut keys: Vec<&str> = Vec::new();
        let mut listed = HashSet::new();
        for k in titles
            .iter()
            .map(|f| f.key.as_str())
            .chain(order.iter().map(String::as_str))
        {
            if listed.insert(k) {
                keys.push(k);
            }
        }
        let mut entries: Vec<(i64, &str, Value)> = Vec::new();
        for k in keys {
            let row = by_key.get(k);
            let seen_at = match row {
                Some(f) if f.status == "watched" => {
                    (f.watched_at.unwrap_or(0.0) as i64).max(f.status_at).max(0)
                }
                _ => -1,
            };
            let episodes = series.get(k);
            let at = seen_at.max(episodes.map_or(-1, |s| s.latest_at.max(0)));
            if at < 0 || row.is_some_and(|f| f.deleted && f.deleted_at >= at) {
                continue;
            }
            let latest = episodes.and_then(|s| s.latest);
            entries.push((
                at,
                k,
                json!([
                    k,
                    at,
                    latest.map(|l| l.0),
                    latest.map(|l| l.1),
                    episodes.map_or(0, |s| s.count)
                ]),
            ));
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        Ok(Value::Array(
            entries.into_iter().map(|(_, _, v)| v).collect(),
        ))
    }
}

/// The bindings' face of the index: text in, the versioned envelope out, as `evaluate` answers.
impl Columns {
    /// `documents`, a JSON array of documents, each added or replaced; the answer is how many are held.
    pub fn upsert_text(&mut self, documents: &str) -> String {
        crate::envelope((|| {
            let documents: Vec<Value> =
                serde_json::from_str(documents).map_err(|_| "invalid_request")?;
            for document in documents {
                self.upsert(document)?;
            }
            Ok(json!(self.len()))
        })())
    }

    /// `doc_decode` of one plaintext into the index (`decode`).
    pub fn decode_text(&mut self, plaintext: &[u8], expected: Option<&str>) -> String {
        crate::envelope(Ok(self.decode(plaintext, expected)))
    }

    /// The named lists, `queries` a JSON array of `"home"` and `"history"`.
    pub fn query_text(&self, queries: &str, now: i64) -> String {
        crate::envelope((|| {
            let queries: Vec<String> =
                serde_json::from_str(queries).map_err(|_| "invalid_request")?;
            self.lists(&queries, now)
        })())
    }

    fn lists(&self, queries: &[String], now: i64) -> Result<Value, String> {
        let mut out = Map::new();
        for query in queries {
            let value = match query.as_str() {
                "home" => self.home(now)?,
                "history" => self.history(now)?,
                _ => return Err("unknown_query".into()),
            };
            out.insert(query.clone(), value);
        }
        Ok(Value::Object(out))
    }
}

/// `library_columns` (stateless form): `documents` read into an index once and the named `queries` answered from it.
pub fn answer(documents: Vec<Value>, queries: &[String], now: i64) -> Result<Value, String> {
    let mut columns = Columns::new();
    for document in documents {
        columns.upsert(document)?;
    }
    columns.lists(queries, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_v4::state;

    const NOW: i64 = 4_000;

    fn documents() -> Vec<Value> {
        let d = "a1b2c3d4e5f60718";
        let future = NOW + 2 * DAY;
        vec![
            // policy-v4's season projection case: resets on both levels, imported plays, cleared plays, a future
            // stamp, extension keys that are no episodes.
            json!({"format":4,"kind":"title","title":{"id":1399,"type":"tv"},"episodesReset":[2000,0,d]}),
            json!({"episodes":{"01":{"futureExtension":true},
                "1":{"cleared":null,"imported":false,"plays":{"0":1000},"progress":{"at":[1000,0,d],"value":1,"viewing":0}},
                "100000":{"futureExtension":true},
                "2":{"cleared":null,"imported":true,"plays":{"-9007199254737992":3000}},
                "3":{"cleared":[0,[2500,0,d]],"imported":false,"plays":{"0":1000,"1":3000},
                     "progress":{"at":[3000,0,d],"value":0.4,"viewing":1,"seconds":120,"playing":false}},
                "4":{"cleared":null,"imported":false,"plays":{},"progress":{"at":[864000000,0,d],"value":1,"viewing":4}},
                "5":{}},
                "format":4,"kind":"season","season":1,"seasonReset":[1500,0,d],"title":{"id":1399,"type":"tv"}}),
            json!({"episodes":{
                "1":{"cleared":null,"imported":true,"plays":{}},
                "2":{"cleared":null,"imported":false,"plays":{"0":3500,"1":3500},"progress":{"at":[3500,0,d],"value":1,"viewing":1}},
                "3":{"cleared":null,"imported":false,"plays":{},"progress":{"at":[3600,0,""],"value":0.01,"viewing":0}}},
                "format":4,"kind":"season","season":0,"seasonReset":null,"title":{"id":1399,"type":"tv"}}),
            // A series with no title document.
            json!({"episodes":{"7":{"cleared":null,"imported":false,"plays":{"0":10},"progress":{"at":[10,0,d],"value":1,"viewing":0}}},
                "format":4,"kind":"season","season":2,"seasonReset":null,"title":{"id":7,"type":"tv"}}),
            // Future stamps on every title register, and a future reset.
            json!({"format":4,"kind":"title","title":{"id":5,"type":"movie"},
                "status":{"value":"inProgress","at":[future,0,d]},
                "resume":{"value":0.5,"at":[future,0,d],"viewing":2,"seconds":30,"playing":true},
                "reaction":{"value":"love","at":[future,0,d]},
                "deleted":{"value":false,"at":[future,0,d]},
                "dismissed":{"value":true,"at":[future,0,d]},
                "episodesReset":[future,0,d],"addedAt":12,"watchedAt":null}),
            json!({"format":4,"kind":"title","title":{"id":6,"type":"movie"},
                "status":{"value":"watched","at":[300,0,d]},
                "resume":{"value":0.3,"at":[250,0,d],"viewing":0,"seconds":30},
                "reaction":{"value":"dislike","at":[200,0,d]},
                "deleted":{"value":true,"at":[400,0,d]},
                "dismissed":{"value":true,"at":[100,0,d]},
                "addedAt":10,"watchedAt":90}),
            json!({"format":4,"kind":"title","title":{"id":8,"type":"tv"}}),
        ]
    }

    #[test]
    fn typed_registers_derive_as_the_rules_do() {
        let mut columns = Columns::new();
        for document in documents() {
            columns.upsert(document).unwrap();
        }
        for now in [NOW, 864_000_000 - DAY, 864_000_000 + 1] {
            // Titles against `title_state`.
            let facts = columns.titles(now);
            let mut i = 0;
            for (_, slot) in columns.slots.iter().flatten() {
                let Slot::Title(doc, _) = slot else { continue };
                let s = state::title_state(doc, now);
                let f = &facts[i];
                i += 1;
                let t = |v: &Value| v.get(0).and_then(Value::as_i64).unwrap_or(0);
                assert_eq!(
                    f.status,
                    s["status"]["value"].as_str().unwrap_or("none"),
                    "{}",
                    f.key
                );
                assert_eq!(f.status_at, t(&s["status"]["at"]), "{}", f.key);
                assert_eq!(
                    f.progress,
                    s["resume"]["value"].as_f64().unwrap_or(0.0),
                    "{}",
                    f.key
                );
                assert_eq!(f.progress_at, t(&s["resume"]["at"]), "{}", f.key);
                assert_eq!(
                    f.by.as_deref(),
                    s["resume"]["at"][2].as_str().filter(|d| !d.is_empty())
                );
                assert_eq!(f.seconds, s["resume"]["seconds"].as_f64());
                assert_eq!(f.playing, s["resume"]["playing"].as_bool());
                assert_eq!(f.deleted, s["deleted"]["value"] == true);
                assert_eq!(f.deleted_at, t(&s["deleted"]["at"]));
                assert_eq!(
                    f.dismissed,
                    (s["dismissed"]["value"] == true).then(|| t(&s["dismissed"]["at"]))
                );
                assert_eq!(f.reaction.as_deref(), s["reaction"]["value"].as_str());
                assert_eq!(f.reaction_at, t(&s["reaction"]["at"]));
                assert_eq!(
                    f.reset,
                    s["episodesReset"]
                        .is_array()
                        .then(|| t(&s["episodesReset"])),
                    "{}",
                    f.key
                );
                assert_eq!(f.added_at, s["addedAt"].as_f64().unwrap_or(0.0));
                assert_eq!(f.watched_at, s["watchedAt"].as_f64());
            }
            assert_eq!(i, facts.len());
            // Episodes against `season_state`.
            let mut typed = Vec::new();
            columns
                .episodes(now, |series, e| {
                    typed.push((
                        series.to_owned(),
                        e.season,
                        e.episode,
                        e.value,
                        e.at,
                        e.by,
                        e.seconds,
                        e.playing,
                    ))
                })
                .unwrap();
            let mut ruled = Vec::new();
            for (_, slot) in columns.slots.iter().flatten() {
                let Slot::Season(doc, s) = slot else { continue };
                let title = columns
                    .slots
                    .iter()
                    .flatten()
                    .find_map(|(name, slot)| match slot {
                        Slot::Title(doc, _) if *name == format!("title:tv:{}", s.id) => Some(doc),
                        _ => None,
                    });
                let states = state::season_state(title, Some(doc), now).unwrap();
                let mut numbered: Vec<(u64, &Value)> = states
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.parse().unwrap(), v))
                    .collect();
                numbered.sort_by_key(|(n, _)| *n);
                for (episode, st) in numbered {
                    let row = match st["resume"].as_object() {
                        Some(r) => (
                            r["value"].as_f64().unwrap(),
                            r["at"][0].as_i64().unwrap(),
                            r["at"][2]
                                .as_str()
                                .filter(|d| !d.is_empty())
                                .map(str::to_owned),
                            r.get("seconds").and_then(Value::as_f64),
                            r.get("playing").and_then(Value::as_bool),
                        ),
                        None => (
                            if st["watched"] == true { 1.0 } else { 0.0 },
                            st["watched_at"].as_i64().unwrap_or(0),
                            None,
                            None,
                            None,
                        ),
                    };
                    ruled.push((
                        format!("tv:{}", s.id),
                        s.season,
                        episode,
                        row.0,
                        row.1,
                        row.2,
                        row.3,
                        row.4,
                    ));
                }
            }
            assert_eq!(typed, ruled, "at {now}");
        }
    }
}
