use serde::Deserialize;
use serde_json::{json, Value};

/// den-spec / LibraryRecord.watchedThreshold: the fraction at or above which an episode counts as watched.
/// One definition, so a client cannot hold a different opinion about what "watched" means.
pub const WATCHED: f64 = 0.95;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct Coord {
    pub season: i64,
    pub episode: i64,
}

/// A season as the layout knows it. Only the count matters here; names, art and dates are the client's.
#[derive(Deserialize)]
pub struct SeasonCount {
    pub season: i64,
    pub episodes: i64,
}

/// Where the viewer had got to: the episode last played and how far into it.
#[derive(Deserialize)]
pub struct LastPlayed {
    pub season: i64,
    pub episode: i64,
    pub fraction: f64,
}

fn coord(at: Coord) -> Value {
    json!({"season": at.season, "episode": at.episode})
}

/// Regular seasons in order, Specials excluded.
///
/// Season 0 is extras, not the show. Every count the clients display already excludes it, so a layout that
/// keeps it makes "10 of 10" unreachable — and, worse, offers a Special as the next episode to watch once the
/// finale is done. Excluding it here is what stops the two clients disagreeing about that.
fn regular(seasons: &[SeasonCount]) -> Vec<&SeasonCount> {
    let mut regular: Vec<&SeasonCount> = seasons
        .iter()
        .filter(|s| s.season > 0 && s.episodes > 0)
        .collect();
    regular.sort_by_key(|s| s.season);
    regular
}

/// Whether a coordinate has aired, judged against the newest aired episode. Unknown counts as aired, and so
/// does anything when the newest aired episode is itself a Special — that says nothing about where the regular
/// seasons have got to.
pub fn is_aired(at: Coord, last_aired: Option<Coord>) -> bool {
    match last_aired {
        Some(last) if last.season > 0 => (at.season, at.episode) <= (last.season, last.episode),
        _ => true,
    }
}

/// The episodes that make up "the series so far", in order.
fn aired(seasons: &[SeasonCount], last_aired: Option<Coord>) -> Vec<Coord> {
    regular(seasons)
        .iter()
        .flat_map(|s| {
            (1..=s.episodes).map(move |episode| Coord {
                season: s.season,
                episode,
            })
        })
        .filter(|at| is_aired(*at, last_aired))
        .collect()
}

/// The episodes that make up "the series so far", in order: regular seasons, nothing after `last_aired`.
///
/// Exposed because the clients need the list itself, not only counts drawn from it — marking a season watched
/// acts on exactly these, and asking for them is what keeps that set from being derived a second way.
pub fn aired_episodes(seasons: &[SeasonCount], last_aired: Option<Coord>) -> Value {
    json!(aired(seasons, last_aired)
        .into_iter()
        .map(coord)
        .collect::<Vec<_>>())
}

/// How much of a series has been watched, and what to offer next.
///
/// `watched` is the set of episodes the client holds as watched — passed as data because the clients keep it
/// in different shapes, and because a predicate cannot cross this boundary.
pub fn series_state(
    seasons: &[SeasonCount],
    last_aired: Option<Coord>,
    watched: &[Coord],
) -> Value {
    let episodes = aired(seasons, last_aired);
    let mut count = 0;
    let mut next_up: Option<Coord> = None;
    for at in &episodes {
        if watched.contains(at) {
            count += 1;
        } else if next_up.is_none() {
            next_up = Some(*at);
        }
    }
    let total = episodes.len();
    json!({
        "watched": count,
        "total": total,
        "nextUp": next_up.map(coord),
        "complete": total > 0 && count >= total,
    })
}

/// The episode immediately after `at` in the layout: the next in its season, else the next season's first.
///
/// `aired` is reported separately rather than folded in, because "nothing listed after this" and "listed but
/// not out yet" are different answers: the first means the series is finished until it is renewed, the second
/// means it is merely waiting for a date.
pub fn episode_after(at: Coord, seasons: &[SeasonCount], last_aired: Option<Coord>) -> Value {
    let seasons = regular(seasons);
    let next = match seasons.iter().position(|s| s.season == at.season) {
        None => None,
        Some(index) if at.episode < seasons[index].episodes => Some(Coord {
            season: at.season,
            episode: at.episode + 1,
        }),
        Some(index) => seasons.get(index + 1..).and_then(|rest| {
            rest.first().map(|s| Coord {
                season: s.season,
                episode: 1,
            })
        }),
    };
    json!({
        "next": next.map(coord),
        "aired": next.map(|at| is_aired(at, last_aired)).unwrap_or(false),
    })
}

/// Where "Continue" should take the viewer: resume the episode last played if it is unfinished, else the one
/// after it, else the first — a finished finale is a rewatch from the start, not a dead end.
pub fn continue_target(
    seasons: &[SeasonCount],
    last_aired: Option<Coord>,
    last_played: Option<LastPlayed>,
) -> Value {
    let episodes = aired(seasons, last_aired);
    let first = episodes.first().copied();
    let start =
        |at: Option<Coord>| json!({"episode": at.map(coord), "fraction": 0.0, "kind": "start"});
    let Some(played) = last_played else {
        return start(first);
    };
    let at = Coord {
        season: played.season,
        episode: played.episode,
    };
    if played.fraction < WATCHED {
        return json!({"episode": coord(at), "fraction": played.fraction, "kind": "resume"});
    }
    match episodes.iter().position(|c| *c == at) {
        Some(index) => match episodes.get(index + 1) {
            Some(next) => json!({"episode": coord(*next), "fraction": 0.0, "kind": "next"}),
            None => start(first),
        },
        None => start(first),
    }
}
