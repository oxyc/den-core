//! Writes on the held documents: `apply_write` over the index, write after write, and `doc_merge` of a document into
//! it. The held document and its registers change in place, and the answer is what a client stores and sends: each
//! changed document's name and its plaintext as `doc_encode` makes it, never the document itself.

use super::{Columns, Index};
use crate::library_v4::doc::safe_u64;
use crate::library_v4::{checked, encoded, jcs, merge, name, write::apply_write, Encoded};
use serde_json::{json, Map, Value};

/// What a batch of writes, or a merge, did to the index.
#[derive(Default)]
pub struct Applied {
    /// Each document changed and held, once, in the order first changed: its name and its plaintext as `doc_encode`
    /// makes it (`write` for a write, not for a merge).
    pub documents: Vec<(String, Vec<u8>)>,
    /// The documents over `doc_encode`'s cap: not held, so the version held before stands.
    pub too_large: Vec<String>,
    /// For each write, in order: `changed`, `unchanged`, `not_newer` (an `if_newer` write no later than the episode
    /// registers it names hold), or `apply_write`'s refusal code. A merge's one outcome is `changed` or `unchanged`.
    pub outcomes: Vec<String>,
}

const CHANGED: &str = "changed";
const UNCHANGED: &str = "unchanged";
const NOT_NEWER: &str = "not_newer";

/// The documents a batch has read or written, parsed once for the whole batch. Its maps are types the module already
/// holds (`index`).
#[derive(Default)]
struct Work {
    documents: Map<String, Value>,
    /// The documents a write changed, in the order first changed, and that order by name.
    changed: Vec<String>,
    order: Index<String>,
}

impl Work {
    /// The document `name` as the batch has it so far, taken out until `put` back.
    fn take(&mut self, columns: &Columns, name: &str) -> Result<Option<Value>, String> {
        if let Some(document) = self.documents.remove(name) {
            return Ok(Some(document));
        }
        columns
            .document(name)
            .map(|text| serde_json::from_str(text).map_err(|_| "invalid_document".to_owned()))
            .transpose()
    }

    fn put(&mut self, name: String, document: Option<Value>) {
        if let Some(document) = document {
            self.documents.insert(name, document);
        }
    }
}

/// The TV's test that an episode state is an edit and not a projection of the document: the latest time its register
/// and the covering resets hold, the season's `seasonReset` and the series' `episodesReset`.
fn latest_held(title: Option<&Value>, season: Option<&Value>, episode: u64) -> i64 {
    let t = |stamp: &Value| stamp[0].as_f64().unwrap_or(0.0);
    let null = Value::Null;
    let season = season.unwrap_or(&null);
    let register = &season["episodes"][episode.to_string()];
    let plays = register["plays"].as_object().into_iter().flatten();
    [
        t(&register["progress"]["at"]),
        t(&register["cleared"][1]),
        t(&season["seasonReset"]),
        t(&title.unwrap_or(&null)["episodesReset"]),
    ]
    .into_iter()
    .chain(plays.filter_map(|(_, at)| at.as_f64()))
    .fold(0.0, f64::max) as i64
}

/// The episodes a stamped write names (`episode`, `episodes`), as `apply_write` reads them.
fn coordinates(write: &Value) -> Vec<(u64, u64)> {
    let pair = |c: &Value| safe_u64(&c[0]).zip(safe_u64(&c[1]));
    let episodes = write["episodes"].as_array().into_iter().flatten();
    pair(&write["episode"])
        .into_iter()
        .chain(episodes.filter_map(pair))
        .collect()
}

/// The season documents a write may touch.
fn seasons(write: &Value) -> Vec<u64> {
    let mut seasons: Vec<u64> = coordinates(write).iter().map(|(s, _)| *s).collect();
    let items = write["items"].as_array().into_iter().flatten();
    seasons.extend(items.filter_map(|item| safe_u64(&item["season"])));
    seasons.extend(safe_u64(&write["season"]));
    seasons.sort_unstable();
    seasons.dedup();
    seasons
}

impl Columns {
    /// `apply_write` of one write on the held documents: a batch of one, held only when it is not refused.
    pub fn apply(&mut self, write: &Value, target: &Value, now: i64) -> Result<Applied, String> {
        self.apply_batch(&[json!({"target": target, "write": write})], now, true)
    }

    /// `apply_write` of each `{target, write}` in order, each on the documents as the writes before it left them, the
    /// importing account's receipts the held delivery documents. `if_newer: true` on a write applies it only when its
    /// stamp is later than every time held on the episode registers it names (`not_newer` otherwise): the TV's test
    /// that a local episode state is an edit, not a projection of the document.
    ///
    /// Each changed document is encoded once, at the end, as a write. A refused write changes nothing and the rest
    /// still apply, unless `atomic`: then a refusal or a document over the cap holds nothing of the batch. An error is
    /// a document that does not encode to itself (`doc_encode`'s `self_check`), and holds nothing.
    pub fn apply_batch(
        &mut self,
        writes: &[Value],
        now: i64,
        atomic: bool,
    ) -> Result<Applied, String> {
        let mut work = Work::default();
        let mut outcomes = Vec::with_capacity(writes.len());
        for op in writes {
            outcomes.push(self.apply_one(&mut work, op, now));
        }
        let refused = outcomes
            .iter()
            .any(|o| ![CHANGED, UNCHANGED, NOT_NEWER].contains(&o.as_str()));
        let mut applied = Applied {
            outcomes,
            ..Applied::default()
        };
        if atomic && refused {
            return Ok(applied);
        }
        let changed = std::mem::take(&mut work.changed);
        let mut held = Vec::with_capacity(changed.len());
        for name in changed {
            let document = work.documents.remove(&name).unwrap_or_default();
            match encoded(&document, true)? {
                Encoded::Plaintext { plaintext, .. } => {
                    applied.documents.push((name, plaintext));
                    held.push(document);
                }
                Encoded::TooLarge(_) => applied.too_large.push(name),
            }
        }
        if atomic && !applied.too_large.is_empty() {
            applied.documents.clear();
            return Ok(applied);
        }
        self.hold_all(held)?;
        Ok(applied)
    }

    fn apply_one(&self, work: &mut Work, op: &Value, now: i64) -> String {
        let (target, write) = (&op["target"], &op["write"]);
        let (Some(media), Some(id)) = (
            target["type"]
                .as_str()
                .filter(|t| matches!(*t, "movie" | "tv")),
            safe_u64(&target["id"]),
        ) else {
            // `apply_write` refuses it before it reads a document.
            return apply_write(write, target, None, &[], &[], now)
                .err()
                .unwrap_or_else(|| UNCHANGED.into());
        };
        let numbers = if media == "tv" {
            seasons(write)
        } else {
            vec![]
        };
        let title_name = format!("title:{media}:{id}");
        let season_names: Vec<String> = numbers
            .iter()
            .map(|s| format!("season:tv:{id}:{s}"))
            .collect();
        // The importing account's delivery documents for the target, which `apply_write` finds by name.
        let account = write["provider"].as_str().zip(write["account"].as_str());
        let receipt_names: Vec<String> = match account {
            Some((p, a))
                if write["kind"]
                    .as_str()
                    .is_some_and(|k| k.starts_with("import_")) =>
            {
                if media == "tv" {
                    numbers
                        .iter()
                        .map(|s| format!("dlv:{p}:{a}:tv:{id}:{s}"))
                        .collect()
                } else {
                    vec![format!("dlv:{p}:{a}:movie:{id}")]
                }
            }
            _ => vec![],
        };
        let title = match work.take(self, &title_name) {
            Ok(title) => title,
            Err(code) => return code,
        };
        // Those held, by name: `apply_write` takes them as a list, and creates the rest.
        let mut read = |names: Vec<String>| -> Result<(Vec<String>, Vec<Value>), String> {
            let mut held = (Vec::new(), Vec::new());
            for name in names {
                if let Some(document) = work.take(self, &name)? {
                    held.0.push(name);
                    held.1.push(document);
                }
            }
            Ok(held)
        };
        let (seasons, receipts) = match (read(season_names), read(receipt_names)) {
            (Ok(s), Ok(r)) => (s, r),
            (Err(code), _) | (_, Err(code)) => return code,
        };
        let newer = op["if_newer"] != true
            || coordinates(write).iter().all(|(s, e)| {
                let name = format!("season:tv:{id}:{s}");
                let season = seasons.0.iter().position(|n| *n == name);
                let season = season.map(|i| &seasons.1[i]);
                write["at"][0].as_i64().unwrap_or(0) > latest_held(title.as_ref(), season, *e)
            });
        let answer = if newer {
            apply_write(write, target, title.as_ref(), &seasons.1, &receipts.1, now)
        } else {
            Err(NOT_NEWER.into())
        };
        work.put(title_name, title);
        for (names, documents) in [seasons, receipts] {
            for (name, document) in names.into_iter().zip(documents) {
                work.put(name, Some(document));
            }
        }
        let mut answer = match answer {
            Ok(answer) => answer,
            Err(code) => return code,
        };
        let documents = match answer["documents"].take() {
            Value::Array(documents) => documents,
            _ => vec![],
        };
        if documents.is_empty() {
            return UNCHANGED.into();
        }
        for document in documents {
            let Ok(name) = name(&document) else {
                return "invalid_document".into();
            };
            if !work.order.contains_key(&name) {
                work.order.insert(name.clone(), work.changed.len());
                work.changed.push(name.clone());
            }
            work.documents.insert(name, document);
        }
        CHANGED.into()
    }

    /// `doc_merge` of `document` with the held version of it, held: `changed` with the merge's plaintext (not a
    /// write) when the merge differs from the held version, `unchanged` otherwise. With none held, `document` is held
    /// as a reader reads it. A refusal is `doc_merge`'s, and changes nothing.
    pub fn merge(&mut self, document: &Value) -> Result<Applied, String> {
        let (identity, incoming) = checked(document)?;
        let name = identity.name();
        let merged = match self.document(&name) {
            Some(text) => {
                let held: Value = serde_json::from_str(text).map_err(|_| "invalid_document")?;
                let (held_identity, held_doc) = checked(&held)?;
                if held_identity != identity {
                    return Err("identity_mismatch".into());
                }
                let merged = merge::merge(&identity, &held_doc, &incoming);
                if jcs::same(&held, &merged) {
                    return Ok(Applied {
                        outcomes: vec![UNCHANGED.into()],
                        ..Applied::default()
                    });
                }
                merged
            }
            None => Value::Object(incoming),
        };
        let mut applied = Applied {
            outcomes: vec![CHANGED.into()],
            ..Applied::default()
        };
        match encoded(&merged, false)? {
            Encoded::Plaintext { plaintext, .. } => {
                self.hold_all(vec![merged])?;
                applied.documents.push((name, plaintext));
            }
            Encoded::TooLarge(_) => applied.too_large.push(name),
        }
        Ok(applied)
    }

    /// `apply_batch` of a JSON array of `{target, write, if_newer?}`.
    pub fn apply_batch_text(
        &mut self,
        writes: &str,
        now: i64,
        atomic: bool,
    ) -> Result<Applied, String> {
        let writes: Vec<Value> = serde_json::from_str(writes).map_err(|_| "invalid_request")?;
        self.apply_batch(&writes, now, atomic)
    }

    /// `apply` of the write's and the target's JSON.
    pub fn apply_text(&mut self, write: &str, target: &str, now: i64) -> Result<Applied, String> {
        let parse = |text: &str| serde_json::from_str::<Value>(text).map_err(|_| "invalid_request");
        self.apply(&parse(write)?, &parse(target)?, now)
    }

    /// `merge` of the document's JSON.
    pub fn merge_text(&mut self, document: &str) -> Result<Applied, String> {
        let document: Value = serde_json::from_str(document).map_err(|_| "invalid_request")?;
        self.merge(&document)
    }

    /// Hold documents den-core made, each read as `upsert` reads it.
    fn hold_all(&mut self, documents: Vec<Value>) -> Result<(), String> {
        let slots = documents
            .into_iter()
            .map(|document| {
                let text = serde_json::value::to_raw_value(&document)
                    .map_err(|_| "invalid_document".to_owned())?;
                self.read_value(text, document)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for slot in slots {
            self.hold(slot);
        }
        Ok(())
    }
}
