use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Watched,
    Unwatched,
    List,
    Rating,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Command {
    pub kind: Kind,
    pub at: u64,
    pub current: bool,
    pub baseline: bool,
    pub episode: bool,
    pub added: bool,
    pub rating: Option<i32>,
    #[serde(default)]
    pub p: Option<i64>,
    #[serde(default)]
    pub receipt_p: Option<i64>,
    #[serde(default)]
    pub watched_at: Option<u64>,
    #[serde(default)]
    pub removals_held: bool,
    #[serde(default)]
    pub acknowledged_rating: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteTime {
    pub at: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteRating {
    pub at: Option<u64>,
    pub value: Option<i32>,
}

/// Facts from a successful provider read, reduced for one title/episode by the platform adapter.
/// `None` for the whole mark means absent; a present mark with `at: None` means unknown ordering.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Remote {
    pub authoritative: bool,
    pub account_matches: bool,
    pub simkl: bool,
    pub watched: Option<RemoteTime>,
    pub listed: Option<RemoteTime>,
    pub rated: Option<RemoteRating>,
    pub any_title_watch: bool,
    pub unknown_or_newer_title_watch: bool,
    pub episodes_complete: bool,
    #[serde(default)]
    pub plays: Vec<u64>,
    #[serde(default)]
    pub unwatch_then_remark: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Send,
    Acknowledge,
    Superseded,
    Hold,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub reason: &'static str,
}

fn decision(action: Action, reason: &'static str) -> Decision {
    Decision { action, reason }
}

/// This function never performs a write. The caller must recheck account and current intent immediately
/// before the request, and persist the provider receipt only after actual acknowledgement.
pub fn decide(command: &Command, remote: &Remote) -> Decision {
    use Action::*;
    if !command.current {
        return decision(Superseded, "newer_local_intent");
    }
    if !remote.authoritative {
        return decision(Hold, "snapshot_unavailable");
    }
    if !remote.account_matches {
        return decision(Hold, "account_changed");
    }
    if command.at > super::wire::MAX_SAFE_INTEGER
        || command.rating.is_some_and(|n| !(1..=10).contains(&n))
    {
        return decision(Hold, "invalid_command");
    }
    let older = |at: Option<u64>| at.is_some_and(|at| at > 0 && at <= command.at);
    let rating = remote.rated.as_ref().and_then(|r| r.value);
    match command.kind {
        Kind::Watched if remote.watched.is_some() => {
            let rewatch = command
                .p
                .zip(command.receipt_p)
                .is_some_and(|(p, old)| p > old);
            if !rewatch
                || command.watched_at.is_some_and(|wanted| {
                    let second = wanted / 1000 * 1000;
                    remote.plays.iter().any(|play| play / 1000 * 1000 == second)
                })
            {
                return decision(Acknowledge, "already_seen");
            }
        }
        Kind::Unwatched => {
            let Some(mark) = &remote.watched else {
                return if !command.episode || remote.episodes_complete {
                    decision(Acknowledge, "already_unseen")
                } else {
                    decision(Hold, "incomplete_episode_coverage")
                };
            };
            if !older(mark.at) {
                return decision(Hold, "remote_order_unknown_or_newer");
            }
            if remote.simkl && !command.episode && (rating.is_some() || remote.listed.is_some()) {
                return decision(Hold, "would_remove_independent_state");
            }
        }
        Kind::List if command.added => {
            if remote.listed.is_some() || (command.baseline && remote.any_title_watch) {
                return decision(Acknowledge, "already_present");
            }
            if remote.simkl && remote.unknown_or_newer_title_watch {
                return decision(Hold, "remote_order_unknown_or_newer");
            }
        }
        Kind::List => {
            if command.removals_held {
                return decision(Hold, "removals_held");
            }
            let Some(listed) = &remote.listed else {
                return decision(Acknowledge, "already_absent");
            };
            if !older(listed.at) {
                return decision(Hold, "remote_order_unknown_or_newer");
            }
            if remote.simkl && (remote.any_title_watch || rating.is_some()) {
                return decision(Hold, "would_remove_independent_state");
            }
        }
        Kind::Rating => {
            let mapped = |rating: i32| {
                if rating <= 4 {
                    2
                } else if rating <= 7 {
                    7
                } else {
                    10
                }
            };
            if rating
                .zip(command.rating)
                .is_some_and(|(remote, local)| mapped(remote) == local)
                || (command.baseline && remote.rated.is_some())
            {
                return decision(Acknowledge, "rating_already_satisfied");
            }
            if remote
                .rated
                .as_ref()
                .is_some_and(|remote| remote.at.is_none())
                && rating != command.acknowledged_rating
            {
                return decision(Hold, "remote_order_unknown_or_newer");
            }
            if remote.rated.as_ref().is_some_and(|r| !older(r.at)) {
                return decision(Hold, "remote_order_unknown_or_newer");
            }
        }
        _ => {}
    }
    decision(Send, "explicit_current_intent")
}
