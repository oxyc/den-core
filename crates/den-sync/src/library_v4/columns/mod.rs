//! `LibraryColumns`: the library's documents read once into typed registers, and whole-library lists answered from
//! them in one call, where clients asked `title_state` / `season_state_v4` per document and `episode_mark` per episode.
//!
//! Each document is held by name, in the order it was first added (a replaced document keeps its place), as its text
//! (the canonical form, unknown members and all, §6) and its typed registers; device ids, statuses and reactions are
//! held once (`Strings`). Nothing held depends on the time: a stamp more than a day ahead is read as timeless only when
//! a list is asked for, and `reconsider_at` says when the next such reading changes without a write. Writes and merges
//! can be made on the held documents (`apply`), by the per-document ops, which are unchanged.
//!
//! Lists a client renders come back as typed columns (`Answer`); lists that are input to another den-core op
//! (`pending_targets_v4`, `assistant_projection`) come back as that op's JSON, to be passed on untouched. The browser
//! startup module holds the rendered lists only: the assistant projection runs in the assistant module, which takes the
//! held documents (`texts`), and delivery in the deferred one.

mod apply;
#[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
mod assistant;
mod derive;
mod lists;
mod read;

use super::doc::{identity, Kind};
use super::{decode, into_readable};
use read::{SeasonDoc, Strings, TitleDoc};
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};
use std::cmp::Ordering;

pub(crate) use crate::index::{indices, permute, sort, Index};
pub use apply::Applied;
pub use lists::{Airing, Layouts};

/// A title's `type:id` key as one number: the TMDB id, then 1 for a series.
pub(crate) fn code(tv: bool, id: u64) -> u64 {
    (id << 1) | u64::from(tv)
}

/// The byte order of two `code`s' `type:id` keys: `movie:…` before `tv:…`, then the ids' decimal text. The order every
/// client breaks a tie between titles by (den-edge `compareHistoryTitleKeys`).
pub(crate) fn key_cmp(a: u64, b: u64) -> Ordering {
    fn digits(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                return &buf[i..];
            }
        }
    }
    let (mut x, mut y) = ([0; 20], [0; 20]);
    (a & 1)
        .cmp(&(b & 1))
        .then_with(|| digits(a >> 1, &mut x).cmp(digits(b >> 1, &mut y)))
}

/// One column of an answer: numbers (`NaN` for none), or text (`""` for none).
pub enum Column {
    Numbers(Vec<f64>),
    Strings(Vec<String>),
}

/// A query's answer: named columns, `<list>.<column>`, every column of a list the same length.
#[derive(Default)]
pub struct Answer {
    columns: Vec<(String, Column)>,
}

impl Answer {
    pub fn names(&self) -> Vec<String> {
        self.columns.iter().map(|(name, _)| name.clone()).collect()
    }

    fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|(n, _)| n == name).map(|(_, c)| c)
    }

    pub fn numbers(&self, name: &str) -> Option<&[f64]> {
        match self.column(name)? {
            Column::Numbers(n) => Some(n),
            Column::Strings(_) => None,
        }
    }

    pub fn strings(&self, name: &str) -> Option<&[String]> {
        match self.column(name)? {
            Column::Strings(s) => Some(s),
            Column::Numbers(_) => None,
        }
    }

    fn put(&mut self, name: &str, column: Column) {
        self.columns.push((name.to_owned(), column));
    }

    /// The columns as one JSON object, `NaN` as `null`: the stateless `library_columns` op's form.
    pub fn to_json(&self) -> Value {
        let columns = self.columns.iter().map(|(name, column)| {
            let value = match column {
                Column::Numbers(n) => n
                    .iter()
                    .map(|x| json!(x.is_finite().then_some(*x)))
                    .collect(),
                Column::Strings(s) => s.iter().map(|x| json!(x)).collect(),
            };
            (name.clone(), Value::Array(value))
        });
        Value::Object(columns.collect())
    }
}

enum Body {
    Title(Box<TitleDoc>),
    Season(SeasonDoc),
    /// A delivery document: held for `pending`, read by no list.
    Delivery,
}

struct Slot {
    name: String,
    text: Box<RawValue>,
    body: Body,
    /// Its stamps for `newest` (`read::stamps`).
    stamps: Vec<(i64, i64, u32)>,
}

pub struct Columns {
    slots: Vec<Option<Slot>>,
    by_name: Index<String>,
    /// Each series' title document, by TMDB id.
    series_title: Index<u64>,
    strings: Strings,
}

impl Default for Columns {
    fn default() -> Self {
        Columns {
            slots: Vec::new(),
            by_name: Index::new(),
            series_title: Index::new(),
            strings: Strings::new(),
        }
    }
}

impl Columns {
    pub fn new() -> Self {
        Self::default()
    }

    /// Documents held.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// A document read as state is read from it (`readable`), not yet held.
    fn read(&mut self, text: Box<RawValue>) -> Result<Slot, String> {
        let value: Value = serde_json::from_str(text.get()).map_err(|_| "invalid_request")?;
        self.read_value(text, value)
    }

    /// `read` of a document already parsed from `text`.
    fn read_value(&mut self, text: Box<RawValue>, value: Value) -> Result<Slot, String> {
        let doc = into_readable(value)?;
        let identity = identity(&doc)?;
        let strings = &mut self.strings;
        let body = match identity.kind {
            Kind::Title => Body::Title(Box::new(TitleDoc::read(
                identity.media == "tv",
                identity.id,
                &doc,
                strings,
            ))),
            Kind::Season => Body::Season(SeasonDoc::read(
                identity.id,
                identity.season.unwrap_or(0),
                &doc,
                strings,
            )),
            Kind::Delivery => Body::Delivery,
        };
        Ok(Slot {
            name: identity.name(),
            stamps: read::stamps(&doc, strings),
            text,
            body,
        })
    }

    fn hold(&mut self, slot: Slot) {
        let index = match self.by_name.get(&slot.name) {
            Some(&index) => index,
            None => {
                self.by_name.insert(slot.name.clone(), self.slots.len());
                self.slots.push(None);
                self.slots.len() - 1
            }
        };
        if let Body::Title(t) = &slot.body {
            if t.tv {
                self.series_title.insert(t.id, index);
            }
        }
        self.slots[index] = Some(slot);
    }

    /// Add or replace documents, each the JSON text of one document. All are read before any is held, so a document
    /// that cannot be read refuses the batch and changes nothing.
    pub fn upsert(&mut self, documents: Vec<Box<RawValue>>) -> Result<usize, String> {
        let slots = documents
            .into_iter()
            .map(|text| self.read(text))
            .collect::<Result<Vec<_>, _>>()?;
        for slot in slots {
            self.hold(slot);
        }
        Ok(self.len())
    }

    /// `upsert` of a JSON array of documents.
    pub fn upsert_text(&mut self, documents: &str) -> Result<usize, String> {
        let documents: Vec<Box<RawValue>> =
            serde_json::from_str(documents).map_err(|_| "invalid_request")?;
        self.upsert(documents)
    }

    /// `doc_decode` of one row's plaintext, the document held here rather than handed back: the answer is the decode's
    /// status, name and dropped parts. A JSON row or an unreadable one is answered as `doc_decode` answers it.
    pub fn decode(&mut self, plaintext: &[u8], expected: Option<&str>) -> Value {
        let mut decoded = decode(plaintext, expected);
        if let Some(document) = decoded.as_object_mut().and_then(|d| d.remove("document")) {
            let held = serde_json::value::to_raw_value(&document)
                .map_err(|_| "invalid_document".to_owned())
                .and_then(|text| self.read(text));
            match held {
                Ok(slot) => self.hold(slot),
                Err(reason) => return json!({"status": "unreadable", "reason": reason}),
            }
        }
        decoded
    }

    /// One held document's text, as it was added.
    pub fn document(&self, name: &str) -> Option<&str> {
        let slot = self.slots[*self.by_name.get(name)?].as_ref()?;
        Some(slot.text.get())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let Some(index) = self.by_name.remove(name) else {
            return false;
        };
        if let Some(Slot {
            body: Body::Title(t),
            ..
        }) = &self.slots[index]
        {
            if t.tv {
                self.series_title.remove(&t.id);
            }
        }
        self.slots[index] = None;
        // Keep removed places from outgrowing the library.
        if self.slots.len() > 64 && self.slots.len() > 2 * self.by_name.len() {
            self.compact();
        }
        true
    }

    fn compact(&mut self) {
        let slots: Vec<Slot> = std::mem::take(&mut self.slots)
            .into_iter()
            .flatten()
            .collect();
        self.by_name.clear();
        self.series_title.clear();
        for slot in slots {
            self.hold(slot);
        }
    }

    fn held(&self) -> impl Iterator<Item = &Slot> {
        self.slots.iter().flatten()
    }

    /// Every held document's text, for an op that reads documents (`pending_targets_v4`).
    pub fn texts(&self) -> impl Iterator<Item = &RawValue> {
        self.held().map(|slot| &*slot.text)
    }

    /// Every held document as one JSON array, in order: a request's `documents` for an op in another module.
    pub fn documents_text(&self) -> String {
        let mut out = String::from("[");
        for (i, text) in self.texts().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(text.get());
        }
        out.push(']');
        out
    }

    /// `pending_targets_v4` over the held documents.
    #[cfg(feature = "tracker-delivery")]
    pub fn pending(&self, deliver: &Value, now: i64) -> Result<String, String> {
        let pending =
            super::delivery::pending(self.texts().map(super::delivery::Source::Raw), deliver, now)?;
        serde_json::to_string(&pending).map_err(|_| "invalid_request".to_owned())
    }

    /// `pending` of the `deliver` facts' JSON text.
    #[cfg(feature = "tracker-delivery")]
    pub fn pending_text(&self, deliver: &str, now: i64) -> Result<String, String> {
        let deliver: Value = serde_json::from_str(deliver).map_err(|_| "invalid_request")?;
        self.pending(&deliver, now)
    }
}

/// `library_columns` (stateless): `documents` read into an index once and the named `queries` answered from it, each
/// as its columns' JSON. `layouts` and `airing` are `continue`'s; `layouts` is also `assistant`'s.
pub fn answer(
    documents: Vec<Value>,
    queries: &[String],
    now: i64,
    layouts: Option<&Value>,
    airing: Option<&Value>,
) -> Result<Value, String> {
    let mut columns = Columns::new();
    let texts = documents
        .iter()
        .map(serde_json::value::to_raw_value)
        .collect::<Result<_, _>>()
        .map_err(|_| "invalid_request")?;
    columns.upsert(texts)?;
    let layouts = Layouts::read(layouts.unwrap_or(&Value::Null))?;
    let airing = airing.map(Airing::read).transpose()?;
    let mut out = Map::new();
    for query in queries {
        let value = match query.as_str() {
            "home" => columns.home(now).to_json(),
            "history" => columns.history(now).to_json(),
            "watchlist" => columns.watchlist(now).to_json(),
            "continue" => columns.continuing(now, &layouts, airing.as_ref()).to_json(),
            "newest" => columns.newest(now).to_json(),
            #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
            "assistant" => columns.assistant(now, &layouts),
            _ => return Err("unknown_query".into()),
        };
        out.insert(query.clone(), value);
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests;
