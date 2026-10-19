//! §10 The switch: `v4_form` converts a v3 log through `base` into documents, and `v4_dry_run` checks the form
//! against the shipped `library_v3` reference before anything is committed.

use super::codec;
use super::delivery::{self, Snapshot, Source};
use super::doc::{
    identity, sanitize, sanitize_register, valid_account, valid_episode_key, valid_provider,
    Identity, Kind,
};
use super::jcs;
use super::merge;
use super::state;
use crate::library_v3;
use crate::wire::{name, stamp, Stamp};
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ROWS: usize = 50_000;
const STORED_CAP: u64 = 32 * 1024 * 1024;
const KEY_BYTES: u64 = 64;
const BLOCK: u64 = 32;

const REC_FIELDS: [&str; 8] = [
    "status",
    "resume",
    "reaction",
    "deleted",
    "dismissed",
    "episodesReset",
    "addedAt",
    "watchedAt",
];

fn bump(counts: &mut BTreeMap<String, u64>, what: &str, by: u64) {
    if by > 0 {
        *counts.entry(what.into()).or_default() += by;
    }
}

fn is_event(row: &Value) -> bool {
    (row["kind"] == "set"
        && row["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("tracker-event:")))
        || (row["schema"] == 1 && row.get("after").is_some())
}

fn is_v3(row: &Value) -> bool {
    matches!(row["kind"].as_str(), Some("rec" | "wat" | "snt" | "ep")) || is_event(row)
}

fn u(value: &Value) -> Option<u64> {
    super::doc::safe_u64(value)
}

/// The v3 row name a v3 row's state lands in after §8's fold: an episode row lands in its block's watch row, and
/// a v1 event in the row its `after` names.
fn landing(row: &Value) -> Option<String> {
    let row = if is_event(row) {
        if row["kind"] == "set" {
            let event: Value =
                serde_json::from_str(row["values"]["event"]["value"]["string"].as_str()?).ok()?;
            event["after"].clone()
        } else {
            row["after"].clone()
        }
    } else {
        row.clone()
    };
    if row["kind"] == "ep" {
        if row["title"]["type"] != "tv" {
            return None;
        }
        return Some(format!(
            "wat:tv:{}:{}:{}",
            u(&row["title"]["id"])?,
            u(&row["season"])?,
            u(&row["episode"])? / BLOCK
        ));
    }
    name(&row).ok()
}

/// A row of the log as the request spells it (`{k, seq, bytes, row}`): a library's rows are many times the size of
/// their text once read, so a v4 document is only found here and read again when it is used.
pub type Entry<'a> = &'a RawValue;

/// An entry read; one that is no JSON value refuses the request, as when the request was parsed whole.
fn entry(entry: Entry) -> Result<Value, String> {
    serde_json::from_str(entry.get()).map_err(|_| "invalid_request".to_string())
}

/// A v4 document of the switch: a row of the log read again when it is used, or one already read (a converted v3
/// row, or two rows of one name merged).
enum Doc<'a> {
    Row(Identity, Entry<'a>),
    Read(Map<String, Value>),
}

impl Doc<'_> {
    fn identity(&self) -> Option<Identity> {
        match self {
            Doc::Row(id, _) => Some(id.clone()),
            Doc::Read(doc) => identity(doc).ok(),
        }
    }

    /// The document, `checked`. A row's was checked when it was found, so it reads the same again.
    fn read(&self) -> Map<String, Value> {
        match self {
            Doc::Row(_, raw) => entry(raw)
                .ok()
                .and_then(|entry| super::checked(&entry["row"]).ok())
                .map(|(_, doc)| doc)
                .unwrap_or_default(),
            Doc::Read(doc) => doc.clone(),
        }
    }

    fn into_read(self) -> Map<String, Value> {
        match self {
            Doc::Read(doc) => doc,
            row => row.read(),
        }
    }

    /// The document to be written: a row of the log is read first.
    fn read_mut(&mut self) -> &mut Map<String, Value> {
        if let Doc::Row(..) = self {
            *self = Doc::Read(self.read());
        }
        match self {
            Doc::Read(doc) => doc,
            Doc::Row(..) => unreachable!("a row is read above"),
        }
    }
}

/// The switch's documents by name, kept apart by kind as the switch writes them.
#[derive(Default)]
struct Docs<'a> {
    titles: BTreeMap<String, Doc<'a>>,
    seasons: BTreeMap<String, Doc<'a>>,
    delivery: BTreeMap<String, Doc<'a>>,
}

impl<'a> Docs<'a> {
    fn bucket(&mut self, kind: Kind) -> &mut BTreeMap<String, Doc<'a>> {
        match kind {
            Kind::Title => &mut self.titles,
            Kind::Season => &mut self.seasons,
            Kind::Delivery => &mut self.delivery,
        }
    }

    fn names(&self) -> impl Iterator<Item = &String> {
        self.titles
            .keys()
            .chain(self.seasons.keys())
            .chain(self.delivery.keys())
    }
}

struct Input<'a> {
    v3: Vec<Value>,
    seqs: BTreeMap<String, u64>,
    documents: Docs<'a>,
    keep: Vec<(String, u64)>,
    settings: Vec<Value>,
}

fn read<'a>(rows: &[Entry<'a>]) -> Result<Input<'a>, String> {
    let mut input = Input {
        v3: Vec::new(),
        seqs: BTreeMap::new(),
        documents: Docs::default(),
        keep: Vec::new(),
        settings: Vec::new(),
    };
    for raw in rows {
        let mut entry = entry(raw)?;
        let k = entry["k"].as_str().unwrap_or_default().to_owned();
        let bytes = entry["bytes"].as_u64().unwrap_or(0);
        let seq = entry["seq"].as_u64().unwrap_or(0);
        let row = entry.get_mut("row").map(Value::take).unwrap_or_default();
        if is_v3(&row) {
            if let Some(target) = landing(&row) {
                let at = input.seqs.entry(target).or_default();
                *at = (*at).max(seq);
            }
            input.v3.push(row);
            continue;
        }
        if row["kind"]
            .as_str()
            .is_some_and(|k| Kind::parse(k).is_some())
        {
            if let Ok((id, doc)) = super::checked(&row) {
                let bucket = input.documents.bucket(id.kind);
                let name = id.name();
                let doc = match bucket.remove(&name) {
                    Some(old) => Doc::Read(
                        merge::merge(&id, &old.into_read(), &doc)
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                    ),
                    None => Doc::Row(id, raw),
                };
                bucket.insert(name, doc);
                continue;
            }
        }
        if row["kind"] == "set" {
            input.settings.push(row);
        }
        input.keep.push((k, bytes));
    }
    Ok(input)
}

fn has(input: &Input, test: impl Fn(&Value) -> bool) -> bool {
    input.v3.iter().any(test)
}

fn seeded_through(setting: &Value) -> Option<u64> {
    setting["values"]["seededThrough"]["value"]["int"].as_u64()
}

/// The accounts a `set:deliver:<provider>:<account>` row names, with its row.
fn deliver_rows(settings: &[Value]) -> Vec<(String, String, Value)> {
    settings
        .iter()
        .filter_map(|row| {
            let rest = row["name"].as_str()?.strip_prefix("deliver:")?;
            let (provider, account) = rest.split_once(':')?;
            Some((provider.to_owned(), account.to_owned(), row.clone()))
        })
        .collect()
}

#[derive(Default)]
struct Form {
    titles: BTreeMap<String, Map<String, Value>>,
    seasons: BTreeMap<String, Map<String, Value>>,
    delivery: BTreeMap<String, Map<String, Value>>,
    /// v3 rows v3 itself keeps but the conversion could not place, as `(reason, row name)`: the dry run aborts on
    /// each, so state or a receipt is never lost silently.
    dropped: Vec<(&'static str, String)>,
}

/// One receipt entry's place in v4: the delivery document's title and season, and the entry's key there.
type Place = (String, u64, Option<u64>, String);

fn title_coordinate(media: &str, id: &str) -> Option<(String, u64)> {
    let id = id.parse::<u64>().ok().filter(|id| *id > 0)?;
    matches!(media, "movie" | "tv").then(|| (media.to_owned(), id))
}

/// A title's own receipt keys: `list` and `rating`, and `watch` for a film (v3 §6).
fn title_key(media: &str, key: &str) -> bool {
    matches!(key, "list" | "rating") || (key == "watch" && media == "movie")
}

/// Where each entry of a v3 `snt` row lands in v4, with its entry, and how many keys v3 never reads (invalid keys,
/// dropped and counted). `None` when the row names a target of no shape v3 delivers to, so none of its entries can
/// be placed. The shapes:
/// - `target` `wat:<type>:<id>:<season>:<block>`: an episode key (the block condition holding), or a film's `"0"`;
/// - `target` `rec:<type>:<id>`: the title's `watch|list|rating`, the form both shipped clients write title
///   receipts in (stored under the `t<shard>` name);
/// - no `target` (a `t<shard>` row): `rec:<type>:<id>#watch|list|rating`.
fn receipt_places(row: &Value) -> Option<(Vec<(Place, Value)>, u64)> {
    enum Target {
        Watch(String, u64, u64, u64),
        Title(String, u64),
        Shard,
    }
    let target = match row["target"].as_str() {
        None => Target::Shard,
        Some(target) => match target.split(':').collect::<Vec<_>>().as_slice() {
            ["wat", media, id, season, block] => {
                let (media, id) = title_coordinate(media, id)?;
                Target::Watch(media, id, season.parse().ok()?, block.parse().ok()?)
            }
            ["rec", media, id] => {
                let (media, id) = title_coordinate(media, id)?;
                Target::Title(media, id)
            }
            _ => return None,
        },
    };
    let mut placed = Vec::new();
    let mut invalid = 0;
    for (key, entry) in row["entries"].as_object().into_iter().flatten() {
        let place = match &target {
            Target::Watch(media, id, _, _) if media == "movie" => {
                (key == "0").then(|| (media.clone(), *id, None, "watch".to_owned()))
            }
            Target::Watch(media, id, season, block) => (valid_episode_key(key)
                && key.parse::<u64>().is_ok_and(|e| e / BLOCK == *block))
            .then(|| (media.clone(), *id, Some(*season), key.clone())),
            Target::Title(media, id) => {
                title_key(media, key).then(|| (media.clone(), *id, None, key.clone()))
            }
            Target::Shard => key.split_once('#').and_then(|(rec, field)| {
                let ["rec", media, id] = rec.split(':').collect::<Vec<_>>()[..] else {
                    return None;
                };
                let (media, id) = title_coordinate(media, id)?;
                title_key(&media, field).then(|| (media, id, None, field.to_owned()))
            }),
        };
        match place {
            Some(place) => placed.push((place, entry.clone())),
            None => invalid += 1,
        }
    }
    Some((placed, invalid))
}

fn doc_for(
    bucket: &mut BTreeMap<String, Map<String, Value>>,
    identity: Identity,
) -> &mut Map<String, Value> {
    bucket
        .entry(identity.name())
        .or_insert_with(|| new_doc(identity))
}

fn new_doc(identity: Identity) -> Map<String, Value> {
    let mut doc = identity.members();
    match identity.kind {
        Kind::Season => {
            doc.insert("seasonReset".into(), Value::Null);
            doc.insert("episodes".into(), json!({}));
        }
        Kind::Delivery => {
            doc.insert("entries".into(), json!({}));
        }
        Kind::Title => {}
    }
    doc
}

fn title_identity(row: &Value, kind: Kind, season: Option<u64>) -> Option<Identity> {
    let media = row["title"]["type"]
        .as_str()
        .filter(|t| matches!(*t, "movie" | "tv"))?;
    Some(Identity {
        kind,
        media: media.into(),
        id: u(&row["title"]["id"]).filter(|id| *id > 0)?,
        season,
        provider: None,
        account: None,
    })
}

fn row_unknowns(row: &Value, known: &[&str]) -> u64 {
    row.as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| !known.contains(&k.as_str()))
        .count() as u64
}

/// The documents `compacted`'s rows convert to. Their state is moved into the documents, not copied.
fn convert(compacted: Vec<Value>, counts: &mut BTreeMap<String, u64>) -> Result<Form, String> {
    let mut form = Form::default();
    for mut row in compacted {
        let row = &mut row;
        let kind = row["kind"].as_str().map(str::to_owned);
        match kind.as_deref() {
            Some("rec") => {
                let Some(id) = title_identity(row, Kind::Title, None) else {
                    bump(counts, "failed_identity", 1);
                    continue;
                };
                bump(
                    counts,
                    "row_unknown_fields",
                    row_unknowns(
                        row,
                        &[&["kind", "schema", "title"][..], &REC_FIELDS[..]].concat(),
                    ),
                );
                let doc = doc_for(&mut form.titles, id);
                for field in REC_FIELDS {
                    if let Some(value) = row.get_mut(field) {
                        doc.insert(field.into(), value.take());
                    }
                }
            }
            Some("wat") => {
                let season = u(&row["season"]).unwrap_or(0);
                let block = u(&row["block"]).unwrap_or(0);
                bump(
                    counts,
                    "row_unknown_fields",
                    row_unknowns(
                        row,
                        &[
                            "kind",
                            "schema",
                            "title",
                            "season",
                            "block",
                            "seasonReset",
                            "entries",
                        ],
                    ),
                );
                let mut entries = row
                    .get_mut("entries")
                    .and_then(Value::as_object_mut)
                    .map(std::mem::take)
                    .unwrap_or_default();
                if row["title"]["type"] == "movie" {
                    let Some(id) = title_identity(row, Kind::Title, None) else {
                        bump(counts, "failed_identity", 1);
                        continue;
                    };
                    bump(
                        counts,
                        "film_wat_keys",
                        entries.keys().filter(|k| *k != "0").count() as u64,
                    );
                    if let Some(register) = entries.remove("0") {
                        doc_for(&mut form.titles, id).insert("watch".into(), register);
                    }
                    continue;
                }
                let Some(id) = title_identity(row, Kind::Season, Some(season)) else {
                    bump(counts, "failed_identity", 1);
                    continue;
                };
                let doc = doc_for(&mut form.seasons, id);
                if block == 0 {
                    let reset = row.get_mut("seasonReset").map(Value::take);
                    doc.insert("seasonReset".into(), reset.unwrap_or_default());
                }
                for (key, register) in entries {
                    let valid = valid_episode_key(&key)
                        && key.parse::<u64>().is_ok_and(|e| e / BLOCK == block);
                    if !valid {
                        bump(counts, "invalid_keys", 1);
                        continue;
                    }
                    doc["episodes"][&key] = register;
                }
            }
            Some("snt") => {
                let provider = row["provider"].as_str().unwrap_or_default();
                let account = row["account"].as_str().unwrap_or_default();
                if !valid_provider(provider) || !valid_account(account) {
                    return Err(format!("invalid_account:{provider}:{account}"));
                }
                bump(
                    counts,
                    "row_unknown_fields",
                    row_unknowns(
                        row,
                        &[
                            "kind", "schema", "provider", "account", "target", "shard", "entries",
                        ],
                    ),
                );
                let delivery = |media: &str, id: u64, season: Option<u64>| Identity {
                    kind: Kind::Delivery,
                    media: media.into(),
                    id,
                    season,
                    provider: Some(provider.into()),
                    account: Some(account.into()),
                };
                let Some((placed, invalid)) = receipt_places(row) else {
                    bump(counts, "receipt_dropped", 1);
                    form.dropped
                        .push(("receipt_dropped", name(row).unwrap_or_default()));
                    continue;
                };
                bump(counts, "invalid_keys", invalid);
                for ((media, id, season, key), entry) in placed {
                    let entries =
                        &mut doc_for(&mut form.delivery, delivery(&media, id, season))["entries"];
                    // Two v3 rows can hold one title's receipt (a shard entry and a `rec:` target): §9's merge.
                    let entry = match entries.get(&key) {
                        Some(held) => merge::delivery_entry(held, &entry),
                        None => entry,
                    };
                    entries[&key] = entry;
                }
            }
            // A row v3 cannot name (a film's `ep` row among them) is one v3 drops too (§10 *Unknown kinds*); any
            // other row reaching here is one v3 keeps and the conversion would lose.
            _ => match name(row) {
                Err(_) => bump(counts, "failed_identity", 1),
                Ok(row_name) => {
                    bump(counts, "row_dropped", 1);
                    form.dropped.push(("row_dropped", row_name));
                }
            },
        }
    }
    Ok(form)
}

/// Every target a seeded account has, with the v3 row seq it is judged by (§10 *Seeding*). Only a target judged by a
/// row after `through` is seeded, so only those are collected.
fn seed(
    docs: &mut Docs,
    seqs: &BTreeMap<String, u64>,
    provider: &str,
    account: &str,
    through: u64,
    performer: &str,
) {
    let mut n = 0u64;
    let mut targets: Vec<(Identity, String, &'static str)> = Vec::new();
    let mut target = |delivery: &Identity, key: &str, seq: Option<u64>, shape| {
        if seq.is_some_and(|seq| seq > through) {
            targets.push((delivery.clone(), key.to_owned(), shape));
        }
    };
    for (title_name, title) in &docs.titles {
        let Some(id) = title.identity() else { continue };
        let coordinate = title_name.trim_start_matches("title:").to_owned();
        let rec = seqs.get(&format!("rec:{coordinate}")).copied();
        let film = seqs.get(&format!("wat:{coordinate}:0:0")).copied();
        let delivery = Identity {
            kind: Kind::Delivery,
            provider: Some(provider.into()),
            account: Some(account.into()),
            ..id.clone()
        };
        target(&delivery, "list", rec, "list");
        target(&delivery, "rating", rec, "rating");
        if id.is_film() {
            target(&delivery, "watch", rec.max(film), "watch");
        }
    }
    for season in docs.seasons.values() {
        let Some(id) = season.identity() else {
            continue;
        };
        let delivery = Identity {
            kind: Kind::Delivery,
            provider: Some(provider.into()),
            account: Some(account.into()),
            ..id.clone()
        };
        let season = season.read();
        for key in season
            .get("episodes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(k, _)| k)
        {
            if !valid_episode_key(key) {
                continue;
            }
            let block = key.parse::<u64>().unwrap_or(0) / BLOCK;
            let seq = seqs
                .get(&format!(
                    "wat:tv:{}:{}:{block}",
                    id.id,
                    id.season.unwrap_or(0)
                ))
                .copied();
            target(&delivery, key, seq, "watch");
        }
    }
    targets.sort_by(|a, b| (a.0.name(), &a.1).cmp(&(b.0.name(), &b.1)));
    for (delivery, key, shape) in targets {
        let doc = docs
            .delivery
            .entry(delivery.name())
            .or_insert_with(|| Doc::Read(new_doc(delivery)))
            .read_mut();
        // A document already in the log may hold no `entries` member.
        let entries = doc.entry("entries").or_insert_with(|| json!({}));
        let current = entries.get(&key).cloned();
        match current {
            None => {
                n += 1;
                let order = json!([0, n, performer]);
                entries[&key] = match shape {
                    "list" => json!(["out", [0, 0, ""], order]),
                    "rating" => json!([null, [0, 0, ""], order]),
                    _ => json!(["n", 0, null, [0, 0, ""], order]),
                };
            }
            Some(mut entry) if entry[0] == "n" && entry[1] == -1 => {
                entry[1] = json!(0);
                entries[&key] = entry;
            }
            Some(_) => {}
        }
    }
}

fn parse_since(row: &Value) -> Option<Stamp> {
    let text = row["values"]["since"]["value"]["string"].as_str()?;
    stamp(&serde_json::from_str(text).ok()?).ok()
}

/// `v4_form` (§10, §14): every row through `base` (`{k, seq, bytes, row}`, `row` the decoded plaintext or null)
/// and the performer's id → the switch's documents and the rows staged unchanged, or a failure.
pub fn v4_form(
    rows: &[Entry],
    base: u64,
    performer: &str,
    now: i64,
    stored_cap: Option<u64>,
) -> Result<Form4, String> {
    let input = read(rows)?;
    let mut counts = BTreeMap::new();
    let has_deliver = !deliver_rows(&input.settings).is_empty();
    // A log that already holds v4 documents is a v4 library a stray `ep` or v1 event was written into, not a
    // pre-v3 backup: it switches again (§10), folding those rows into its documents.
    if has(&input, |r| r["kind"] == "ep" || is_event(r))
        && !has(&input, |r| {
            matches!(r["kind"].as_str(), Some("wat" | "snt"))
        })
        && !has_deliver
        && input.documents.names().next().is_none()
    {
        return Err("pre_v3".into());
    }
    for row in &input.v3 {
        let schema = row["schema"].as_u64().unwrap_or(0);
        let newer = match row["kind"].as_str() {
            Some("wat" | "snt") => schema > 3,
            Some("rec") => schema > 2,
            _ => false,
        };
        if newer {
            return Err("newer_v3_row".into());
        }
    }
    bump(
        &mut counts,
        "events_folded",
        input.v3.iter().filter(|r| is_event(r)).count() as u64,
    );
    let Input {
        v3,
        seqs,
        mut documents,
        keep,
        settings,
    } = input;
    let compacted = library_v3::v3_compact_owned(v3, now)?;
    let form = convert(compacted, &mut counts)?;
    // The converted rows join the log's documents, merged with the one of the same name.
    for (kind, bucket) in [
        (Kind::Title, form.titles),
        (Kind::Season, form.seasons),
        (Kind::Delivery, form.delivery),
    ] {
        let docs = documents.bucket(kind);
        for (name, converted) in bucket {
            let doc = match docs.remove(&name) {
                Some(doc) => match doc.identity() {
                    Some(id) => merge::merge(&id, &converted, &doc.into_read())
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                    None => doc.into_read(),
                },
                None => converted,
            };
            docs.insert(name, Doc::Read(doc));
        }
    }
    for (provider, account, row) in deliver_rows(&settings) {
        if let Some(through) = seeded_through(&row).filter(|t| *t <= base) {
            seed(
                &mut documents,
                &seqs,
                &provider,
                &account,
                through,
                performer,
            );
        }
    }
    let mut written = Vec::new();
    let mut stored = keep.iter().map(|(_, b)| *b).sum::<u64>();
    let mut largest: Option<Largest> = None;
    for doc in documents
        .titles
        .into_values()
        .chain(documents.seasons.into_values())
        .chain(documents.delivery.into_values())
    {
        let mut doc = doc.into_read();
        let id = identity(&doc)?;
        let mut dropped = Vec::new();
        sanitize(&id, &mut doc, &mut dropped);
        bump(&mut counts, "malformed_parts", dropped.len() as u64);
        let document = Value::Object(doc);
        let mut encoded = super::encode(&document, true)?;
        if encoded.get("too_large").is_some() {
            return Err(format!("too_large:{}", id.name()));
        }
        let sealed = encoded["sealed"].as_u64().unwrap_or(0) as usize;
        stored += KEY_BYTES + sealed as u64;
        if largest.as_ref().is_none_or(|l| sealed > l.sealed) {
            largest = Some(Largest {
                name: id.name(),
                sealed,
            });
        }
        written.push(FormDocument {
            document: serde_json::value::to_raw_value(&document).map_err(|_| "invalid_document")?,
            name: id.name(),
            plaintext: encoded["plaintext"].take(),
            sealed,
        });
    }
    let row_count = written.len() + keep.len();
    if row_count > MAX_ROWS {
        return Err("too_many_rows".into());
    }
    if stored > stored_cap.unwrap_or(STORED_CAP) {
        return Err("too_many_bytes".into());
    }
    Ok(Form4 {
        counts,
        documents: written,
        keep: keep.into_iter().map(|(k, _)| k).collect(),
        largest,
        rows: row_count,
        stored_bytes: stored,
    })
}

/// `v4_form`'s answer. Each document is held as its JSON text, which is all the answer needs of it: as `Value`s the
/// library's documents are many times their size. The members are in the order a JSON object of them sorts in.
#[derive(Serialize)]
pub struct Form4 {
    counts: BTreeMap<String, u64>,
    documents: Vec<FormDocument>,
    keep: Vec<String>,
    largest: Option<Largest>,
    rows: usize,
    stored_bytes: u64,
}

#[derive(Serialize)]
struct FormDocument {
    document: Box<RawValue>,
    name: String,
    plaintext: Value,
    sealed: usize,
}

#[derive(Serialize)]
struct Largest {
    name: String,
    sealed: usize,
}

/// The v3 targets and receipts of one account, built from the reference log with the same target rules, keyed
/// `<coordinate>#<key>` to line up with the documents' (`movie:550#list`, `tv:1399:1#2`).
fn v3_targets(
    compacted: &[Value],
    provider: &str,
    account: &str,
    now: i64,
) -> Result<(Vec<Value>, Value), String> {
    // The reference reads v3 rows; its rows are put in document shape here only to reuse the target rules, while
    // receipts are read straight from the `snt` rows. Nothing here goes through `v4_form`.
    let mut counts = BTreeMap::new();
    let only_state: Vec<Value> = compacted
        .iter()
        .filter(|r| matches!(r["kind"].as_str(), Some("rec" | "wat")))
        .cloned()
        .collect();
    let form = convert(only_state, &mut counts)?;
    let mut docs: Vec<Value> = form
        .titles
        .into_values()
        .chain(form.seasons.into_values())
        .map(Value::Object)
        .collect();
    let mut receipts = Map::new();
    for row in compacted
        .iter()
        .filter(|r| r["kind"] == "snt" && r["provider"] == provider && r["account"] == account)
    {
        let Some((placed, _)) = receipt_places(row) else {
            continue;
        };
        for ((media, id, season, key), entry) in placed {
            let coordinate = match season {
                Some(season) => format!("{media}:{id}:{season}"),
                None => format!("{media}:{id}"),
            };
            receipts.insert(format!("{coordinate}#{key}"), entry);
        }
    }
    // Targets via the v4 rules with no receipts, then re-keyed: the reference decides them with shipped v3 code.
    // Known limit: shipped v3 has no target builder of its own (its `pending_targets` takes normalized targets), so
    // this reference is not independent of v4's target rules. A bug in how v4 builds a target is invisible here, and
    // the receipt-dependent rules (*An un-watch survives playback*, *Un-watch then re-mark*) never apply, so they
    // show as differences that are not real. Pending differences are logged and counted, never an abort (§10), and
    // this comparison does not verify the target rules: the delivery vectors do.
    let deliver = json!({"provider": provider, "account": account, "since": [0, 0, ""]});
    docs.retain(|d| d.is_object());
    let snapshot = Snapshot::read(docs.iter().map(Source::Value))??;
    let account_facts = delivery::account(&deliver)?;
    let mut targets = Vec::new();
    delivery::for_each_target(&snapshot, &account_facts, now, &mut |target, _| {
        let coordinate = target
            .document
            .splitn(4, ':')
            .nth(3)
            .unwrap_or_default()
            .to_owned();
        let mut built = target.built_from();
        built["key"] = json!(format!("{coordinate}#{}", target.key));
        // A watch target valued `none` makes no command and writes no receipt (v3 §6 command table).
        if !((built["kind"] == "film" || built["kind"] == "episode") && built["value"] == "none") {
            targets.push(built);
        }
        Ok(())
    })?;
    Ok((targets, Value::Object(receipts)))
}

fn command_key(cmd: &Value) -> Value {
    json!([
        cmd["key"],
        cmd["kind"],
        cmd["added"],
        cmd["rating"],
        cmd.get("p").cloned().unwrap_or(Value::Null)
    ])
}

/// `v4_dry_run` (§10 step 2): the log through `base` + `v4_form`'s output + clock → pass or abort, the
/// pending-command differences, and the counts the switch logs.
pub fn v4_dry_run(rows: &[Entry], form: &RawValue, now: i64) -> Result<Value, String> {
    let Input {
        v3,
        documents: in_log,
        settings,
        ..
    } = read(rows)?;
    // The form's members as the request spells them, each checked to be a JSON value before any is used (its
    // documents one at a time), as when the request was parsed whole. A form that is no object has no documents.
    let members: BTreeMap<String, &RawValue> = match serde_json::from_str(form.get()) {
        Ok(members) => members,
        Err(_) => {
            entry(form)?;
            BTreeMap::new()
        }
    };
    let mut form_documents = None;
    let mut form = Map::new();
    for (member, raw) in members {
        if member == "documents" {
            if let Ok(documents) = serde_json::from_str::<Vec<&RawValue>>(raw.get()) {
                for document in &documents {
                    entry(document)?;
                }
                form_documents = Some(documents);
                continue;
            }
        }
        form.insert(member, entry(raw)?);
    }
    let mut abort = Vec::new();
    // Each document of the form that decodes as it reads, by name, as its JSON text: read again when it is used.
    let mut decoded: BTreeMap<String, Box<RawValue>> = BTreeMap::new();
    for raw in form_documents.ok_or("invalid_form")? {
        let doc = entry(raw)?;
        let name = doc["name"].as_str().ok_or("invalid_form")?;
        let plaintext = codec::unbase64(doc["plaintext"].as_str().unwrap_or_default())?;
        let back = super::decode(&plaintext, Some(name));
        if back["status"] != "document" || !jcs::same(&back["document"], &doc["document"]) {
            abort.push(json!({"reason": "round_trip", "name": name}));
            continue;
        }
        if codec::sealed_len(plaintext.len()) > codec::WRITE_CAP {
            abort.push(json!({"reason": "cap", "name": name}));
        }
        let document = match &back["document"] {
            document @ Value::Object(_) => document,
            _ => &json!({}),
        };
        let text = serde_json::value::to_raw_value(document).map_err(|_| "invalid_form")?;
        decoded.insert(name.into(), text);
    }
    let decoded_doc = |name: &str| decoded.get(name).map(|text| read_decoded(text));
    let reference = library_v3::v3_compact_owned(v3, now)?;
    let in_log: BTreeSet<&String> = in_log.names().collect();
    // Every v3 row v3 keeps must land somewhere, and every receipt entry it holds must reach its delivery document
    // as stored: through the merge with other rows of the same receipt, sanitizing, encoding and seeding (which
    // only moves an `n` at −1 to 0). A delivery document the log already held is checked for the round trip only.
    // Only receipts and rows of other kinds can be dropped, so the state rows are not converted for it.
    let placed = convert(
        reference
            .iter()
            .filter(|r| !matches!(r["kind"].as_str(), Some("rec" | "wat")))
            .cloned()
            .collect(),
        &mut BTreeMap::new(),
    )?;
    for (reason, row) in &placed.dropped {
        abort.push(json!({"reason": reason, "row": row}));
    }
    for (doc_name, doc) in &placed.delivery {
        if in_log.contains(doc_name) {
            continue;
        }
        let held = decoded_doc(doc_name);
        for (key, entry) in doc["entries"].as_object().into_iter().flatten() {
            let mut seeded = entry.clone();
            if seeded[0] == "n" && seeded[1] == -1 {
                seeded[1] = json!(0);
            }
            let kept = held
                .as_ref()
                .and_then(|d| d.get("entries"))
                .and_then(|e| e.get(key))
                .is_some_and(|got| jcs::same(got, entry) || jcs::same(got, &seeded));
            if !kept {
                abort.push(json!({"reason": "receipt_dropped", "name": doc_name, "key": key}));
            }
        }
    }
    // Derived state: every title's rec fields, and every coordinate's episode or film state.
    let mut recs: BTreeMap<String, &Value> = BTreeMap::new();
    let mut registers: BTreeMap<(String, String), &Value> = BTreeMap::new();
    let mut season_resets: BTreeMap<String, &Value> = BTreeMap::new();
    let mut film_registers: BTreeMap<String, &Value> = BTreeMap::new();
    for row in &reference {
        let Some(id) = title_identity(row, Kind::Title, None) else {
            continue;
        };
        let title_name = id.name();
        match row["kind"].as_str() {
            Some("rec") => {
                recs.insert(title_name, row);
            }
            Some("wat") if id.is_film() => {
                film_registers.insert(title_name, &row["entries"]["0"]);
            }
            Some("wat") => {
                let season = u(&row["season"]).unwrap_or(0);
                let block = u(&row["block"]).unwrap_or(0);
                let season_name = format!("season:tv:{}:{season}", id.id);
                if block == 0 {
                    season_resets.insert(season_name.clone(), &row["seasonReset"]);
                }
                for (key, register) in row["entries"].as_object().into_iter().flatten() {
                    if valid_episode_key(key)
                        && key.parse::<u64>().is_ok_and(|e| e / BLOCK == block)
                    {
                        registers.insert((season_name.clone(), key.clone()), register);
                    }
                }
            }
            _ => {}
        }
    }
    let differ = |a: &Result<Value, String>, b: &Result<Value, String>| match (a, b) {
        (Ok(a), Ok(b)) => !jcs::same(a, b),
        (Err(a), Err(b)) => a != b,
        _ => true,
    };
    // A register shipped v3 cannot derive (a malformed play key, say) is one `v4_form` reads with that member
    // dropped (§4 *Malformed parts*). The reference derives it the same way, with the malformed member dropped, so
    // the switch is not aborted for good over a part neither version can read; each such register is counted.
    let mut malformed_reference = 0u64;
    let mut derive_v3 = |register: &Value, derive: &dyn Fn(&Value) -> Result<Value, String>| {
        let v3 = derive(register);
        match (&v3, register.as_object()) {
            (Err(_), Some(register)) => {
                let mut register = register.clone();
                sanitize_register(&mut register, "", &mut Vec::new());
                malformed_reference += 1;
                derive(&Value::Object(register))
            }
            _ => v3,
        }
    };
    for (title_name, rec) in &recs {
        if in_log.contains(title_name) {
            continue;
        }
        let title = &decoded_doc(title_name).unwrap_or_default();
        for field in REC_FIELDS {
            let same = match (rec.get(field), title.get(field)) {
                (Some(a), Some(b)) => jcs::same(a, b),
                (None, None) => true,
                _ => false,
            };
            if !same {
                abort.push(json!({"reason": "derived", "title": title_name, "field": field}));
            }
        }
        if title_name.starts_with("title:movie:") {
            let v3 = derive_v3(
                film_registers
                    .get(title_name)
                    .copied()
                    .unwrap_or(&json!({})),
                &|register| library_v3::film_state(rec, register, &[], now),
            );
            let v4 = state::film_state(title, now);
            if differ(&v3, &v4) {
                abort.push(json!({"reason": "derived", "title": title_name, "coordinate": "film"}));
            }
        }
    }
    let mut coordinates: BTreeSet<(String, String)> = registers.keys().cloned().collect();
    for (name, text) in &decoded {
        if name.starts_with("season:") {
            for key in read_decoded(text)
                .get("episodes")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(k, _)| k)
                .filter(|k| valid_episode_key(k))
            {
                coordinates.insert((name.clone(), key.clone()));
            }
        }
    }
    // The coordinates are in season order, so each season and its series title are decoded once.
    type Held = Option<Map<String, Value>>;
    let mut held: Option<(String, Held, Held)> = None;
    for (season_name, key) in coordinates {
        let title_name = format!(
            "title:{}",
            season_name
                .trim_start_matches("season:")
                .rsplit_once(':')
                .map(|(t, _)| t)
                .unwrap_or("")
        );
        if in_log.contains(&season_name) || in_log.contains(&title_name) {
            continue;
        }
        let mut resets: Vec<Stamp> = Vec::new();
        if let Some(rec) = recs.get(&title_name) {
            resets.extend(stamp(&rec["episodesReset"]).ok());
        }
        if let Some(reset) = season_resets.get(&season_name) {
            resets.extend(stamp(reset).ok());
        }
        let v3 = derive_v3(
            registers
                .get(&(season_name.clone(), key.clone()))
                .copied()
                .unwrap_or(&json!({})),
            &|register| library_v3::episode_state(register, &resets, now),
        );
        if held
            .as_ref()
            .is_none_or(|(name, _, _)| *name != season_name)
        {
            held = Some((
                season_name.clone(),
                decoded_doc(&title_name),
                decoded_doc(&season_name),
            ));
        }
        let Some((_, title, season)) = &held else {
            continue;
        };
        let v4 = state::episode_state(title.as_ref(), season.as_ref(), &key, now);
        if differ(&v3, &v4) {
            abort.push(json!({"reason": "derived", "title": title_name, "coordinate": format!("{season_name}#{key}")}));
        }
    }
    // Pending commands: v4's on the decoded documents against shipped v3's on the reference, per account.
    let mut differences = Vec::new();
    let mut window = 0u64;
    for (provider, account, row) in deliver_rows(&settings) {
        let Some(since) = parse_since(&row) else {
            continue;
        };
        if !valid_provider(&provider) || !valid_account(&account) {
            continue;
        }
        let (targets, receipts) = v3_targets(&reference, &provider, &account, now)?;
        let v3 = library_v3::pending_targets(&targets, &receipts, &since, now)?;
        let deliver =
            json!({"provider": provider, "account": account, "since": since, "removals_sent": []});
        let v4 = delivery::pending(
            decoded.values().map(|text| Source::Raw(text)),
            &deliver,
            now,
        )?;
        // §9 known limit: entries carrying a `[p, null, T]` element, and viewings at or before `seedBound`.
        let seed_bound = row["values"]["seedBound"]["value"]["int"].as_i64();
        let prefix = format!("dlv:{provider}:{account}:");
        let mut v3_set: BTreeMap<String, Value> = BTreeMap::new();
        for cmd in v3.as_array().into_iter().flatten() {
            v3_set.insert(jcs_text(&command_key(cmd)), cmd.clone());
        }
        // Each command's key alone, one command read at a time: a first switch can have one pending for nearly every
        // title and episode.
        let mut v4_set: BTreeMap<String, Value> = BTreeMap::new();
        let mut watched_by_bound = 0u64;
        for cmd in v4.commands() {
            let mut keyed = serde_json::to_value(cmd).map_err(|_| "invalid_command")?;
            if let Some(bound) = seed_bound {
                let watched = keyed["kind"] == "watched"
                    && keyed["watched_at"].as_i64().is_some_and(|w| w <= bound);
                watched_by_bound += u64::from(watched);
            }
            let coordinate = keyed["document"]
                .as_str()
                .unwrap_or_default()
                .trim_start_matches(&prefix)
                .to_owned();
            keyed["key"] = json!(format!(
                "{coordinate}#{}",
                keyed["key"].as_str().unwrap_or_default()
            ));
            let key = command_key(&keyed);
            v4_set.insert(jcs_text(&key), key);
        }
        for (key, cmd) in &v3_set {
            if !v4_set.contains_key(key) {
                differences.push(json!({"account": format!("{provider}:{account}"), "key": cmd["key"], "v3": command_key(cmd), "v4": null}));
            }
        }
        for (key, v4_key) in &v4_set {
            if !v3_set.contains_key(key) {
                differences.push(json!({"account": format!("{provider}:{account}"), "key": v4_key[0], "v3": null, "v4": v4_key}));
            }
        }
        for doc in decoded
            .iter()
            .filter(|(n, _)| n.starts_with(&prefix))
            .map(|(_, text)| read_decoded(text))
        {
            for entry in doc
                .get("entries")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(_, e)| e)
            {
                let marked = entry
                    .get(5)
                    .and_then(Value::as_array)
                    .is_some_and(|s| s.iter().any(|e| e.as_array().is_some_and(|a| a.len() == 3)));
                window += u64::from(marked);
            }
        }
        window += watched_by_bound;
    }
    let form = Value::Object(form);
    Ok(json!({
        "pass": abort.is_empty(),
        "abort": abort,
        "pending_differences": differences,
        "counts": {
            "rows": form["rows"],
            "stored_bytes": form["stored_bytes"],
            "largest": form["largest"],
            "pending_differences": differences.len(),
            "malformed_reference": malformed_reference,
            "window_known_limit": window,
            "dropped": form["counts"],
        },
    }))
}

/// A document the dry run decoded, read from the JSON text it was kept as: a `Value` reads back from its text the same.
fn read_decoded(text: &RawValue) -> Map<String, Value> {
    serde_json::from_str(text.get()).unwrap_or_default()
}

fn jcs_text(value: &Value) -> String {
    String::from_utf8(jcs::jcs(value)).unwrap_or_default()
}
