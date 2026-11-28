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
    /// The episode *finished* most recently, and when. Not the same as `mark`, and not the furthest: a rewatch
    /// of an earlier episode, or an autoplay that ran on unwatched and was rewound, continues from what was
    /// actually watched last.
    pub finished: Option<Finished>,
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
    /// The release dates the client knows for this series' episodes: den-atlas's airing answer (its previous,
    /// next and upcoming episodes), else TMDB's next episode to air. Only consulted once the series is caught up.
    #[serde(default)]
    pub airing: Vec<DatedEpisode>,
    /// The viewer's calendar day, `YYYY-MM-DD`, that `airing`'s dates are counted from. Without it no date is
    /// judged and a caught-up series drops as it always did.
    #[serde(default)]
    pub today: Option<String>,
    /// Whether the series has ended. An ended series is not kept waiting for an episode.
    #[serde(default)]
    pub ended: bool,
}

/// One episode's release, as a calendar day (`YYYY-MM-DD`): the day it comes out wherever the viewer is.
#[derive(Deserialize)]
pub struct DatedEpisode {
    pub season: i64,
    pub episode: i64,
    pub date: String,
}

/// How many calendar days ahead a caught-up series' next episode may be and still keep the series on the row.
/// Further out it is a series between episodes or seasons, which "Coming back soon" shows instead.
pub const UPCOMING_DAYS: i64 = 14;

/// Days since 1970-01-01 of a `YYYY-MM-DD` date, or `None` when it is not one (Hinnant's `days_from_civil`).
pub(crate) fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day): (i64, i64, i64) =
        (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// A caught-up series' place on the row from its next episode's release date, or `None` to drop it.
///
/// `listed` is the episode the season layout puts after `front`; when the layout lists nothing after it (a next
/// season TMDB has not laid out yet) the earliest dated episode past `front` stands in. A date that has come means
/// the episode is out even though the layout's last aired episode has not caught up with it: it is offered as the
/// next episode. One within `UPCOMING_DAYS` keeps the series on the row, waiting for it.
fn airing_soon(front: Coord, listed: Option<Coord>, input: &ContinueInput) -> Option<Value> {
    if input.ended {
        return None;
    }
    let today = day_number(input.today.as_deref()?)?;
    let (dated, day) = input
        .airing
        .iter()
        .filter(|d| d.season > 0)
        .filter(|d| match listed {
            Some(next) => (d.season, d.episode) == (next.season, next.episode),
            None => (d.season, d.episode) > (front.season, front.episode),
        })
        .filter_map(|d| day_number(&d.date).map(|day| (d, day)))
        .min_by_key(|(d, _)| (d.season, d.episode))?;
    let at = Coord {
        season: dated.season,
        episode: dated.episode,
    };
    let days = day - today;
    if days <= 0 {
        return Some(json!({
            "action": "next", "code": "next", "episode": coord(at), "fraction": 0.0,
            "reason": "the next episode is out by its release date",
        }));
    }
    if days > UPCOMING_DAYS {
        return None;
    }
    Some(json!({
        "action": "upcoming", "code": "upcoming", "episode": coord(at), "fraction": 0.0, "date": dated.date,
        "reason": "caught up: the next episode comes out within 14 days",
    }))
}

#[derive(Deserialize)]
pub struct Finished {
    pub season: i64,
    pub episode: i64,
    /// When it was finished. A client with no time for it is judged by position, as a flag is.
    #[serde(default)]
    pub at: Option<i64>,
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
/// - A series' place is decided by the episode *finished* most recently, not the one touched last. Watching
///   E1-E4 and then opening E5 for two seconds leaves the newest mark on E5, below the resume floor — and
///   reading that alone concludes nothing is finished and drops a series being actively watched.
/// - Most recently, not furthest: rewatching E1 after an autoplay ran on through E2 unwatched continues at E2,
///   and an episode left part-way after that resumes even when it is earlier than something finished.
/// - A bare watched flag counts. A tracker pull writes nothing else, so a series watched through SIMKL or on
///   another device was watched everywhere except the row whose job is to offer its next episode.
/// - The resume floor suppresses a *resume*, not the series: when the front carries it further, it still
///   belongs on the row as the next episode.
/// - A title-level "watched" cannot expire — a tracker reporting a series completed hid shows still airing —
///   so where the layout is known, the episodes decide. With no layout there is nothing better than the flag.
/// - Caught up is not gone: a series whose next episode comes out within `UPCOMING_DAYS` stays, as `upcoming`
///   with that episode and its `date`. Watching the one aired episode of a new season emptied the row of the
///   show being followed most closely.
///
/// `reason` comes back with every answer so a client can say why a series is missing without a debugger.
pub fn continue_entry(input: &ContinueInput) -> Value {
    // `code` is what a client may branch on; `reason` is for whoever reads the log. A client that has to match
    // on prose to tell "dismissed" from "I have not fetched the layout yet" will get it wrong, and did.
    let none = |code: &str, reason: &str| json!({"action": "none", "code": code, "episode": null, "fraction": 0.0, "reason": reason});
    let layout_known = !regular(&input.seasons).is_empty();
    if input.title_watched && !layout_known {
        return none("title_watched", "title watched, no layout to judge it by");
    }
    let activity = input.mark.as_ref().map(|m| m.at).unwrap_or(i64::MIN);
    if input.dismissed_at.is_some_and(|at| at >= activity) {
        return none("dismissed", "dismissed");
    }
    // A flag has no time, so it can only be weighed by position: one past what was finished last is watched
    // somewhere this library has no time for, and that is still how far the series got.
    let finished = input.finished.as_ref();
    let finished_at = finished.map(|f| Coord {
        season: f.season,
        episode: f.episode,
    });
    let (front, front_time) = match (finished_at, input.flag) {
        (Some(f), Some(flag)) if (flag.season, flag.episode) > (f.season, f.episode) => {
            (Some(flag), None)
        }
        (Some(f), _) => (Some(f), finished.and_then(|f| f.at)),
        (None, flag) => (flag, None),
    };
    if let Some(mark) = &input.mark {
        let at = Coord {
            season: mark.season,
            episode: mark.episode,
        };
        // Touched after the front was finished; with no time for the front, past it.
        let ahead = match (front, front_time) {
            (None, _) => true,
            (Some(_), Some(time)) => mark.at > time,
            (Some(f), None) => (f.season, f.episode) < (at.season, at.episode),
        };
        if mark.fraction > RESUME_FLOOR && mark.fraction < WATCHED && ahead {
            return json!({
                "action": "resume", "code": "resume", "episode": coord(at), "fraction": mark.fraction,
                "reason": "resuming where it was left",
            });
        }
    }
    let Some(front) = front else {
        return none(
            "nothing_finished",
            "nothing finished, and no mark past the resume floor",
        );
    };
    if !layout_known {
        return none("no_layout", "no season layout known");
    }
    let Some(next) = next_after(front, &input.seasons) else {
        return airing_soon(front, None, input)
            .unwrap_or_else(|| none("finished", "finished: nothing listed after it"));
    };
    if !is_aired(next, input.last_aired) {
        return airing_soon(front, Some(next), input)
            .unwrap_or_else(|| none("not_aired", "caught up: the next episode has not aired"));
    }
    json!({
        "action": "next", "code": "next", "episode": coord(next), "fraction": 0.0,
        "reason": "the next episode after the one finished last",
    })
}

/// The Continue Watching row, from every started series and every in-progress film: who is on it, and in what order.
///
/// Input: `series` (each: `id`, the client's key for the title; `input`, the `continue_entry` fields; `progress_at`
/// and `added_at`, the library record's own times, when it has one) and `films` (`id`, `progress_at`,
/// `dismissed_at`). Read from the JSON by hand, and `input` nested: a derived `Deserialize` per shape, and above all
/// serde's `flatten`, cost the browser cores far more code than this op is worth.
///
/// Three clients each assembled this and disagreed, so the assembly is the policy's:
///
/// - A series is on the row by `continue_entry`, and only by it. A library record that says a series is in progress
///   never adds it: the episode decision already weighed that series, and a caught-up or dismissed one it dropped
///   would otherwise come back. Only a *film* is added from its record, unless dismissed at or after its last play.
/// - A series with no season layout yet (`no_layout`) stays on the row at its latest mark, as `hold`, rather than
///   vanishing until the layout is fetched. A finished mark comes back at fraction 0 with `finished`: nothing is
///   known to offer after it, so a full bar would claim a finish the pending fetch is about to advance.
/// - Newest first, by the latest of the series' mark, the record's last play and its added time (a series known only
///   by watched flags carries no mark of its own, and would sink to the end); a film by its last play. Ties keep
///   series ahead of films, then the order given. An `id` is the client's key for one title (its media type and id),
///   given once: no row is deduplicated here.
///
/// Each row: `kind` (`series` or `film`), `id`, `activity`. A series' row adds where it is placed: `action`,
/// `episode`, `fraction`, and `date` for an `upcoming` one; a held one has `action: "hold"`, the mark's `episode`
/// and `fraction`, and `finished`. The answer's `code` and `reason` stay out: they explain a series that is *not* on
/// the row, which a client asks `continue_entry` about, and carrying them cost every row call more than the rule.
/// A film's row carries no more: its progress and display are the client's own record.
///
/// A client that keeps each series' `continue_entry` answer passes it as `entry`, with the series' `mark`, in place
/// of `input`: that series is placed by the answer it already has rather than decided again, so a write that changed
/// one series costs one decision, not one per started series. Of the `none` answers only `no_layout` can be on the
/// row (held), so a series answered `none` with any other `code` adds nothing and may be left out.
pub fn continue_row(input: Value) -> Result<Value, String> {
    rows_of(input).ok_or_else(|| "invalid_request".to_string())
}

fn rows_of(mut input: Value) -> Option<Value> {
    let mut list = |key: &str| match input.get_mut(key).map(Value::take) {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(items)) => Some(items),
        Some(_) => None,
    };
    let (series, films) = (list("series")?, list("films")?);
    let mut rows = Vec::new();
    for mut item in series {
        let (mut row, mark) = match item.get_mut("entry").map(Value::take) {
            Some(entry) if entry.is_object() => {
                let mark = item.get_mut("mark").map(Value::take).unwrap_or(Value::Null);
                (
                    entry,
                    serde_json::from_value::<Option<ContinueMark>>(mark).ok()?,
                )
            }
            Some(_) => return None,
            None => {
                let input: ContinueInput =
                    serde_json::from_value(item.get_mut("input")?.take()).ok()?;
                (continue_entry(&input), input.mark)
            }
        };
        let id = item["id"].as_str()?;
        let activity = mark
            .as_ref()
            .map(|m| m.at)
            .max(item["progress_at"].as_i64())
            .max(item["added_at"].as_i64())
            .unwrap_or(0);
        match mark.as_ref().filter(|_| row["code"] == "no_layout") {
            Some(mark) => {
                let finished = mark.fraction >= WATCHED;
                row["action"] = "hold".into();
                row["episode"] = coord(Coord {
                    season: mark.season,
                    episode: mark.episode,
                });
                row["fraction"] = (if finished { 0.0 } else { mark.fraction }).into();
                row["finished"] = finished.into();
            }
            None if row["action"] == "none" => continue,
            None => {}
        }
        if let Some(fields) = row.as_object_mut() {
            fields.remove("code");
            fields.remove("reason");
        }
        place(&mut rows, row, "series", id, activity);
    }
    for item in &films {
        let id = item["id"].as_str()?;
        let played = item["progress_at"].as_i64()?;
        if item["dismissed_at"].as_i64().is_some_and(|at| at >= played) {
            continue;
        }
        place(&mut rows, json!({}), "film", id, played);
    }
    Some(json!({ "rows": rows }))
}

/// One `continue_row` row, named and placed after every row at least as new: ties keep series ahead of films, then
/// the order given. Not a `sort_by_key`, whose generic stable sort costs the browser cores tens of kilobytes for a
/// few hundred rows at most; and one out-of-line function, so the startup core carries this code once.
#[inline(never)]
fn place(rows: &mut Vec<Value>, mut row: Value, kind: &str, id: &str, activity: i64) {
    row["kind"] = kind.into();
    row["id"] = id.into();
    row["activity"] = activity.into();
    let at = rows.partition_point(|newer| newer["activity"].as_i64() >= Some(activity));
    rows.insert(at, row);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Dexter: Resurrection as it stood on 2026-10-31: S2E1 finished yesterday, the only aired episode of S2.
    fn dexter(next_date: &str, extra: Value) -> ContinueInput {
        let mut input = json!({
            "mark": {"season": 2, "episode": 1, "fraction": 1.0, "at": 2000},
            "finished": {"season": 2, "episode": 1, "at": 2000},
            "flag": null,
            "seasons": [{"season": 1, "episodes": 10}, {"season": 2, "episodes": 10}],
            "last_aired": {"season": 2, "episode": 1},
            "dismissed_at": null,
            "airing": [
                {"season": 2, "episode": 1, "date": "2026-10-30"},
                {"season": 2, "episode": 2, "date": next_date},
            ],
            "today": "2026-10-31",
        });
        for (key, value) in extra.as_object().into_iter().flatten() {
            input[key] = value.clone();
        }
        serde_json::from_value(input).unwrap()
    }

    #[test]
    fn a_row_carries_an_upcoming_date_and_keeps_series_ahead_of_a_film_on_a_tie() {
        let series = json!({
            "id": "tv:1",
            "input": {
                "mark": {"season": 2, "episode": 1, "fraction": 1.0, "at": 2000},
                "finished": {"season": 2, "episode": 1, "at": 2000},
                "flag": null,
                "seasons": [{"season": 1, "episodes": 10}, {"season": 2, "episodes": 10}],
                "last_aired": {"season": 2, "episode": 1},
                "dismissed_at": null,
                "airing": [{"season": 2, "episode": 2, "date": "2026-11-06"}],
                "today": "2026-10-31",
            },
            "progress_at": 2000,
        });
        let input = json!({
            "series": [series],
            "films": [{"id": "movie:1", "progress_at": 2000, "dismissed_at": null}],
        });
        let rows = continue_row(input).unwrap()["rows"].clone();
        assert_eq!(rows[0]["id"], "tv:1");
        assert_eq!(rows[0]["action"], "upcoming");
        assert_eq!(rows[0]["date"], "2026-11-06");
        assert_eq!(rows[1]["id"], "movie:1");
        assert_eq!(rows[1]["kind"], "film");
    }

    #[test]
    fn a_series_passed_with_its_answer_is_placed_as_if_decided_here() {
        let layout = json!([{"season": 1, "episodes": 10}]);
        let inputs = [
            json!({"mark": {"season": 1, "episode": 2, "fraction": 0.4, "at": 5000}, "seasons": layout}),
            json!({"flag": {"season": 1, "episode": 2}, "seasons": layout}),
            json!({"mark": {"season": 1, "episode": 2, "fraction": 1.0, "at": 4000},
                   "finished": {"season": 1, "episode": 2, "at": 4000}}),
            json!({"mark": {"season": 1, "episode": 3, "fraction": 0.5, "at": 3000}, "seasons": layout,
                   "dismissed_at": 6000}),
            json!({"mark": {"season": 1, "episode": 10, "fraction": 1.0, "at": 2000},
                   "finished": {"season": 1, "episode": 10, "at": 2000}, "seasons": layout}),
        ];
        let films = json!([{"id": "movie:1", "progress_at": 4000, "dismissed_at": null}]);
        let raw: Vec<Value> = inputs
            .iter()
            .enumerate()
            .map(|(n, input)| json!({"id": format!("tv:{n}"), "input": input, "progress_at": 3500}))
            .collect();
        let decided: Vec<Value> = inputs
            .iter()
            .enumerate()
            .map(|(n, input)| {
                let answer = continue_entry(&serde_json::from_value(input.clone()).unwrap());
                json!({"id": format!("tv:{n}"), "entry": answer, "mark": input.get("mark"), "progress_at": 3500})
            })
            .collect();
        let expected = continue_row(json!({"series": raw, "films": films})).unwrap();
        assert_eq!(expected["rows"].as_array().unwrap().len(), 4);
        assert_eq!(
            continue_row(json!({"series": decided, "films": films})).unwrap(),
            expected
        );
        assert!(continue_row(json!({"series": [{"id": "tv:1", "entry": "next"}]})).is_err());
    }

    #[test]
    fn day_numbers_count_calendar_days() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(
            day_number("2026-11-06")
                .zip(day_number("2026-10-31"))
                .map(|(a, b)| a - b),
            Some(6)
        );
        assert_eq!(
            day_number("2024-03-01")
                .zip(day_number("2024-02-28"))
                .map(|(a, b)| a - b),
            Some(2)
        );
        assert_eq!(day_number("2026-13-01"), None);
        assert_eq!(day_number("2026-11-6"), None);
        assert_eq!(day_number("2026-11-06T00:00"), None);
    }

    #[test]
    fn a_caught_up_series_waits_for_an_episode_out_within_the_window() {
        let answer = continue_entry(&dexter("2026-11-06", json!({})));
        assert_eq!(answer["action"], "upcoming");
        assert_eq!(answer["code"], "upcoming");
        assert_eq!(answer["episode"], json!({"season": 2, "episode": 2}));
        assert_eq!(answer["date"], "2026-11-06");
        // The window's last day still counts.
        assert_eq!(
            continue_entry(&dexter("2026-11-14", json!({})))["action"],
            "upcoming"
        );
    }

    #[test]
    fn a_caught_up_series_further_out_drops() {
        let answer = continue_entry(&dexter("2026-11-20", json!({})));
        assert_eq!(answer["code"], "not_aired");
        assert_eq!(
            continue_entry(&dexter("2026-11-15", json!({})))["code"],
            "not_aired"
        );
    }

    #[test]
    fn an_ended_series_drops() {
        let answer = continue_entry(&dexter("2026-11-06", json!({"ended": true})));
        assert_eq!(answer["code"], "not_aired");
    }

    #[test]
    fn no_today_judges_no_date() {
        let answer = continue_entry(&dexter("2026-11-06", json!({"today": null})));
        assert_eq!(answer["code"], "not_aired");
    }

    #[test]
    fn a_date_that_has_come_offers_the_episode_though_the_layout_lags() {
        let answer = continue_entry(&dexter("2026-10-31", json!({})));
        assert_eq!(answer["action"], "next");
        assert_eq!(answer["episode"], json!({"season": 2, "episode": 2}));
    }

    #[test]
    fn only_the_episode_after_the_front_is_judged() {
        // S2E3 within the window says nothing about S2E2, which is the one the viewer is waiting for.
        let answer = continue_entry(&dexter(
            "2026-12-01",
            json!({"airing": [{"season": 2, "episode": 3, "date": "2026-11-06"}]}),
        ));
        assert_eq!(answer["code"], "not_aired");
    }

    #[test]
    fn a_season_not_laid_out_yet_waits_for_its_dated_premiere() {
        let answer = continue_entry(&dexter(
            "2026-11-06",
            json!({
                "seasons": [{"season": 1, "episodes": 10}],
                "last_aired": {"season": 1, "episode": 10},
                "finished": {"season": 1, "episode": 10, "at": 2000},
                "mark": {"season": 1, "episode": 10, "fraction": 1.0, "at": 2000},
                "airing": [{"season": 2, "episode": 1, "date": "2026-11-03"}],
            }),
        ));
        assert_eq!(answer["action"], "upcoming");
        assert_eq!(answer["episode"], json!({"season": 2, "episode": 1}));
    }

    #[test]
    fn an_aired_unwatched_episode_still_wins() {
        let answer = continue_entry(&dexter(
            "2026-11-06",
            json!({"last_aired": {"season": 2, "episode": 2}}),
        ));
        assert_eq!(answer["action"], "next");
        assert_eq!(answer["episode"], json!({"season": 2, "episode": 2}));
    }

    #[test]
    fn a_rewatch_in_progress_still_resumes() {
        let answer = continue_entry(&dexter(
            "2026-11-06",
            json!({"mark": {"season": 1, "episode": 3, "fraction": 0.4, "at": 3000}}),
        ));
        assert_eq!(answer["action"], "resume");
        assert_eq!(answer["episode"], json!({"season": 1, "episode": 3}));
    }
}
