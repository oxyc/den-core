//! Pure library state and delivery decisions. No clocks, storage, network, credentials, or async runtime.
//! All time and provider facts arrive as inputs; bindings return the same versioned JSON envelope.

mod delivery;
mod episodes;
mod events;
mod series;
mod wire;

use serde::Deserialize;
use serde_json::{json, Value};

pub use delivery::{decide, Action, Command, Decision, Kind, Remote, RemoteRating, RemoteTime};
pub use episodes::episode_mark;
pub use events::commands;
pub use series::{
    continue_target, episode_after, is_aired, series_state, Coord, LastPlayed, SeasonCount,
};
pub use wire::{capture, merge, Stamp};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
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
    Retry {
        attempts: u32,
        now: u64,
        retry_after: Option<u64>,
    },
}

/// Versioned, non-throwing FFI boundary. An error is never an empty snapshot or an acknowledgement.
pub fn evaluate(input: &str) -> String {
    fn run(input: &str) -> Result<Value, String> {
        if input.len() > 1024 * 1024 {
            return Err("request_too_large".into());
        }
        let request: Request = serde_json::from_str(input).map_err(|_| "invalid_request")?;
        match request {
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
