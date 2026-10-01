//! §3 Documents: identity, names, and the shape of every known part (§4 *Malformed parts*, *Shape*).

use crate::wire::{stamp, Stamp, MAX_SAFE_INTEGER};
use serde_json::{Map, Value};

pub const FORMAT: u64 = 4;
const MAX_EPISODE: u64 = 99_999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Title,
    Season,
    Delivery,
}

impl Kind {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "title" => Some(Self::Title),
            "season" => Some(Self::Season),
            "delivery" => Some(Self::Delivery),
            _ => None,
        }
    }
}

/// What names a document (§3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub kind: Kind,
    pub media: String,
    pub id: u64,
    pub season: Option<u64>,
    pub provider: Option<String>,
    pub account: Option<String>,
}

impl Identity {
    pub fn name(&self) -> String {
        let coordinate = match self.season {
            Some(season) => format!("{}:{}:{season}", self.media, self.id),
            None => format!("{}:{}", self.media, self.id),
        };
        match self.kind {
            Kind::Title => format!("title:{coordinate}"),
            Kind::Season => format!("season:{coordinate}"),
            Kind::Delivery => format!(
                "dlv:{}:{}:{coordinate}",
                self.provider.as_deref().unwrap_or_default(),
                self.account.as_deref().unwrap_or_default()
            ),
        }
    }

    /// The members that carry this identity, as a document stores them.
    pub fn members(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("format".into(), FORMAT.into());
        out.insert(
            "kind".into(),
            match self.kind {
                Kind::Title => "title",
                Kind::Season => "season",
                Kind::Delivery => "delivery",
            }
            .into(),
        );
        out.insert(
            "title".into(),
            serde_json::json!({"type": self.media, "id": self.id}),
        );
        if let Some(season) = self.season {
            out.insert("season".into(), season.into());
        }
        if let (Some(provider), Some(account)) = (&self.provider, &self.account) {
            out.insert("provider".into(), provider.clone().into());
            out.insert("account".into(), account.clone().into());
        }
        out
    }

    pub fn is_film(&self) -> bool {
        self.media == "movie"
    }
}

pub fn safe_u64(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= MAX_SAFE_INTEGER)
}

pub fn safe_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .filter(|n| n.unsigned_abs() <= MAX_SAFE_INTEGER)
}

pub fn valid_account(account: &str) -> bool {
    !account.is_empty()
        && account
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn valid_provider(provider: &str) -> bool {
    !provider.is_empty() && !provider.contains(':')
}

/// The identity fields, or why they fail (which makes the row unreadable, §4 *Bounds*).
pub fn identity(doc: &Map<String, Value>) -> Result<Identity, String> {
    let kind = doc
        .get("kind")
        .and_then(Value::as_str)
        .and_then(Kind::parse)
        .ok_or("unknown_kind")?;
    let title = doc
        .get("title")
        .and_then(Value::as_object)
        .ok_or("identity")?;
    let media = title
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| matches!(*t, "movie" | "tv"))
        .ok_or("identity")?;
    let id = title
        .get("id")
        .and_then(safe_u64)
        .filter(|id| *id > 0)
        .ok_or("identity")?;
    let season = match doc.get("season") {
        Some(value) if kind != Kind::Title => Some(safe_u64(value).ok_or("identity")?),
        None if kind == Kind::Season => return Err("identity".into()),
        _ => None,
    };
    if season.is_some() && media != "tv" {
        return Err("identity".into());
    }
    let (provider, account) = if kind == Kind::Delivery {
        let provider = doc
            .get("provider")
            .and_then(Value::as_str)
            .filter(|p| valid_provider(p))
            .ok_or("identity")?;
        let account = doc
            .get("account")
            .and_then(Value::as_str)
            .filter(|a| valid_account(a))
            .ok_or("identity")?;
        (Some(provider.to_owned()), Some(account.to_owned()))
    } else {
        (None, None)
    };
    Ok(Identity {
        kind,
        media: media.to_owned(),
        id,
        season,
        provider,
        account,
    })
}

/// The members this spec defines at the top level of a document of `identity` (§6 *Unknowns*).
pub fn known_member(identity: &Identity, member: &str) -> bool {
    if matches!(member, "format" | "kind" | "title") {
        return true;
    }
    match identity.kind {
        Kind::Title => matches!(
            member,
            "status"
                | "resume"
                | "reaction"
                | "deleted"
                | "dismissed"
                | "episodesReset"
                | "addedAt"
                | "watchedAt"
                | "watch"
        ),
        Kind::Season => matches!(member, "season" | "seasonReset" | "episodes"),
        Kind::Delivery => {
            matches!(member, "provider" | "account" | "entries")
                || (member == "season" && identity.season.is_some())
        }
    }
}

/// A valid `episodes` or season `entries` key: canonical decimal of `0 ≤ e ≤ 99999` (§3).
pub fn valid_episode_key(key: &str) -> bool {
    key.parse::<u64>()
        .is_ok_and(|e| e <= MAX_EPISODE && e.to_string() == key)
}

/// Whether `key` is a key this spec defines for the map (`episodes` or `entries`) of a document.
pub fn valid_key(identity: &Identity, key: &str) -> bool {
    match (identity.kind, identity.season) {
        (Kind::Delivery, None) => {
            matches!(key, "list" | "rating") || (key == "watch" && identity.is_film())
        }
        _ => valid_episode_key(key),
    }
}

/// The map a document keys by episode or target: `episodes` (season) or `entries` (delivery).
pub fn keyed_member(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Title => None,
        Kind::Season => Some("episodes"),
        Kind::Delivery => Some("entries"),
    }
}

pub const REGISTER_MEMBERS: [&str; 4] = ["progress", "imported", "plays", "cleared"];

fn is_stamp(value: &Value) -> bool {
    stamp(value).is_ok()
}

fn stamped(value: &Value, check: impl Fn(&Value) -> bool) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.get("value").is_some_and(&check) && v.get("at").is_some_and(is_stamp))
}

fn fraction(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v))
}

/// v2's progress value `{value, at, viewing, seconds?}` with v3 §3's bounds (a film's `resume` too).
pub fn progress_ok(value: &Value) -> bool {
    let Some(p) = value.as_object() else {
        return false;
    };
    p.get("value").is_some_and(fraction)
        && p.get("at").is_some_and(is_stamp)
        && p.get("viewing").and_then(safe_u64).is_some()
        && p.get("seconds").is_none_or(|s| {
            s.is_null()
                || s.as_f64()
                    .is_some_and(|s| s.is_finite() && (0.0..=1e7).contains(&s))
        })
}

fn stamp_or_null(value: &Value) -> bool {
    value.is_null() || is_stamp(value)
}

pub fn play_key(key: &str) -> Option<i64> {
    key.parse::<i64>()
        .ok()
        .filter(|k| k.to_string() == key && k.unsigned_abs() <= MAX_SAFE_INTEGER)
}

fn plays_ok(value: &Value) -> bool {
    value.as_object().is_some_and(|plays| {
        plays
            .iter()
            .all(|(key, at)| play_key(key).is_some() && safe_u64(at).is_some_and(|at| at >= 1))
    })
}

fn cleared_ok(value: &Value) -> bool {
    value.is_null()
        || value.as_array().is_some_and(|parts| {
            parts.len() == 2 && safe_u64(&parts[0]).is_some() && is_stamp(&parts[1])
        })
}

fn register_member_ok(member: &str, value: &Value) -> bool {
    match member {
        "progress" => progress_ok(value),
        "imported" => value.is_boolean(),
        "plays" => plays_ok(value),
        "cleared" => cleared_ok(value),
        _ => true,
    }
}

fn title_field_ok(field: &str, value: &Value) -> bool {
    match field {
        "status" => stamped(value, Value::is_string),
        "reaction" => stamped(value, |v| v.is_string() || v.is_null()),
        "deleted" | "dismissed" => stamped(value, Value::is_boolean),
        "resume" => progress_ok(value),
        "episodesReset" => stamp_or_null(value),
        "addedAt" => safe_i64(value).is_some(),
        "watchedAt" => value.is_null() || safe_i64(value).is_some(),
        "watch" => value.is_object(),
        _ => true,
    }
}

pub fn order_ok(value: &Value) -> bool {
    value.as_array().is_some_and(|parts| {
        parts.len() == 3
            && safe_u64(&parts[0]).is_some()
            && safe_u64(&parts[1]).is_some()
            && parts[2].is_string()
    })
}

fn sending_ok(value: &Value) -> bool {
    value.as_array().is_some_and(|items| {
        items.iter().all(|item| {
            item.as_array().is_some_and(|e| match e.as_slice() {
                [p, w] => safe_i64(p).is_some_and(|p| p >= -1) && safe_u64(w).is_some(),
                [p, Value::Null, t] => safe_i64(p).is_some_and(|p| p >= 0) && safe_u64(t).is_some(),
                _ => false,
            })
        })
    })
}

/// The settle order of an entry of a known shape (§9): the element its merge compares.
pub fn entry_order(entry: &Value) -> Option<(u64, u64, String)> {
    let parts = entry.as_array()?;
    let index = match parts.first()?.as_str() {
        Some("w" | "u" | "n") => 4,
        Some("b") => 3,
        _ => 2,
    };
    let order = parts.get(index)?.as_array()?;
    Some((
        safe_u64(&order[0])?,
        safe_u64(&order[1])?,
        order[2].as_str()?.to_owned(),
    ))
}

/// §9 entry shapes, by the target the key names. Anything else in a format-4 document is malformed.
pub fn entry_ok(key: &str, entry: &Value) -> bool {
    let Some(parts) = entry.as_array() else {
        return false;
    };
    let watch = key != "list" && key != "rating";
    match parts.first().and_then(Value::as_str) {
        Some(class @ ("w" | "u" | "n")) if watch => {
            (parts.len() == 5 || parts.len() == 6)
                && safe_i64(&parts[1]).is_some_and(|p| p >= 0 || (p == -1 && class == "n"))
                && (parts[2].is_null() || safe_u64(&parts[2]).is_some())
                && is_stamp(&parts[3])
                && order_ok(&parts[4])
                && parts.get(5).is_none_or(sending_ok)
        }
        Some("in" | "gone" | "out") if key == "list" => {
            parts.len() == 3 && is_stamp(&parts[1]) && order_ok(&parts[2])
        }
        Some("b") if !watch => parts.len() == 4 && is_stamp(&parts[2]) && order_ok(&parts[3]),
        _ if key == "rating" && parts.first().is_some_and(|r| r.is_null() || r.is_string()) => {
            (parts.len() == 3 || parts.len() == 4)
                && is_stamp(&parts[1])
                && order_ok(&parts[2])
                && parts
                    .get(3)
                    .is_none_or(|r| safe_u64(r).is_some_and(|r| (1..=10).contains(&r)))
        }
        _ => false,
    }
}

/// A part dropped by `sanitize`: where it was, and why.
pub fn dropped(part: String, reason: &str) -> Value {
    serde_json::json!({"part": part, "reason": reason})
}

fn sanitize_register(register: &mut Map<String, Value>, at: &str, out: &mut Vec<Value>) {
    register.retain(|member, value| {
        let ok = register_member_ok(member, value);
        if !ok {
            out.push(dropped(format!("{at}.{member}"), "malformed"));
        }
        ok
    });
}

/// §4 *Malformed parts*: drop every malformed known part, keep the rest and every unknown member.
pub fn sanitize(identity: &Identity, doc: &mut Map<String, Value>, out: &mut Vec<Value>) {
    let fields: Vec<String> = doc.keys().cloned().collect();
    for field in fields {
        if matches!(
            field.as_str(),
            "format" | "kind" | "title" | "season" | "provider" | "account"
        ) || !known_member(identity, &field)
        {
            continue;
        }
        let ok = match (identity.kind, field.as_str()) {
            (Kind::Title, name) => title_field_ok(name, &doc[&field]),
            (Kind::Season, "seasonReset") => stamp_or_null(&doc[&field]),
            _ => doc[&field].is_object(),
        };
        if !ok {
            doc.remove(&field);
            out.push(dropped(field, "malformed"));
        }
    }
    if let Some(Value::Object(watch)) = doc.get_mut("watch") {
        sanitize_register(watch, "watch", out);
    }
    if let Some(member) = keyed_member(identity.kind) {
        if let Some(Value::Object(map)) = doc.get_mut(member) {
            map.retain(|key, value| {
                if !valid_key(identity, key) {
                    return true;
                }
                let ok = match identity.kind {
                    Kind::Delivery => entry_ok(key, value),
                    _ => value.is_object(),
                };
                if !ok {
                    out.push(dropped(format!("{member}.{key}"), "malformed"));
                }
                ok
            });
            if identity.kind == Kind::Season {
                for (key, value) in map.iter_mut() {
                    if let (true, Value::Object(register)) = (valid_episode_key(key), value) {
                        sanitize_register(register, &format!("episodes.{key}"), out);
                    }
                }
            }
        }
    }
}

/// The stamp of a stamped field (`status.at`), or a bare stamp field (`episodesReset`), when present and valid.
pub fn field_stamp(doc: &Map<String, Value>, field: &str) -> Option<Stamp> {
    let value = doc.get(field)?;
    let at = if value.is_object() {
        value.get("at")?
    } else {
        value
    };
    stamp(at).ok()
}
