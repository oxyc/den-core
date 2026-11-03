//! The lists a client renders: Home's inputs, Continue Watching, Seen history, the watchlist and the newest stamp.
//!
//! Each is today's client fold moved here, step for step (den-edge `projectHomeLibraryView`, `continueCandidates`,
//! `watchedHistory`, `watchlist`, `newestSummary`). A tie between titles is broken by the byte order of their
//! `type:id` key (`key_cmp`): the order History already used, and the one Home's seeds now use too.

use super::derive::{self, floor, row, title, Row, TitleFacts, DAY};
use super::read::{At, Register, EMPTY, IN_PROGRESS, NONE, WATCHED, WATCHLIST};
use super::{code, indices, key_cmp, sort, Answer, Body, Column, Columns, Index};
use crate::series::{
    continue_entry, ContinueInput, ContinueMark, Coord, DatedEpisode, Finished, SeasonCount,
};
use serde_json::Value;
use std::cmp::Ordering;

const FINISHED: f64 = 0.95;

/// `(season, episodes)` per season, and the last aired episode.
type Layout = (Vec<(i64, i64)>, Option<(i64, i64)>);

/// `(season, episode, date)` per dated episode, and whether the series has ended.
type Dated = (Vec<(i64, i64, String)>, bool);

/// Series layouts by TMDB id: `{"1399": {"seasons": [{"season", "episodes"}], "last_aired": {…} | null}}`.
#[derive(Default)]
pub struct Layouts {
    at: Index<u64>,
    layouts: Vec<Layout>,
}

fn coord(v: &Value) -> Option<(i64, i64)> {
    Some((v["season"].as_i64()?, v["episode"].as_i64()?))
}

fn by_id<'a>(v: &'a Value, invalid: &str) -> Result<Vec<(u64, &'a Value)>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| Ok((k.parse().map_err(|_| invalid.to_owned())?, v)))
            .collect(),
        _ => Err(invalid.into()),
    }
}

fn parse(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|_| "invalid_request".to_owned())
}

impl Layouts {
    /// `read` of the layouts' JSON text.
    pub fn read_text(text: &str) -> Result<Self, String> {
        Self::read(&parse(text)?)
    }

    pub fn read(v: &Value) -> Result<Self, String> {
        let mut out = Layouts::default();
        for (id, layout) in by_id(v, "invalid_layout")? {
            let seasons = layout["seasons"]
                .as_array()
                .ok_or("invalid_layout")?
                .iter()
                .map(|s| Some((s["season"].as_i64()?, s["episodes"].as_i64()?)))
                .collect::<Option<Vec<_>>>()
                .ok_or("invalid_layout")?;
            let last = match &layout["last_aired"] {
                Value::Null => None,
                v => Some(coord(v).ok_or("invalid_layout")?),
            };
            out.at.insert(id, out.layouts.len());
            out.layouts.push((seasons, last));
        }
        Ok(out)
    }

    pub(super) fn seasons(&self, id: u64) -> (Vec<SeasonCount>, Option<Coord>) {
        match self.at.get(&id).map(|&i| &self.layouts[i]) {
            None => (Vec::new(), None),
            Some((seasons, last)) => (
                seasons
                    .iter()
                    .map(|&(season, episodes)| SeasonCount { season, episodes })
                    .collect(),
                last.map(|(season, episode)| Coord { season, episode }),
            ),
        }
    }
}

/// `continue_entry`'s airing: the viewer's day and each followed series' dated episodes, by TMDB id:
/// `{"today": "YYYY-MM-DD", "series": {"1399": {"episodes": [{"season", "episode", "date"}], "ended": bool}}}`.
pub struct Airing {
    today: String,
    at: Index<u64>,
    series: Vec<Dated>,
}

impl Airing {
    /// `read` of the airing's JSON text.
    pub fn read_text(text: &str) -> Result<Self, String> {
        Self::read(&parse(text)?)
    }

    pub fn read(v: &Value) -> Result<Self, String> {
        let mut out = Airing {
            today: v["today"].as_str().ok_or("invalid_airing")?.to_owned(),
            at: Index::new(),
            series: Vec::new(),
        };
        for (id, s) in by_id(&v["series"], "invalid_airing")? {
            let episodes = s["episodes"]
                .as_array()
                .ok_or("invalid_airing")?
                .iter()
                .map(|e| {
                    let date = e["date"].as_str()?.to_owned();
                    Some((e["season"].as_i64()?, e["episode"].as_i64()?, date))
                })
                .collect::<Option<Vec<_>>>()
                .ok_or("invalid_airing")?;
            out.at.insert(id, out.series.len());
            out.series.push((episodes, s["ended"] == true));
        }
        Ok(out)
    }

    fn get(&self, id: u64) -> Option<&Dated> {
        self.at.get(&id).map(|&i| &self.series[i])
    }
}

#[derive(Clone, Copy)]
struct Mark {
    id: u64,
    season: u64,
    episode: u64,
    fraction: f64,
    at: i64,
    seconds: Option<f64>,
    playing: Option<bool>,
    by: u32,
}

/// Continue's three summaries of one series (den-edge `ContinueSummaries`): the mark touched last, the one finished
/// last, and the furthest timeless watched bit.
struct Series {
    id: u64,
    latest: Option<Mark>,
    finished: Option<Mark>,
    flag: Option<(u64, u64)>,
}

/// Every title's facts and every series' summaries, with the orders series first appear in by a mark and by a flag.
struct Summaries<'a> {
    titles: Vec<TitleFacts<'a>>,
    /// A title's position in `titles`, by `code`.
    title_at: Index<u64>,
    series: Vec<Series>,
    series_at: Index<u64>,
    /// Positions in `series`, in the order a series' first mark and first flag came.
    order: Vec<usize>,
    flag_order: Vec<usize>,
}

impl<'a> Summaries<'a> {
    fn title(&self, tv: bool, id: u64) -> Option<&TitleFacts<'a>> {
        self.title_at.get(&code(tv, id)).map(|&i| &self.titles[i])
    }

    fn series(&self, id: u64) -> Option<&Series> {
        self.series_at.get(&id).map(|&i| &self.series[i])
    }

    /// The compact marks: each series' latest, then its latest finished when that is another episode.
    fn marks(&self) -> Vec<&Mark> {
        let mut marks = Vec::new();
        for &i in &self.order {
            let s = &self.series[i];
            let Some(newest) = &s.latest else { continue };
            marks.push(newest);
            if let Some(front) = &s.finished {
                if (front.season, front.episode) != (newest.season, newest.episode) {
                    marks.push(front);
                }
            }
        }
        marks
    }

    /// Whether a series has a mark or a flag.
    fn active(&self, id: u64) -> bool {
        self.series(id)
            .is_some_and(|s| s.latest.is_some() || s.flag.is_some())
    }

    fn flagged(&self, id: u64) -> bool {
        self.series(id).is_some_and(|s| s.flag.is_some())
    }

    /// Whether a record is one Continue's policy can still use (`ContinueSummaries.compact`): a film in progress, a
    /// watched or flagged series with a mark or a flag.
    fn kept(&self, f: &TitleFacts) -> bool {
        !f.deleted
            && if f.doc.tv {
                self.active(f.doc.id) && (f.status == WATCHED || self.flagged(f.doc.id))
            } else {
                f.status == IN_PROGRESS
            }
    }
}

fn nan(x: Option<f64>) -> f64 {
    x.unwrap_or(f64::NAN)
}

fn flag(x: Option<bool>) -> f64 {
    x.map_or(f64::NAN, |b| if b { 1.0 } else { 0.0 })
}

/// A list's title columns.
#[derive(Default)]
struct Keys {
    media: Vec<f64>,
    id: Vec<f64>,
}

impl Keys {
    fn push(&mut self, tv: bool, id: u64) {
        self.media.push(if tv { 1.0 } else { 0.0 });
        self.id.push(id as f64);
    }

    fn push_code(&mut self, code: u64) {
        self.push(code & 1 == 1, code >> 1);
    }

    fn put(self, answer: &mut Answer, list: &str) {
        answer.put(&format!("{list}.media"), Column::Numbers(self.media));
        answer.put(&format!("{list}.id"), Column::Numbers(self.id));
    }
}

/// The codes of `items`, each once, in the order first met.
fn unique(items: impl Iterator<Item = u64>) -> Vec<u64> {
    let mut seen = Index::new();
    items.filter(|&c| seen.insert(c, 0).is_none()).collect()
}

impl Columns {
    pub(super) fn titles(&self, now: i64) -> Vec<TitleFacts<'_>> {
        self.held()
            .filter_map(|slot| match &slot.body {
                Body::Title(d) => Some(title(d, now)),
                _ => None,
            })
            .collect()
    }

    fn series_reset(&self, id: u64) -> Option<At> {
        let slot = self.slots[*self.series_title.get(&id)?].as_ref()?;
        match &slot.body {
            Body::Title(t) => t.reset,
            _ => None,
        }
    }

    /// Every season document's episodes as `v2 ep` rows: season documents in order, episodes in number order.
    pub(super) fn rows(&self, now: i64, mut visit: impl FnMut(u64, u64, u64, Row)) {
        for slot in self.held() {
            let Body::Season(s) = &slot.body else {
                continue;
            };
            let floor = floor([self.series_reset(s.id), s.reset], now);
            let mut order = indices(s.registers.len());
            sort(&mut order, &|a, b| {
                s.registers[a].episode.cmp(&s.registers[b].episode)
            });
            for i in order {
                let r: &Register = &s.registers[i];
                visit(
                    s.id,
                    s.season,
                    r.episode,
                    row(&derive::episode(r, floor, now)),
                );
            }
        }
    }

    fn summaries(&self, now: i64) -> Summaries<'_> {
        let titles = self.titles(now);
        let title_at: Index<u64> = titles
            .iter()
            .enumerate()
            .map(|(i, f)| (f.code(), i))
            .collect();
        let mut s = Summaries {
            titles,
            title_at,
            series: Vec::new(),
            series_at: Index::new(),
            order: Vec::new(),
            flag_order: Vec::new(),
        };
        let mut rows = Vec::new();
        self.rows(now, |id, season, episode, e| {
            rows.push((id, season, episode, e))
        });
        for (id, season, episode, e) in rows {
            let reset = s.title(true, id).and_then(|f| f.reset);
            // `episode_mark` of an authoritative row with nothing held: an un-watch clears, a timeless watched bit
            // flags, anything else replaces; a series reset drops marks at or before it and every flag.
            if e.value == 0.0 || (e.at == 0 && (e.value < FINISHED || reset.is_some())) {
                continue;
            }
            if e.at != 0 && reset.is_some_and(|r| e.at <= r) {
                continue;
            }
            let at = match s.series_at.get(&id) {
                Some(&at) => at,
                None => {
                    s.series_at.insert(id, s.series.len());
                    s.series.push(Series {
                        id,
                        latest: None,
                        finished: None,
                        flag: None,
                    });
                    s.series.len() - 1
                }
            };
            let series = &mut s.series[at];
            if e.at == 0 {
                if series.flag.is_none() {
                    s.flag_order.push(at);
                }
                if series.flag.is_none_or(|front| (season, episode) > front) {
                    series.flag = Some((season, episode));
                }
                continue;
            }
            let mark = Mark {
                id,
                season,
                episode,
                fraction: e.value,
                at: e.at,
                seconds: e.seconds,
                playing: e.playing,
                by: e.by,
            };
            match series.latest {
                None => {
                    s.order.push(at);
                    series.latest = Some(mark);
                }
                Some(current) if mark.at > current.at => series.latest = Some(mark),
                _ => {}
            }
            if mark.fraction >= FINISHED
                && series.finished.is_none_or(|front| {
                    mark.at > front.at
                        || (mark.at == front.at
                            && (season, episode) > (front.season, front.episode))
                })
            {
                series.finished = Some(mark);
            }
        }
        s
    }

    /// Home's library inputs (den-edge `HomeLibraryView` without downloads).
    ///
    /// Lists, each `media` (0 film, 1 series) and `id`: `owned`, `watched`, `watchlist` (newest addition first),
    /// `standings` (+ `standing`, by key), `weighted` (+ `weight`, `at`), `seeds.watched`, `seeds.watchlisted`, `shelf`,
    /// `required`; and Continue's library: `records` (+ `status`, `progress`, `progress_at`, `seconds`, `playing`, `by`,
    /// `added_at`), `marks` (+ `season`, `episode`, `fraction`, `at`, `seconds`, `playing`, `by`), `flags` (+ `season`,
    /// `episode`), `dismissed` (+ `at`). `playing` is 0/1, `NaN` when unknown.
    pub fn home(&self, now: i64) -> Answer {
        let strings = &self.strings;
        let s = self.summaries(now);
        let marks = s.marks();
        let active: Vec<&TitleFacts> = s.titles.iter().filter(|f| !f.deleted).collect();
        let mut answer = Answer::default();
        let put_codes = |answer: &mut Answer, list: &str, codes: &[u64]| {
            let mut keys = Keys::default();
            codes.iter().for_each(|&c| keys.push_code(c));
            keys.put(answer, list);
        };
        let codes = |titles: &mut dyn Iterator<Item = &&TitleFacts>| -> Vec<u64> {
            titles.map(|f| f.code()).collect()
        };
        put_codes(&mut answer, "owned", &codes(&mut active.iter()));
        put_codes(
            &mut answer,
            "watched",
            &codes(&mut active.iter().filter(|f| f.status == WATCHED)),
        );
        let mut watchlist: Vec<usize> = (0..active.len())
            .filter(|&i| active[i].status == WATCHLIST)
            .collect();
        sort(&mut watchlist, &|a, b| {
            active[b].listed_at().total_cmp(&active[a].listed_at())
        });
        put_codes(
            &mut answer,
            "watchlist",
            &watchlist
                .iter()
                .map(|&i| active[i].code())
                .collect::<Vec<_>>(),
        );

        // Standings: a series with a mark is in progress; a record's own status otherwise, a watchlisted series
        // already being watched staying in progress. By key.
        let mut standings: Vec<(u64, u32)> = Vec::new();
        let mut standing_at = Index::new();
        for m in &marks {
            let c = code(true, m.id);
            let next = standings.len();
            if *standing_at.entry(c).or_insert(next) == next {
                standings.push((c, IN_PROGRESS));
            }
        }
        for f in &s.titles {
            if f.deleted || f.status == NONE {
                continue;
            }
            match standing_at.get(&f.code()) {
                Some(_) if f.status == WATCHLIST => {}
                Some(&i) => standings[i].1 = f.status,
                None => {
                    standing_at.insert(f.code(), standings.len());
                    standings.push((f.code(), f.status));
                }
            }
        }
        let mut by_key = indices(standings.len());
        sort(&mut by_key, &|a, b| key_cmp(standings[a].0, standings[b].0));
        put_codes(
            &mut answer,
            "standings",
            &by_key.iter().map(|&i| standings[i].0).collect::<Vec<_>>(),
        );
        answer.put(
            "standings.standing",
            Column::Strings(
                by_key
                    .iter()
                    .map(|&i| strings.get(standings[i].1).to_owned())
                    .collect(),
            ),
        );

        let named = |f: &TitleFacts, r: &str| strings.get(f.reaction) == r;
        let weight = |f: &TitleFacts| -> f64 {
            if named(f, "dislike") {
                return -1.5;
            }
            let seen = match f.status {
                WATCHED | IN_PROGRESS => 1.0,
                WATCHLIST => 0.6,
                _ => 0.0,
            };
            seen + if named(f, "love") {
                1.0
            } else if named(f, "like") {
                0.5
            } else {
                0.0
            }
        };
        let (mut weighted, mut weights, mut weighted_at) = (Keys::default(), vec![], vec![]);
        for f in &active {
            let w = weight(f);
            if w != 0.0 {
                weighted.push(f.doc.tv, f.doc.id);
                weights.push(w);
                weighted_at.push((f.progress_at as f64).max(f.added_at));
            }
        }
        weighted.put(&mut answer, "weighted");
        answer.put("weighted.weight", Column::Numbers(weights));
        answer.put("weighted.at", Column::Numbers(weighted_at));

        // The two most recent watched-or-liked and watchlisted titles; a tie by key.
        let recency = |f: &TitleFacts| {
            f.doc
                .watched_at
                .unwrap_or(0.0)
                .max(f.reaction_at as f64)
                .max(f.added_at)
        };
        let before = |a: &TitleFacts, b: &TitleFacts| {
            let (ra, rb) = (recency(a), recency(b));
            rb < ra || (rb == ra && key_cmp(a.code(), b.code()) == Ordering::Less)
        };
        let offer = |selected: &mut Vec<usize>, i: usize| match selected
            .iter()
            .position(|&held| before(active[i], active[held]))
        {
            None if selected.len() < 2 => selected.push(i),
            None => {}
            Some(at) => {
                selected.insert(at, i);
                selected.truncate(2);
            }
        };
        let (mut seed_watched, mut seed_listed) = (Vec::new(), Vec::new());
        for (i, f) in active.iter().enumerate() {
            if f.status == WATCHED || named(f, "like") || named(f, "love") {
                offer(&mut seed_watched, i);
            }
            if f.status == WATCHLIST {
                offer(&mut seed_listed, i);
            }
        }
        let seeds = |selected: &[usize]| -> Vec<u64> {
            selected.iter().map(|&i| active[i].code()).collect()
        };
        put_codes(&mut answer, "seeds.watched", &seeds(&seed_watched));
        put_codes(&mut answer, "seeds.watchlisted", &seeds(&seed_listed));

        // The shelves' refs in priority order, and the series among them whose layout decides Continue.
        let shelf = unique(
            seeds(&seed_watched)
                .into_iter()
                .chain(seeds(&seed_listed))
                .chain(
                    s.titles
                        .iter()
                        .filter(|f| {
                            !f.deleted && (f.status == WATCHLIST || f.status == IN_PROGRESS)
                        })
                        .map(|f| f.code()),
                )
                .chain(marks.iter().map(|m| code(true, m.id))),
        );
        let watched_at = |c: u64| {
            s.series(c >> 1)
                .and_then(|x| x.latest.map(|l| l.at))
                .unwrap_or(0)
        };
        let mut required: Vec<u64> = shelf
            .iter()
            .copied()
            .filter(|&c| c & 1 == 1 && s.active(c >> 1))
            .collect();
        let mut order = indices(required.len());
        sort(&mut order, &|a, b| {
            watched_at(required[b]).cmp(&watched_at(required[a]))
        });
        required = order.iter().map(|&i| required[i]).collect();
        put_codes(&mut answer, "shelf", &shelf);
        put_codes(&mut answer, "required", &required);

        // Continue's library.
        let mut records = Keys::default();
        let (
            mut status,
            mut progress,
            mut progress_at,
            mut seconds,
            mut playing,
            mut by,
            mut added,
        ) = (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
        for f in s.titles.iter().filter(|f| s.kept(f)) {
            records.push(f.doc.tv, f.doc.id);
            status.push(strings.get(f.status).to_owned());
            progress.push(f.progress);
            progress_at.push(f.progress_at as f64);
            seconds.push(nan(f.seconds));
            playing.push(flag(f.playing));
            by.push(strings.get(f.by).to_owned());
            added.push(f.added_at);
        }
        records.put(&mut answer, "records");
        answer.put("records.status", Column::Strings(status));
        answer.put("records.progress", Column::Numbers(progress));
        answer.put("records.progress_at", Column::Numbers(progress_at));
        answer.put("records.seconds", Column::Numbers(seconds));
        answer.put("records.playing", Column::Numbers(playing));
        answer.put("records.by", Column::Strings(by));
        answer.put("records.added_at", Column::Numbers(added));

        put_marks(&mut answer, "marks", &marks, strings);
        let (mut flags, mut flag_season, mut flag_episode) = (Keys::default(), vec![], vec![]);
        for &i in &s.flag_order {
            let series = &s.series[i];
            let Some((season, episode)) = series.flag else {
                continue;
            };
            flags.push(true, series.id);
            flag_season.push(season as f64);
            flag_episode.push(episode as f64);
        }
        flags.put(&mut answer, "flags");
        answer.put("flags.season", Column::Numbers(flag_season));
        answer.put("flags.episode", Column::Numbers(flag_episode));
        // A dismissal is kept for a kept record or a series with a mark or a flag, deleted title or not, and for a
        // series marked Seen as a whole: the new episodes a client follows it for are taken off by the same dismissal.
        let (mut dismissed, mut dismissed_at) = (Keys::default(), vec![]);
        for f in &s.titles {
            let seen = f.doc.tv && !f.deleted && f.status == WATCHED;
            let live = s.kept(f) || (f.doc.tv && s.active(f.doc.id)) || seen;
            if let Some(at) = f.dismissed.filter(|_| live) {
                dismissed.push(f.doc.tv, f.doc.id);
                dismissed_at.push(at as f64);
            }
        }
        dismissed.put(&mut answer, "dismissed");
        answer.put("dismissed.at", Column::Numbers(dismissed_at));
        answer
    }

    /// Continue Watching's entries before titles are named (den-edge `continueCandidates` over Home's Continue
    /// library), newest first: series by `continue_entry` with their `layouts` and `airing`, then films in progress.
    ///
    /// `continue`: `media`, `id`, `action` (`resume`, `next`, `start`, `upcoming`; `""` for a film), `season`,
    /// `episode`, `fraction`, `upcoming` (its date), `display` (0 by its latest mark, 1 by its record), and where it
    /// was last playing: `seconds`, `playing`, `at`, `live_season`, `live_episode`, `live_fraction`, `by`.
    pub fn continuing(&self, now: i64, layouts: &Layouts, airing: Option<&Airing>) -> Answer {
        struct Entry {
            tv: bool,
            id: u64,
            activity: i64,
            action: String,
            episode: Option<(i64, i64)>,
            fraction: f64,
            upcoming: String,
            /// The latest mark that carries a position, or a film's position: seconds, playing, at, device.
            live: Option<(Option<f64>, Option<bool>, i64, u32)>,
            live_episode: Option<(u64, u64, f64)>,
            display: f64,
        }
        let s = self.summaries(now);
        let kept = |tv: bool, id: u64| s.title(tv, id).filter(|f| s.kept(f));
        let mut series: Vec<usize> = s
            .order
            .iter()
            .chain(
                s.flag_order
                    .iter()
                    .filter(|&&i| s.series[i].latest.is_none()),
            )
            .copied()
            .collect();
        let activity = |i: usize| s.series[i].latest.map_or(i64::MIN, |m| m.at);
        sort(&mut series, &|a, b| activity(b).cmp(&activity(a)));
        let mut entries = Vec::new();
        for i in series {
            let summary = &s.series[i];
            let id = summary.id;
            let mark = summary.latest;
            if mark.is_none() && kept(true, id).is_none() {
                continue;
            }
            let (seasons, last_aired) = layouts.seasons(id);
            let dated = airing.and_then(|a| Some((a, a.get(id)?)));
            let input = ContinueInput {
                mark: mark.map(|m| ContinueMark {
                    season: m.season as i64,
                    episode: m.episode as i64,
                    fraction: m.fraction,
                    at: m.at,
                }),
                finished: summary.finished.map(|f| Finished {
                    season: f.season as i64,
                    episode: f.episode as i64,
                    at: Some(f.at),
                }),
                flag: summary.flag.map(|(season, episode)| Coord {
                    season: season as i64,
                    episode: episode as i64,
                }),
                seasons,
                last_aired,
                // Every series asked about has a mark or a flag, so Continue's library keeps its dismissal.
                dismissed_at: s.title(true, id).and_then(|f| f.dismissed),
                title_watched: kept(true, id).is_some_and(|f| f.status == WATCHED),
                airing: dated.map_or(Vec::new(), |(_, (episodes, _))| {
                    episodes
                        .iter()
                        .map(|(season, episode, date)| DatedEpisode {
                            season: *season,
                            episode: *episode,
                            date: date.clone(),
                        })
                        .collect()
                }),
                today: dated.map(|(a, _)| a.today.clone()),
                ended: dated.is_some_and(|(_, (_, ended))| *ended),
            };
            let answer = continue_entry(&input);
            let action = answer["action"].as_str().unwrap_or("none");
            let Some(episode) = coord(&answer["episode"]).filter(|_| action != "none") else {
                continue;
            };
            let positioned = mark.filter(|m| m.seconds.is_some());
            entries.push(Entry {
                tv: true,
                id,
                activity: mark.map_or(i64::MIN, |m| m.at),
                action: action.to_owned(),
                episode: Some(episode),
                fraction: answer["fraction"].as_f64().unwrap_or(0.0),
                upcoming: match action {
                    "upcoming" => answer["date"].as_str().unwrap_or_default().to_owned(),
                    _ => String::new(),
                },
                live: positioned.map(|m| (m.seconds, m.playing, m.at, m.by)),
                live_episode: positioned.map(|m| (m.season, m.episode, m.fraction)),
                display: if mark.is_some() { 0.0 } else { 1.0 },
            });
        }
        let mut films: Vec<usize> = (0..s.titles.len())
            .filter(|&i| !s.titles[i].doc.tv && s.kept(&s.titles[i]))
            .collect();
        sort(&mut films, &|a, b| {
            s.titles[b].progress_at.cmp(&s.titles[a].progress_at)
        });
        for i in films {
            let f = &s.titles[i];
            if f.dismissed.is_some_and(|d| d >= f.progress_at) {
                continue;
            }
            entries.push(Entry {
                tv: false,
                id: f.doc.id,
                activity: f.progress_at,
                action: String::new(),
                episode: None,
                fraction: f.progress,
                upcoming: String::new(),
                live: f
                    .seconds
                    .is_some()
                    .then_some((f.seconds, f.playing, f.progress_at, f.by)),
                live_episode: None,
                display: 1.0,
            });
        }
        let mut order = indices(entries.len());
        sort(&mut order, &|a, b| {
            entries[b].activity.cmp(&entries[a].activity)
        });

        let strings = &self.strings;
        let mut answer = Answer::default();
        let mut keys = Keys::default();
        let mut cols: [Vec<f64>; 10] = Default::default();
        let (mut action, mut upcoming, mut by) = (vec![], vec![], vec![]);
        for e in order.iter().map(|&i| &entries[i]) {
            keys.push(e.tv, e.id);
            action.push(e.action.clone());
            upcoming.push(e.upcoming.clone());
            let (season, episode) = e
                .episode
                .map_or((f64::NAN, f64::NAN), |(s, e)| (s as f64, e as f64));
            let (seconds, playing, at, device) = e.live.unwrap_or((None, None, 0, EMPTY));
            let live = e
                .live_episode
                .map_or((f64::NAN, f64::NAN, f64::NAN), |(s, ep, f)| {
                    (s as f64, ep as f64, f)
                });
            let at = if e.live.is_some() {
                at as f64
            } else {
                f64::NAN
            };
            for (column, value) in cols.iter_mut().zip([
                season,
                episode,
                e.fraction,
                e.display,
                nan(seconds),
                flag(playing),
                at,
                live.0,
                live.1,
                live.2,
            ]) {
                column.push(value);
            }
            by.push(strings.get(device).to_owned());
        }
        keys.put(&mut answer, "continue");
        answer.put("continue.action", Column::Strings(action));
        answer.put("continue.upcoming", Column::Strings(upcoming));
        answer.put("continue.by", Column::Strings(by));
        let names = [
            "season",
            "episode",
            "fraction",
            "display",
            "seconds",
            "playing",
            "at",
            "live_season",
            "live_episode",
            "live_fraction",
        ];
        for (name, column) in names.iter().zip(cols) {
            answer.put(&format!("continue.{name}"), Column::Numbers(column));
        }
        answer
    }

    /// Seen history before titles are named (den-edge `watchedHistory`), newest first, then by key.
    ///
    /// `history`: `media`, `id`, `at` (0 when the time is unknown), `season`, `episode` (its latest seen regular
    /// episode, `NaN` for none), `episodes` (how many regular episodes are seen).
    pub fn history(&self, now: i64) -> Answer {
        struct Seen {
            id: u64,
            latest_at: i64,
            latest: Option<(u64, u64, i64)>,
            count: u64,
        }
        let titles = self.titles(now);
        let title_at: Index<u64> = titles
            .iter()
            .enumerate()
            .map(|(i, f)| (f.code(), i))
            .collect();
        let title = |c: u64| title_at.get(&c).map(|&i| &titles[i]);
        let mut seen: Vec<Seen> = Vec::new();
        let mut series_at = Index::new();
        self.rows(now, |id, season, episode, e| {
            if e.value < FINISHED {
                return;
            }
            if title(code(true, id))
                .and_then(|f| f.reset)
                .is_some_and(|reset| e.at <= reset)
            {
                return;
            }
            let at = *series_at.entry(id).or_insert_with(|| {
                seen.push(Seen {
                    id,
                    latest_at: e.at,
                    latest: None,
                    count: 0,
                });
                seen.len() - 1
            });
            let found = &mut seen[at];
            found.latest_at = found.latest_at.max(e.at);
            if season == 0 {
                return;
            }
            found.count += 1;
            if found.latest.is_none_or(|l| e.at > l.2) {
                found.latest = Some((season, episode, e.at));
            }
        });
        /// When, which title (`code`), its latest seen episode, how many episodes are seen.
        type Entry = (i64, u64, Option<(u64, u64)>, u64);
        let mut entries: Vec<Entry> = Vec::new();
        let keys = unique(
            titles
                .iter()
                .map(|f| f.code())
                .chain(seen.iter().map(|s| code(true, s.id))),
        );
        for c in keys {
            let row = title(c);
            let seen_at = match row {
                Some(f) if f.status == WATCHED => (f.doc.watched_at.unwrap_or(0.0) as i64)
                    .max(f.status_at)
                    .max(0),
                _ => -1,
            };
            let episodes = (c & 1 == 1)
                .then(|| series_at.get(&(c >> 1)))
                .flatten()
                .map(|&i| &seen[i]);
            let at = seen_at.max(episodes.map_or(-1, |s| s.latest_at.max(0)));
            // Watched state outlives a watchlist removal: a title `deleted` by one (before `watchlist_remove`) keeps
            // its history.
            if at < 0 {
                continue;
            }
            entries.push((
                at,
                c,
                episodes.and_then(|s| s.latest).map(|l| (l.0, l.1)),
                episodes.map_or(0, |s| s.count),
            ));
        }
        let mut order = indices(entries.len());
        sort(&mut order, &|a, b| {
            entries[b]
                .0
                .cmp(&entries[a].0)
                .then_with(|| key_cmp(entries[a].1, entries[b].1))
        });
        let mut answer = Answer::default();
        let mut keys = Keys::default();
        let (mut at, mut season, mut episode, mut count) = (vec![], vec![], vec![], vec![]);
        for &i in &order {
            let (t, c, latest, episodes) = entries[i];
            keys.push_code(c);
            at.push(t as f64);
            season.push(latest.map_or(f64::NAN, |l| l.0 as f64));
            episode.push(latest.map_or(f64::NAN, |l| l.1 as f64));
            count.push(episodes as f64);
        }
        keys.put(&mut answer, "history");
        answer.put("history.at", Column::Numbers(at));
        answer.put("history.season", Column::Numbers(season));
        answer.put("history.episode", Column::Numbers(episode));
        answer.put("history.episodes", Column::Numbers(count));
        answer
    }

    /// The watchlist (den-edge `watchlist`, before titles are named): `media`, `id`, `added_at` (when it was listed,
    /// `listed_at`), newest addition first, a tie in document order.
    pub fn watchlist(&self, now: i64) -> Answer {
        let titles = self.titles(now);
        let mut listed: Vec<usize> = (0..titles.len())
            .filter(|&i| !titles[i].deleted && titles[i].status == WATCHLIST)
            .collect();
        sort(&mut listed, &|a, b| {
            titles[b].listed_at().total_cmp(&titles[a].listed_at())
        });
        let mut answer = Answer::default();
        let mut keys = Keys::default();
        listed
            .iter()
            .for_each(|&i| keys.push(titles[i].doc.tv, titles[i].doc.id));
        keys.put(&mut answer, "watchlist");
        answer.put(
            "watchlist.added_at",
            Column::Numbers(listed.iter().map(|&i| titles[i].listed_at()).collect()),
        );
        answer
    }

    /// The newest stamp in any held document that is not more than a day ahead (den-edge `documentNewest`, the zero
    /// stamp when there is none), and `reconsider_at`.
    ///
    /// `newest`: `t`, `c`, `d` (one row); `reconsider`: `at` (one row, `NaN` for never).
    pub fn newest(&self, now: i64) -> Answer {
        let bound = now.saturating_add(DAY);
        let strings = &self.strings;
        let mut newest = (0i64, 0i64, EMPTY);
        for slot in self.held() {
            if let Some(&s) = slot.stamps.iter().find(|s| s.0 <= bound) {
                let later = (s.0, s.1)
                    .cmp(&(newest.0, newest.1))
                    .then_with(|| strings.get(s.2).cmp(strings.get(newest.2)));
                if later.is_gt() {
                    newest = s;
                }
            }
        }
        let mut answer = Answer::default();
        answer.put("newest.t", Column::Numbers(vec![newest.0 as f64]));
        answer.put("newest.c", Column::Numbers(vec![newest.1 as f64]));
        answer.put(
            "newest.d",
            Column::Strings(vec![strings.get(newest.2).to_owned()]),
        );
        answer.put(
            "reconsider.at",
            Column::Numbers(vec![nan(self.reconsider_at(now).map(|t| t as f64))]),
        );
        answer
    }

    /// When an answer at `now` may next change with no write: the earliest time a stamp or play now more than a day
    /// ahead stops being so. `None` when nothing held is ahead.
    pub fn reconsider_at(&self, now: i64) -> Option<i64> {
        let bound = now.saturating_add(DAY);
        let plays = |r: &Register| {
            r.plays
                .iter()
                .map(|(_, at)| *at as i64)
                .filter(|at| *at > bound)
                .min()
        };
        let mut earliest: Option<i64> = None;
        for slot in self.held() {
            let stamp = slot
                .stamps
                .iter()
                .take_while(|s| s.0 > bound)
                .last()
                .map(|s| s.0);
            let play = match &slot.body {
                Body::Title(t) => t.watch.as_ref().and_then(plays),
                Body::Season(s) => s.registers.iter().filter_map(plays).min(),
                Body::Delivery => None,
            };
            for t in [stamp, play].into_iter().flatten() {
                earliest = Some(earliest.map_or(t, |e| e.min(t)));
            }
        }
        earliest.map(|t| t - DAY)
    }
}

fn put_marks(answer: &mut Answer, list: &str, marks: &[&Mark], strings: &super::read::Strings) {
    let mut keys = Keys::default();
    let mut cols: [Vec<f64>; 6] = Default::default();
    let mut by = Vec::new();
    for m in marks {
        keys.push(true, m.id);
        for (column, value) in cols.iter_mut().zip([
            m.season as f64,
            m.episode as f64,
            m.fraction,
            m.at as f64,
            nan(m.seconds),
            flag(m.playing),
        ]) {
            column.push(value);
        }
        by.push(strings.get(m.by).to_owned());
    }
    keys.put(answer, list);
    for (name, column) in ["season", "episode", "fraction", "at", "seconds", "playing"]
        .iter()
        .zip(cols)
    {
        answer.put(&format!("{list}.{name}"), Column::Numbers(column));
    }
    answer.put(&format!("{list}.by"), Column::Strings(by));
}
