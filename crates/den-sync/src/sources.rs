//! Source discovery's outcome (den#723): what a title's source lookup means once its replies are in, and how long a
//! client waits on one. Every client asks here, so "confirmed empty", "unknown" and "keep the old list" mean the same
//! thing on every platform. Clients keep the requests, timers, aborts and copy.
//!
//! Two rules matter more than the rest: a partial failure never becomes a confirmed empty, and only a confirmed empty
//! may fade or hide a title as having nothing to play.
//!
//! The requests are read from `Value`s rather than derived types: derived visitors cost the web modules, and the
//! Tizen 3 core most of all, far more than these few fields.

use serde_json::{json, Map, Value};

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

const INVALID: &str = "invalid_request";

/// A field that may be absent or null: `None` then, `Some` of what `read` makes of anything else.
fn field<'a, T>(
    object: &'a Map<String, Value>,
    name: &str,
    read: impl FnOnce(&'a Value) -> Option<T>,
) -> Result<Option<T>, String> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => read(value).map(Some).ok_or_else(|| INVALID.into()),
    }
}

fn number(object: &Map<String, Value>, name: &str) -> Result<Option<u64>, String> {
    field(object, name, Value::as_u64)
}

fn required(object: &Map<String, Value>, name: &str) -> Result<u64, String> {
    number(object, name)?.ok_or_else(|| INVALID.into())
}

/// One answer: the result, the dependency it names, the list to show and its caveat, and the two decisions.
fn verdict(
    result: &str,
    category: Option<&str>,
    list: &str,
    caveat: Value,
    retryable: bool,
) -> Value {
    json!({
        "result": result,
        "category": category,
        "list": list,
        "caveat": caveat,
        "may_mark_unavailable": result == "confirmed_empty",
        "retryable": retryable,
    })
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

/// `source_outcome`: `imdb`, `replies` and `prior` (den#723; the request and answer are documented in its PR).
pub fn outcome(input: &Map<String, Value>) -> Result<Value, String> {
    // The list on screen before this lookup: kept, with the caveat it was shown with, when this one can't replace it.
    let prior = field(input, "prior", Value::as_object)?;
    let prior = match prior {
        Some(prior) if required(prior, "count")? > 0 => Some(field(prior, "caveat", |c| {
            c.is_object().then(|| c.clone())
        })?),
        _ => None,
    };
    let kept = |result: &str, category: Option<&str>, retryable: bool| match &prior {
        Some(caveat) => verdict(result, category, "retained", json!(caveat), retryable),
        None => verdict(result, category, "none", Value::Null, retryable),
    };
    let replies = field(input, "replies", Value::as_array)?.map_or(&[][..], Vec::as_slice);
    match field(input, "imdb", Value::as_str)? {
        // A data gap, not an outage.
        Some("missing") => return Ok(kept("no_imdb", None, false)),
        // The lookup itself failed: it says nothing about the title.
        Some("lookup_failed") => return Ok(kept("failed_retryable", None, true)),
        Some("resolved") if !replies.is_empty() => {}
        _ => return Err(INVALID.into()),
    }

    let mut streams = 0u64;
    let (mut stale_since, mut missing, mut late, mut failed) = (None::<u64>, 0u64, 0u64, 0u64);
    let (mut debrid, mut impaired) = (false, false);
    // Something kept this lookup from speaking for every source: never a confirmed empty.
    let mut gap = false;
    // Something kept the list it found from being complete.
    let mut short = false;
    for reply in replies {
        let reply = reply.as_object().ok_or(INVALID)?;
        match field(reply, "reply", Value::as_str)? {
            // No usable answer: a network failure, a non-2xx, an unreadable body.
            Some("errored") => {}
            // The client's own timer ran out: the addon is up, its search is slow.
            Some("timed_out") => impaired = true,
            Some("answered") => {
                let count = required(reply, "streams")?;
                streams = streams.saturating_add(count);
                // The response's `X-Den-Degraded` reason, as sent.
                if let Some(reason) = field(reply, "degraded", Value::as_str)? {
                    debrid |= category(reason) == Some("debrid");
                    impaired |= category(reason) == Some("source_discovery");
                    gap = true;
                }
                // The addon's own account of its list (Scout's `den`); absent from an older addon.
                let Some(answer) = field(reply, "answer", Value::as_object)? else {
                    continue;
                };
                let kind = field(answer, "kind", Value::as_str)?.ok_or(INVALID)?;
                if !["live", "partial", "empty", "unknown", "stale"].contains(&kind) {
                    return Err(INVALID.into());
                }
                let unanswered = number(answer, "missing")?.unwrap_or(0);
                let behind = number(answer, "late")?.unwrap_or(0);
                let outage = number(answer, "outage_built_at")?;
                missing = missing.saturating_add(unanswered);
                late = late.saturating_add(behind);
                let incomplete = kind == "partial" || kind == "unknown" || unanswered > 0;
                // An early answer is `partial` for its late sources alone: they are still answering, not failing.
                impaired |= unanswered > 0 || outage.is_some() || (incomplete && behind == 0);
                short |= incomplete || behind > 0;
                // A stale list speaks for when it was built, not for now.
                gap |= short || kind == "stale" || outage.is_some();
                if let (Some(at), true) = (outage, count > 0) {
                    stale_since = Some(stale_since.map_or(at, |since| since.min(at)));
                }
                continue;
            }
            _ => return Err(INVALID.into()),
        }
        failed += 1;
        gap = true;
        short = true;
    }
    let category = if debrid {
        Some("debrid")
    } else if impaired {
        Some("source_discovery")
    } else {
        None
    };

    if streams > 0 {
        let result = if stale_since.is_some() {
            "stale_held"
        } else if short {
            "partial"
        } else {
            "live"
        };
        let caveat = if result == "live" {
            Value::Null
        } else {
            json!({
                "stale_since": stale_since,
                "missing": missing,
                "late": late,
                "failed_addons": failed,
            })
        };
        return Ok(verdict(result, category, "new", caveat, result != "live"));
    }
    Ok(match (gap, category) {
        (false, _) => verdict("confirmed_empty", None, "none", Value::Null, false),
        (true, Some(category)) => kept("degraded", Some(category), true),
        (true, None) => kept("failed_retryable", None, true),
    })
}

/// `source_wait`: `elapsed_ms` since the current request began, `attempt` (0, or 1 once the automatic retry ran),
/// `online_restored`, `retry_after_ms`, and a platform `jitter` sample in [0, 1).
pub fn wait(input: &Map<String, Value>) -> Result<Value, String> {
    let elapsed = required(input, "elapsed_ms")?;
    let attempt = required(input, "attempt")?;
    let online = field(input, "online_restored", Value::as_bool)?.unwrap_or(false);
    let retry_after = number(input, "retry_after_ms")?;
    let jitter = field(input, "jitter", Value::as_f64)?
        .filter(|jitter| (0.0..1.0).contains(jitter))
        .ok_or(INVALID)?;
    let (phase, next) = if elapsed < SLOW_MS {
        ("loading", Some(SLOW_MS - elapsed))
    } else if elapsed < DEADLINE_MS {
        ("slow", Some(DEADLINE_MS - elapsed))
    } else {
        ("deadline", None)
    };
    // One automatic retry per lookup, on a Retry-After or on connectivity coming back; Retry is the viewer's after.
    let retry = (attempt == 0 && (online || retry_after.is_some())).then(|| {
        // The jitter is below 1, so the spread stays below RETRY_SPREAD_MS.
        let spread = (jitter * RETRY_SPREAD_MS as f64) as u64;
        retry_after
            .unwrap_or(0)
            .max(RETRY_BASE_MS)
            .saturating_add(spread)
    });
    Ok(json!({ "phase": phase, "next_ms": next, "retry_in_ms": retry }))
}
