//! "New episode" and "Coming up" (den#566): which series a viewer follows, which followed series has an episode out
//! that is new to them, and what each followed series releases next. The Apple apps and the web each held this rule
//! and disagreed: an episode was new for 7 days on one and 14 on the other, and "already started" was any progress on
//! the TV but over 2% on the iPhone.
//!
//! Pure: the viewer's calendar day and den-atlas's release dates arrive as inputs.

use crate::series::{day_number, RESUME_FLOOR};
use serde_json::{json, Value};

/// How many calendar days after its date an episode can still be new. Older, it is an episode the viewer has not
/// watched: an ended show's last episode from years ago is not news.
pub const FRESH_DAYS: i64 = 7;
/// How many days ahead Coming up looks, today included.
pub const COMING_UP_DAYS: i64 = 30;

type At = (i64, i64);

struct Release<'a> {
    at: At,
    day: i64,
    date: &'a str,
    name: Option<&'a str>,
}

/// One series' den-atlas answer: `previous` (its newest episode out), every dated episode (`previous`, `next`, then
/// `upcoming`, each coordinate once), and the season premiere.
struct Dates<'a> {
    previous: Option<At>,
    dated: Vec<Release<'a>>,
    /// Season, day number, and the date as atlas wrote it.
    premiere: Option<(i64, i64, &'a str)>,
}

/// A series on Continue Watching: the row's episode, whether the row waits for it, and whether the viewer started it.
struct Row {
    series: u64,
    entry: At,
    upcoming: bool,
    started: bool,
}

fn at(value: &Value) -> Option<At> {
    Some((
        value.get("season")?.as_i64()?,
        value.get("episode")?.as_i64()?,
    ))
}

/// An episode with a date that is a calendar day; anything else is no release.
fn release(value: &Value) -> Option<Release<'_>> {
    let date = value.get("date")?.as_str()?;
    Some(Release {
        at: at(value)?,
        day: day_number(date)?,
        date,
        name: value.get("name").and_then(Value::as_str),
    })
}

fn dates(value: &Value) -> Dates<'_> {
    let mut dated: Vec<Release> = Vec::new();
    let listed = [value.get("previous"), value.get("next")];
    let upcoming = value.get("upcoming").and_then(Value::as_array);
    for one in listed
        .into_iter()
        .flatten()
        .chain(upcoming.into_iter().flatten())
    {
        if let Some(one) = release(one).filter(|r| !dated.iter().any(|d| d.at == r.at)) {
            dated.push(one);
        }
    }
    let premiere = value.get("premiere").and_then(|p| {
        let date = p.get("date")?.as_str()?;
        Some((p.get("season")?.as_i64()?, day_number(date)?, date))
    });
    Dates {
        previous: value.get("previous").and_then(release).map(|r| r.at),
        dated,
        premiere,
    }
}

/// The day atlas gives an episode: its own date, or for a first episode its season's premiere.
fn day_of(dates: &Dates, episode: At) -> Option<i64> {
    match dates.dated.iter().find(|d| d.at == episode) {
        Some(found) => Some(found.day),
        None => dates
            .premiere
            .filter(|p| episode.1 == 1 && p.0 == episode.0)
            .map(|p| p.1),
    }
}

/// Whether a viewer whose Continue Watching entry is `entry` is caught up to `new`: nothing aired and unwatched
/// predates the release that brought it. On the episode itself they are; earlier in the same season they are when
/// that season came out in one release (atlas dates its premiere the new episode's day), since everything they have
/// left came out with it. Weekly, on S4E2 when S4E5 comes out, they are behind; with no entry nothing says they are
/// caught up.
fn caught_up(entry: Option<At>, new: At, dates: Option<&Dates>) -> bool {
    let Some(entry) = entry else { return false };
    if entry == new {
        return true;
    }
    let (Some(dates), true) = (dates, entry.0 == new.0 && entry.1 < new.1) else {
        return false;
    };
    match (dates.premiere, day_of(dates, new)) {
        (Some((season, premiere, _)), Some(released)) => season == new.0 && premiere == released,
        _ => false,
    }
}

/// The episode out now that may be new: the newest regular episode atlas dates whose day has come, at most
/// `FRESH_DAYS` ago.
fn due<'a>(dates: &'a Dates, today: i64) -> Option<&'a Release<'a>> {
    dates
        .dated
        .iter()
        .filter(|d| d.at.0 > 0 && (-FRESH_DAYS..=0).contains(&(d.day - today)))
        .max_by_key(|d| d.at)
}

/// The series' ids under `key`: none when absent, `None` when it is not a list of ids.
fn ids(input: &Value, key: &str) -> Option<Vec<u64>> {
    match input.get(key) {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(value) => value.as_array()?.iter().map(Value::as_u64).collect(),
    }
}

/// The followed series' new episodes and Coming up row.
///
/// Input: `today` (`YYYY-MM-DD`, the viewer's day; without it nothing is judged new or coming up, and the answer is
/// who is followed); the library's facts, as series ids: `watchlist`, `caught_up`
/// (off the row, watched up to the newest aired episode), `seen` (marked Seen as a whole) and `dismissed` (taken
/// off the row since last watched); `continuing`, each series on Continue Watching as `{series, season, episode,
/// upcoming, fraction, watched}` — the row's episode, whether the row waits for it to come out, and the viewer's
/// progress in that episode; and `airing`, den-atlas's answer per series as `{series, previous, next, upcoming,
/// premiere}`, each episode `{season, episode, date, name}` and the premiere `{season, date}`.
///
/// Answer:
/// - `candidates`: every series that might be followed, which atlas is asked about: the Watchlist's, the row's,
///   the caught-up and the Seen ones, in that order, each once, leaving out the dismissed.
/// - `followed`: those followed, in the same order. One on the Watchlist, caught up or marked Seen (unless the row
///   holds it, which then decides); on the row, one waiting for its episode, past atlas's newest, or caught up to
///   it. A series sampled once or seasons behind is not followed.
/// - `new`: each followed series' new episode, `{series, season, episode, released, name}`. `released` is the
///   newest regular episode out within `FRESH_DAYS`; `season`/`episode` is what plays, the series' row entry, which
///   the viewer must be caught up to (S2E1 of a season out at once, never its finale) and must not have started:
///   neither watched nor past `RESUME_FLOOR`, the row's own "barely begun". `name` is the released episode's when
///   that is what plays. Whether sources exist for it is the client's search.
/// - `coming_up`: each followed series' soonest release out today or within `COMING_UP_DAYS` (a season's first
///   episode is its premiere), `{series, season, episode, date, premiere}`, soonest first, then by series.
pub fn new_episodes(input: &Value) -> Result<Value, String> {
    answer(input).ok_or_else(|| "invalid_request".to_string())
}

fn answer(input: &Value) -> Option<Value> {
    let today = match input.get("today") {
        None | Some(Value::Null) => None,
        Some(day) => Some(day_number(day.as_str()?)?),
    };
    let dismissed = ids(input, "dismissed")?;
    let mut rows: Vec<Row> = Vec::new();
    if let Some(list) = input.get("continuing").filter(|v| !v.is_null()) {
        for entry in list.as_array()? {
            rows.push(Row {
                series: entry.get("series")?.as_u64()?,
                entry: at(entry)?,
                upcoming: entry.get("upcoming").and_then(Value::as_bool) == Some(true),
                started: entry.get("watched").and_then(Value::as_bool) == Some(true)
                    || entry.get("fraction").and_then(Value::as_f64).unwrap_or(0.0) > RESUME_FLOOR,
            });
        }
    }
    // Each series once, by its first listing: `true` when that is the row, which then decides whether it is followed.
    let mut candidates: Vec<(u64, bool)> = Vec::new();
    let mut add = |id: u64, by_row: bool| {
        if id > 0 && !dismissed.contains(&id) && !candidates.iter().any(|c| c.0 == id) {
            candidates.push((id, by_row));
        }
    };
    for id in ids(input, "watchlist")? {
        add(id, false);
    }
    for row in &rows {
        add(row.series, true);
    }
    for id in ids(input, "caught_up")?
        .into_iter()
        .chain(ids(input, "seen")?)
    {
        add(id, false);
    }
    let mut airing: Vec<(u64, Dates)> = Vec::new();
    if let Some(list) = input.get("airing").filter(|v| !v.is_null()) {
        for one in list.as_array()? {
            airing.push((one.get("series")?.as_u64()?, dates(one)));
        }
    }
    let dates_of = |id: u64| airing.iter().find(|a| a.0 == id).map(|a| &a.1);

    let (mut followed, mut new) = (Vec::new(), Vec::new());
    let mut coming_up: Vec<(i64, u64, Value)> = Vec::new();
    for (id, by_row) in &candidates {
        let dates = dates_of(*id);
        // A series on the Watchlist keeps its place on the row too: that is still the episode that plays.
        let row = rows.iter().find(|r| r.series == *id);
        let follows = match row.filter(|_| *by_row) {
            None => true,
            Some(Row { upcoming: true, .. }) => true,
            Some(Row { entry, .. }) => dates
                .and_then(|d| d.previous.map(|newest| (d, newest)))
                .is_some_and(|(d, newest)| {
                    *entry > newest || caught_up(Some(*entry), newest, Some(d))
                }),
        };
        if !follows {
            continue;
        }
        followed.push(json!(id));
        let (Some(dates), Some(today)) = (dates, today) else {
            continue;
        };
        if let (Some(released), Some(row)) = (due(dates, today), row) {
            let plays = row.entry;
            if !row.started && caught_up(Some(plays), released.at, Some(dates)) {
                new.push(json!({
                    "series": id, "season": plays.0, "episode": plays.1,
                    "released": {"season": released.at.0, "episode": released.at.1},
                    "name": released.name.filter(|_| plays == released.at),
                }));
            }
        }
        let premiere = dates
            .premiere
            .map(|(season, day, date)| ((season, 1), day, date));
        let soonest = dates
            .dated
            .iter()
            .map(|d| (d.at, d.day, d.date))
            .chain(premiere)
            .filter(|(at, day, _)| at.0 > 0 && (0..=COMING_UP_DAYS).contains(&(day - today)))
            .min_by_key(|(at, day, _)| (*day, *at));
        if let Some((at, day, date)) = soonest {
            // Placed rather than sorted: a generic sort costs the browser cores more than this row is worth.
            let place = coming_up.partition_point(|c| (c.0, c.1) <= (day, *id));
            let release = json!({"series": id, "season": at.0, "episode": at.1, "date": date,
                                 "premiere": at.1 == 1});
            coming_up.insert(place, (day, *id, release));
        }
    }
    let candidates: Vec<Value> = candidates.iter().map(|c| json!(c.0)).collect();
    let coming_up: Vec<Value> = coming_up.into_iter().map(|c| c.2).collect();
    Some(
        json!({"candidates": candidates, "followed": followed, "new": new, "coming_up": coming_up}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_week_old_episode_is_new_and_an_eight_day_old_one_is_not() {
        let request = |today: &str| {
            json!({
                "today": today,
                "continuing": [{"series": 1, "season": 2, "episode": 1, "upcoming": false}],
                "airing": [{"series": 1, "previous": {"season": 2, "episode": 1, "date": "2026-10-30"}}],
            })
        };
        let new = |today| new_episodes(&request(today)).unwrap()["new"].clone();
        assert_eq!(new("2026-11-06").as_array().map(Vec::len), Some(1));
        assert_eq!(new("2026-11-07"), json!([]));
    }
}
