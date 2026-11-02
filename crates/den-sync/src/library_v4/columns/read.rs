//! A document read once into typed registers. Nothing here depends on the time: a stamp more than a day ahead is
//! held as written and read as timeless only when a list is asked for (`derive`).

use super::super::doc::valid_episode_key;
use super::{indices, permute, sort, Index};
use crate::wire::stamp;
use serde_json::{Map, Value};

/// Every string a document holds that a list reads (stamp devices, statuses, reactions), held once. Id 0 is the empty
/// string, which a stamp's device reads as "none"; the four statuses are fixed so a status is compared as a number.
pub(super) struct Strings {
    list: Vec<String>,
    index: Index<String>,
}

pub(super) const EMPTY: u32 = 0;
pub(super) const NONE: u32 = 1;
pub(super) const WATCHLIST: u32 = 2;
pub(super) const IN_PROGRESS: u32 = 3;
pub(super) const WATCHED: u32 = 4;

impl Strings {
    pub fn new() -> Self {
        let mut strings = Strings {
            list: Vec::new(),
            index: Index::new(),
        };
        for s in ["", "none", "watchlist", "inProgress", "watched"] {
            strings.id(s);
        }
        strings
    }

    pub fn id(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.index.get(s) {
            return id as u32;
        }
        let id = self.list.len();
        self.list.push(s.to_owned());
        self.index.insert(s.to_owned(), id);
        id as u32
    }

    pub fn get(&self, id: u32) -> &str {
        &self.list[id as usize]
    }
}

/// A stamp: time, counter, device (`Strings` id).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct At {
    pub t: i64,
    pub c: u64,
    pub d: u32,
}

fn at_of(value: &Value, strings: &mut Strings) -> Option<At> {
    let s = stamp(value).ok()?;
    Some(At {
        t: s.0,
        c: s.1,
        d: strings.id(&s.2),
    })
}

/// A stamped title field: `{"value": …, "at": stamp}`. `at` is absent when it is no valid stamp.
pub(super) struct Field<T> {
    pub value: T,
    pub at: Option<At>,
}

pub(super) struct Progress {
    pub value: Option<f64>,
    pub at: Option<At>,
    pub viewing: u64,
    pub seconds: Option<f64>,
    pub playing: Option<bool>,
}

/// A watch register: a film's `watch`, or one episode of a season.
pub(super) struct Register {
    pub episode: u64,
    pub progress: Option<Progress>,
    pub imported: bool,
    /// In the document's key order, as `watch::plays` reads them.
    pub plays: Vec<(i64, u64)>,
    /// `cleared`: the viewing it cleared through and when; `(-1, 0)` for none.
    pub cleared: (i64, i64),
}

pub(super) struct TitleDoc {
    pub tv: bool,
    pub id: u64,
    pub status: Option<Field<u32>>,
    pub resume: Option<Progress>,
    /// `EMPTY` for a cleared (null) reaction.
    pub reaction: Option<Field<u32>>,
    pub deleted: Option<Field<bool>>,
    pub dismissed: Option<Field<bool>>,
    pub reset: Option<At>,
    pub added_at: Option<f64>,
    /// `addedAt` when it is written as an integer, which is how the assistant projection reads it.
    pub added_at_integer: Option<i64>,
    pub watched_at: Option<f64>,
    pub watch: Option<Register>,
}

pub(super) struct SeasonDoc {
    pub id: u64,
    pub season: u64,
    pub reset: Option<At>,
    /// In the document's key order (`"1"`, `"10"`, `"2"`), as the assistant projection walks them.
    pub registers: Vec<Register>,
}

fn progress(value: &Value, strings: &mut Strings) -> Progress {
    Progress {
        value: value["value"].as_f64(),
        at: at_of(&value["at"], strings),
        viewing: value["viewing"].as_u64().unwrap_or(0),
        seconds: value["seconds"].as_f64(),
        playing: value["playing"].as_bool(),
    }
}

fn register(episode: u64, register: &Map<String, Value>, strings: &mut Strings) -> Register {
    Register {
        episode,
        progress: register
            .get("progress")
            .filter(|p| !p.is_null())
            .map(|p| progress(p, strings)),
        imported: register
            .get("imported")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        plays: register
            .get("plays")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_u64()?)))
            .collect(),
        cleared: register
            .get("cleared")
            .and_then(Value::as_array)
            .filter(|c| c.len() == 2)
            .and_then(|c| Some((c[0].as_u64()? as i64, stamp(&c[1]).ok()?.0)))
            .unwrap_or((-1, 0)),
    }
}

impl TitleDoc {
    pub fn read(tv: bool, id: u64, doc: &Map<String, Value>, strings: &mut Strings) -> Self {
        let field = |name: &str| doc.get(name).filter(|v| v.is_object());
        let flag = |name: &str, strings: &mut Strings| {
            field(name).map(|v| Field {
                value: v["value"] == true,
                at: at_of(&v["at"], strings),
            })
        };
        let deleted = flag("deleted", strings);
        let dismissed = flag("dismissed", strings);
        TitleDoc {
            tv,
            id,
            status: field("status").map(|v| Field {
                value: strings.id(v["value"].as_str().unwrap_or("none")),
                at: at_of(&v["at"], strings),
            }),
            resume: field("resume").map(|v| progress(v, strings)),
            reaction: field("reaction").map(|v| Field {
                value: v["value"].as_str().map_or(EMPTY, |r| strings.id(r)),
                at: at_of(&v["at"], strings),
            }),
            deleted,
            dismissed,
            reset: doc.get("episodesReset").and_then(|r| at_of(r, strings)),
            added_at: doc.get("addedAt").and_then(Value::as_f64),
            added_at_integer: doc.get("addedAt").and_then(Value::as_i64),
            watched_at: doc.get("watchedAt").and_then(Value::as_f64),
            watch: doc
                .get("watch")
                .and_then(Value::as_object)
                .map(|w| register(0, w, strings)),
        }
    }
}

impl SeasonDoc {
    pub fn read(id: u64, season: u64, doc: &Map<String, Value>, strings: &mut Strings) -> Self {
        let registers = doc
            .get("episodes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(key, _)| valid_episode_key(key))
            .filter_map(|(key, value)| {
                Some(register(key.parse().ok()?, value.as_object()?, strings))
            })
            .collect();
        SeasonDoc {
            id,
            season,
            reset: doc.get("seasonReset").and_then(|r| at_of(r, strings)),
            registers,
        }
    }
}

/// Every stamp-shaped value in a document (`[integer, integer, string]`, anywhere, unknown members included), as
/// den-edge's `documentNewest` finds them: sorted newest first, one per time (the greatest), which is all a newest
/// answer at any time can read.
pub(super) fn stamps(doc: &Map<String, Value>, strings: &mut Strings) -> Vec<(i64, i64, u32)> {
    fn integer(v: &Value) -> Option<i64> {
        v.as_i64()
            .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
    }
    fn visit(v: &Value, strings: &mut Strings, out: &mut Vec<(i64, i64, u32)>) {
        match v {
            Value::Array(a) if a.len() == 3 && a[2].is_string() => {
                if let (Some(t), Some(c)) = (integer(&a[0]), integer(&a[1])) {
                    out.push((t, c, strings.id(a[2].as_str().unwrap_or_default())));
                    return;
                }
                a.iter().for_each(|v| visit(v, strings, out));
            }
            Value::Array(a) => a.iter().for_each(|v| visit(v, strings, out)),
            Value::Object(o) => o.values().for_each(|v| visit(v, strings, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    doc.values().for_each(|v| visit(v, strings, &mut found));
    // Newest first by `compareStamps`: time, counter, then device text.
    let mut order = indices(found.len());
    sort(&mut order, &|a, b| {
        let (a, b) = (found[a], found[b]);
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then_with(|| strings.get(b.2).cmp(strings.get(a.2)))
    });
    let mut out = permute(found, &order);
    out.dedup_by_key(|s| s.0);
    out
}
