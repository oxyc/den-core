use crate::series::WATCHED;
use crate::wire::{name, Stamp};
use serde_json::{json, Value};

/// What one episode row means for the watch state a client already holds.
///
/// Both clients fold rows into local state on every pull, and they must agree on what a row *means* — above
/// all on the zero stamp: `wire/library-v2.md` §3, "A watched bit learned without a time (a tracker import) is
/// written with the zero stamp `[0, 0, ""]`, so any real edit beats it."
///
/// The answer is an action rather than a mark, because the two clients store watch state differently and
/// neither shape belongs in here. The TV keeps marks, bare watched flags and un-watch stamps in three places —
/// a timeless bit becomes a flag there precisely so imports cannot fill its bounded mark cache — while the web
/// keeps one mark per episode. An action each can apply to its own store keeps the rule shared without the
/// storage coming with it:
///
/// - `clear` — the episode was un-watched; hold nothing for it.
/// - `keep` — what is already held still wins; change nothing.
/// - `flag` — watched, with no time known. Worth recording as watched, but it must never displace or restamp
///   real progress, and it must not be treated as a resume position.
/// - `replace` — take the row's `fraction`, `at`, and `seconds` when present.
///
/// `mark` is what the client holds now — `{"fraction", "at"}`, or null when it holds nothing.
///
/// `authoritative` marks a row read back from the log's own canonical store rather than reconstructed from
/// local state: it *is* the record, not a claim about it, so it wins on stamp order. It does not override the
/// zero stamp, which says only that the time is unknown.
pub fn episode_mark(row: &Value, mark: &Value, authoritative: bool) -> Result<Value, String> {
    if !name(row)?.starts_with("ep:") {
        return Err("invalid_kind".into());
    }
    let at: Stamp =
        serde_json::from_value(row["progress"]["at"].clone()).map_err(|_| "invalid_stamp")?;
    at.validate()?;
    let value = row["progress"]["value"]
        .as_f64()
        .ok_or("invalid_progress")?;
    if !(0.0..=1.0).contains(&value) {
        return Err("invalid_progress".into());
    }

    // Value 0 is an un-watch, written in a new viewing (§3). An authoritative row IS the record, and an
    // un-watch with no time of its own has nothing to compare, so both clear outright. A reconstructed row
    // carrying a real stamp is only a claim about the record, and the rule that governs `replace` governs it
    // here too: an un-watch must not undo progress written after it, or a stale local row replayed on a pull
    // silently discards the newer viewing it never saw.
    if value == 0.0 {
        if !authoritative && at.0 != 0 && holds_newer(mark, at.0) {
            return Ok(json!({"action": "keep"}));
        }
        return Ok(json!({"action": "clear"}));
    }

    let held = mark.as_object();
    if at.0 == 0 {
        // A bit with no time says only "this was watched". Against anything already watched it adds nothing,
        // and restamping to the epoch would make every later comparison read real progress as the older side.
        // Anything already held wins, whatever its fraction. A mark below the threshold is real progress with
        // a real time behind it, and a timeless import must neither overwrite it nor declare it watched: doing
        // so leaves the episode reading "watched" while its resume position still says otherwise, and the
        // fabricated bit then propagates to every other device and never gets cleaned up.
        //
        // A bit that is itself below the threshold says nothing at all — no completion, no time. The rule this
        // replaced did nothing with it, and refusing it instead would never converge: the row stays in the
        // client's canonical store and is offered again on every pull, failing identically forever.
        if value < WATCHED || held.is_some() {
            return Ok(json!({"action": "keep"}));
        }
        return Ok(json!({"action": "flag"}));
    }

    // Otherwise the newer stamp wins; a held mark with no stamp of its own loses to anything. An
    // authoritative row skips the comparison: it is the record itself, so there is nothing to outrank.
    if !authoritative && holds_newer(mark, at.0) {
        return Ok(json!({"action": "keep"}));
    }
    let mut result = json!({"action": "replace", "fraction": value, "at": at.0});
    if let Some(seconds) = row["progress"]["seconds"].as_f64() {
        result["seconds"] = json!(seconds);
    }
    Ok(result)
}

/// Whether what the client holds is at least as new as `at`.
///
/// Ties go to what is held: a row has to be strictly newer to displace it, or two writes in the same
/// millisecond would flip the answer on whichever happened to be asked last. Asked for, not indexed: indexing
/// a map panics on a missing key, and a client is free to hand over a mark it holds without a time. A mark
/// with no time of its own — or one that is not an object at all — loses to anything, as it should.
fn holds_newer(mark: &Value, at: i64) -> bool {
    mark.as_object()
        .is_some_and(|held| held.get("at").and_then(Value::as_i64).unwrap_or(0) >= at)
}
