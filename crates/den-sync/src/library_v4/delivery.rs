//! §9 Tracker delivery on documents: targets from title and season documents, receipts from delivery documents,
//! pending commands for `decide`, settling an entry, and the fit check of a delivery document write.

use super::doc::{entry_order, identity, safe_u64, Identity, Kind, FORMAT};
use super::jcs;
use super::state;
use crate::library_v3;
use crate::wire::{stamp, Stamp};
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

const DAY: i64 = 86_400_000;
const REMOVAL_LATCH: usize = 20;

fn effective_t(at: &Stamp, now: i64) -> i64 {
    if at.0 > now.saturating_add(DAY) {
        0
    } else {
        at.0
    }
}

fn timeless() -> Stamp {
    Stamp::default()
}

/// A target's current deliverable value (v3 §6 *Targets and values*).
#[derive(Clone, Debug)]
pub struct Target {
    pub document: String,
    pub key: String,
    pub kind: &'static str,
    pub value: Value,
    pub stamp: Stamp,
    pub p: Option<i64>,
    pub watched_at: Option<u64>,
    /// A two-step un-watch owed before this `watched` (v3 §6 *Un-watch then re-mark*): (stamp, p).
    pub unwatch_first: Option<(Stamp, i64)>,
    /// The title's name, for logs and the dry run.
    pub title: String,
}

impl Target {
    pub fn built_from(&self) -> Value {
        let mut out =
            json!({"key": self.key, "kind": self.kind, "value": self.value, "stamp": self.stamp});
        if let Some(p) = self.p {
            out["p"] = json!(p);
        }
        if let Some(w) = self.watched_at {
            out["watched_at"] = json!(w);
        }
        out
    }
}

/// A receipt entry, read by its shape (§9).
#[derive(Clone, Debug)]
pub enum Receipt {
    Watch {
        class: String,
        p: i64,
        watched_at: Option<u64>,
        stamp: Stamp,
    },
    Field {
        value: Value,
        stamp: Stamp,
        remote: Option<u64>,
    },
    Owed {
        value: Value,
        stamp: Stamp,
    },
}

pub fn receipt(entry: &Value) -> Option<Receipt> {
    let parts = entry.as_array()?;
    match parts.first()?.as_str() {
        Some(class @ ("w" | "u" | "n")) if parts.len() >= 5 => Some(Receipt::Watch {
            class: class.into(),
            p: parts[1].as_i64()?,
            watched_at: safe_u64(&parts[2]),
            stamp: stamp(&parts[3]).ok()?,
        }),
        Some("b") => Some(Receipt::Owed {
            value: parts[1].clone(),
            stamp: stamp(&parts[2]).ok()?,
        }),
        _ => Some(Receipt::Field {
            value: parts[0].clone(),
            stamp: stamp(&parts[1]).ok()?,
            remote: parts.get(3).and_then(safe_u64),
        }),
    }
}

fn class_of(value: &Value) -> &'static str {
    match value.as_str() {
        Some("watched") => "w",
        Some("unwatched") => "u",
        _ => "n",
    }
}

/// Order values for the regression order: `u` = 0, `n` = 0.5, `w` = 1 (doubled to stay integral).
fn order_value(class: &str) -> u8 {
    match class {
        "u" => 0,
        "n" => 1,
        _ => 2,
    }
}

/// A receipt's floor, at second precision: its watched-at for `w`, its value stamp's `t` for `u`, else none.
fn receipt_floor(receipt: &Receipt) -> Option<i64> {
    match receipt {
        Receipt::Watch {
            class,
            watched_at,
            stamp,
            ..
        } => match class.as_str() {
            "w" => watched_at.map(|w| (w / 1000 * 1000) as i64),
            "u" => Some(stamp.0 / 1000 * 1000),
            _ => None,
        },
        _ => None,
    }
}

fn latest_reset(resets: &[Stamp], now: i64) -> Option<Stamp> {
    resets
        .iter()
        .filter(|r| effective_t(r, now) > 0)
        .max()
        .cloned()
}

fn reset_later_than_floor(reset: Option<&Stamp>, floor: Option<i64>) -> bool {
    reset.is_some_and(|r| floor.is_none_or(|f| r.0 / 1000 * 1000 > f))
}

/// Value, value stamp, `p`, watched-at, and an un-watch owed first.
type WatchValue = (Value, Stamp, i64, Option<u64>, Option<(Stamp, i64)>);

/// An episode's value against its receipt (v3 §6 *Targets*, *An un-watch survives playback*).
fn episode_value(
    register: &Value,
    resets: &[Stamp],
    now: i64,
    receipt: Option<&Receipt>,
) -> Result<WatchValue, String> {
    let state = library_v3::episode_state(register, resets, now)?;
    let p = state["viewing"].as_i64().unwrap_or(0);
    let reset_floor = resets
        .iter()
        .map(|r| effective_t(r, now))
        .filter(|t| *t > 0)
        .max()
        .unwrap_or(0);
    let progress = register.get("progress").filter(|v| !v.is_null());
    let progress_at = progress.and_then(|p| stamp(&p["at"]).ok());
    let cleared = register
        .get("cleared")
        .filter(|v| !v.is_null())
        .and_then(|c| Some((safe_u64(&c[0])? as i64, stamp(&c[1]).ok()?)));
    let reset = latest_reset(resets, now);
    if state["watched"] == json!(true) {
        let visible = progress.filter(|_| {
            progress_at
                .as_ref()
                .is_some_and(|at| effective_t(at, now) > reset_floor)
        });
        let real = visible.is_some_and(|p| p["value"].as_f64().unwrap_or(0.0) >= 0.95);
        let at = if real {
            progress_at.clone().unwrap_or_default()
        } else {
            timeless()
        };
        // Un-watch then re-mark: a re-mark in a later viewing than a `w` receipt whose viewing was cleared, or
        // with a covering reset later than its floor, owes the un-watch first.
        let first = match receipt {
            Some(r @ Receipt::Watch { class, p: rp, .. }) if class == "w" && p > *rp => {
                match &cleared {
                    Some((viewing, at)) if *viewing >= *rp => Some((at.clone(), viewing + 1)),
                    _ if reset_later_than_floor(reset.as_ref(), receipt_floor(r)) => {
                        Some((reset.clone().unwrap_or_default(), *rp))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        return Ok((json!("watched"), at, p, state["watched_at"].as_u64(), first));
    }
    if let (Some(progress), Some((viewing, at))) = (progress, &cleared) {
        let value = progress["value"].as_f64().unwrap_or(-1.0);
        if value == 0.0 && progress_at.as_ref().is_some_and(|a| a.0 > 0) && *viewing == p - 1 {
            return Ok((json!("unwatched"), at.clone(), p, None, None));
        }
    }
    // A watched state hidden by a covering reset (v3 §6 *Targets and values*), judged as v3 §5 hides it: finished
    // progress whose `t` is not after the reset, or `imported` with no visible imported play later than the reset.
    // Visible plays are already later than every covering reset.
    let finished_hidden = progress.is_some_and(|p| p["value"].as_f64().unwrap_or(0.0) >= 0.95)
        && progress_at
            .as_ref()
            .is_some_and(|at| effective_t(at, now) <= reset_floor);
    let imported_play = state["plays"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|play| play[0].as_i64().is_some_and(|key| key < 0));
    let imported_hidden =
        register.get("imported").and_then(Value::as_bool) == Some(true) && !imported_play;
    if reset.is_some() && (finished_hidden || imported_hidden) {
        return Ok((json!("unwatched"), reset.unwrap_or_default(), p, None, None));
    }
    if let Some(r @ Receipt::Watch { class, p: rp, .. }) = receipt {
        if class == "w" {
            if let Some((_, at)) = cleared.as_ref().filter(|(v, _)| *v >= *rp) {
                return Ok((json!("unwatched"), at.clone(), p, None, None));
            }
            if reset_later_than_floor(reset.as_ref(), receipt_floor(r)) {
                return Ok((json!("unwatched"), reset.unwrap_or_default(), p, None, None));
            }
        }
    }
    Ok((
        json!("none"),
        progress_at.unwrap_or_default(),
        p,
        None,
        None,
    ))
}

fn film_value(
    title: &Map<String, Value>,
    now: i64,
    receipt: Option<&Receipt>,
) -> Result<WatchValue, String> {
    let state = state::film_state(title, now)?;
    let rec = state::film_rec(title);
    let viewing = safe_u64(&rec["resume"]["viewing"]).unwrap_or(0) as i64;
    let status = rec["status"]["value"].as_str().unwrap_or("none");
    let status_at = stamp(&rec["status"]["at"]).unwrap_or_default();
    let cleared = title
        .get("watch")
        .and_then(|w| w.get("cleared"))
        .filter(|c| !c.is_null())
        .and_then(|c| Some((safe_u64(&c[0])? as i64, stamp(&c[1]).ok()?)));
    if status == "watched" {
        // Un-watch then re-mark, as for an episode (a film has no resets): a re-mark in a later viewing than a `w`
        // receipt whose viewing was cleared owes the un-watch first.
        let first = match receipt {
            Some(Receipt::Watch { class, p: rp, .. }) if class == "w" && viewing > *rp => cleared
                .clone()
                .filter(|(cleared_viewing, _)| *cleared_viewing >= *rp)
                .map(|(cleared_viewing, at)| (at, cleared_viewing + 1)),
            _ => None,
        };
        return Ok((
            json!("watched"),
            status_at,
            viewing,
            state["watched_at"].as_u64(),
            first,
        ));
    }
    if let Some((cleared_viewing, at)) = cleared {
        if status == "none" && cleared_viewing == viewing - 1 {
            return Ok((json!("unwatched"), at, viewing, None, None));
        }
        if let Some(Receipt::Watch { class, p, .. }) = receipt {
            if class == "w" && cleared_viewing >= *p {
                return Ok((json!("unwatched"), at, viewing, None, None));
            }
        }
    }
    Ok((json!("none"), status_at, viewing, None, None))
}

fn field<'a>(title: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    title.get(name)
}

fn field_stamp(title: &Map<String, Value>, name: &str) -> Stamp {
    field(title, name)
        .and_then(|f| stamp(&f["at"]).ok())
        .unwrap_or_default()
}

fn is_deleted(title: &Map<String, Value>) -> bool {
    field(title, "deleted").and_then(|d| d["value"].as_bool()) == Some(true)
}

fn rating_number(reaction: &Value) -> Option<i64> {
    match reaction.as_str() {
        Some("love") => Some(10),
        Some("like") => Some(7),
        Some("dislike") => Some(2),
        _ => None,
    }
}

fn reaction_of(remote: u64) -> &'static str {
    match remote {
        0..=4 => "dislike",
        5..=7 => "like",
        _ => "love",
    }
}

pub struct Account {
    pub provider: String,
    pub account: String,
    pub since: Stamp,
    pub removals: Value,
    pub unverified: BTreeSet<u64>,
}

/// The documents one pass reads, by name.
pub struct Snapshot {
    pub titles: BTreeMap<String, Map<String, Value>>,
    pub seasons: BTreeMap<String, Map<String, Value>>,
    pub delivery: BTreeMap<String, Map<String, Value>>,
    /// Names of documents with a `format` above 4: every target they hold or name is held (§4 *Newer rows*).
    pub newer: BTreeSet<String>,
}

impl Snapshot {
    pub fn read(documents: &[Value]) -> Result<Self, String> {
        let mut out = Snapshot {
            titles: BTreeMap::new(),
            seasons: BTreeMap::new(),
            delivery: BTreeMap::new(),
            newer: BTreeSet::new(),
        };
        for doc in documents {
            let map = doc.as_object().ok_or("invalid_document")?;
            let id = identity(map)?;
            let doc = match super::checked(doc) {
                Ok((_, doc)) => doc,
                Err(e) if e == "newer_format" => {
                    // Read as a format-4 reader reads it, so a part a newer format reshaped cannot fail the pass;
                    // every target it holds or names is held below.
                    out.newer.insert(id.name());
                    super::read_newer(&id, map, &mut Vec::new())
                }
                Err(e) => return Err(e),
            };
            let bucket = match id.kind {
                Kind::Title => &mut out.titles,
                Kind::Season => &mut out.seasons,
                Kind::Delivery => &mut out.delivery,
            };
            bucket.insert(id.name(), doc);
        }
        Ok(out)
    }
}

fn delivery_name(account: &Account, coordinate: &str) -> String {
    format!("dlv:{}:{}:{coordinate}", account.provider, account.account)
}

/// Every target of the snapshot with the receipt name and key it settles into.
pub fn targets_for(
    snapshot: &Snapshot,
    account: &Account,
    now: i64,
) -> Result<Vec<(Target, Option<Value>)>, String> {
    let mut out = Vec::new();
    let entry = |document: &str, key: &str| {
        snapshot
            .delivery
            .get(document)
            .and_then(|d| d.get("entries"))
            .and_then(|e| e.get(key))
            .cloned()
    };
    for (name, title) in &snapshot.titles {
        let coordinate = name.trim_start_matches("title:");
        let document = delivery_name(account, coordinate);
        let film = coordinate.starts_with("movie:");
        let deleted = is_deleted(title);
        let status = field(title, "status").map(|s| s["value"].clone());
        let watchlist = status.as_ref().and_then(Value::as_str) == Some("watchlist");
        let list = match (watchlist, deleted) {
            (true, false) => "in",
            (true, true) => "gone",
            _ => "out",
        };
        let list_stamp = field_stamp(title, "status").max(field_stamp(title, "deleted"));
        let make = |key: &str, kind, value: Value, stamp: Stamp| Target {
            document: document.clone(),
            key: key.into(),
            kind,
            value,
            stamp,
            p: None,
            watched_at: None,
            unwatch_first: None,
            title: name.clone(),
        };
        out.push((
            make("list", "list", json!(list), list_stamp),
            entry(&document, "list"),
        ));
        if deleted {
            continue;
        }
        let reaction = field(title, "reaction")
            .map(|r| r["value"].clone())
            .unwrap_or(Value::Null);
        out.push((
            make("rating", "rating", reaction, field_stamp(title, "reaction")),
            entry(&document, "rating"),
        ));
        if film {
            let current = entry(&document, "watch");
            let parsed = current.as_ref().and_then(receipt);
            let (value, stamp, p, watched_at, first) = film_value(title, now, parsed.as_ref())?;
            let mut target = make("watch", "film", value, stamp);
            target.p = Some(p);
            target.watched_at = watched_at;
            target.unwatch_first = first;
            out.push((target, current));
        }
    }
    for (name, season) in &snapshot.seasons {
        let coordinate = name.trim_start_matches("season:");
        let title_name = format!(
            "title:{}",
            coordinate
                .rsplit_once(':')
                .map(|(t, _)| t)
                .unwrap_or(coordinate)
        );
        let title = snapshot.titles.get(&title_name);
        if title.is_some_and(is_deleted) {
            continue;
        }
        let resets = state::resets(title, Some(season));
        let document = delivery_name(account, coordinate);
        for (key, register) in season
            .get("episodes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if !super::doc::valid_episode_key(key) {
                continue;
            }
            let current = entry(&document, key);
            let parsed = current.as_ref().and_then(receipt);
            let (value, stamp, p, watched_at, first) =
                episode_value(register, &resets, now, parsed.as_ref())?;
            out.push((
                Target {
                    document: document.clone(),
                    key: key.clone(),
                    kind: "episode",
                    value,
                    stamp,
                    p: Some(p),
                    watched_at,
                    unwatch_first: first,
                    title: title_name.clone(),
                },
                current,
            ));
        }
    }
    Ok(out)
}

/// What a pass decides for one target.
pub enum Decision {
    Command(Value),
    /// Settled with its current value, no command (§9 *Pending*, v3 §6 command table).
    Settle(Value),
    Nothing,
}

fn command(target: &Target, kind: &str, baseline: bool, receipt: Option<&Receipt>) -> Value {
    let mut out = json!({
        "kind": kind,
        "key": target.key,
        "document": target.document,
        "title": target.title,
        "at": target.stamp.0.max(0),
        "current": true,
        "baseline": baseline || target.stamp.0 == 0,
        "episode": target.kind == "episode",
        "added": kind == "list" && target.value == json!("in"),
        "rating": if kind == "rating" { json!(rating_number(&target.value)) } else { Value::Null },
        "built_from": target.built_from(),
    });
    if let Some(p) = target.p {
        out["p"] = json!(p);
    }
    if let Some(w) = target.watched_at {
        out["watched_at"] = json!(w);
    }
    match receipt {
        Some(Receipt::Watch { p, .. }) => out["receipt_p"] = json!(p),
        Some(Receipt::Field {
            remote: Some(remote),
            ..
        }) => out["acknowledged_rating"] = json!(remote),
        _ => {}
    }
    out
}

fn regression(target: &Target, receipt: &Receipt) -> bool {
    match receipt {
        Receipt::Watch {
            class, p, stamp, ..
        } => {
            let current = class_of(&target.value);
            let cp = target.p.unwrap_or(0);
            if current == "u" && class == "w" && cp == *p {
                return false;
            }
            (cp, order_value(current), &target.stamp).cmp(&(*p, order_value(class), stamp))
                == Ordering::Less
        }
        Receipt::Field { stamp, .. } | Receipt::Owed { stamp, .. } => target.stamp < *stamp,
    }
}

fn same_value(target: &Target, receipt: &Receipt) -> bool {
    match receipt {
        Receipt::Watch { class, p, .. } => {
            let current = class_of(&target.value);
            current == class && (current == "n" || target.p.unwrap_or(0) == (*p).max(0))
        }
        Receipt::Field { value, .. } | Receipt::Owed { value, .. } => {
            jcs::same(value, &target.value)
        }
    }
}

/// The command a value change makes (v3 §6 command table), or `None` when it settles silently.
fn change_command(target: &Target, from: Option<&Receipt>) -> Option<&'static str> {
    match target.kind {
        "episode" | "film" => match target.value.as_str() {
            Some("watched") => Some("watched"),
            Some("unwatched") if !matches!(from, Some(Receipt::Watch { class, .. }) if class == "u") => {
                Some("unwatched")
            }
            _ => None,
        },
        "list" => match target.value.as_str() {
            Some("in") => Some("list"),
            Some("gone") if matches!(from, Some(Receipt::Field { value, .. } | Receipt::Owed { value, .. }) if value == "in") => {
                Some("list")
            }
            _ => None,
        },
        _ => Some("rating"),
    }
}

fn additive(target: &Target) -> bool {
    match target.kind {
        "episode" | "film" => target.value == json!("watched"),
        "list" => target.value == json!("in"),
        _ => rating_number(&target.value).is_some(),
    }
}

fn decide_target(target: &Target, entry: Option<&Value>, account: &Account, now: i64) -> Decision {
    if effective_t(&target.stamp, now) == 0 && target.stamp.0 != 0 {
        return Decision::Nothing;
    }
    let watch_none = matches!(target.kind, "episode" | "film") && target.value == json!("none");
    let parsed = entry.and_then(receipt).filter(|r| {
        // An `n` entry at −1 counts as no receipt (v3 §6 *Earlier viewings*).
        !matches!(r, Receipt::Watch { class, p, .. } if class == "n" && *p == -1)
    });
    let unverified = entry
        .and_then(entry_order)
        .is_some_and(|(epoch, _, _)| epoch >= 2 && account.unverified.contains(&epoch));
    let Some(r) = parsed else {
        if watch_none {
            return Decision::Nothing;
        }
        if target.stamp.0 != 0 && target.stamp > account.since {
            // Only `in → gone` is a list removal (v3 §6 command table): with no receipt, `gone` settles silently.
            return match change_command(target, None) {
                Some(kind) => Decision::Command(command(target, kind, false, None)),
                None => Decision::Settle(target.built_from()),
            };
        }
        return match change_command(target, None).filter(|_| additive(target)) {
            Some(kind) => Decision::Command(command(target, kind, true, None)),
            None => Decision::Settle(target.built_from()),
        };
    };
    if let Receipt::Owed { value, .. } = &r {
        if jcs::same(value, &target.value) {
            // Owed as baseline: an additive value is decided against the snapshot; any other settles as it is.
            return match change_command(target, None).filter(|_| additive(target)) {
                Some(kind) => Decision::Command(command(target, kind, true, None)),
                None => Decision::Settle(target.built_from()),
            };
        }
        let as_receipt = Receipt::Field {
            value: value.clone(),
            stamp: timeless(),
            remote: None,
        };
        return decide_against(target, &as_receipt, false);
    }
    if same_value(target, &r) {
        if unverified {
            return match change_command(target, Some(&r)) {
                Some(kind) if !watch_none => {
                    let mut cmd = command(target, kind, true, Some(&r));
                    cmd["unverified"] = json!(true);
                    Decision::Command(cmd)
                }
                _ => Decision::Settle(target.built_from()),
            };
        }
        return Decision::Nothing;
    }
    // v3 §6 *Timeless values*: a timeless rating is not pending against a receipt whose value stamp equals its own,
    // which is how a `baseline` rating settles when the remote's maps to another reaction.
    if target.kind == "rating" && target.stamp.0 == 0 {
        if let Receipt::Field { stamp, .. } = &r {
            if *stamp == target.stamp {
                return Decision::Nothing;
            }
        }
    }
    if watch_none || regression(target, &r) {
        return Decision::Nothing;
    }
    decide_against(target, &r, unverified)
}

fn decide_against(target: &Target, r: &Receipt, unverified: bool) -> Decision {
    if let Some((stamp, p)) = &target.unwatch_first {
        let mut first = target.clone();
        first.value = json!("unwatched");
        first.stamp = stamp.clone();
        first.p = Some(*p);
        first.watched_at = None;
        first.unwatch_first = None;
        let mut cmd = command(&first, "unwatched", false, Some(r));
        cmd["step"] = json!("unwatch_then_remark");
        return Decision::Command(cmd);
    }
    // Timeless values against a receipt other than `n`, `out` or `null` are decided as `baseline` (v3 §6).
    let default_receipt = match r {
        Receipt::Watch { class, .. } => class == "n",
        Receipt::Field { value, .. } => value.is_null() || value == "out",
        Receipt::Owed { .. } => false,
    };
    let baseline = target.stamp.0 == 0 && !default_receipt;
    match change_command(target, Some(r)) {
        Some(kind) => {
            let mut cmd = command(target, kind, baseline, Some(r));
            if unverified {
                cmd["unverified"] = json!(true);
            }
            Decision::Command(cmd)
        }
        None => Decision::Settle(target.built_from()),
    }
}

pub fn account(deliver: &Value) -> Result<Account, String> {
    let provider = deliver["provider"].as_str().ok_or("invalid_account")?;
    let id = deliver["account"].as_str().ok_or("invalid_account")?;
    if !super::doc::valid_provider(provider) || !super::doc::valid_account(id) {
        return Err("invalid_account".into());
    }
    Ok(Account {
        provider: provider.into(),
        account: id.into(),
        since: stamp(&deliver["since"]).map_err(|_| "invalid_since")?,
        removals: deliver.get("removals").cloned().unwrap_or(Value::Null),
        unverified: deliver["unverified"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .collect(),
    })
}

/// `pending_targets` (§9, v3 §6) for one account. `deliver` is the account's `set:deliver` facts, read by the
/// client: `{provider, account, since, removals?, unverified?}`.
pub fn pending_targets(documents: &[Value], deliver: &Value, now: i64) -> Result<Value, String> {
    let mut account = account(deliver)?;
    let snapshot = Snapshot::read(documents)?;
    account.unverified = unverified(&snapshot, &account);
    let mut commands = Vec::new();
    let mut settles = Vec::new();
    let mut held = Vec::new();
    for (target, entry) in targets_for(&snapshot, &account, now)? {
        let season_doc = target
            .document
            .split(':')
            .skip(3)
            .collect::<Vec<_>>()
            .join(":");
        let source = if target.kind == "episode" {
            format!("season:{season_doc}")
        } else {
            target.title.clone()
        };
        // An episode's series title names its covering reset, so a newer series title holds its episodes too.
        if [&target.document, &source, &target.title]
            .iter()
            .any(|name| snapshot.newer.contains(*name))
        {
            held.push(
                json!({"document": target.document, "key": target.key, "reason": "newer_format"}),
            );
            continue;
        }
        match decide_target(&target, entry.as_ref(), &account, now) {
            Decision::Command(cmd) => commands.push(cmd),
            Decision::Settle(built) => settles
                .push(json!({"document": target.document, "key": target.key, "built_from": built})),
            Decision::Nothing => {}
        }
    }
    // The removals latch (v3 §6 *Accounts*): only list removals stamped after an approval count.
    let approved = stamp(&account.removals["approved"]).ok();
    let is_counted_removal = |c: &Value| {
        c["kind"] == "list"
            && c["added"] == false
            && approved
                .as_ref()
                .is_none_or(|a| stamp(&c["built_from"]["stamp"]).is_ok_and(|s| s > *a))
    };
    let removals = commands.iter().filter(|c| is_counted_removal(c)).count();
    // Closed when `set:deliver` says so — v3's `"held"`, or a `held` stamp beside the approval it leaves standing —
    // or when this pass counts more than 20. Either way it holds only removals stamped after the approval.
    let latch = account.removals == json!("held")
        || account.removals.get("held").is_some()
        || removals > REMOVAL_LATCH;
    if latch {
        for c in commands.iter_mut().filter(|c| is_counted_removal(c)) {
            c["removals_held"] = json!(true);
        }
    }
    commands.sort_by(|a, b| {
        a["at"]
            .as_i64()
            .cmp(&b["at"].as_i64())
            .then(a["document"].as_str().cmp(&b["document"].as_str()))
            .then(a["key"].as_str().cmp(&b["key"].as_str()))
    });
    // The epoch rule of a lease take reads this account's settle epochs only (§9).
    let own = delivery_name(&account, "");
    let greatest_epoch = snapshot
        .delivery
        .iter()
        .filter(|(name, _)| name.starts_with(&own))
        .flat_map(|(_, d)| {
            d.get("entries")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
        })
        .filter_map(|(_, e)| entry_order(e))
        .map(|(epoch, _, _)| epoch)
        .max()
        .unwrap_or(0);
    Ok(json!({
        "commands": commands,
        "settle": settles,
        "held": held,
        "removals": if removals > REMOVAL_LATCH { json!("held") } else { Value::Null },
        "greatest_epoch": greatest_epoch,
        "unverified": account.unverified,
    }))
}

/// The account's unverified epochs after this read (v3 §6 *Unverified receipts*): those `set:deliver` lists, and every
/// settle epoch ≥ 2 its receipts hold from two different devices, less any epoch no receipt holds any more. The
/// holder writes the result back to `unverified` when it differs.
fn unverified(snapshot: &Snapshot, account: &Account) -> BTreeSet<u64> {
    let own = delivery_name(account, "");
    let mut devices: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    for (_, document) in snapshot
        .delivery
        .iter()
        .filter(|(name, _)| name.starts_with(&own))
    {
        for entry in document
            .get("entries")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|entries| entries.values())
        {
            if let Some((epoch, _, device)) = entry_order(entry) {
                devices.entry(epoch).or_default().insert(device);
            }
        }
    }
    let shared = devices
        .iter()
        .filter(|(epoch, by)| **epoch >= 2 && by.len() > 1)
        .map(|(epoch, _)| *epoch);
    account
        .unverified
        .iter()
        .copied()
        .chain(shared)
        .filter(|epoch| *epoch >= 2 && devices.contains_key(epoch))
        .collect()
}

/// Sending elements every settle and intent of a target keeps: `[-1, I]` and `[p, null, T]` (v3 §6 *Intent*).
fn lasting(entry: Option<&Value>) -> Vec<Value> {
    entry
        .and_then(|e| e.get(5))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e[0].as_i64() == Some(-1) || (e.as_array().is_some_and(|a| a.len() == 3)))
        .cloned()
        .collect()
}

/// `settle` (§9, v3 §6 *Settling*): outcome + built-from value + the entry it replaces → the entry, or nothing.
pub fn settle(
    outcome: &Value,
    built: &Value,
    order: &Value,
    entry: Option<&Value>,
) -> Result<Value, String> {
    let mut built = built.clone();
    let action = outcome["action"].as_str().ok_or("invalid_outcome")?;
    let remote = outcome
        .get("remote_rating")
        .and_then(safe_u64)
        .filter(|r| (1..=10).contains(r));
    if built["kind"] == "rating" && action == "acknowledge" {
        // A `baseline` rating built from a timeless value settles as the remote's reaction (v3 §6 *Timeless values*).
        let timeless = stamp(&built["stamp"]).is_ok_and(|s| s.0 == 0);
        if let (true, Some(remote)) = (timeless, remote) {
            if rating_number(&built["value"]).is_some() {
                built["value"] = json!(reaction_of(remote));
            }
        }
    }
    let mut out = library_v3::settle(&json!({"action": action}), &built, order)?;
    if out.is_null() {
        return Ok(out);
    }
    if built["kind"] == "rating" {
        // The fourth element only when the acknowledging remote value differs from what Den sends (v3 §6 *Ratings*).
        if let (true, Some(remote)) = (action == "acknowledge", remote) {
            if rating_number(&built["value"]) != Some(remote as i64) {
                out.as_array_mut()
                    .expect("rating entry")
                    .push(json!(remote));
            }
        }
    }
    let keep = lasting(entry);
    if matches!(built["kind"].as_str(), Some("episode" | "film")) && !keep.is_empty() {
        out.as_array_mut()
            .expect("watch entry")
            .push(Value::Array(keep));
    }
    Ok(out)
}

fn default_watch_entry(order: &Value) -> Value {
    json!(["n", 0, null, [0, 0, ""], order])
}

/// An intent (v3 §6 *Intent*): the entry keeps its value, takes a fresh settle order, and its `[p, W]` elements
/// are replaced by the ones it will send; every `[-1, …]` and `[p, null, T]` element is kept.
fn intent(entry: Option<&Value>, intent: &Value) -> Result<Value, String> {
    let order = &intent["order"];
    if !super::doc::order_ok(order) {
        return Err("invalid_settle_order".into());
    }
    let mut out = entry
        .filter(|e| e[0].as_str().is_some_and(|c| matches!(c, "w" | "u" | "n")))
        .cloned()
        .unwrap_or_else(|| default_watch_entry(order));
    let parts = out.as_array_mut().ok_or("invalid_receipt")?;
    parts.truncate(5);
    parts[4] = order.clone();
    let mut sending: Vec<Value> = intent["sending"]
        .as_array()
        .ok_or("invalid_intent")?
        .iter()
        .filter(|e| {
            e[0].as_i64().is_some_and(|p| p >= 0) && e.as_array().is_some_and(|a| a.len() == 2)
        })
        .cloned()
        .collect();
    sending.extend(lasting(entry));
    parts.push(Value::Array(sending));
    Ok(out)
}

/// `delivery_write` (§9 *Fit before sending*): the delivery document after the pass's commands, in order, held
/// from the first command whose intent and settle no longer fit with those before it at the 256 KiB cap.
pub fn delivery_write(
    document: Option<&Value>,
    identity_value: &Value,
    commands: &[Value],
) -> Result<Value, String> {
    let base: Map<String, Value> = match document {
        Some(doc) => super::checked(doc)?.1,
        None => {
            let mut doc = identity_value
                .as_object()
                .cloned()
                .ok_or("invalid_identity")?;
            doc.insert("format".into(), json!(FORMAT));
            doc.insert("kind".into(), json!("delivery"));
            let id: Identity = identity(&doc)?;
            let mut out = id.members();
            out.insert("entries".into(), json!({}));
            out
        }
    };
    let id = identity(&base)?;
    if id.kind != Kind::Delivery {
        return Err("invalid_identity".into());
    }
    let build = |count: usize| -> Result<(Value, Option<Value>), String> {
        let mut settled = base.clone();
        let mut intents = base.clone();
        let mut any_intent = false;
        for cmd in &commands[..count] {
            let key = cmd["key"].as_str().ok_or("invalid_command")?;
            if !super::doc::valid_key(&id, key) {
                return Err("invalid_key".into());
            }
            let old = base.get("entries").and_then(|e| e.get(key)).cloned();
            let mut latest = old.clone();
            if let Some(i) = cmd.get("intent").filter(|i| !i.is_null()) {
                let e = intent(old.as_ref(), i)?;
                intents.entry("entries").or_insert_with(|| json!({}))[key] = e.clone();
                latest = Some(e);
                any_intent = true;
            }
            if let Some(s) = cmd.get("settle").filter(|s| !s.is_null()) {
                if !super::doc::entry_ok(key, s) {
                    return Err("invalid_entry".into());
                }
                latest = Some(s.clone());
            }
            if let Some(e) = latest {
                settled.entry("entries").or_insert_with(|| json!({}))[key] = e;
            }
        }
        Ok((
            Value::Object(settled),
            any_intent.then_some(Value::Object(intents)),
        ))
    };
    let fits = |doc: &Value| -> Result<bool, String> {
        Ok(super::encode(doc, false)?.get("too_large").is_none())
    };
    let fit = |count: usize| -> Result<bool, String> {
        let (settled, intents) = build(count)?;
        Ok(fits(&settled)? && intents.as_ref().map(fits).transpose()?.unwrap_or(true))
    };
    // Fitting is monotone in the prefix (a document only grows), so the longest prefix that fits is found by
    // bisection: each probe encodes and self-checks up to 256 KiB, and a long season can carry thousands of commands.
    let mut accepted = commands.len();
    if !fit(accepted)? {
        let (mut fits_up_to, mut fails_at) = (0, commands.len());
        while fails_at - fits_up_to > 1 {
            let mid = fits_up_to + (fails_at - fits_up_to) / 2;
            if fit(mid)? {
                fits_up_to = mid;
            } else {
                fails_at = mid;
            }
        }
        accepted = fits_up_to;
    }
    let (document, intent_document) = build(accepted)?;
    let held: Vec<Value> = commands[accepted..]
        .iter()
        .map(|c| json!({"key": c["key"], "reason": "receipt_full"}))
        .collect();
    Ok(json!({
        "document": document,
        "intent_document": intent_document,
        "accepted": accepted,
        "held": held,
    }))
}
