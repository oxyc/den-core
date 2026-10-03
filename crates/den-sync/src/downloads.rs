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
const READY_TTL: i64 = 2 * 86_400_000;
const PENDING_TTL: i64 = 7 * 86_400_000;
const NEVER_STARTED_TTL: i64 = 15 * 60_000;
/// Live rows kept at most; the oldest past it are pruned.
const LIVE_CAP: usize = 100;

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

    /// Live: queued and naming a release. A row that is only a tombstone is not.
    fn live(&self) -> bool {
        self.queued_at().is_some() && self.object("release").is_some()
    }

    fn identity(&self) -> Option<String> {
        self.object("release")?["identity"]
            .as_str()
            .map(str::to_owned)
    }

    /// The stall clock as the row holds it: `{lastProgress, progressAt}`, else nothing moved since `queuedAt`.
    fn clock(&self) -> Clock {
        let queued = self.queued_at().unwrap_or(0);
        match self.object("progress") {
            Some(p) => Clock {
                last: p["lastProgress"].as_f64().unwrap_or(0.0),
                at: p["progressAt"].as_i64().unwrap_or(queued),
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
        json!("no_working_release")
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

// ---- download_prune ---------------------------------------------------------------------------------------------

/// `download_prune`: the live rows to tombstone now. Three lifetimes from `queuedAt` — a finished download two days,
/// one den-scout never once described (`reported`) and that last read as never started fifteen minutes, anything
/// else seven days — then the oldest past the cap of 100. `states` is the caller's last state per row name.
pub fn download_prune(
    rows: &[Value],
    states: &Map<String, Value>,
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
        let ttl = if row.flag("announced") || state == Some("ready") {
            READY_TTL
        } else if matches!(state, Some("not_started" | "refused")) && !row.flag("reported") {
            NEVER_STARTED_TTL
        } else {
            PENDING_TTL
        };
        if now - queued > ttl {
            remove.push(name);
        } else {
            live.push((queued, name));
        }
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

    /// The auto-pick tier, lowest first: a bitfield of demotions, each outweighing everything beneath it.
    fn tier(&self, runt: Option<i64>, original: Option<&str>, preferred: Option<&str>) -> i64 {
        let is_runt = matches!((runt, self.size), (Some(t), Some(s)) if s > 0 && s < t);
        (if self.dead_swarm() { 32 } else { 0 })
            + (if self.cached == Some(false) { 16 } else { 0 })
            + (if self.dub(original, preferred) { 8 } else { 0 })
            + (if is_runt { 4 } else { 0 })
            + (if self.software { 2 } else { 0 })
            + (if self.three_d { 1 } else { 0 })
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
    let mut sizes: Vec<i64> = indices
        .iter()
        .filter_map(|&i| facts[i].size)
        .filter(|&s| s > 0)
        .collect();
    sizes.sort_unstable();
    let runt = (sizes.len() >= 3).then(|| sizes[sizes.len() / 2] / 4);
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

/// The release a download starts with (the TV's `DownloadQueue.pick`): `tried` left out, a cached release over any
/// other, then `best` within whichever group is used.
fn pick(
    facts: &[Release],
    indices: &[usize],
    original: Option<&str>,
    preferred: Option<&str>,
    tried: &[String],
) -> Option<usize> {
    let open: Vec<usize> = indices
        .iter()
        .copied()
        .filter(|&i| !tried.contains(&facts[i].identity))
        .collect();
    let cached: Vec<usize> = open
        .iter()
        .copied()
        .filter(|&i| facts[i].cached == Some(true))
        .collect();
    best(
        facts,
        if cached.is_empty() { &open } else { &cached },
        original,
        preferred,
    )
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
    let mut sizes: Vec<i64> = facts
        .iter()
        .filter_map(|r| r.size)
        .filter(|&s| s > 0)
        .collect();
    sizes.sort_unstable();
    let runt = (sizes.len() >= 3).then(|| sizes[sizes.len() / 2] / 4);
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
    use super::canonical;

    #[test]
    fn languages_match_across_code_forms() {
        for code in ["sv", "swe", "SV", "Swedish", "sv-SE"] {
            assert_eq!(canonical(code), "sv", "{code}");
        }
        assert_eq!(canonical("pob"), "pt");
        assert_eq!(canonical("no"), "nb");
        assert_eq!(canonical("hrv"), "hrv");
    }
}
