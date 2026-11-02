//! The assistant projection's lists (`projection::lists`) from the held registers, step for step: the watchlist,
//! Continue Watching, Seen history and ratings, as `assistant_projection` takes them in place of documents.

use super::derive::{episode, floor, plays};
use super::lists::Layouts;
use super::read::{At, Field, SeasonDoc, TitleDoc, IN_PROGRESS, WATCHED, WATCHLIST};
use super::{sort, Body, Columns};
use crate::projection::Lists;
use crate::series::{continue_entry, ContinueInput, ContinueMark, Coord, Finished};
use den_assistant::{ContinueRow, Media, RatedRow, SeenRow, WatchlistRow};
use serde_json::{json, Value};

const FINISHED: f64 = 0.95;

fn at(field: &Option<Field<impl Copy>>) -> Option<i64> {
    field.as_ref().and_then(|f| f.at).map(|a: At| a.t)
}

fn is_true(field: &Option<Field<bool>>) -> bool {
    field.as_ref().is_some_and(|f| f.value)
}

fn seen_rows(
    seen: &mut Vec<SeenRow>,
    plays: &[(i64, u64)],
    watched: bool,
    title: (Media, u64),
    place: Option<(u64, u64)>,
) {
    let row = |at: Value| SeenRow {
        title,
        season: place.map(|(s, _)| json!(s)),
        episode: place.map(|(_, e)| json!(e)),
        at,
    };
    for (_, at) in plays {
        seen.push(row(json!(at)));
    }
    if plays.is_empty() && watched {
        seen.push(row(Value::Null));
    }
}

impl Columns {
    /// `projection::lists` of the held documents with `layouts`. Every held document was readable, so none is
    /// skipped.
    pub fn assistant_lists(&self, now: i64, layouts: &Layouts) -> Lists {
        let strings = &self.strings;
        // Titles by type then id, and seasons by series then number, as the projection walks them.
        let mut titles: Vec<&TitleDoc> = Vec::new();
        let mut seasons: Vec<&SeasonDoc> = Vec::new();
        for slot in self.held() {
            match &slot.body {
                Body::Title(t) => titles.push(t),
                Body::Season(s) => seasons.push(s),
                Body::Delivery => {}
            }
        }
        let mut order = super::indices(titles.len());
        sort(&mut order, &|a, b| {
            (titles[a].tv, titles[a].id).cmp(&(titles[b].tv, titles[b].id))
        });
        let titles: Vec<&TitleDoc> = order.iter().map(|&i| titles[i]).collect();
        let mut order = super::indices(seasons.len());
        sort(&mut order, &|a, b| {
            (seasons[a].id, seasons[a].season).cmp(&(seasons[b].id, seasons[b].season))
        });
        let seasons: Vec<&SeasonDoc> = order.iter().map(|&i| seasons[i]).collect();
        let series_title = |id: u64| -> Option<&TitleDoc> {
            titles
                .binary_search_by(|t| (t.tv, t.id).cmp(&(true, id)))
                .ok()
                .map(|i| titles[i])
        };
        let status = |t: &TitleDoc| t.status.as_ref().map(|s| s.value);
        let (mut watchlist, mut seen, mut films, mut rated) = (vec![], vec![], vec![], vec![]);
        for t in &titles {
            let title = (if t.tv { Media::Tv } else { Media::Movie }, t.id);
            // A title `deleted` by a watchlist removal keeps its plays in `seen` and leaves every other list.
            let deleted = is_true(&t.deleted);
            if let Some(reaction) = t
                .reaction
                .as_ref()
                .map(|r| strings.get(r.value))
                .filter(|r| !deleted && matches!(*r, "dislike" | "like" | "love"))
            {
                rated.push(RatedRow {
                    title,
                    value: json!(reaction),
                    at: json!(at(&t.reaction).filter(|t| *t > 0)),
                });
            }
            if !deleted && status(t) == Some(WATCHLIST) {
                watchlist.push(WatchlistRow {
                    title,
                    // When it was listed (`TitleFacts::listed_at`): the later of `addedAt` and the status stamp.
                    added_at: json!(t.added_at_integer.max(at(&t.status))),
                });
            }
            if t.tv {
                continue;
            }
            // `watch::film_state`: a film's plays show unless it is unwatched or only listed.
            let film_status = status(t).map_or("none", |s| strings.get(s));
            let visible = match (&t.watch, film_status) {
                (_, "none" | "watchlist") | (None, _) => Vec::new(),
                (Some(w), _) => plays(w, 0, now),
            };
            seen_rows(&mut seen, &visible, film_status == "watched", title, None);
            let fraction = t.resume.as_ref().and_then(|r| r.value).unwrap_or(0.0);
            let progress_at = t.resume.as_ref().and_then(|r| r.at).map_or(0, |a| a.t);
            let dismissed =
                is_true(&t.dismissed) && at(&t.dismissed).is_some_and(|d| d >= progress_at);
            if !deleted && status(t) == Some(IN_PROGRESS) && !dismissed {
                films.push(ContinueRow {
                    title,
                    action: json!("resume"),
                    season: None,
                    episode: None,
                    fraction: json!(fraction),
                    at: json!(progress_at),
                });
            }
        }

        let mut series = Vec::new();
        for by_season in seasons.chunk_by(|a, b| a.id == b.id) {
            let id = by_season[0].id;
            let title = series_title(id);
            let deleted = title.is_some_and(|t| is_true(&t.deleted));
            let mut latest: Option<(i64, u64, u64, f64)> = None;
            let mut finished: Option<(i64, u64, u64)> = None;
            let mut flag: Option<(u64, u64)> = None;
            for s in by_season {
                let season = s.season;
                let floor = floor([title.and_then(|t| t.reset), s.reset], now);
                for r in &s.registers {
                    let e = episode(r, floor, now);
                    seen_rows(
                        &mut seen,
                        &e.plays,
                        e.watched,
                        (Media::Tv, id),
                        Some((season, r.episode)),
                    );
                    let mark = match (e.resume, &r.progress) {
                        (Some(p), _) => Some((p.at.map_or(0, |a| a.t), p.value.unwrap_or(0.0))),
                        (None, Some(p)) if e.watched => Some((p.at.map_or(0, |a| a.t), 1.0)),
                        (None, None) if e.watched => {
                            if flag.is_none_or(|f| (season, r.episode) > f) {
                                flag = Some((season, r.episode));
                            }
                            None
                        }
                        _ => None,
                    };
                    if let Some((at, fraction)) = mark {
                        if latest.is_none_or(|l| at > l.0) {
                            latest = Some((at, season, r.episode, fraction));
                        }
                        if fraction >= FINISHED
                            && finished.is_none_or(|f| (at, season, r.episode) > f)
                        {
                            finished = Some((at, season, r.episode));
                        }
                    }
                }
            }
            if deleted || (latest.is_none() && flag.is_none()) {
                continue;
            }
            let (seasons, last_aired) = layouts.seasons(id);
            let input = ContinueInput {
                mark: latest.map(|(at, season, episode, fraction)| ContinueMark {
                    season: season as i64,
                    episode: episode as i64,
                    fraction,
                    at,
                }),
                finished: finished.map(|(at, season, episode)| Finished {
                    season: season as i64,
                    episode: episode as i64,
                    at: Some(at),
                }),
                flag: flag.map(|(season, episode)| Coord {
                    season: season as i64,
                    episode: episode as i64,
                }),
                seasons,
                last_aired,
                dismissed_at: title
                    .filter(|t| is_true(&t.dismissed))
                    .and_then(|t| at(&t.dismissed)),
                title_watched: title.and_then(status) == Some(WATCHED),
                airing: Vec::new(),
                today: None,
                ended: false,
            };
            let answer = continue_entry(&input);
            if answer["action"] == "none" || answer["episode"].is_null() {
                continue;
            }
            series.push(ContinueRow {
                title: (Media::Tv, id),
                action: answer["action"].clone(),
                season: Some(answer["episode"]["season"].clone()),
                episode: Some(answer["episode"]["episode"].clone()),
                fraction: answer["fraction"].clone(),
                at: json!(latest.map(|l| l.0)),
            });
        }
        let mut lists = Lists {
            watchlist,
            continuing: series.into_iter().chain(films).collect(),
            seen,
            rated,
            skipped: 0,
        };
        lists.sort();
        lists
    }

    /// `assistant_lists` as `assistant_projection` takes them (`lists`).
    pub fn assistant(&self, now: i64, layouts: &Layouts) -> Value {
        self.assistant_lists(now, layouts).to_json()
    }
}
