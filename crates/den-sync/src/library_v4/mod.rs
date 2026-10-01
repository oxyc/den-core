//! Library-v4 policy (den-spec `wire/library-v4.md`): documents, their encoding and merge, derived state, writes,
//! tracker delivery and the switch from v3. Pure: every clock and provider fact is an input, and sealing and the
//! HMAC check stay with the clients (§14).

pub mod codec;
pub mod delivery;
pub mod doc;
pub mod jcs;
pub mod merge;
pub mod state;
pub mod switch;
pub mod write;

use doc::{identity, sanitize, Identity, Kind, FORMAT};
use serde_json::{json, Map, Value};

/// A document checked as a reader checks one: a format-4 object of a known kind with a valid identity, its
/// malformed parts dropped. For ops whose input is a document the client already decoded.
pub fn checked(value: &Value) -> Result<(Identity, Map<String, Value>), String> {
    let mut doc = value.as_object().cloned().ok_or("invalid_document")?;
    let format = doc.get("format").and_then(Value::as_u64);
    let identity = identity(&doc)?;
    match format {
        Some(FORMAT) => {}
        Some(f) if f > FORMAT => return Err("newer_format".into()),
        _ => return Err("invalid_format".into()),
    }
    sanitize(&identity, &mut doc, &mut Vec::new());
    Ok((identity, doc))
}

/// A document as state is read from it: like `checked`, but a newer `format` is read for the fields this spec
/// defines (§4 *Newer rows*).
pub fn readable(value: &Value) -> Result<Map<String, Value>, String> {
    match checked(value) {
        Ok((_, doc)) => Ok(doc),
        Err(error) if error == "newer_format" => Ok(value.as_object().cloned().unwrap_or_default()),
        Err(error) => Err(error),
    }
}

/// `doc_merge` (§6, §9): two versions of one document.
pub fn doc_merge(a: &Value, b: &Value) -> Result<Value, String> {
    let (ia, a) = checked(a)?;
    let (ib, b) = checked(b)?;
    if ia != ib {
        return Err("identity_mismatch".into());
    }
    Ok(merge::merge(&ia, &a, &b))
}

/// `write_back` (§11): after a generation change, the merge of every held document with the log's when it
/// differs, the kept writes re-applied as ops, and settled delivery entries merged by settle order. Held v2 or v3
/// rows are discarded; nothing here writes a `lease`.
pub fn write_back(
    held: &[Value],
    kept: &[Value],
    log: &[Value],
    now: i64,
) -> Result<Value, String> {
    let mut current: std::collections::BTreeMap<String, Value> = Default::default();
    let mut newer = std::collections::BTreeSet::new();
    for doc in log {
        match checked(doc) {
            Ok((id, doc)) => {
                current.insert(id.name(), Value::Object(doc));
            }
            Err(error) if error == "newer_format" => {
                newer.insert(name(doc)?);
            }
            Err(error) => return Err(error),
        }
    }
    let log_version = current.clone();
    let mut write_cap: std::collections::BTreeSet<String> = Default::default();
    let mut discarded = 0u64;
    let mut dropped = Vec::new();
    for doc in held {
        let Ok((id, doc)) = checked(doc) else {
            discarded += 1;
            continue;
        };
        let name = id.name();
        if newer.contains(&name) {
            dropped.push(json!({"name": name, "reason": "newer_format"}));
            continue;
        }
        let merged = match current.get(&name) {
            Some(log_doc) => merge::merge(&id, log_doc.as_object().unwrap_or(&Map::new()), &doc),
            None => Value::Object(doc),
        };
        current.insert(name, merged);
    }
    for write in kept {
        let target = &write["target"];
        let title_name = format!(
            "title:{}:{}",
            target["type"].as_str().unwrap_or_default(),
            target["id"]
        );
        let season_prefix = format!("season:tv:{}:", target["id"]);
        let title = current.get(&title_name).cloned();
        let seasons: Vec<Value> = current
            .iter()
            .filter(|(n, _)| n.starts_with(&season_prefix))
            .map(|(_, d)| d.clone())
            .collect();
        let receipts: Vec<Value> = current
            .iter()
            .filter(|(n, _)| n.starts_with("dlv:"))
            .map(|(_, d)| d.clone())
            .collect();
        let out = write::apply_write(
            &write["write"],
            target,
            title.as_ref(),
            &seasons,
            &receipts,
            now,
        )?;
        for doc in out["documents"].as_array().into_iter().flatten() {
            let name = name(doc)?;
            write_cap.insert(name.clone());
            current.insert(name, doc.clone());
        }
    }
    let mut writes = Vec::new();
    for (name, doc) in current {
        if log_version
            .get(&name)
            .is_some_and(|log_doc| jcs::same(log_doc, &doc))
        {
            continue;
        }
        let encoded = encode(&doc, write_cap.contains(&name))?;
        if encoded.get("too_large").is_some() {
            // The log's version stands and the held copy's extra state is dropped (§11).
            dropped.push(json!({"name": name, "reason": "too_large"}));
            continue;
        }
        writes.push(json!({"name": name, "document": doc}));
    }
    Ok(json!({"writes": writes, "dropped": dropped, "discarded": discarded}))
}

/// `doc_decode` (§4, §14): plaintext → a document with the parts it dropped, a JSON row, or unreadable / newer.
/// `expected` is the name the row's `k` was checked against, when the caller knows it.
pub fn decode(plaintext: &[u8], expected: Option<&str>) -> Value {
    let unreadable = |reason: &str| json!({"status": "unreadable", "reason": reason});
    let text = match plaintext.first() {
        None => return unreadable("empty"),
        Some(&codec::COMPRESSED) => match codec::inflate(&plaintext[1..]) {
            Ok(text) => text,
            Err(reason) => return unreadable(reason),
        },
        Some(b'{') => plaintext.to_vec(),
        // §15 pins a JSON plaintext with leading whitespace as unreadable, not as a newer framing.
        Some(b' ' | b'\t' | b'\n' | b'\r') => return unreadable("leading_whitespace"),
        Some(_) => return json!({"status": "newer", "reason": "framing"}),
    };
    // serde_json refuses invalid UTF-8 and a lone surrogate escape, which JCS cannot serialize.
    let value: Value = match serde_json::from_slice(&text) {
        Ok(value) => value,
        Err(_) => return unreadable("invalid_json"),
    };
    let Some(row) = value.as_object() else {
        return unreadable("not_object");
    };
    if codec::depth(&value) > codec::MAX_DEPTH {
        return unreadable("depth");
    }
    let kind = row.get("kind").and_then(Value::as_str);
    if kind.and_then(Kind::parse).is_none() {
        let legacy = matches!(kind, Some("rec" | "wat" | "snt" | "ep"))
            || (kind == Some("set")
                && row["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("tracker-event:")));
        return json!({
            "status": "row",
            "name": crate::wire::name(&value).ok(),
            "legacy": legacy,
            "row": value,
        });
    }
    let format = match row.get("format").and_then(Value::as_u64) {
        Some(format) if format >= FORMAT => format,
        _ => return unreadable("format"),
    };
    let identity = match identity(row) {
        Ok(identity) => identity,
        Err(_) => return unreadable("identity"),
    };
    let name = identity.name();
    if expected.is_some_and(|expected| expected != name) {
        return unreadable("identity");
    }
    let mut doc = row.clone();
    let mut dropped = Vec::new();
    if format == FORMAT {
        sanitize(&identity, &mut doc, &mut dropped);
    } else {
        // §4 *Newer rows*: read for the fields this spec defines. A new entry shape comes with a newer format
        // (§9), so a newer document's entries are not judged by format 4's shapes.
        let entries = doc.remove("entries");
        sanitize(&identity, &mut doc, &mut dropped);
        if let Some(entries) = entries {
            doc.insert("entries".into(), entries);
        }
    }
    json!({
        "status": if format == FORMAT { "document" } else { "newer" },
        "name": name,
        "document": doc,
        "dropped": dropped,
    })
}

/// `doc_encode` (§4, §14): document → plaintext, or `too_large` against the cap. A writer decodes what it
/// encoded and refuses a value that does not decode to the same document (§4 *Writers check themselves*).
pub fn encode(document: &Value, write: bool) -> Result<Value, String> {
    let (identity, _) = checked(document)?;
    let cap = if write {
        codec::WRITE_CAP
    } else {
        codec::ROW_CAP
    };
    let size = jcs::jcs(document).len();
    if size > codec::MAX_JCS {
        return Ok(json!({"too_large": true, "reason": "jcs", "jcs": size, "cap": cap}));
    }
    let plaintext = codec::compress(document)?;
    let sealed = codec::sealed_len(plaintext.len());
    if sealed > cap {
        return Ok(json!({"too_large": true, "reason": "sealed", "sealed": sealed, "cap": cap}));
    }
    let back = decode(&plaintext, Some(&identity.name()));
    if back["status"] != "document" || !jcs::same(&back["document"], document) {
        return Err(format!("self_check:{}", identity.name()));
    }
    Ok(json!({
        "name": identity.name(),
        "plaintext": codec::base64(&plaintext),
        "sealed": sealed,
    }))
}

/// `doc_name` (§3): the name a document's identity spells.
pub fn name(document: &Value) -> Result<String, String> {
    Ok(identity(document.as_object().ok_or("invalid_document")?)?.name())
}

#[cfg(test)]
mod tests;
