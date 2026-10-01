//! §6 Merge, and §9's per-entry merge of delivery documents. Every choice is a maximum under a total order, so the
//! merge is commutative, associative and idempotent.

use super::doc::{
    entry_order, field_stamp, keyed_member, known_member, play_key, safe_u64, valid_key, Identity,
    Kind, REGISTER_MEMBERS,
};
use super::jcs;
use crate::wire::{stamp, Stamp};
use serde_json::{Map, Value};
use std::cmp::Ordering;

/// The greater of two versions under `order`, then the byte-greater JCS (§6 *Ties*).
fn pick(a: &Value, b: &Value, order: Ordering) -> Value {
    match order.then_with(|| jcs::cmp(a, b)) {
        Ordering::Less => b.clone(),
        _ => a.clone(),
    }
}

fn at(value: &Value) -> Stamp {
    stamp(&value["at"]).unwrap_or_default()
}

/// `status`, `reaction`, `deleted`, `dismissed`: the later stamp.
pub fn later(a: &Value, b: &Value) -> Value {
    pick(a, b, at(a).cmp(&at(b)))
}

fn seconds(value: &Value) -> f64 {
    value["seconds"].as_f64().unwrap_or(-1.0)
}

/// v2 §5's progress rule (`resume`, `progress`): viewing, then value, then stamp, then `seconds`.
fn furthest(a: &Value, b: &Value) -> Value {
    let order = safe_u64(&a["viewing"])
        .cmp(&safe_u64(&b["viewing"]))
        .then_with(|| {
            // Numeric comparison, so -0 and +0 tie and go to the stamp.
            a["value"]
                .as_f64()
                .partial_cmp(&b["value"].as_f64())
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| at(a).cmp(&at(b)))
        .then_with(|| {
            seconds(a)
                .partial_cmp(&seconds(b))
                .unwrap_or(Ordering::Equal)
        });
    pick(a, b, order)
}

/// `episodesReset`, `seasonReset`: the later stamp, null lowest.
fn later_reset(a: &Value, b: &Value) -> Value {
    let key = |v: &Value| (!v.is_null(), stamp(v).unwrap_or_default());
    pick(a, b, key(a).cmp(&key(b)))
}

/// `cleared`: the greater viewing, then the later stamp; null lowest.
fn cleared(a: &Value, b: &Value) -> Value {
    let key = |v: &Value| {
        (
            !v.is_null(),
            safe_u64(&v[0]).unwrap_or(0),
            stamp(&v[1]).unwrap_or_default(),
        )
    };
    pick(a, b, key(a).cmp(&key(b)))
}

/// v3 §3: the union of keys, the lesser `watchedAt` per key, then the least key and the 7 greatest.
pub fn plays(a: &Value, b: &Value) -> Value {
    let mut all: Vec<(i64, u64)> = [a, b]
        .into_iter()
        .filter_map(Value::as_object)
        .flatten()
        .filter_map(|(key, at)| Some((play_key(key)?, safe_u64(at)?)))
        .collect();
    Value::Object(select(&mut all))
}

/// Writers apply the same selection as the merge.
pub fn select(all: &mut Vec<(i64, u64)>) -> Map<String, Value> {
    all.sort_unstable();
    all.dedup_by(|later, first| later.0 == first.0);
    if all.len() > 8 {
        let tail = all.split_off(all.len() - 7);
        all.truncate(1);
        all.extend(tail);
    }
    all.iter()
        .map(|(key, at)| (key.to_string(), Value::from(*at)))
        .collect()
}

/// An unknown set's rank (§6 *Unknowns*): an empty set lowest, two non-empty ones by byte-greater JCS.
fn rank(set: &Value) -> (bool, Vec<u8>) {
    let empty = match set {
        Value::Array(parts) => parts
            .iter()
            .all(|p| p.as_object().is_none_or(Map::is_empty)),
        Value::Object(map) => map.is_empty(),
        _ => true,
    };
    (!empty, jcs::jcs(set))
}

/// Merge two members by `rule` when both versions hold them; a member only one holds is kept.
fn both(
    out: &mut Map<String, Value>,
    a: &Map<String, Value>,
    b: &Map<String, Value>,
    member: &str,
    rule: impl Fn(&Value, &Value) -> Value,
) {
    let value = match (a.get(member), b.get(member)) {
        (Some(x), Some(y)) => rule(x, y),
        (Some(x), None) | (None, Some(x)) => x.clone(),
        (None, None) => return,
    };
    out.insert(member.into(), value);
}

/// A v3 register (an episode's, or a film's `watch`): each member by its own rule; members this spec does not
/// define come wholesale from the version whose set of them ranks higher.
pub fn register(a: &Value, b: &Value) -> Value {
    let empty = Map::new();
    let a = a.as_object().unwrap_or(&empty);
    let b = b.as_object().unwrap_or(&empty);
    let unknown = |r: &Map<String, Value>| {
        Value::Object(
            r.iter()
                .filter(|(k, _)| !REGISTER_MEMBERS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    };
    let (ua, ub) = (unknown(a), unknown(b));
    let mut out = match rank(&ua).cmp(&rank(&ub)) {
        Ordering::Less => ub,
        _ => ua,
    }
    .as_object()
    .cloned()
    .unwrap_or_default();
    both(&mut out, a, b, "progress", furthest);
    both(&mut out, a, b, "imported", |x, y| {
        Value::Bool(x.as_bool().unwrap_or(false) || y.as_bool().unwrap_or(false))
    });
    both(&mut out, a, b, "plays", plays);
    both(&mut out, a, b, "cleared", cleared);
    Value::Object(out)
}

/// The greatest of the stamps merged by later stamp alone (§6 *Unknowns*); `None` ranks below every stamp.
fn monotone_newest(identity: &Identity, doc: &Map<String, Value>) -> Option<Stamp> {
    let fields: &[&str] = match identity.kind {
        Kind::Title => &[
            "status",
            "reaction",
            "deleted",
            "dismissed",
            "episodesReset",
        ],
        Kind::Season => &["seasonReset"],
        Kind::Delivery => &[],
    };
    fields.iter().filter_map(|f| field_stamp(doc, f)).max()
}

/// The unknown set: top-level members the spec does not define, and entries under invalid keys.
fn unknown_set(identity: &Identity, doc: &Map<String, Value>) -> Value {
    let top: Map<String, Value> = doc
        .iter()
        .filter(|(k, _)| !known_member(identity, k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let keys: Map<String, Value> = keyed_member(identity.kind)
        .and_then(|m| doc.get(m))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(k, _)| !valid_key(identity, k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Value::Array(vec![Value::Object(top), Value::Object(keys)])
}

pub fn delivery_entry(a: &Value, b: &Value) -> Value {
    pick(a, b, entry_order(a).cmp(&entry_order(b)))
}

/// `doc_merge`: two checked versions of one document (same identity, format 4).
pub fn merge(identity: &Identity, a: &Map<String, Value>, b: &Map<String, Value>) -> Value {
    let (ua, ub) = (unknown_set(identity, a), unknown_set(identity, b));
    let winner =
        (monotone_newest(identity, a), rank(&ua)).cmp(&(monotone_newest(identity, b), rank(&ub)));
    let unknown = if winner == Ordering::Less { ub } else { ua };
    let mut out = identity.members();
    // Members inside the identity's `title` object travel like members inside any known field.
    out.insert(
        "title".into(),
        pick(&a["title"], &b["title"], Ordering::Equal),
    );
    out.extend(unknown[0].as_object().cloned().unwrap_or_default());
    match identity.kind {
        Kind::Title => {
            for field in ["status", "reaction", "deleted", "dismissed"] {
                both(&mut out, a, b, field, later);
            }
            both(&mut out, a, b, "resume", furthest);
            both(&mut out, a, b, "episodesReset", later_reset);
            both(&mut out, a, b, "addedAt", |x, y| {
                if x.as_i64() <= y.as_i64() { x } else { y }.clone()
            });
            both(&mut out, a, b, "watchedAt", |x, y| {
                match (x.as_i64(), y.as_i64()) {
                    (Some(p), Some(q)) => Value::from(p.min(q)),
                    (Some(_), None) => x.clone(),
                    _ => y.clone(),
                }
            });
            both(&mut out, a, b, "watch", register);
        }
        Kind::Season => both(&mut out, a, b, "seasonReset", later_reset),
        Kind::Delivery => {}
    }
    if let Some(member) = keyed_member(identity.kind) {
        let (ma, mb) = (
            a.get(member).and_then(Value::as_object),
            b.get(member).and_then(Value::as_object),
        );
        if ma.is_some() || mb.is_some() {
            let mut map = unknown[1].as_object().cloned().unwrap_or_default();
            let empty = Map::new();
            let (ma, mb) = (ma.unwrap_or(&empty), mb.unwrap_or(&empty));
            for key in ma.keys().chain(mb.keys()) {
                if !valid_key(identity, key) || map.contains_key(key) {
                    continue;
                }
                let value = match (ma.get(key), mb.get(key)) {
                    (Some(x), Some(y)) if identity.kind == Kind::Delivery => delivery_entry(x, y),
                    (Some(x), Some(y)) => register(x, y),
                    (Some(x), None) | (None, Some(x)) => x.clone(),
                    (None, None) => continue,
                };
                map.insert(key.clone(), value);
            }
            out.insert(member.into(), Value::Object(map));
        }
    }
    Value::Object(out)
}
