use crate::wire::{capture, name, Stamp};
use serde_json::{json, Value};

/// Translate only recorded, still-winning user intent. Imports never invent events.
/// Transport, credentials, receipts, and the final current/account recheck belong to the caller.
pub fn commands(event: &Value, current: &Value) -> Result<Value, String> {
    if event["schema"] != 1 {
        return Err("invalid_event_schema".into());
    }
    let at: Stamp = serde_json::from_value(event["at"].clone()).map_err(|_| "invalid_stamp")?;
    let id = event["id"].as_str().ok_or("invalid_event_id")?;
    let captured = capture(&event["before"], &event["after"], &at, id)?;
    if captured.is_null() || captured["changes"] != event["changes"] {
        return Err("invalid_event_changes".into());
    }
    if name(current)? != name(&event["after"])? {
        return Ok(json!([]));
    }
    let mut output = Vec::new();
    for (field, change) in event["changes"]
        .as_object()
        .ok_or("invalid_event_changes")?
    {
        if current[field] != change["after"] {
            continue;
        }
        let media = current["title"]["type"].as_str().ok_or("invalid_title")?;
        let mut kind = None;
        let mut added = true;
        let mut rating = None;
        match current["kind"].as_str() {
            Some("rec") => {
                let deleted = current["deleted"]["value"]
                    .as_bool()
                    .ok_or("invalid_deleted")?;
                if field == "status" && !deleted {
                    kind = match current["status"]["value"].as_str() {
                        Some("watchlist") => Some("list"),
                        Some("watched") if media == "movie" => Some("watched"),
                        Some("none")
                            if media == "movie"
                                && event["before"]["status"]["value"] == "watched" =>
                        {
                            Some("unwatched")
                        }
                        _ => None,
                    };
                } else if field == "reaction" && !deleted {
                    rating = match current["reaction"]["value"].as_str() {
                        Some("love") => Some(10),
                        Some("like") => Some(7),
                        Some("dislike") => Some(2),
                        None if current["reaction"]["value"].is_null() => None,
                        _ => return Err("invalid_reaction".into()),
                    };
                    kind = Some("rating");
                } else if field == "deleted"
                    && deleted
                    && event["before"]["status"]["value"] == "watchlist"
                {
                    kind = Some("list");
                    added = false;
                }
            }
            Some("ep") if field == "progress" => {
                let progress = current["progress"]["value"]
                    .as_f64()
                    .ok_or("invalid_progress")?;
                // den-spec / LibraryRecord.watchedThreshold. Completion is not inferred from a series row.
                if progress >= 0.95 {
                    kind = Some("watched");
                } else if progress == 0.0 {
                    kind = Some("unwatched");
                }
            }
            _ => {}
        }
        if let Some(kind) = kind {
            output.push(json!({"kind": kind, "title": current["title"],
                "season": current.get("season"), "episode": current.get("episode"),
                "added": added, "rating": rating, "at": at.0, "stamp": at,
                "eventID": format!("{id}:{field}")}));
        }
    }
    Ok(json!(output))
}
