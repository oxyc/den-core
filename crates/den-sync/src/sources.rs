//! Source discovery's outcome (den#723): what a title's source lookup means once its replies are in, and how long a
//! client waits on one. Every client asks here, so "confirmed empty", "unknown" and "keep the old list" mean the same
//! thing on every platform. Clients keep the requests, timers, aborts and copy.
//!
//! Two rules matter more than the rest: a partial failure never becomes a confirmed empty, and only a confirmed empty
//! may fade or hide a title as having nothing to play.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// When "taking longer than usual" shows. Below Scout's 8 s scrape budget (den-hearth `DEFAULT_TIMEOUT`,
/// `SCRAPE_TIMEOUT_SECS`), so the viewer hears about a slow indexer before Scout gives up on it. Over a week of the
/// box's `/stream` request lines (2026-11, 138 requests) the median was 0.4 s, p95 3.8 s and p99 5.6 s: at 5 s the
/// notice shows for about one lookup in a hundred.
pub const SLOW_MS: u64 = 5_000;

/// When the client stops waiting and shows Retry. Under Edge's 30 s relay deadline (den-edge `relay.rs` `TIMEOUT`),
/// so the client, not a 504, ends the wait; above the slowest lookup seen in the same sample (18 s, a debrid cache
/// check inside Scout's 8 s + 20 s build budget). Scout's list build is detached from the request, so a Retry after
/// the deadline joins that build or reads the list it cached rather than starting over.
pub const DEADLINE_MS: u64 = 20_000;

/// The automatic retry waits at least this long: a connectivity event fires when the interface comes up, before its
/// routes and DNS have settled.
pub const RETRY_BASE_MS: u64 = 500;

/// The automatic retry is spread over this much more, by the caller's jitter sample, so a household's devices on
/// one Wi-Fi coming back together don't ask at once.
pub const RETRY_SPREAD_MS: u64 = 2_000;

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Imdb {
    Resolved,
    /// The title has no IMDb id: a data gap, not an outage.
    Missing,
    /// The lookup itself failed (network, rate limit): says nothing about the title.
    LookupFailed,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Live,
    Partial,
    Empty,
    Unknown,
    Stale,
}

/// An addon's own account of its list (Scout's `den` object), as the client read it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    kind: Kind,
    /// Sources the addon could have asked that didn't answer.
    #[serde(default)]
    missing: u32,
    /// Sources an early answer went out without; still answering, not failed.
    #[serde(default)]
    late: u32,
    /// Set on a list held through an outage (`degraded: "stale_list"`): when that list was built, in ms.
    #[serde(default)]
    outage_built_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Answered {
        streams: u32,
        /// Absent when the addon sent no account the client could read (an older addon).
        #[serde(default)]
        answer: Option<Answer>,
        /// The response's `X-Den-Degraded` reason, as sent.
        #[serde(default)]
        degraded: Option<String>,
    },
    /// No usable answer: a network failure, a non-2xx, an unreadable body.
    Errored {},
    /// The client's own timer ran out on this addon.
    TimedOut {},
}

/// What a list can't vouch for.
#[derive(Deserialize, Serialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Caveat {
    /// The oldest list in it that was held through an outage, built at (ms).
    pub stale_since: Option<u64>,
    /// Sources behind the addons that didn't answer.
    pub missing: u32,
    /// Sources an early answer went out without.
    pub late: u32,
    /// Addons that errored or timed out.
    pub failed_addons: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prior {
    count: u32,
    #[serde(default)]
    caveat: Option<Caveat>,
}

/// `source_outcome`'s input.
#[derive(Deserialize)]
pub struct OutcomeInput {
    imdb: Imdb,
    #[serde(default)]
    replies: Vec<Reply>,
    /// The list on screen before this lookup, if any: kept when the lookup can't replace it.
    #[serde(default)]
    prior: Option<Prior>,
}

/// `source_wait`'s input.
#[derive(Deserialize)]
pub struct WaitInput {
    /// Since the current request began.
    elapsed_ms: u64,
    /// 0 for the first request of a lookup, 1 once its automatic retry has run.
    attempt: u32,
    /// The device's connectivity came back since the last ask.
    #[serde(default)]
    online_restored: bool,
    /// The delay a `Retry-After` gave, in ms.
    #[serde(default)]
    retry_after_ms: Option<u64>,
    /// A uniform random sample in [0, 1), from the platform.
    jitter: f64,
}

/// The dependency a degraded answer names, in words safe to show and report.
fn category(reason: &str) -> Option<&'static str> {
    match reason {
        "cache-check" => Some("debrid"),
        // A held list: its body's `outage_built_at` says so, and Scout also sends it while a healthy list rebuilds.
        "stale_list" => None,
        // `indexers`, and any reason a newer addon sends: its search is what is impaired.
        _ => Some("source_discovery"),
    }
}

pub fn outcome(input: OutcomeInput) -> Result<Value, String> {
    let prior = input.prior.filter(|prior| prior.count > 0);
    let kept = |result: &str, category: Option<&str>, retryable: bool| {
        let (list, caveat) = match &prior {
            Some(prior) => ("retained", json!(prior.caveat)),
            None => ("none", Value::Null),
        };
        json!({
            "result": result,
            "category": category,
            "list": list,
            "caveat": caveat,
            "may_mark_unavailable": false,
            "retryable": retryable,
        })
    };
    match input.imdb {
        Imdb::Missing => return Ok(kept("no_imdb", None, false)),
        Imdb::LookupFailed => return Ok(kept("failed_retryable", None, true)),
        Imdb::Resolved if input.replies.is_empty() => return Err("invalid_request".into()),
        Imdb::Resolved => {}
    }

    let mut streams = 0u64;
    let mut caveat = Caveat::default();
    let mut debrid = false;
    let mut impaired = false;
    // Something kept this lookup from speaking for every source: never a confirmed empty.
    let mut gap = false;
    // Something kept the list it found from being complete.
    let mut short = false;
    for reply in &input.replies {
        match reply {
            Reply::Errored {} => {
                caveat.failed_addons = caveat.failed_addons.saturating_add(1);
                gap = true;
                short = true;
            }
            Reply::TimedOut {} => {
                caveat.failed_addons = caveat.failed_addons.saturating_add(1);
                impaired = true;
                gap = true;
                short = true;
            }
            Reply::Answered {
                streams: count,
                answer,
                degraded,
            } => {
                streams += u64::from(*count);
                if let Some(reason) = degraded {
                    debrid |= category(reason) == Some("debrid");
                    impaired |= category(reason) == Some("source_discovery");
                    gap = true;
                }
                let Some(answer) = answer else { continue };
                caveat.missing = caveat.missing.saturating_add(answer.missing);
                caveat.late = caveat.late.saturating_add(answer.late);
                let incomplete =
                    matches!(answer.kind, Kind::Partial | Kind::Unknown) || answer.missing > 0;
                // An early answer is `partial` for its late sources alone: they are still answering, not failing.
                if answer.missing > 0
                    || answer.outage_built_at.is_some()
                    || (incomplete && answer.late == 0)
                {
                    impaired = true;
                }
                short |= incomplete || answer.late > 0;
                // A stale list speaks for when it was built, not for now.
                gap |= short || answer.kind == Kind::Stale || answer.outage_built_at.is_some();
                if *count > 0 {
                    if let Some(at) = answer.outage_built_at {
                        caveat.stale_since = Some(caveat.stale_since.map_or(at, |s| s.min(at)));
                    }
                }
            }
        }
    }
    let category = if debrid {
        Some("debrid")
    } else if impaired {
        Some("source_discovery")
    } else {
        None
    };

    if streams > 0 {
        let result = if caveat.stale_since.is_some() {
            "stale_held"
        } else if short {
            "partial"
        } else {
            "live"
        };
        let caveat = (result != "live").then_some(caveat);
        return Ok(json!({
            "result": result,
            "category": category,
            "list": "new",
            "caveat": caveat,
            "may_mark_unavailable": false,
            "retryable": result != "live",
        }));
    }
    if !gap {
        return Ok(json!({
            "result": "confirmed_empty",
            "category": null,
            "list": "none",
            "caveat": null,
            "may_mark_unavailable": true,
            "retryable": false,
        }));
    }
    Ok(match category {
        Some(category) => kept("degraded", Some(category), true),
        None => kept("failed_retryable", None, true),
    })
}

pub fn wait(input: WaitInput) -> Result<Value, String> {
    if !(0.0..1.0).contains(&input.jitter) {
        return Err("invalid_request".into());
    }
    let elapsed = input.elapsed_ms;
    let (phase, next) = if elapsed < SLOW_MS {
        ("loading", Some(SLOW_MS - elapsed))
    } else if elapsed < DEADLINE_MS {
        ("slow", Some(DEADLINE_MS - elapsed))
    } else {
        ("deadline", None)
    };
    // One automatic retry per lookup, on a Retry-After or on connectivity coming back; Retry is the viewer's after.
    let retry = (input.attempt == 0 && (input.online_restored || input.retry_after_ms.is_some()))
        .then(|| {
            // The jitter is below 1, so the spread stays below RETRY_SPREAD_MS.
            let spread = (input.jitter * RETRY_SPREAD_MS as f64) as u64;
            input
                .retry_after_ms
                .unwrap_or(0)
                .max(RETRY_BASE_MS)
                .saturating_add(spread)
        });
    Ok(json!({ "phase": phase, "next_ms": next, "retry_in_ms": retry }))
}
