//! "New episode" and "Coming up" (den#566): which series a viewer follows, which followed series has an episode out
//! that is new to them, and what each followed series releases next. The Apple apps and the web each held this rule
//! and disagreed: an episode was new for 7 days on one and 14 on the other, and "already started" was any progress on
//! the TV but over 2% on the iPhone.
//!
//! Pure: the viewer's calendar day and den-atlas's release dates arrive as inputs. Read from the JSON by hand, with
//! plain loops and the input's own values reused in the answer: every browser core carries this op, Tizen 3's as
//! JavaScript, and derived readers, iterator adaptors and `json!` objects each cost those cores far more than the
//! rule is worth.

use crate::series::{day_number, RESUME_FLOOR};
use serde_json::{Map, Value};

/// How many calendar days after its date an episode can still be new. Older, it is an episode the viewer has not
/// watched: an ended show's last episode from years ago is not news.
pub const FRESH_DAYS: i64 = 7;
/// How many days ahead Coming up looks, today included.
pub const COMING_UP_DAYS: i64 = 30;

/// Season and episode.
type At = (i64, i64);

/// A release: what comes out, its day number, and its JSON (`date`, `name`) as atlas wrote it.
struct Release<'a> {
    at: At,
    day: i64,
    value: &'a Value,
}

/// A series on Continue Watching: the row's episode, whether the row waits for it, and whether the viewer started it.
struct Row {
    series: u64,
    entry: At,
    upcoming: bool,
    started: bool,
}

fn at(value: &Value) -> Option<At> {
    Some((value["season"].as_i64()?, value["episode"].as_i64()?))
}

/// An episode or premiere whose date is a calendar day; anything else is no release.
fn release(value: &Value, at: Option<At>) -> Option<Release<'_>> {
    Some(Release {
        at: at?,
        day: day_number(value["date"].as_str()?)?,
        value,
    })
}

/// Every episode atlas dates for a series — `previous`, `next`, then `upcoming`, each coordinate once — and its
/// season premiere, as that season's first episode.
fn dated(airing: &Value) -> (Vec<Release<'_>>, Option<Release<'_>>) {
    let mut episodes: Vec<Release> = Vec::new();
    let upcoming = airing["upcoming"].as_array().map_or(&[][..], Vec::as_slice);
    for value in [&airing["previous"], &airing["next"]]
        .into_iter()
        .chain(upcoming)
    {
        if let Some(found) = release(value, at(value)) {
            if !episodes.iter().any(|e| e.at == found.at) {
                episodes.push(found);
            }
        }
    }
    let premiere = &airing["premiere"];
    let premiere = release(premiere, premiere["season"].as_i64().map(|s| (s, 1)));
    (episodes, premiere)
}

/// Whether a viewer whose Continue Watching entry is `entry` is caught up to `new`: nothing aired and unwatched
/// predates the release that brought it. On the episode itself they are; earlier in the same season they are when
/// that season came out in one release (atlas dates its premiere the new episode's day), since everything they have
/// left came out with it. Weekly, on S4E2 when S4E5 comes out, they are behind.
fn caught_up(entry: At, new: At, episodes: &[Release], premiere: &Option<Release>) -> bool {
    if entry == new {
        return true;
    }
    let Some(premiere) = premiere else {
        return false;
    };
    let mut released = None;
    for episode in episodes {
        if episode.at == new {
            released = Some(episode.day);
        }
    }
    if released.is_none() && premiere.at == new {
        released = Some(premiere.day);
    }
    entry.0 == new.0 && entry.1 < new.1 && premiere.at.0 == new.0 && released == Some(premiere.day)
}

/// The series' ids under `key`: none when absent, `None` when it is not a list of ids.
fn ids(input: &Value, key: &str) -> Option<Vec<u64>> {
    let mut ids = Vec::new();
    match &input[key] {
        Value::Null => {}
        value => {
            for id in value.as_array()? {
                ids.push(id.as_u64()?);
            }
        }
    }
    Some(ids)
}

#[inline(never)]
fn object(fields: Vec<(&str, Value)>) -> Value {
    let mut object = Map::new();
    for (key, value) in fields {
        object.insert(key.to_string(), value);
    }
    Value::Object(object)
}

fn episode(series: u64, at: At) -> Vec<(&'static str, Value)> {
    vec![
        ("series", series.into()),
        ("season", at.0.into()),
        ("episode", at.1.into()),
    ]
}

/// The followed series' new episodes and Coming up row.
///
/// Input: `today` (`YYYY-MM-DD`, the viewer's day; without it nothing is judged new or coming up, and the answer is
/// who is followed); the library's facts, as series ids: `watchlist`, `caught_up` (off the row, watched up to the
/// newest aired episode), `seen` (marked Seen as a whole) and `dismissed` (taken off the row since last watched);
/// `continuing`, each series on Continue Watching as `{series, season, episode, upcoming, fraction, watched}` — the
/// row's episode, whether the row waits for it to come out, and the viewer's progress in that episode; and `airing`,
/// den-atlas's answer per series as `{series, previous, next, upcoming, premiere}`, each episode `{season, episode,
/// date, name}` and the premiere `{season, date}`.
///
/// Answer:
/// - `candidates`: every series that might be followed, which atlas is asked about: the Watchlist's, the row's,
///   the caught-up and the Seen ones, in that order, each once, leaving out the dismissed.
/// - `followed`: those followed, in the same order. One on the Watchlist, caught up or marked Seen (unless the row
///   holds it, which then decides); on the row, one waiting for its episode, past atlas's newest, or caught up to
///   it. A series sampled once or seasons behind is not followed.
/// - `released`: each followed series' newest regular episode out within `FRESH_DAYS`, `{series, season, episode}`,
///   whatever the viewer's standing on it: the series whose progress a client reads to say whether it was started.
/// - `new`: each followed series' new episode, `{series, season, episode, released, name}`. `released` is the
///   newest regular episode out within `FRESH_DAYS`; `season`/`episode` is what plays, the series' row entry (kept
///   for a series the Watchlist names first), which the viewer must be caught up to (S2E1 of a season out at once,
///   never its finale) and must not have started: neither watched nor past `RESUME_FLOOR`, the row's own "barely
///   begun". `name` is the released episode's when that is what plays. Whether sources exist for it is the
///   client's search.
/// - `coming_up`: each followed series' soonest release out today or within `COMING_UP_DAYS` (a season's first
///   episode is its premiere), `{series, season, episode, date, premiere}`, soonest first, then by series.
pub fn new_episodes(input: &Value) -> Result<Value, String> {
    answer(input).ok_or_else(|| "invalid_request".to_string())
}

fn answer(input: &Value) -> Option<Value> {
    let today = match &input["today"] {
        Value::Null => None,
        day => Some(day_number(day.as_str()?)?),
    };
    let dismissed = ids(input, "dismissed")?;
    let mut rows: Vec<Row> = Vec::new();
    if !input["continuing"].is_null() {
        for entry in input["continuing"].as_array()? {
            rows.push(Row {
                series: entry["series"].as_u64()?,
                entry: at(entry)?,
                upcoming: entry["upcoming"] == true,
                started: entry["watched"] == true
                    || entry["fraction"].as_f64().unwrap_or(0.0) > RESUME_FLOOR,
            });
        }
    }
    // Each series once, by its first listing: `true` when that is the row, which then decides whether it is followed.
    let mut listed: Vec<(u64, bool)> = Vec::new();
    for id in ids(input, "watchlist")? {
        listed.push((id, false));
    }
    for row in &rows {
        listed.push((row.series, true));
    }
    for key in ["caught_up", "seen"] {
        for id in ids(input, key)? {
            listed.push((id, false));
        }
    }
    let mut candidates: Vec<(u64, bool)> = Vec::new();
    for (id, by_row) in listed {
        if id > 0 && !dismissed.contains(&id) && !candidates.iter().any(|c| c.0 == id) {
            candidates.push((id, by_row));
        }
    }
    let airing = match &input["airing"] {
        Value::Null => &[][..],
        list => list.as_array()?.as_slice(),
    };
    for one in airing {
        one["series"].as_u64()?;
    }

    let (mut followed, mut fresh, mut new) = (Vec::new(), Vec::new(), Vec::new());
    let mut coming_up: Vec<(i64, u64, Value)> = Vec::new();
    for &(id, by_row) in &candidates {
        let answer = airing.iter().find(|a| a["series"].as_u64() == Some(id));
        let (episodes, premiere) = answer.map(dated).unwrap_or_default();
        // A series on the Watchlist keeps its place on the row too: that is still the episode that plays.
        let row = rows.iter().find(|r| r.series == id);
        if let Some(row) = row.filter(|r| by_row && !r.upcoming) {
            // Past atlas's newest is out by TMDB's count only, so nothing came out after it either.
            let newest = answer.and_then(|a| release(&a["previous"], at(&a["previous"])));
            let newest = newest.map(|r| r.at);
            let follows = newest.is_some_and(|newest| {
                row.entry > newest || caught_up(row.entry, newest, &episodes, &premiere)
            });
            if !follows {
                continue;
            }
        }
        followed.push(Value::from(id));
        let (Some(today), Some(_)) = (today, answer) else {
            continue;
        };
        // The newest regular episode out within `FRESH_DAYS` (a dated episode, never the premiere alone), and the
        // soonest release from today on.
        let mut released: Option<&Release> = None;
        let mut soonest: Option<&Release> = None;
        for (index, one) in episodes.iter().chain(&premiere).enumerate() {
            let days = one.day - today;
            if one.at.0 <= 0 {
                continue;
            }
            if index < episodes.len()
                && (-FRESH_DAYS..=0).contains(&days)
                && released.is_none_or(|r| one.at > r.at)
            {
                released = Some(one);
            }
            if (0..=COMING_UP_DAYS).contains(&days)
                && soonest.is_none_or(|s| (one.day, one.at) < (s.day, s.at))
            {
                soonest = Some(one);
            }
        }
        if let Some(released) = released {
            fresh.push(object(episode(id, released.at)));
        }
        if let (Some(released), Some(row)) = (released, row) {
            if !row.started && caught_up(row.entry, released.at, &episodes, &premiere) {
                let name = if row.entry == released.at {
                    released.value["name"].clone()
                } else {
                    Value::Null
                };
                let mut fields = episode(id, row.entry);
                fields.push(("released", object(episode(id, released.at)[1..].to_vec())));
                fields.push(("name", name));
                new.push(object(fields));
            }
        }
        if let Some(soonest) = soonest {
            let mut fields = episode(id, soonest.at);
            fields.push(("date", soonest.value["date"].clone()));
            fields.push(("premiere", (soonest.at.1 == 1).into()));
            // Placed rather than sorted: a generic sort costs the browser cores more than this row is worth.
            let place = coming_up
                .iter()
                .take_while(|c| (c.0, c.1) <= (soonest.day, id))
                .count();
            coming_up.insert(place, (soonest.day, id, object(fields)));
        }
    }
    let candidates = candidates.iter().map(|c| Value::from(c.0)).collect();
    let coming_up = coming_up.into_iter().map(|c| c.2).collect();
    Some(object(vec![
        ("candidates", Value::Array(candidates)),
        ("followed", Value::Array(followed)),
        ("released", Value::Array(fresh)),
        ("new", Value::Array(new)),
        ("coming_up", Value::Array(coming_up)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
