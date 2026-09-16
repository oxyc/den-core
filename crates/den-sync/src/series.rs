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
    let next = next_after(at, seasons);
    json!({
        "next": next.map(coord),
        "aired": next.map(|at| is_aired(at, last_aired)).unwrap_or(false),
    })
}

fn next_after(at: Coord, seasons: &[SeasonCount]) -> Option<Coord> {
    let seasons = regular(seasons);
    match seasons.iter().position(|s| s.season == at.season) {
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
    }
}

/// Below this, a play has barely begun: it does not offer itself as somewhere to resume.
pub const RESUME_FLOOR: f64 = 0.02;

/// One series' watch state as Continue Watching needs it, in whatever shape the client keeps it.
#[derive(Deserialize)]
pub struct ContinueInput {
    /// The most recently touched mark — where a resume would go.
    pub mark: Option<ContinueMark>,
    /// The furthest episode with a *finished* mark. Not the same as `mark`.
    pub finished: Option<Coord>,
    /// The furthest episode known watched with no mark of its own — a tracker import, or an evicted mark.
    pub flag: Option<Coord>,
    /// The season layout; empty when this client does not know it yet.
    #[serde(default)]
    pub seasons: Vec<SeasonCount>,
    pub last_aired: Option<Coord>,
    /// When the series was taken off the row, if it was.
    pub dismissed_at: Option<i64>,
    /// Whether the library holds the whole title as watched.
    #[serde(default)]
    pub title_watched: bool,
}

#[derive(Deserialize)]
pub struct ContinueMark {
    pub season: i64,
    pub episode: i64,
    pub fraction: f64,
    pub at: i64,
}

/// What Continue Watching should do with one series.
///
/// This is where the two clients disagreed most, and every part of it was a real defect in one of them:
///
/// - A series' place is decided by the furthest episode *finished*, not the one touched last. Watching E1-E4
///   and then opening E5 for two seconds leaves the newest mark on E5, below the resume floor — and reading
///   that alone concludes nothing is finished and drops a series being actively watched.
/// - A bare watched flag counts. A tracker pull writes nothing else, so a series watched through SIMKL or on
///   another device was watched everywhere except the row whose job is to offer its next episode.
/// - The resume floor suppresses a *resume*, not the series: when the front carries it further, it still
///   belongs on the row as the next episode.
/// - A title-level "watched" cannot expire — a tracker reporting a series completed hid shows still airing —
///   so where the layout is known, the episodes decide. With no layout there is nothing better than the flag.
///
/// `reason` comes back with every answer so a client can say why a series is missing without a debugger.
pub fn continue_entry(input: &ContinueInput) -> Value {
    let none = |reason: &str| json!({"action": "none", "episode": null, "fraction": 0.0, "reason": reason});
    let layout_known = !regular(&input.seasons).is_empty();
    if input.title_watched && !layout_known {
        return none("title watched, no layout to judge it by");
    }
    let activity = input.mark.as_ref().map(|m| m.at).unwrap_or(i64::MIN);
    if input.dismissed_at.is_some_and(|at| at >= activity) {
        return none("dismissed");
    }
    let front = [input.finished, input.flag]
        .into_iter()
        .flatten()
        .max_by_key(|at| (at.season, at.episode));
    if let Some(mark) = &input.mark {
        let at = Coord {
            season: mark.season,
            episode: mark.episode,
        };
        let ahead = front
            .map(|f| (f.season, f.episode) < (at.season, at.episode))
            .unwrap_or(true);
        if mark.fraction > RESUME_FLOOR && mark.fraction < WATCHED && ahead {
            return json!({
                "action": "resume", "episode": coord(at), "fraction": mark.fraction,
                "reason": "resuming where it was left",
            });
        }
    }
    let Some(front) = front else {
        return none("nothing finished, and no mark past the resume floor");
    };
    if !layout_known {
        return none("no season layout known");
    }
    let Some(next) = next_after(front, &input.seasons) else {
        return none("finished: nothing listed after it");
    };
    if !is_aired(next, input.last_aired) {
        return none("caught up: the next episode has not aired");
    }
    json!({
        "action": "next", "episode": coord(next), "fraction": 0.0,
        "reason": "the next episode after the furthest finished",
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
