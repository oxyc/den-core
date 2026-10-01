//! Pure library state and delivery decisions. No clocks, storage, network, credentials, or async runtime.
//! All time and provider facts arrive as inputs; bindings return the same versioned JSON envelope.

mod delivery;
mod episodes;
mod events;
mod library_v3;
mod series;
mod tilt;
mod wire;

use serde::Deserialize;
use serde_json::{json, Value};

// `v3_form` and `write_back` receive a whole library. Keep the boundary bounded, but large enough
// for den-edge's 32 MiB stored-library limit plus JSON field names and request framing.
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

pub use delivery::{decide, Action, Command, Decision, Kind, Remote, RemoteRating, RemoteTime};
pub use episodes::episode_mark;
pub use events::commands;
pub use library_v3::{
    episode_state, film_state, import_write, lease, pending_targets, register_write, settle,
    switch_ready, v2_reading, v3_form, v3_form_with_context, write_back,
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
    EpisodeState {
        register: Value,
        #[serde(default)]
        resets: Vec<Stamp>,
        now: i64,
    },
    FilmState {
        rec: Value,
        register: Value,
        #[serde(default)]
        resets: Vec<Stamp>,
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
    Settle {
        outcome: Value,
        built_from: Value,
        order: Value,
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
    WriteBack {
        held: Vec<Value>,
        log: Vec<Value>,
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
            Request::FilmState {
                rec,
                register,
                resets,
                now,
            } => library_v3::film_state(&rec, &register, &resets, now),
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
            Request::Settle {
                outcome,
                built_from,
                order,
            } => library_v3::settle(&outcome, &built_from, &order),
            Request::Lease { input } => library_v3::lease(&input),
            Request::V2Reading { rows, now } => library_v3::v2_reading(&rows, now),
            Request::V3Form { rows, now, context } => {
                library_v3::v3_form_with_context(&rows, now, context.as_ref())
            }
            Request::WriteBack { held, log, now } => library_v3::write_back(&held, &log, now),
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
