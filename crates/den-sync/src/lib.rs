//! Pure library state and delivery decisions. No clocks, storage, network, credentials, or async runtime.
//! All time and provider facts arrive as inputs; bindings return the same versioned JSON envelope.

mod delivery;
mod episodes;
mod events;
mod library_v3;
mod library_v4;
mod series;
mod tilt;
mod wire;

use serde::Deserialize;
use serde_json::{json, Value};

// `v3_form`, `v3_compact`, `write_back`, `v4_form` and `v4_dry_run` receive a whole library. Keep the boundary bounded, but
// large enough for den-edge's 32 MiB stored-library limit plus JSON field names and request framing.
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

pub use delivery::{decide, Action, Command, Decision, Kind, Remote, RemoteRating, RemoteTime};
pub use episodes::episode_mark;
pub use events::commands;
pub use library_v3::{
    episode_state, film_state, import_write, lease, pending_targets, register_write, settle,
    switch_ready, v2_reading, v3_compact, v3_form, v3_form_with_context, write_back,
};
pub use series::{
    aired_episodes, continue_entry, continue_target, episode_after, is_aired, series_state,
    ContinueInput, ContinueMark, Coord, LastPlayed, SeasonCount,
};
pub use tilt::{boost, order, Era, Signals, Weights};
pub use wire::{capture, merge, Stamp};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Name {
        row: Value,
    },
    Newest {
        row: Value,
    },
    Merge {
        a: Value,
        b: Value,
    },
    Capture {
        before: Value,
        after: Value,
        at: Stamp,
        id: String,
    },
    Commands {
        event: Value,
        current: Value,
    },
    EpisodeMark {
        row: Value,
        mark: Value,
        /// Absent means "reconstructed", which is the conservative reading.
        #[serde(default)]
        authoritative: bool,
    },
    AiredEpisodes {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
    },
    SeriesState {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
        watched: Vec<Coord>,
    },
    EpisodeAfter {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
        at: Coord,
    },
    ContinueTarget {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
        last_played: Option<LastPlayed>,
    },
    ContinueEntry {
        #[serde(flatten)]
        input: ContinueInput,
    },
    Issue {
        last: Stamp,
        seen: Option<Stamp>,
        now: i64,
        device: String,
    },
    Decide {
        command: Command,
        remote: Remote,
    },
    // The ops v3 and v4 both define take a name per version: the v3 name keeps exactly v3's shape, and the v4 shape
    // is `<name>_v4`. A shape chosen by which fields are present would turn a v4 request whose optional field a
    // client's encoder omitted into a v3 one.
    EpisodeState {
        register: Value,
        #[serde(default)]
        resets: Vec<Stamp>,
        now: i64,
    },
    /// Library v4 §7: an episode's state from its series title and season documents.
    EpisodeStateV4 {
        #[serde(default)]
        title: Option<Value>,
        #[serde(default)]
        season: Option<Value>,
        episode: Value,
        now: i64,
    },
    FilmState {
        rec: Value,
        register: Value,
        #[serde(default)]
        resets: Vec<Stamp>,
        now: i64,
    },
    /// Library v4 §7: a film's state from its title document.
    FilmStateV4 {
        title: Value,
        now: i64,
    },
    RegisterWrite {
        action: Value,
        current: Option<Value>,
        #[serde(default)]
        resets: Vec<Stamp>,
        now: i64,
    },
    ImportWrite {
        register: Option<Value>,
        item: Value,
        #[serde(default)]
        resets: Vec<Stamp>,
        now: i64,
    },
    PendingTargets {
        targets: Vec<Value>,
        receipts: Value,
        since: Stamp,
        now: i64,
    },
    /// Library v4 §9: the documents, delivery documents included, and the account's `set:deliver` facts.
    PendingTargetsV4 {
        documents: Vec<Value>,
        deliver: Value,
        now: i64,
    },
    Settle {
        outcome: Value,
        built_from: Value,
        order: Value,
    },
    /// Library v4 §9: also the entry it replaces (absent or null for none), whose lasting elements it keeps.
    SettleV4 {
        outcome: Value,
        built_from: Value,
        order: Value,
        #[serde(default)]
        entry: Option<Value>,
    },
    Lease {
        input: Value,
    },
    V2Reading {
        rows: Vec<Value>,
        now: i64,
    },
    V3Form {
        rows: Vec<Value>,
        now: i64,
        #[serde(default)]
        context: Option<Value>,
    },
    /// Library v3 §9's compaction of a v3 log: stray `ep` and v1 tracker-event rows folded and dropped.
    V3Compact {
        rows: Vec<Value>,
        now: i64,
    },
    WriteBack {
        held: Vec<Value>,
        log: Vec<Value>,
        now: i64,
    },
    /// Library v4 §11: held documents and kept ops on the new log.
    WriteBackV4 {
        documents: Vec<Value>,
        #[serde(default)]
        kept: Vec<Value>,
        log: Vec<Value>,
        now: i64,
    },
    /// Library v4 §4: a row's opened plaintext (base64) → document, JSON row, unreadable or newer.
    DocDecode {
        plaintext: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Library v4 §4: document → plaintext (base64url), or `too_large` (224 KiB for a §8 write, else 256 KiB).
    DocEncode {
        document: Value,
        #[serde(default)]
        write: bool,
    },
    DocName {
        document: Value,
    },
    DocMerge {
        a: Value,
        b: Value,
    },
    TitleState {
        title: Value,
        now: i64,
    },
    /// Library v4 §8: one write on the documents it touches → the documents to write.
    ApplyWrite {
        write: Value,
        target: Value,
        #[serde(default)]
        title: Option<Value>,
        #[serde(default)]
        seasons: Vec<Value>,
        #[serde(default)]
        receipts: Vec<Value>,
        now: i64,
    },
    /// Library v4 §9 *Fit before sending*.
    DeliveryWrite {
        #[serde(default)]
        document: Option<Value>,
        #[serde(default)]
        identity: Value,
        commands: Vec<Value>,
    },
    /// Library v4 §10: every row through `base` → the switch's rows.
    V4Form {
        rows: Vec<Value>,
        base: u64,
        performer: String,
        now: i64,
        #[serde(default)]
        stored_cap: Option<u64>,
    },
    /// Library v4 §10 step 2: the log through `base` + `v4_form`'s output → pass or abort.
    V4DryRun {
        rows: Vec<Value>,
        form: Value,
        now: i64,
    },
    SwitchReady {
        input: Value,
    },
    WatchName {
        media: String,
        id: u64,
        season: u64,
        episode: u64,
    },
    ReceiptName {
        provider: String,
        account: String,
        target: String,
    },
    Retry {
        attempts: u32,
        now: u64,
        retry_after: Option<u64>,
    },
    /// The taste tilt's order for one page. `signals` are the cosines den-atlas measured plus each
    /// candidate's year; `weights` and `era` are the levers, defaulted so a caller that does not tune
    /// gets what ships.
    Tilt {
        signals: Vec<Signals>,
        #[serde(default)]
        weights: Option<Weights>,
        era: Era,
        #[serde(default = "yes")]
        include_era: bool,
    },
    /// The household's era curve from (year, weight) samples — the other half of the tilt's inputs, and
    /// the only part that is derived rather than measured.
    TiltEra {
        samples: Vec<(i32, f64)>,
        current_year: i32,
    },
}

fn yes() -> bool {
    true
}

/// Versioned, non-throwing FFI boundary. An error is never an empty snapshot or an acknowledgement.
pub fn evaluate(input: &str) -> String {
    fn run(input: &str) -> Result<Value, String> {
        if input.len() > MAX_REQUEST_BYTES {
            return Err("request_too_large".into());
        }
        let request: Request = serde_json::from_str(input).map_err(|_| "invalid_request")?;
        match request {
            Request::Name { row } => Ok(json!(wire::name(&row)?)),
            Request::Newest { row } => Ok(json!(wire::newest(&row)?)),
            Request::Merge { a, b } => merge(&a, &b),
            Request::Capture {
                before,
                after,
                at,
                id,
            } => capture(&before, &after, &at, &id),
            Request::Commands { event, current } => commands(&event, &current),
            Request::EpisodeMark {
                row,
                mark,
                authoritative,
            } => episode_mark(&row, &mark, authoritative),
            Request::AiredEpisodes {
                seasons,
                last_aired,
            } => Ok(aired_episodes(&seasons, last_aired)),
            Request::SeriesState {
                seasons,
                last_aired,
                watched,
            } => Ok(series_state(&seasons, last_aired, &watched)),
            Request::EpisodeAfter {
                seasons,
                last_aired,
                at,
            } => Ok(episode_after(at, &seasons, last_aired)),
            Request::ContinueTarget {
                seasons,
                last_aired,
                last_played,
            } => Ok(continue_target(&seasons, last_aired, last_played)),
            Request::ContinueEntry { input } => Ok(continue_entry(&input)),
            Request::Issue {
                last,
                seen,
                now,
                device,
            } => {
                let last = seen.map(|seen| last.clone().max(seen)).unwrap_or(last);
                Ok(json!(last.issue(now, device)?))
            }
            Request::Decide { command, remote } => Ok(json!(decide(&command, &remote))),
            Request::EpisodeState {
                register,
                resets,
                now,
            } => library_v3::episode_state(&register, &resets, now),
            Request::EpisodeStateV4 {
                title,
                season,
                episode,
                now,
            } => {
                let title = title.as_ref().map(library_v4::readable).transpose()?;
                let season = season.as_ref().map(library_v4::readable).transpose()?;
                let episode = match &episode {
                    Value::String(key) => key.clone(),
                    other => other.to_string(),
                };
                library_v4::state::episode_state(title.as_ref(), season.as_ref(), &episode, now)
            }
            Request::FilmState {
                rec,
                register,
                resets,
                now,
            } => library_v3::film_state(&rec, &register, &resets, now),
            Request::FilmStateV4 { title, now } => {
                library_v4::state::film_state(&library_v4::readable(&title)?, now)
            }
            Request::RegisterWrite {
                action,
                current,
                resets,
                now,
            } => library_v3::register_write(&action, current.as_ref(), &resets, now),
            Request::ImportWrite {
                register,
                item,
                resets,
                now,
            } => library_v3::import_write(register.as_ref(), &item, &resets, now),
            Request::PendingTargets {
                targets,
                receipts,
                since,
                now,
            } => library_v3::pending_targets(&targets, &receipts, &since, now),
            Request::PendingTargetsV4 {
                documents,
                deliver,
                now,
            } => library_v4::delivery::pending_targets(&documents, &deliver, now),
            Request::Settle {
                outcome,
                built_from,
                order,
            } => library_v3::settle(&outcome, &built_from, &order),
            Request::SettleV4 {
                outcome,
                built_from,
                order,
                entry,
            } => library_v4::delivery::settle(
                &outcome,
                &built_from,
                &order,
                entry.as_ref().filter(|e| !e.is_null()),
            ),
            Request::Lease { input } => library_v3::lease(&input),
            Request::V2Reading { rows, now } => library_v3::v2_reading(&rows, now),
            Request::V3Form { rows, now, context } => {
                library_v3::v3_form_with_context(&rows, now, context.as_ref())
            }
            Request::V3Compact { rows, now } => library_v3::v3_compact(&rows, now),
            Request::WriteBack { held, log, now } => library_v3::write_back(&held, &log, now),
            Request::WriteBackV4 {
                documents,
                kept,
                log,
                now,
            } => library_v4::write_back(&documents, &kept, &log, now),
            Request::DocDecode { plaintext, name } => Ok(library_v4::decode(
                &library_v4::codec::unbase64(&plaintext)?,
                name.as_deref(),
            )),
            Request::DocEncode { document, write } => library_v4::encode(&document, write),
            Request::DocName { document } => Ok(json!(library_v4::name(&document)?)),
            Request::DocMerge { a, b } => library_v4::doc_merge(&a, &b),
            Request::TitleState { title, now } => Ok(library_v4::state::title_state(
                &library_v4::readable(&title)?,
                now,
            )),
            Request::ApplyWrite {
                write,
                target,
                title,
                seasons,
                receipts,
                now,
            } => library_v4::write::apply_write(
                &write,
                &target,
                title.as_ref(),
                &seasons,
                &receipts,
                now,
            ),
            Request::DeliveryWrite {
                document,
                identity,
                commands,
            } => library_v4::delivery::delivery_write(document.as_ref(), &identity, &commands),
            Request::V4Form {
                rows,
                base,
                performer,
                now,
                stored_cap,
            } => library_v4::switch::v4_form(&rows, base, &performer, now, stored_cap),
            Request::V4DryRun { rows, form, now } => {
                library_v4::switch::v4_dry_run(&rows, &form, now)
            }
            Request::SwitchReady { input } => library_v3::switch_ready(&input),
            Request::WatchName {
                media,
                id,
                season,
                episode,
            } => Ok(json!(library_v3::watch_row_name(
                &media, id, season, episode
            )?)),
            Request::ReceiptName {
                provider,
                account,
                target,
            } => Ok(json!(library_v3::receipt_row_name(
                &provider, &account, &target
            )?)),
            Request::Tilt {
                signals,
                weights,
                era,
                include_era,
            } => Ok(json!(order(
                &signals,
                &weights.unwrap_or_default(),
                &era,
                include_era
            ))),
            Request::TiltEra {
                samples,
                current_year,
            } => Ok(json!(Era::from_samples(&samples, current_year))),
            Request::Retry {
                attempts,
                now,
                retry_after,
            } => {
                // Same capped exponential delay as the outbox, with provider Retry-After as a lower bound.
                let delay = (5_000u64 * (1u64 << attempts.min(10))).min(1_800_000);
                let at = now
                    .checked_add(delay)
                    .ok_or("time_overflow")?
                    .max(retry_after.unwrap_or(0));
                if at > wire::MAX_SAFE_INTEGER {
                    return Err("time_overflow".into());
                }
                Ok(json!(at))
            }
        }
    }
    match run(input) {
        Ok(value) => json!({ "version": 1, "ok": value }).to_string(),
        Err(error) => json!({ "version": 1, "error": error }).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::evaluate;
    use serde_json::{json, Value};

    #[test]
    fn whole_library_operations_are_not_limited_to_one_megabyte() {
        // serde ignores the framing field, just as the boundary ignores future request fields. This
        // pins the transport limit without making the policy test construct thousands of real rows.
        let request = json!({
            "op": "v3_form",
            "rows": [],
            "now": 0,
            "framing": "x".repeat(2 * 1024 * 1024),
        });
        let response: Value = serde_json::from_str(&evaluate(&request.to_string())).unwrap();
        assert!(response.get("ok").is_some(), "{response}");
    }
}
