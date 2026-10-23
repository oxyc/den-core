//! Shared watch-state, delivery-receipt, lease, and legacy row-name primitives used by Library v4.

use crate::wire::{stamp, Stamp, MAX_SAFE_INTEGER};
use serde_json::{json, Map, Value};

const DAY: i64 = 86_400_000;

fn object(value: &Value) -> Result<&Map<String, Value>, String> {
    value.as_object().ok_or_else(|| "invalid_object".into())
}

fn safe_u64(value: &Value) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|n| *n <= MAX_SAFE_INTEGER)
        .ok_or_else(|| "invalid_integer".into())
}

fn valid_device(device: &str) -> bool {
    device.is_empty()
        || device == "local"
        || (device.len() == 16
            && device
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn validate_writer_stamp(value: &Value) -> Result<Stamp, String> {
    let value = stamp(value)?;
    if !valid_device(&value.2) {
        return Err("invalid_device".into());
    }
    Ok(value)
}

fn effective_t(value: &Stamp, now: i64) -> i64 {
    if value.0 > now.saturating_add(DAY) {
        0
    } else {
        value.0
    }
}

fn floor(resets: &[Stamp], now: i64) -> i64 {
    resets
        .iter()
        .map(|r| effective_t(r, now))
        .filter(|t| *t > 0)
        .max()
        .unwrap_or(0)
}

fn plays(register: &Value, resets: &[Stamp], now: i64) -> Result<Vec<(i64, u64)>, String> {
    let reset = floor(resets, now);
    let cleared = register
        .get("cleared")
        .filter(|v| !v.is_null())
        .map(|v| {
            let parts = v
                .as_array()
                .filter(|v| v.len() == 2)
                .ok_or("invalid_cleared")?;
            Ok::<_, String>((safe_u64(&parts[0])? as i64, stamp(&parts[1])?.0))
        })
        .transpose()?
        .unwrap_or((-1, 0));
    let mut result = Vec::new();
    for (key, value) in register
        .get("plays")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let key = key.parse::<i64>().map_err(|_| "invalid_play")?;
        let at = safe_u64(value)?;
        if at > now.saturating_add(DAY).max(0) as u64 || at <= reset.max(0) as u64 {
            continue;
        }
        if (key >= 0 && key <= cleared.0) || (key < 0 && at <= cleared.1.max(0) as u64) {
            continue;
        }
        result.push((key, at));
    }
    result.sort_unstable_by_key(|(_, at)| *at);
    Ok(result)
}

pub fn episode_state(register: &Value, resets: &[Stamp], now: i64) -> Result<Value, String> {
    let register = object(register)?;
    let floor = floor(resets, now);
    let progress = register.get("progress").filter(|value| !value.is_null());
    let viewing = progress
        .map(|p| safe_u64(&p["viewing"]))
        .transpose()?
        .unwrap_or(0);
    let visible_progress =
        progress.filter(|p| stamp(&p["at"]).is_ok_and(|at| effective_t(&at, now) > floor));
    let visible_plays = plays(&Value::Object(register.clone()), resets, now)?;
    let imported_play = visible_plays.iter().any(|(key, _)| *key < 0);
    let imported = register
        .get("imported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && (floor == 0 || imported_play);
    let value = visible_progress.and_then(|p| p["value"].as_f64());
    let watched = value.is_some_and(|v| v >= 0.95) || (visible_progress.is_none() && imported);
    let resume = visible_progress
        .filter(|p| {
            p["value"]
                .as_f64()
                .is_some_and(|v| (0.02..0.95).contains(&v))
        })
        .cloned();
    let watched_at = visible_plays
        .iter()
        .find(|(key, _)| *key == viewing as i64)
        .or_else(|| visible_plays.last())
        .map(|(_, at)| *at);
    Ok(json!({
        "watched": watched,
        "resume": resume,
        "viewing": viewing,
        "plays": visible_plays.iter().map(|(p, at)| json!([p, at])).collect::<Vec<_>>(),
        "first_play": visible_plays.first().map(|(_, at)| *at),
        "watched_at": watched_at
    }))
}

pub fn film_state(
    rec: &Value,
    register: &Value,
    resets: &[Stamp],
    now: i64,
) -> Result<Value, String> {
    let mut state = episode_state(register, resets, now)?;
    let status = rec["status"]["value"].as_str().ok_or("invalid_status")?;
    let viewing = safe_u64(&rec["resume"]["viewing"])?;
    let visible = if matches!(status, "none" | "watchlist") {
        Vec::new()
    } else {
        plays(register, resets, now)?
    };
    let status_at = stamp(&rec["status"]["at"])?;
    let watched_at = visible
        .iter()
        .find(|(key, _)| *key == viewing as i64)
        .map(|(_, at)| *at)
        .or_else(|| {
            (status_at.0 > 0 && effective_t(&status_at, now) > 0).then_some(status_at.0 as u64)
        })
        .or_else(|| visible.last().map(|(_, at)| *at));
    state["watched"] = json!(status == "watched");
    state["resume"] = rec["resume"].clone();
    state["viewing"] = json!(viewing);
    state["plays"] = json!(visible
        .iter()
        .map(|(p, at)| json!([p, at]))
        .collect::<Vec<_>>());
    state["first_play"] = json!(visible.first().map(|(_, at)| *at));
    state["watched_at"] = json!(watched_at);
    Ok(state)
}

pub fn settle(outcome: &Value, built: &Value, order: &Value) -> Result<Value, String> {
    let action = outcome["action"].as_str().ok_or("invalid_outcome")?;
    if matches!(action, "hold" | "superseded")
        || (built["value"] == json!("none") && action != "resettle")
    {
        return Ok(Value::Null);
    }
    let order = order
        .as_array()
        .filter(|v| v.len() == 3)
        .ok_or("invalid_settle_order")?;
    safe_u64(&order[0])?;
    safe_u64(&order[1])?;
    let stamp = built.get("stamp").ok_or("missing_stamp")?;
    validate_writer_stamp(stamp)?;
    let value = built["value"].clone();
    let receipt = match built["kind"].as_str() {
        Some("episode" | "film") => json!([
            value
                .as_str()
                .and_then(|v| v.chars().next())
                .unwrap_or('n')
                .to_string(),
            built.get("p").and_then(Value::as_i64).unwrap_or(-1),
            built.get("watched_at").cloned().unwrap_or(Value::Null),
            stamp,
            order
        ]),
        Some("list") => json!([value, stamp, order]),
        Some("rating") => {
            let mut values = vec![value, stamp.clone(), Value::Array(order.clone())];
            if action == "acknowledge" {
                if let Some(remote) = outcome.get("remote_rating").filter(|v| !v.is_null()) {
                    values.push(remote.clone());
                }
            }
            Value::Array(values)
        }
        _ => return Err("invalid_target".into()),
    };
    Ok(receipt)
}

/// Pure lease state machine. Durations are supplied as monotonic elapsed milliseconds by the caller.
pub fn lease(input: &Value) -> Result<Value, String> {
    let device = input["device"].as_str().ok_or("invalid_device")?;
    if !valid_device(device) || device.is_empty() || device == "local" {
        return Err("invalid_device".into());
    }
    if input["generation_changed"].as_bool().unwrap_or(false) {
        return Ok(json!({"action":"stop","reason":"generation_changed"}));
    }
    let holder = input["holder"].as_str().unwrap_or("");
    let epoch = input["epoch"].as_u64().unwrap_or(0);
    let elapsed = input["elapsed"].as_u64().unwrap_or(u64::MAX);
    if holder == device {
        if elapsed >= 120_000 || input["clock_backwards"].as_bool().unwrap_or(false) {
            return Ok(json!({"action":"stop","reason":"expired"}));
        }
        return Ok(if elapsed >= 60_000 {
            json!({"action":"renew","epoch":epoch})
        } else {
            json!({"action":"send","epoch":epoch})
        });
    }
    let observed = input["observed"].as_u64().unwrap_or(0);
    let fresh_generation = input["fresh_generation"].as_bool().unwrap_or(false);
    if holder.is_empty() && !fresh_generation || observed >= 600_000 {
        let greatest = input["greatest_epoch"].as_u64().unwrap_or(epoch).max(epoch);
        return Ok(json!({"action":"take","epoch":greatest + 1}));
    }
    Ok(json!({"action":"wait","reason":"observation"}))
}

pub fn receipt_row_name(provider: &str, account: &str, target: &str) -> Result<String, String> {
    if provider.is_empty() || provider.contains(':') || account.is_empty() || account.contains(':')
    {
        return Err("invalid_receipt_identity".into());
    }
    if target.starts_with("wat:") {
        return Ok(format!("snt:{provider}:{account}:{target}"));
    }
    if target.starts_with("rec:") {
        let digest = sha256(target.as_bytes());
        return Ok(format!(
            "snt:{provider}:{account}:t{:02x}{:01x}",
            digest[0],
            digest[1] >> 4
        ));
    }
    Err("invalid_receipt_target".into())
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut bytes = input.to_vec();
    let bit_len = (bytes.len() as u64) * 8;
    bytes.push(0x80);
    while bytes.len() % 64 != 56 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for block in bytes.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            words[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = words[i - 15].rotate_right(7)
                ^ words[i - 15].rotate_right(18)
                ^ (words[i - 15] >> 3);
            let s1 = words[i - 2].rotate_right(17)
                ^ words[i - 2].rotate_right(19)
                ^ (words[i - 2] >> 10);
            words[i] = words[i - 16]
                .wrapping_add(s0)
                .wrapping_add(words[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let first = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[i])
                .wrapping_add(words[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut digest = [0u8; 32];
    for (out, value) in digest.chunks_exact_mut(4).zip(state) {
        out.copy_from_slice(&value.to_be_bytes());
    }
    digest
}

pub fn watch_row_name(media: &str, id: u64, season: u64, episode: u64) -> Result<String, String> {
    if !matches!(media, "tv" | "movie")
        || id == 0
        || (media == "movie" && (season != 0 || episode != 0))
    {
        return Err("invalid_watch_coordinate".into());
    }
    Ok(format!("wat:{media}:{id}:{season}:{}", episode / 32))
}
