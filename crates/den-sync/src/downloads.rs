//! The download queue both clients share (den-spec library-v4 §17): one `set:download:<content>` settings row per
//! download, and the rules every client runs on it — the merge, what a poll answer means, which release a stalled
//! download moves on to, what is pruned, and the release ranking that picks the first one.
//!
//! The queue's live figures (percent, rate, ETA, the swarm) are never synced: each client asks den-scout itself. So
//! nothing here reads a clock or a network; the poll answer, the row and `now` arrive as inputs.

use crate::wire::{stamp, Stamp};
use serde_json::{json, Map, Value};

/// A download that moved nothing for this long moves on to the next release (the TV's `stallLimit`).
const STALL_LIMIT: i64 = 20 * 60_000;
/// How long silence from den-scout is read as "the add hasn't landed yet" rather than "it never started".
const START_GRACE: i64 = 3 * 60_000;
/// The stall clock is written to the row at most this often while a download moves.
const PROGRESS_WRITE_EVERY: i64 = 5 * 60_000;
/// Once watched, a landed download remains visible long enough to confirm that it finished.
const WATCHED_READY_TTL: i64 = 2 * 86_400_000;
/// Live rows kept at most; the oldest past it are pruned.
const LIVE_CAP: usize = 100;

// Choosing and racing a download's releases (§17 *Choosing a download*, *Racing*).
/// What one seeder is assumed to deliver before a fetch starts, bytes/s, and the most any swarm is assumed to.
const PER_SEEDER_RATE: i64 = 100_000;
const MAX_FETCH_RATE: i64 = 5_000_000;
/// An uncached release expected within this is "soon", and among those the best picture wins.
const FINISH_SOON: i64 = 3 * 3600;
/// A fetch the debrid projects to need longer than this is slow, and the download races alternates.
const SLOW_ETA: i64 = 12 * 3600;
/// A fetch's projection is not judged before it has run this long: the first minutes' rate says little.
const JUDGE_AFTER: i64 = 10 * 60_000;
/// How many alternates the first escalation adds beside what is in flight.
const RACE_WIDEN: usize = 2;
/// With nothing finished this long after the first escalation, every remaining live release is added.
const RACE_ALL_AFTER: i64 = 4 * 3_600_000;
/// With nothing watchable this long after the download was asked for, a cached low-quality release is taken.
const FALLBACK_AFTER: i64 = 24 * 3_600_000;
/// After the debrid refused an add (its active-download limit), nothing more is added for this long.
const CAP_RETRY: i64 = 15 * 60_000;
/// The first fresh search after a download runs out of releases; each further one waits twice as long.
const RETRY_FIRST: i64 = 15 * 60_000;
/// The longest wait between searches. A download is never given up on, so it settles at four searches a day.
const RETRY_CEILING: i64 = 6 * 3_600_000;

pub(crate) fn is_download(name: &str) -> bool {
    name.starts_with("download:")
}

/// A download row's values merged (each by the later stamp, as every settings row), then every value stamped
/// earlier than the `removed` tombstone dropped: a removal ends the download, and a later start writes its values
/// after it. Taking the later stamp per value and dropping below one maximum keeps the merge a join.
pub(crate) fn clean(values: &mut Map<String, Value>) -> Result<(), String> {
    let Some(removed) = values.get("removed") else {
        return Ok(());
    };
    let at = stamp(&removed["at"])?;
    let mut dropped = Vec::new();
    for (key, value) in values.iter() {
        if key != "removed" && stamp(&value["at"])? < at {
            dropped.push(key.clone());
        }
    }
    for key in dropped {
        values.remove(&key);
    }
    Ok(())
}

/// `download_merge`: two versions of one `set:download:<content>` row.
pub fn download_merge(a: &Value, b: &Value) -> Result<Value, String> {
    if !a["name"].as_str().is_some_and(is_download) {
        return Err("not_a_download_row".into());
    }
    crate::wire::merge(a, b)
}

// ---- reading a row ----------------------------------------------------------------------------------------------

struct Row<'a> {
    values: &'a Map<String, Value>,
}

impl<'a> Row<'a> {
    fn new(row: &'a Value) -> Result<Self, String> {
        if row["kind"] != json!("set") || !row["name"].as_str().is_some_and(is_download) {
            return Err("not_a_download_row".into());
        }
        let values = row["values"].as_object().ok_or("invalid_row")?;
        Ok(Self { values })
    }

    fn value(&self, key: &str) -> Option<&'a Value> {
        self.values
            .get(key)
            .map(|v| &v["value"])
            .filter(|v| !v.is_null())
    }

    fn at(&self, key: &str) -> Option<Stamp> {
        self.values.get(key).and_then(|v| stamp(&v["at"]).ok())
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.value(key)?["int"].as_i64()
    }

    fn flag(&self, key: &str) -> bool {
        self.value(key)
            .and_then(|v| v["bool"].as_bool())
            .unwrap_or(false)
    }

    fn object(&self, key: &str) -> Option<Value> {
        let text = self.value(key)?["string"].as_str()?;
        serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
    }

    fn strings(&self, key: &str) -> Vec<String> {
        self.value(key)
            .and_then(|v| v["strings"].as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn queued_at(&self) -> Option<i64> {
        self.int("queuedAt")
    }

    /// Live: queued, and naming a release or still wanted without one (`exhausted`: a search found nothing to fetch
    /// yet). A row that is only a tombstone is not.
    fn live(&self) -> bool {
        self.queued_at().is_some() && (self.object("release").is_some() || self.flag("exhausted"))
    }

    /// When the content airs, in ms, when the starter knew it: an episode announced but not yet out.
    fn air_date(&self) -> Option<i64> {
        self.object("title")?["airDate"].as_i64()
    }

    fn identity(&self) -> Option<String> {
        self.object("release")?["identity"]
            .as_str()
            .map(str::to_owned)
    }

    /// The identities the row races beside its `release` (§17 `race`).
    fn racing(&self) -> Vec<String> {
        self.object("race")
            .and_then(|race| race["entries"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["identity"].as_str().map(str::to_owned))
            .collect()
    }

    /// The stall clock as the row holds it: `{lastProgress, progressAt}`, else nothing moved since `queuedAt`. A
    /// `queuedAt` later than `progressAt` is an add made again (a re-press of the same release, a resumed add), and
    /// the clock starts from it: a download asked for again a minute ago hasn't stalled for an hour.
    fn clock(&self) -> Clock {
        let queued = self.queued_at().unwrap_or(0);
        match self.object("progress") {
            Some(p) => Clock {
                last: p["lastProgress"].as_f64().unwrap_or(0.0),
                at: p["progressAt"].as_i64().unwrap_or(queued).max(queued),
            },
            None => Clock {
                last: 0.0,
                at: queued,
            },
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Clock {
    last: f64,
    at: i64,
}

impl Clock {
    fn from(value: &Value) -> Option<Self> {
        Some(Self {
            last: value["lastProgress"].as_f64()?,
            at: value["progressAt"].as_i64()?,
        })
    }

    fn json(self) -> Value {
        json!({"lastProgress": self.last, "progressAt": self.at})
    }
}

// ---- download_status --------------------------------------------------------------------------------------------

/// `download_status`: what one poll answer means for a download (the TV's `DownloadQueue.apply`, `hasStalled`,
/// `shouldReannounce`). `answer` absent is the state a row stands in before anything was asked.
///
/// `clock` is the caller's own stall clock, which can be ahead of the row's: the row's is written coarsely.
pub fn download_status(
    row: &Value,
    answer: Option<&Value>,
    clock: Option<&Value>,
    now: i64,
) -> Result<Value, String> {
    let row_value = row;
    let row = Row::new(row)?;
    let queued = row.queued_at().ok_or("not_queued_row")?;
    let stored = row.clock();
    let mut current = match clock.and_then(Clock::from) {
        Some(given) if given.at > stored.at => given,
        _ => stored,
    };
    let unchanged = current;
    let grace = now - queued < START_GRACE;
    let mut out = Map::new();
    let mut stalled = false;
    let mut reannounce = false;
    let mut reported = false;
    let state = if row.flag("exhausted") {
        let (at, _) = retry_at(&row, name_of(row_value));
        out.insert("until".into(), json!(at));
        if row.air_date().is_some_and(|air| air > now) {
            json!("awaiting_air")
        } else {
            json!("no_working_release")
        }
    } else {
        match answer {
            None => match row.int("resumeAt") {
                Some(until) if until > now => {
                    out.insert("until".into(), json!(until));
                    json!("paused")
                }
                _ => json!("starting"),
            },
            Some(answer) => match answer["kind"].as_str().ok_or("invalid_answer")? {
                "ready" => {
                    reported = true;
                    json!("ready")
                }
                "preparing" => {
                    reported = true;
                    let fetch = &answer["fetch"];
                    let fetch_state = fetch["state"].as_str();
                    let (seeds, peers) = (fetch["seeds"].as_i64(), fetch["peers"].as_i64());
                    let swarm_empty = (seeds.is_some() || peers.is_some())
                        && seeds.unwrap_or(0) == 0
                        && peers.unwrap_or(0) == 0;
                    if fetch_state == Some("failed") {
                        stalled = true;
                    } else {
                        let dead_swarm = fetch_state == Some("stalled") && swarm_empty;
                        let progress = answer["progress"].as_f64().unwrap_or(0.0);
                        let rate = answer["bytesPerSecond"].as_f64().unwrap_or(0.0);
                        if !dead_swarm && (progress > current.last || rate > 0.0) {
                            current = Clock {
                                last: progress.max(current.last),
                                at: now,
                            };
                        } else {
                            stalled = now - current.at >= STALL_LIMIT;
                        }
                    }
                    reannounce = !stalled
                        && swarm_empty
                        && !row.flag("reannounced")
                        && now - current.at >= STALL_LIMIT / 2;
                    json!("fetching")
                }
                "dead" | "not_queued" => {
                    stalled = answer["kind"] == json!("dead") && now - current.at >= STALL_LIMIT;
                    json!(if grace { "starting" } else { "not_started" })
                }
                "service_unavailable" => {
                    if let Some(service) = answer["service"].as_str() {
                        out.insert("service".into(), json!(service));
                    }
                    json!("refused")
                }
                "reserved_for_play" => {
                    let until = answer["until"].as_i64().ok_or("invalid_answer")?;
                    out.insert("until".into(), json!(until));
                    json!("paused")
                }
                "unknown" => json!(if grace { "starting" } else { "unreachable" }),
                // Says nothing about the fetch: the caller resolves the title again for a fresh ticket.
                "ticket_expired" => {
                    out.insert("renew".into(), json!(true));
                    Value::Null
                }
                _ => return Err("invalid_answer".into()),
            },
        }
    };
    let moved = current != unchanged;
    let last_write = row.at("progress").map(|at| at.0);
    out.insert("state".into(), state);
    out.insert("clock".into(), current.json());
    out.insert("stalled".into(), json!(stalled));
    out.insert("reannounce".into(), json!(reannounce));
    out.insert(
        "write_progress".into(),
        json!(moved && last_write.is_none_or(|at| now - at >= PROGRESS_WRITE_EVERY)),
    );
    out.insert("report".into(), json!(reported && !row.flag("reported")));
    out.insert(
        "announce".into(),
        json!(out["state"] == json!("ready") && !row.flag("announced")),
    );
    Ok(Value::Object(out))
}

// ---- download_next ----------------------------------------------------------------------------------------------

/// `download_next`: the release a stalled download moves on to, from a fresh resolve (the TV's
/// `DownloadQueue.fallback(for:in:)`). The release the row names counts as tried: the caller is giving up on it.
///
/// `resolution` is `streams` (with `complete` false when a source didn't answer), `none` (nothing to offer at all) or
/// `undecided` (unreachable, or not an answer that settles anything).
pub fn download_next(
    row: &Value,
    releases: &[Value],
    resolution: &str,
    complete: bool,
) -> Result<Value, String> {
    let row = Row::new(row)?;
    let mut tried = row.strings("tried");
    if let Some(current) = row.identity() {
        if !tried.contains(&current) {
            tried.push(current);
        }
    }
    let title = row.object("title").unwrap_or(Value::Null);
    let (original, preferred) = (
        title["originalLanguage"].as_str(),
        title["preferredLanguage"].as_str(),
    );
    let decision = match resolution {
        "streams" => {
            let facts: Vec<Release> = releases.iter().map(Release::from).collect();
            let candidates = facts.iter().filter(|r| !r.dead_swarm()).count();
            let live: Vec<usize> = (0..facts.len())
                .filter(|&i| !facts[i].dead_swarm())
                .collect();
            match pick(&facts, &live, original, preferred, &tried) {
                Some(index) => {
                    json!({"decision": "next", "index": index, "candidates": candidates})
                }
                None if complete => json!({"decision": "exhausted", "candidates": candidates}),
                None => json!({"decision": "undecided", "candidates": candidates}),
            }
        }
        "none" => json!({"decision": "exhausted"}),
        "undecided" => json!({"decision": "undecided"}),
        _ => return Err("invalid_resolution".into()),
    };
    let mut out = decision;
    out["tried"] = json!(tried);
    Ok(out)
}

// ---- download_retry ---------------------------------------------------------------------------------------------

fn name_of(row: &Value) -> &str {
    row["name"].as_str().unwrap_or_default()
}

/// FNV-1a, 64-bit: a stable spread for the jitter, the same on every client.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// When an exhausted row searches again, and which search that is (`retries`, 0 for the first). The wait is 15
/// minutes doubled per search, capped at 6 hours, then spread to 80–120% by the row's name and the search number
/// so a season that ran out together doesn't search together. It counts from the stamp `exhausted` was last
/// written with (the row's `queuedAt` without one), and never falls before the content's `airDate`.
fn retry_at(row: &Row, name: &str) -> (i64, i64) {
    let attempt = row.int("retries").unwrap_or(0).max(0);
    let base = row
        .at("exhausted")
        .map(|at| at.0)
        .or(row.queued_at())
        .unwrap_or(0);
    let wait = RETRY_FIRST
        .saturating_mul(1i64 << attempt.min(20))
        .min(RETRY_CEILING);
    let spread = 80 + (fnv1a(&format!("{name}:{attempt}")) % 41) as i64;
    let at = base.saturating_add(wait / 100 * spread);
    (row.air_date().map_or(at, |air| at.max(air)), attempt)
}

/// `download_retry`: whether an exhausted download (no release left, or wanted with none found yet) is due to search
/// again, when, and which search it is. A download is never given up on: the holder resolves the content again
/// when `due`, and writes `retries` as `attempt + 1` with whatever that search decides, so a search that finds
/// nothing waits longer next time. A start (a fresh `removed` and every value after it) begins again at 0.
pub fn download_retry(row: &Value, now: i64) -> Result<Value, String> {
    let name = name_of(row);
    let row = Row::new(row)?;
    if !row.flag("exhausted") {
        return Err("not_exhausted".into());
    }
    let (at, attempt) = retry_at(&row, name);
    Ok(json!({"due": now >= at, "at": at, "attempt": attempt}))
}

// ---- download_cancel_safe ---------------------------------------------------------------------------------------

/// `download_cancel_safe`: whether the release a row names may be cancelled at the debrid. Not while any other live,
/// non-exhausted row names the same `release.identity`: one season pack is one torrent across every episode, and a
/// sibling that reads "not started", "unreachable" or "ready" may still be fetching or playing it. Only a client sees
/// the whole queue, so this is the one check a client makes before den-scout's own (§17 *Cancelling*).
///
/// `identity` names one of the row's race releases instead of its `release`; a sibling's race releases protect an
/// identity as its `release` does.
pub fn download_cancel_safe(
    row: &Value,
    rows: &[Value],
    identity: Option<&str>,
) -> Result<Value, String> {
    let name = row["name"].as_str().unwrap_or_default();
    let own = Row::new(row)?;
    let Some(identity) = identity.map(str::to_owned).or_else(|| own.identity()) else {
        return Ok(json!(true));
    };
    for other in rows {
        if other["name"].as_str() == Some(name) {
            continue;
        }
        let sibling = Row::new(other)?;
        if sibling.live()
            && !sibling.flag("exhausted")
            && (sibling.identity().as_ref() == Some(&identity)
                || sibling.racing().contains(&identity))
        {
            return Ok(json!(false));
        }
    }
    Ok(json!(true))
}

// ---- download_prune ---------------------------------------------------------------------------------------------

/// `download_prune`: the live rows to tombstone now. Pending and exhausted requests do not expire: they are a
/// user's request, not a cache entry, and remain available for later source retries. A ready row also remains while
/// unwatched. Once its own episode or film is watched it stays for two more days from the ready announcement, so
/// Downloads still confirms what landed instead of deleting it at the moment it succeeds. The caller may always
/// remove a row by hand (not this op's job). Past the cap of 100, the oldest live rows go.
pub fn download_prune(
    rows: &[Value],
    states: &Map<String, Value>,
    watched: &Map<String, Value>,
    now: i64,
) -> Result<Value, String> {
    let mut live = Vec::new();
    let mut remove = Vec::new();
    for value in rows {
        let row = Row::new(value)?;
        if !row.live() {
            continue;
        }
        let name = value["name"].as_str().unwrap_or_default().to_owned();
        let queued = row.queued_at().unwrap_or(0);
        let state = states.get(&name).and_then(Value::as_str);
        if row.flag("announced") || state == Some("ready") {
            let ready_at = row.at("announced").map(|at| at.0).unwrap_or(queued);
            if watched.get(&name).and_then(Value::as_bool).unwrap_or(false)
                && now - ready_at > WATCHED_READY_TTL
            {
                remove.push(name);
            } else {
                live.push((queued, name));
            }
            continue;
        }
        live.push((queued, name));
    }
    if live.len() > LIVE_CAP {
        live.sort();
        let over = live.len() - LIVE_CAP;
        remove.extend(live.into_iter().take(over).map(|(_, name)| name));
    }
    remove.sort();
    Ok(json!({ "remove": remove }))
}

// ---- rank_releases ----------------------------------------------------------------------------------------------

/// One release as den-scout describes it in a stream's `attributes`, with the release's `identity`
/// (`DeadStreamStore.identity`: the info-hash, else the file name).
struct Release {
    identity: String,
    cached: Option<bool>,
    seeders: Option<i64>,
    size: Option<i64>,
    resolution: i64,
    dolby_vision: bool,
    hdr: bool,
    software: bool,
    /// The release file is an AVI, or names XviD or DivX: MPEG-4 Part 2 in practice, whatever `codec` says.
    legacy_file: bool,
    three_d: bool,
    probed: bool,
    languages: Vec<String>,
    untagged: i64,
}

impl Release {
    fn from(value: &Value) -> Self {
        let dolby_vision = value["dolbyVision"].as_bool().unwrap_or(false);
        Self {
            identity: value["identity"].as_str().unwrap_or_default().to_owned(),
            cached: value["cached"].as_bool(),
            // Negative or absurd counts are the addon talking nonsense, and read as no count.
            seeders: value["seeders"]
                .as_i64()
                .filter(|n| (0..=1_000_000).contains(n)),
            size: value["sizeBytes"].as_i64(),
            resolution: match value["resolution"].as_str() {
                Some("2160p") => 4,
                Some("1080p") => 3,
                Some("720p") => 2,
                Some("480p") => 1,
                _ => 0,
            },
            dolby_vision,
            hdr: value["hdr"].as_bool().unwrap_or(false) || dolby_vision,
            software: value["codec"].as_str().is_some_and(software_decode),
            legacy_file: [&value["filename"], &value["identity"]]
                .into_iter()
                .filter_map(Value::as_str)
                .any(legacy_file),
            three_d: value["threeD"].as_bool().unwrap_or(false),
            probed: value["probed"].as_bool().unwrap_or(false),
            languages: value["audioLanguages"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(canonical)
                        .collect()
                })
                .unwrap_or_default(),
            untagged: value["untaggedAudioTracks"].as_i64().unwrap_or(0),
        }
    }

    /// The debrid doesn't hold it and the indexer counted nobody seeding it: an add would fetch nothing.
    fn dead_swarm(&self) -> bool {
        self.cached == Some(false) && self.seeders == Some(0)
    }

    /// A proven dub: probed audio, every track tagged, none of it in the viewer's or the title's own language.
    fn dub(&self, original: Option<&str>, preferred: Option<&str>) -> bool {
        if !self.probed || self.untagged > 0 || self.languages.is_empty() {
            return false;
        }
        let wanted: Vec<String> = [preferred, original]
            .into_iter()
            .flatten()
            .map(canonical)
            .collect();
        !wanted.is_empty() && !self.languages.iter().any(|l| wanted.contains(l))
    }

    /// Smaller than `threshold`, a quarter of its siblings' median size.
    fn runt(&self, threshold: Option<i64>) -> bool {
        matches!((threshold, self.size), (Some(t), Some(s)) if s > 0 && s < t)
    }

    /// The auto-pick tier, lowest first: a bitfield of demotions, each outweighing everything beneath it.
    fn tier(&self, runt: Option<i64>, original: Option<&str>, preferred: Option<&str>) -> i64 {
        (if self.dead_swarm() { 32 } else { 0 })
            + (if self.cached == Some(false) { 16 } else { 0 })
            + (if self.dub(original, preferred) { 8 } else { 0 })
            + (if self.runt(runt) { 4 } else { 0 })
            + (if self.software { 2 } else { 0 })
            + (if self.three_d { 1 } else { 0 })
    }

    /// Decoded in software on the Apple TV, or an AVI/XviD/DivX file: watchable only as a last resort.
    fn legacy(&self) -> bool {
        self.software || self.legacy_file
    }

    /// Worth keeping (§17 *Choosing a download*): 720p or better, decoded in hardware, not 3D, not a proven dub, not a
    /// runt.
    fn good(&self, runt: Option<i64>, original: Option<&str>, preferred: Option<&str>) -> bool {
        self.resolution >= 2
            && !self.legacy()
            && !self.three_d
            && !self.dub(original, preferred)
            && !self.runt(runt)
    }

    /// The debrid holds it: it plays the moment it is added.
    fn ready(&self) -> bool {
        self.cached == Some(true)
    }

    /// How long an uncached release should take to arrive before it starts, in seconds: its size (or a typical size
    /// for its resolution) at `PER_SEEDER_RATE` per seeder, up to `MAX_FETCH_RATE`. An unreported count reads as one
    /// seeder; a count of 0 never arrives.
    fn eta(&self) -> i64 {
        if self.ready() {
            return 0;
        }
        let size = self
            .size
            .filter(|&s| s > 0)
            .unwrap_or(match self.resolution {
                4 => 16_000_000_000,
                3 => 4_000_000_000,
                2 => 1_500_000_000,
                _ => 700_000_000,
            });
        let rate = (self.seeders.unwrap_or(1) * PER_SEEDER_RATE).min(MAX_FETCH_RATE);
        if rate <= 0 {
            i64::MAX
        } else {
            size / rate
        }
    }

    /// Picture quality alone, for choosing within one tier.
    fn picture(&self) -> i64 {
        self.resolution * 4
            + (if self.dolby_vision { 2 } else { 0 })
            + (if self.hdr { 1 } else { 0 })
    }
}

/// A codec this Apple TV can only decode in software (no VideoToolbox decoder), by any of its tags.
fn software_decode(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "av1"
            | "vp9"
            | "mpeg4"
            | "mpeg-4"
            | "divx"
            | "xvid"
            | "msmpeg4"
            | "mpeg2"
            | "mpeg-2"
            | "mpeg2video"
            | "h262"
            | "vc1"
            | "vc-1"
            | "wvc1"
    )
}

/// A release file name that is an AVI, or names XviD or DivX as a word. den-scout reads no codec from such a name,
/// and an AVI is MPEG-4 Part 2 in practice, so the name is the only evidence a download pick has.
fn legacy_file(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".avi")
        || name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == "xvid" || word == "divx")
}

const COMMON: [(&str, &str); 24] = [
    ("en", "english"),
    ("es", "spanish"),
    ("fr", "french"),
    ("de", "german"),
    ("it", "italian"),
    ("pt", "portuguese"),
    ("ru", "russian"),
    ("ja", "japanese"),
    ("ko", "korean"),
    ("zh", "chinese"),
    ("hi", "hindi"),
    ("ta", "tamil"),
    ("te", "telugu"),
    ("ml", "malayalam"),
    ("ar", "arabic"),
    ("tr", "turkish"),
    ("th", "thai"),
    ("sv", "swedish"),
    ("da", "danish"),
    ("nb", "norwegian"),
    ("fi", "finnish"),
    ("nl", "dutch"),
    ("pl", "polish"),
    ("id", "indonesian"),
];

/// A canonical two-letter key for a language, across ISO 639-1 and both 639-2 forms (the TV's
/// `LanguageCatalog.canonical`): `swe`, `sv` and `Swedish` all read `sv`. A code with no two-letter equivalent here
/// comes back as its lowercased primary subtag.
fn canonical(code: &str) -> String {
    let lowered = code.to_lowercase();
    let base: String = lowered.chars().take_while(|c| c.is_alphabetic()).collect();
    if COMMON.iter().any(|(two, _)| *two == base) {
        return base;
    }
    let alias = match base.as_str() {
        // OpenSubtitles' own legacy codes.
        "pob" | "pb" => Some("pt"),
        "scc" => Some("sr"),
        "scr" => Some("hr"),
        // ISO 639-2, bibliographic and terminologic, and the macrolanguage code for Norwegian.
        "eng" => Some("en"),
        "spa" => Some("es"),
        "fra" | "fre" => Some("fr"),
        "deu" | "ger" => Some("de"),
        "ita" => Some("it"),
        "por" => Some("pt"),
        "rus" => Some("ru"),
        "jpn" => Some("ja"),
        "kor" => Some("ko"),
        "zho" | "chi" => Some("zh"),
        "hin" => Some("hi"),
        "tam" => Some("ta"),
        "tel" => Some("te"),
        "mal" => Some("ml"),
        "ara" => Some("ar"),
        "tur" => Some("tr"),
        "tha" => Some("th"),
        "swe" => Some("sv"),
        "dan" => Some("da"),
        "nor" | "nob" | "no" => Some("nb"),
        "fin" => Some("fi"),
        "nld" | "dut" => Some("nl"),
        "pol" => Some("pl"),
        "ind" => Some("id"),
        _ => None,
    };
    if let Some(alias) = alias {
        return alias.into();
    }
    if let Some((two, _)) = COMMON.iter().find(|(_, name)| *name == lowered) {
        return (*two).into();
    }
    base
}

/// The highest-quality release among `indices` (the TV's `QualityBadge.bestStream`): the lowest tier, then the best
/// picture, the first of equals. The runt threshold is a quarter of the median size among `indices`, when at least
/// three report one.
fn best(
    facts: &[Release],
    indices: &[usize],
    original: Option<&str>,
    preferred: Option<&str>,
) -> Option<usize> {
    let runt = runt_threshold(facts, indices);
    let tier = |i: usize| facts[i].tier(runt, original, preferred);
    let lowest = indices.iter().map(|&i| tier(i)).min()?;
    let mut chosen: Option<usize> = None;
    for &i in indices.iter().filter(|&&i| tier(i) == lowest) {
        if chosen.is_none_or(|c| facts[i].picture() > facts[c].picture()) {
            chosen = Some(i);
        }
    }
    chosen
}

/// A quarter of the median size among `indices`, when at least three report one.
fn runt_threshold(facts: &[Release], indices: &[usize]) -> Option<i64> {
    let mut sizes: Vec<i64> = indices
        .iter()
        .filter_map(|&i| facts[i].size)
        .filter(|&s| s > 0)
        .collect();
    sizes.sort_unstable();
    (sizes.len() >= 3).then(|| sizes[sizes.len() / 2] / 4)
}

/// The releases a download chooses among, judged together: `indices` less `tried`, with the runt threshold over them.
struct Choice<'a> {
    facts: &'a [Release],
    open: Vec<usize>,
    runt: Option<i64>,
    original: Option<&'a str>,
    preferred: Option<&'a str>,
}

impl<'a> Choice<'a> {
    fn new(
        facts: &'a [Release],
        indices: &[usize],
        original: Option<&'a str>,
        preferred: Option<&'a str>,
        tried: &[String],
    ) -> Self {
        let open: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| !tried.contains(&facts[i].identity))
            .collect();
        let runt = runt_threshold(facts, &open);
        Self {
            facts,
            open,
            runt,
            original,
            preferred,
        }
    }

    fn good(&self, i: usize) -> bool {
        self.facts[i].good(self.runt, self.original, self.preferred)
    }

    /// The best picture of `among`, the first of equals.
    fn sharpest(&self, among: impl Iterator<Item = usize>) -> Option<usize> {
        let mut chosen: Option<usize> = None;
        for i in among {
            if chosen.is_none_or(|c| self.facts[i].picture() > self.facts[c].picture()) {
                chosen = Some(i);
            }
        }
        chosen
    }

    /// A good release the debrid already holds: the best of them.
    fn cached_good(&self) -> Option<usize> {
        self.sharpest(
            self.open
                .iter()
                .copied()
                .filter(|&i| self.facts[i].ready() && self.good(i)),
        )
    }

    /// The releases a download races, best first: live and uncached, only the good ones while any is good. Those
    /// expected within `FINISH_SOON` come first, the best picture among them; the rest by how soon they should arrive.
    fn race(&self, except: &[String]) -> Vec<usize> {
        let pool: Vec<usize> = self
            .open
            .iter()
            .copied()
            .filter(|&i| {
                let r = &self.facts[i];
                !r.ready() && !r.dead_swarm() && !except.contains(&r.identity)
            })
            .collect();
        let any_good = pool.iter().any(|&i| self.good(i));
        let mut pool: Vec<usize> = pool
            .into_iter()
            .filter(|&i| !any_good || self.good(i))
            .collect();
        pool.sort_by_key(|&i| {
            let r = &self.facts[i];
            let eta = r.eta();
            let soon = eta <= FINISH_SOON;
            (!soon, if soon { -r.picture() } else { 0 }, eta)
        });
        pool
    }

    /// A release the debrid already holds that isn't good: hardware-decoded before software or AVI, then the best
    /// picture. Taken only when nothing else can be waited for.
    fn cached_fallback(&self) -> Option<usize> {
        let cached: Vec<usize> = self
            .open
            .iter()
            .copied()
            .filter(|&i| self.facts[i].ready())
            .collect();
        let modern: Vec<usize> = cached
            .iter()
            .copied()
            .filter(|&i| !self.facts[i].legacy())
            .collect();
        self.sharpest(if modern.is_empty() { cached } else { modern }.into_iter())
    }
}

/// The release a download starts with (§17 *Choosing a download*): `tried` left out, a good cached release (the best
/// of them), else the first of the race, else a cached release that isn't good, else the best picture left. Play's
/// `order` and `best` never read this.
fn pick(
    facts: &[Release],
    indices: &[usize],
    original: Option<&str>,
    preferred: Option<&str>,
    tried: &[String],
) -> Option<usize> {
    let choice = Choice::new(facts, indices, original, preferred, tried);
    choice
        .cached_good()
        .or_else(|| choice.race(&[]).first().copied())
        .or_else(|| choice.cached_fallback())
        .or_else(|| choice.sharpest(choice.open.iter().copied()))
}

// ---- download_escalate ------------------------------------------------------------------------------------------

/// One release being fetched for a download, with the debrid's latest word on it.
struct Flight {
    identity: String,
    finished: bool,
    failed: bool,
    behind: bool,
}

impl Flight {
    fn from(value: &Value, facts: &[Release], now: i64) -> Result<Self, String> {
        let identity = value["identity"]
            .as_str()
            .ok_or("invalid_in_flight")?
            .to_owned();
        let since = value["since"].as_i64().ok_or("invalid_in_flight")?;
        let answer = &value["answer"];
        let kind = answer["kind"].as_str().unwrap_or("unknown");
        let fetch = answer["fetch"]["state"].as_str();
        let progress = answer["progress"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
        let rate = answer["bytesPerSecond"].as_f64().unwrap_or(0.0);
        let size = facts
            .iter()
            .find(|r| r.identity == identity)
            .and_then(|r| r.size)
            .filter(|&s| s > 0);
        let projected = answer["etaSeconds"].as_f64().or_else(|| {
            (rate > 0.0)
                .then_some(size)
                .flatten()
                .map(|s| (1.0 - progress) * s as f64 / rate)
        });
        let age = now - since;
        let preparing = kind == "preparing";
        let slow =
            preparing && age >= JUDGE_AFTER && projected.is_some_and(|p| p > SLOW_ETA as f64);
        let still = preparing && age >= STALL_LIMIT && progress == 0.0 && rate <= 0.0;
        Ok(Self {
            identity,
            finished: kind == "ready",
            failed: kind == "dead" || fetch == Some("failed"),
            behind: value["stalled"].as_bool().unwrap_or(false) || slow || still,
        })
    }
}

/// The race's state, as the row's `race` value (or a caller without rows) holds it.
pub struct RaceState {
    pub stage: i64,
    pub stage_at: i64,
    pub started_at: i64,
    pub capped_at: Option<i64>,
}

/// `download_escalate`: one pass of a download's race (§17 *Racing*), from the releases listed now, the ones in flight
/// with the debrid's latest answer for each, and the race's state. Pure and row-free, so den-hearth calls it too.
///
/// In order: a finished release wins (the best picture of those finished at once), and the rest are cancelled. A good
/// release the debrid now holds wins the same way. With nothing watchable `FALLBACK_AFTER` after the download was asked
/// for, or nothing left to race at all, a cached release that isn't good is taken. Otherwise failed releases are
/// dropped (and count as tried), the race escalates — from one release to `1 + RACE_WIDEN` once everything in flight is
/// behind (stalled, or projected past `SLOW_ETA`), to every live release `RACE_ALL_AFTER` later or once nothing is
/// alive — and the best releases not yet in flight are added up to the stage's width, unless the debrid refused an add
/// in the last `CAP_RETRY`.
#[allow(clippy::too_many_arguments)]
pub fn download_escalate(
    releases: &[Value],
    original: Option<&str>,
    preferred: Option<&str>,
    tried: &[String],
    in_flight: &[Value],
    state: &RaceState,
    now: i64,
) -> Result<Value, String> {
    let facts: Vec<Release> = releases.iter().map(Release::from).collect();
    let all: Vec<usize> = (0..facts.len()).collect();
    let choice = Choice::new(&facts, &all, original, preferred, tried);
    let flights = in_flight
        .iter()
        .map(|v| Flight::from(v, &facts, now))
        .collect::<Result<Vec<_>, _>>()?;
    let flying: Vec<String> = flights.iter().map(|f| f.identity.clone()).collect();
    let index_of = |identity: &str| facts.iter().position(|r| r.identity == identity);
    let mut out = json!({
        "stage": state.stage,
        "stage_at": state.stage_at,
        "add": [],
        "cancel": [],
        "capped": false,
    });
    let take = |out: &mut Value, identity: &str, reason: &str| {
        out["decision"] = json!("take");
        out["take"] = json!(identity);
        out["reason"] = json!(reason);
        out["cancel"] = json!(flying
            .iter()
            .filter(|f| f.as_str() != identity)
            .collect::<Vec<_>>());
    };

    let finished: Vec<&Flight> = flights.iter().filter(|f| f.finished).collect();
    if let Some(winner) = finished.iter().min_by_key(|f| match index_of(&f.identity) {
        Some(i) => (!choice.good(i), -facts[i].picture()),
        None => (true, 0),
    }) {
        take(&mut out, &winner.identity, "finished");
        return Ok(out);
    }
    if let Some(i) = choice.cached_good() {
        take(&mut out, &facts[i].identity, "cached");
        return Ok(out);
    }

    let live: Vec<&Flight> = flights.iter().filter(|f| !f.failed).collect();
    let failed: Vec<String> = flights
        .iter()
        .filter(|f| f.failed)
        .map(|f| f.identity.clone())
        .collect();
    let pool = choice.race(&flying);
    let nothing_left = live.is_empty() && pool.is_empty();
    if now - state.started_at >= FALLBACK_AFTER || nothing_left {
        if let Some(i) = choice.cached_fallback() {
            take(&mut out, &facts[i].identity, "fallback");
            return Ok(out);
        }
    }

    let mut stage = state.stage;
    let mut stage_at = state.stage_at;
    if stage == 0 && !live.is_empty() && live.iter().all(|f| f.behind) {
        stage = 1;
        stage_at = now;
    } else if stage == 1 && (live.is_empty() || now - stage_at >= RACE_ALL_AFTER) {
        stage = 2;
        stage_at = now;
    }
    let width = match stage {
        0 => 1,
        1 => 1 + RACE_WIDEN,
        _ => usize::MAX,
    };
    let capped = state.capped_at.is_some_and(|at| now - at < CAP_RETRY);
    let add: Vec<&str> = if capped {
        Vec::new()
    } else {
        pool.iter()
            .take(width.saturating_sub(live.len()))
            .map(|&i| facts[i].identity.as_str())
            .collect()
    };
    out["decision"] = json!(if !add.is_empty() {
        "add"
    } else if live.is_empty() && !capped {
        "exhausted"
    } else {
        "wait"
    });
    out["stage"] = json!(stage);
    out["stage_at"] = json!(stage_at);
    out["add"] = json!(add);
    out["cancel"] = json!(failed);
    out["capped"] = json!(capped);
    Ok(out)
}

/// `rank_releases`: the play order (the tiers, the addon's order within a tier), the release the quality badge
/// names (`best`), and the one a download starts with (`pick`), as indices into `releases`.
pub fn rank_releases(
    releases: &[Value],
    original: Option<&str>,
    preferred: Option<&str>,
    tried: &[String],
) -> Value {
    let facts: Vec<Release> = releases.iter().map(Release::from).collect();
    let all: Vec<usize> = (0..facts.len()).collect();
    let runt = runt_threshold(&facts, &all);
    let mut order = all.clone();
    order.sort_by_key(|&i| facts[i].tier(runt, original, preferred));
    json!({
        "order": order,
        "best": best(&facts, &all, original, preferred),
        "pick": pick(&facts, &all, original, preferred, tried),
    })
}

#[cfg(test)]
mod tests {
    use super::{canonical, rank_releases};
    use serde_json::json;

    #[test]
    fn languages_match_across_code_forms() {
        for code in ["sv", "swe", "SV", "Swedish", "sv-SE"] {
            assert_eq!(canonical(code), "sv", "{code}");
        }
        assert_eq!(canonical("pob"), "pt");
        assert_eq!(canonical("no"), "nb");
        assert_eq!(canonical("hrv"), "hrv");
    }

    /// The runt threshold is a quarter of the MEDIAN size among the candidates — so one release whose
    /// `sizeBytes` is actually a season pack's total (den-scout, before it probes the resolved file, may
    /// still send that) would drag every genuine episode beside it below the bar and demote them all as
    /// "samples". den-scout corrects `sizeBytes` to the file's own size once it has probed it and carries
    /// the pack's original number separately as `packSizeBytes` for exactly this reason — so this asserts
    /// `rank_releases` never reads that field, and three ordinarily-sized episodes rank normally even with
    /// one sitting right beside them.
    #[test]
    fn rank_releases_ignores_pack_size_bytes() {
        let episode = |id: &str, size: i64, pack: i64| {
            json!({
                "identity": id,
                "cached": true,
                "seeders": 10,
                "sizeBytes": size,
                "packSizeBytes": pack,
                "resolution": "1080p",
            })
        };
        let releases = vec![
            episode("a", 1_136_580_921, 68_000_000_000),
            episode("b", 1_200_000_000, 70_000_000_000),
            episode("c", 1_100_000_000, 69_000_000_000),
        ];
        let out = rank_releases(&releases, None, None, &[]);
        // None of the three is a "runt" relative to the others: every tier is the same (0), so `order`
        // keeps the input order and `best`/`pick` are free to pick on picture quality alone (the first,
        // since all three tie there) — not pushed around by a pack size that never entered the ranking.
        assert_eq!(out["order"], json!([0, 1, 2]));
        assert_eq!(out["best"], json!(0));
        assert_eq!(out["pick"], json!(0));
    }
}
