//! What a typed register reads as at a given time: `state::title_state`'s and `watch::episode_state`'s rules, held to
//! them by `tests::typed_registers_derive_as_the_rules_do`.

use super::read::{At, Progress, Register, TitleDoc, EMPTY, NONE};

pub(super) const DAY: i64 = 86_400_000;

/// `watch::effective_t`: a stamp more than a day ahead counts as timeless.
pub(super) fn effective(t: i64, now: i64) -> i64 {
    if t > now.saturating_add(DAY) {
        0
    } else {
        t
    }
}

/// `watch::floor`: the latest covering reset that is not more than a day ahead, or 0.
pub(super) fn floor(resets: impl IntoIterator<Item = Option<At>>, now: i64) -> i64 {
    resets
        .into_iter()
        .flatten()
        .map(|r| effective(r.t, now))
        .filter(|t| *t > 0)
        .max()
        .unwrap_or(0)
}

/// A title's `rec` fields as `title_state` derives them at `now`, with the defaults an absent field reads as.
pub(super) struct TitleFacts<'a> {
    pub doc: &'a TitleDoc,
    pub status: u32,
    pub status_at: i64,
    pub progress: f64,
    pub progress_at: i64,
    pub seconds: Option<f64>,
    pub playing: Option<bool>,
    /// The resume point's device, `EMPTY` for none.
    pub by: u32,
    pub added_at: f64,
    pub deleted: bool,
    pub dismissed: Option<i64>,
    /// `EMPTY` for none.
    pub reaction: u32,
    pub reaction_at: i64,
    pub reset: Option<i64>,
}

impl TitleFacts<'_> {
    /// Its `type:id` key as `code` packs it.
    pub fn code(&self) -> u64 {
        super::code(self.doc.tv, self.doc.id)
    }

    /// When a listed title went on the watchlist: its status stamp, which only a move onto the watchlist sets while it
    /// stays there, or `addedAt` when that is later. `addedAt` alone is the first time the title entered the library
    /// (every merge keeps the earliest), so a finished title listed again would sort as if listed years ago.
    pub fn listed_at(&self) -> f64 {
        self.added_at.max(self.status_at as f64)
    }
}

pub(super) fn title(d: &TitleDoc, now: i64) -> TitleFacts<'_> {
    // `title_state` reads a stamp more than a day ahead as the zero stamp, device and all.
    let at = |a: Option<At>| a.filter(|a| a.t <= now.saturating_add(DAY));
    let t = |a: Option<At>| at(a).map_or(0, |a| a.t);
    let resume_at = d.resume.as_ref().and_then(|r| at(r.at));
    TitleFacts {
        doc: d,
        status: d.status.as_ref().map_or(NONE, |s| s.value),
        status_at: d.status.as_ref().map_or(0, |s| t(s.at)),
        progress: d.resume.as_ref().and_then(|r| r.value).unwrap_or(0.0),
        progress_at: resume_at.map_or(0, |a| a.t),
        seconds: d.resume.as_ref().and_then(|r| r.seconds),
        playing: d.resume.as_ref().and_then(|r| r.playing),
        by: resume_at.map_or(EMPTY, |a| a.d),
        added_at: d.added_at.unwrap_or(0.0),
        deleted: d.deleted.as_ref().is_some_and(|x| x.value),
        dismissed: d.dismissed.as_ref().filter(|x| x.value).map(|x| t(x.at)),
        reaction: d.reaction.as_ref().map_or(EMPTY, |x| x.value),
        reaction_at: d.reaction.as_ref().map_or(0, |x| t(x.at)),
        reset: d
            .reset
            .filter(|r| r.t <= now.saturating_add(DAY))
            .map(|r| r.t),
    }
}

/// `watch::plays`: the plays a reader sees, oldest first.
pub(super) fn plays(r: &Register, floor: i64, now: i64) -> Vec<(i64, u64)> {
    let mut plays: Vec<(i64, u64)> = r
        .plays
        .iter()
        .copied()
        .filter(|&(key, at)| {
            let ahead = at > now.saturating_add(DAY).max(0) as u64;
            let reset = at <= floor.max(0) as u64;
            let cleared =
                (key >= 0 && key <= r.cleared.0) || (key < 0 && at <= r.cleared.1.max(0) as u64);
            !(ahead || reset || cleared)
        })
        .collect();
    plays.sort_unstable_by_key(|(_, at)| *at);
    plays
}

/// One episode as `watch::episode_state` reads it.
pub(super) struct Episode<'a> {
    pub watched: bool,
    /// The visible progress when it is a resume point (`resume`).
    pub resume: Option<&'a Progress>,
    pub watched_at: Option<u64>,
    pub plays: Vec<(i64, u64)>,
}

pub(super) fn episode(r: &Register, floor: i64, now: i64) -> Episode<'_> {
    let visible = r
        .progress
        .as_ref()
        .filter(|p| p.at.as_ref().is_some_and(|at| effective(at.t, now) > floor));
    let plays = plays(r, floor, now);
    let imported = r.imported && (floor == 0 || plays.iter().any(|(key, _)| *key < 0));
    let value = visible.and_then(|p| p.value);
    let viewing = r.progress.as_ref().map_or(0, |p| p.viewing);
    Episode {
        watched: value.is_some_and(|v| v >= 0.95) || (visible.is_none() && imported),
        resume: visible.filter(|_| value.is_some_and(|v| (0.02..0.95).contains(&v))),
        watched_at: plays
            .iter()
            .find(|(key, _)| *key == viewing as i64)
            .or_else(|| plays.last())
            .map(|(_, at)| *at),
        plays,
    }
}

/// One episode's projected `v2 ep` row (den-edge `episodeRow`): progress value, its time, device, seconds, playing.
pub(super) struct Row {
    pub value: f64,
    pub at: i64,
    pub by: u32,
    pub seconds: Option<f64>,
    pub playing: Option<bool>,
}

pub(super) fn row(e: &Episode) -> Row {
    match e.resume {
        Some(p) => Row {
            value: p.value.unwrap_or(0.0),
            at: p.at.map_or(0, |a| a.t),
            by: p.at.map_or(EMPTY, |a| a.d),
            seconds: p.seconds,
            playing: p.playing,
        },
        None => Row {
            value: if e.watched { 1.0 } else { 0.0 },
            at: e.watched_at.map_or(0, |t| t as i64),
            by: EMPTY,
            seconds: None,
            playing: None,
        },
    }
}
