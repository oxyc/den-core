//! Assistant writes (den-spec `wire/assistant-v1.md`) through `evaluate`, as both clients call them.
//!
//! Every case here is checked in code, and the same cases are the den-spec vectors (`vectors/assistant-v1.json`):
//! `write_assistant_vectors` (ignored) writes them, `den_spec_assistant_vectors` replays the file, and the assistant
//! cases of `fixtures/policy-v1.json` must be the same cases, so the binding contract and the spec cannot drift.

use den_assistant::{
    b64url, grant_public, hex, hpke_seal, kem_public, key_id, seal_claim, seal_request, wrap,
    wrap_key, GrantKey, Request, REQUEST_INFO, SIGN_CONTEXT,
};
use den_sync::evaluate;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const D: &str = "a1b2c3d4e5f60718";
const LIBRARY: &str = "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e";
const OTHER_LIBRARY: &str = "9e2a7b3c4d5e6f708192a3b4c5d6e7f8";
const NOW: u64 = 1_790_000_000_000;
const MIN: u64 = 60_000;
const HOUR: u64 = 3_600_000;
const DAY: u64 = 86_400_000;

fn call(request: &Value) -> Value {
    serde_json::from_str(&evaluate(&request.to_string())).unwrap()
}

fn ok(request: &Value) -> Value {
    let response = call(request);
    assert!(response.get("error").is_none(), "{request}\n→ {response}");
    response["ok"].clone()
}

/// `n` bytes for `label`: SHA-256 of `"den/assistant/vectors/" ‖ label ‖ "/" ‖ i`, for i = 0, 1, …, concatenated.
fn bytes(label: &str, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0u32;
    while out.len() < n {
        out.extend(Sha256::digest(
            format!("den/assistant/vectors/{label}/{i}").as_bytes(),
        ));
        i += 1;
    }
    out.truncate(n);
    out
}

fn seed(label: &str) -> [u8; 32] {
    bytes(label, 32).try_into().unwrap()
}

fn id(label: &str) -> String {
    hex(&bytes(&format!("id/{label}"), 16))
}

fn eseed(label: &str) -> Vec<u8> {
    bytes(&format!("eseed/{label}"), 64)
}

struct Keys {
    dropbox: [u8; 32],
    dropbox2: [u8; 32],
    stranger: [u8; 32],
    grant: [u8; 32],
    limited: [u8; 32],
    revoked: [u8; 32],
    other: [u8; 32],
    mcp: [u8; 32],
}

fn keys() -> Keys {
    Keys {
        dropbox: seed("dropbox"),
        dropbox2: seed("dropbox2"),
        stranger: seed("stranger"),
        grant: seed("grant"),
        limited: seed("limited"),
        revoked: seed("revoked"),
        other: seed("other"),
        mcp: seed("mcp"),
    }
}

fn gid(secret: &[u8; 32]) -> String {
    den_assistant::grant_id(&grant_public(secret))
}

fn setting(value: Value, at: u64) -> Value {
    json!({"value": value, "at": [at, 0, D]})
}

fn row(name: &str, values: Value) -> Value {
    json!({"kind": "set", "schema": 2, "name": name, "values": values})
}

fn dropbox_row(secrets: &[[u8; 32]]) -> Value {
    let mut values = Map::new();
    for (i, secret) in secrets.iter().enumerate() {
        let made = ok(&json!({"op": "assistant_keygen_dropbox", "random": hex(secret)}));
        values.insert(
            made["setting"].as_str().unwrap().into(),
            setting(made["value"].clone(), NOW - 40 * DAY + i as u64),
        );
    }
    row("assistant", Value::Object(values))
}

fn grant_value(
    secret: &[u8; 32],
    client: &str,
    ops: Value,
    cap: u64,
    revoked: Option<u64>,
) -> Value {
    let made = ok(
        &json!({"op": "assistant_keygen_grant", "random": hex(secret), "client": client,
        "ops": ops, "cap": cap, "now": NOW - 30 * DAY}),
    );
    match revoked {
        None => made["value"].clone(),
        Some(at) => ok(
            &json!({"op": "assistant_revoke", "grant": made["grant"], "value": made["value"],
            "now": at}),
        )["value"]
            .clone(),
    }
}

fn grants_row() -> Value {
    let k = keys();
    let all = json!(["watchlist_add", "watchlist_remove", "seen", "rate"]);
    row(
        "assistant-grants",
        json!({
            gid(&k.grant): setting(grant_value(&k.grant, "Claude", all.clone(), 3, None), NOW - 30 * DAY),
            gid(&k.limited): setting(
                grant_value(&k.limited, "ChatGPT", json!(["seen", "watchlist_add"]), 50, None),
                NOW - 30 * DAY,
            ),
            gid(&k.revoked): setting(grant_value(&k.revoked, "Claude", all, 50, Some(NOW - HOUR)), NOW - HOUR),
        }),
    )
}

fn applied_value(grant: &str, at: u64, applied: u64) -> Value {
    json!({"string": json!({"applied": applied, "at": at, "grant": grant}).to_string()})
}

/// One request of the grant's applied in the last day, one two days ago, and the replayed id.
fn applied_row(extra: &[(String, u64, u64)]) -> Value {
    let g = gid(&keys().grant);
    let mut values = Map::new();
    values.insert(
        id("replayed"),
        setting(
            applied_value(&g, NOW - 3 * HOUR, NOW - 2 * HOUR),
            NOW - 2 * HOUR,
        ),
    );
    values.insert(
        id("older"),
        setting(
            applied_value(&g, NOW - 2 * DAY, NOW - 2 * DAY),
            NOW - 2 * DAY,
        ),
    );
    for (i, (grant, at, applied)) in extra.iter().enumerate() {
        values.insert(
            id(&format!("extra{i}")),
            setting(applied_value(grant, *at, *applied), *applied),
        );
    }
    row("assistant-applied", Value::Object(values))
}

fn movie() -> Value {
    json!({"type": "movie", "id": 550})
}

fn series() -> Value {
    json!({"type": "tv", "id": 1399})
}

/// A message object (not yet bytes) from the grant, for `library`.
fn fields(grant: &[u8; 32], label: &str, at: u64, op: &str, args: Value) -> Value {
    json!({"v": 1, "library": LIBRARY, "grant": gid(grant), "id": id(label), "at": at, "op": op, "args": args})
}

/// Seals `message` bytes signed by `signer`, to `dropbox`, with `aad` — the raw path, for requests den-assistant would
/// refuse to build.
fn seal_raw(
    signer: &[u8; 32],
    message: &[u8],
    dropbox: &[u8; 32],
    aad: &str,
    label: &str,
) -> String {
    let signature = ed25519_sign(signer, message);
    let plaintext = [&signature[..], message].concat();
    b64url(
        &hpke_seal(
            &kem_public(dropbox),
            REQUEST_INFO,
            aad.as_bytes(),
            &plaintext,
            &eseed(label),
        )
        .unwrap(),
    )
}

fn ed25519_sign(secret: &[u8; 32], message: &[u8]) -> [u8; 64] {
    den_assistant::sign(secret, message)
}

/// A valid request sealed the way den-mcp seals one, checked to match the raw path byte for byte.
fn seal(grant: &[u8; 32], fields: &Value, dropbox: &[u8; 32], label: &str) -> String {
    let args = fields["args"].clone();
    let request = Request {
        library: fields["library"].as_str().unwrap(),
        grant: fields["grant"].as_str().unwrap(),
        id: fields["id"].as_str().unwrap(),
        at: fields["at"].as_u64().unwrap(),
        op: fields["op"].as_str().unwrap(),
        args: &args,
    };
    let sealed = seal_request(
        &kem_public(dropbox),
        &GrantKey::from_secret(grant),
        &request,
        &eseed(label),
    )
    .unwrap();
    let raw = seal_raw(
        grant,
        &serde_json::to_vec(fields).unwrap(),
        dropbox,
        LIBRARY,
        label,
    );
    assert_eq!(sealed, raw, "{label}");
    sealed
}

fn open(sealed: &str, assistant: Value, grants: Value, applied: Value) -> Value {
    json!({"op": "assistant_open", "library": LIBRARY, "sealed": sealed, "assistant": assistant,
        "grants": grants, "applied": applied, "now": NOW})
}

struct Case {
    name: String,
    request: Value,
    /// `{"ok": …}` or `{"error": …}`.
    expect: Value,
}

fn accept(fields: &Value) -> Value {
    json!({"ok": {"accept": {
        "grant": fields["grant"], "id": fields["id"], "at": fields["at"], "op": fields["op"], "args": fields["args"],
        "setting": fields["id"],
        "value": applied_value(fields["grant"].as_str().unwrap(), fields["at"].as_u64().unwrap(), NOW),
    }}})
}

fn reject(reason: &str) -> Value {
    json!({"ok": {"reject": reason}})
}

fn open_cases() -> Vec<Case> {
    let k = keys();
    let (one, both) = (
        dropbox_row(&[k.dropbox]),
        dropbox_row(&[k.dropbox, k.dropbox2]),
    );
    let grants = grants_row();
    let applied = applied_row(&[]);
    let mut cases = Vec::new();
    let mut add = |name: &str, request: Value, expect: Value| {
        cases.push(Case {
            name: format!("assistant_open: {name}"),
            request,
            expect,
        })
    };
    let g = &k.grant;

    // Accepted, one per op and shape.
    let accepted: [(&str, &str, Value); 8] = [
        (
            "a film added to the watchlist",
            "watchlist_add",
            json!({"title": movie()}),
        ),
        (
            "a series taken off the watchlist",
            "watchlist_remove",
            json!({"title": series()}),
        ),
        (
            "a film marked seen",
            "seen",
            json!({"title": movie(), "value": true}),
        ),
        (
            "a whole series marked unseen",
            "seen",
            json!({"title": series(), "value": false}),
        ),
        (
            "a season marked seen",
            "seen",
            json!({"title": series(), "season": 1, "value": true}),
        ),
        (
            "an episode marked unseen",
            "seen",
            json!({"title": series(), "season": 1, "episode": 2, "value": false}),
        ),
        (
            "a film rated love",
            "rate",
            json!({"title": movie(), "value": "love"}),
        ),
        (
            "a rating cleared",
            "rate",
            json!({"title": series(), "value": null}),
        ),
    ];
    for (name, op, args) in accepted {
        let f = fields(g, name, NOW - MIN, op, args);
        add(
            name,
            open(
                &seal(g, &f, &k.dropbox, name),
                one.clone(),
                grants.clone(),
                applied.clone(),
            ),
            accept(&f),
        );
    }
    let f = fields(
        g,
        "second key",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "sealed to the library's second drop-box key",
        open(
            &seal(g, &f, &k.dropbox2, "second key"),
            both.clone(),
            grants.clone(),
            applied.clone(),
        ),
        accept(&f),
    );
    let f = fields(
        g,
        "oldest",
        NOW - 7 * DAY,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "exactly 7 days old",
        open(
            &seal(g, &f, &k.dropbox, "oldest"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        accept(&f),
    );
    let f = fields(
        g,
        "ahead",
        NOW + 5 * MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "exactly 5 minutes ahead",
        open(
            &seal(g, &f, &k.dropbox, "ahead"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        accept(&f),
    );
    let gs = gid(g);
    let two_today = applied_row(&[(gs.clone(), NOW - HOUR, NOW - HOUR)]);
    let f = fields(
        g,
        "third today",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "the third of a cap of 3 in a day",
        open(
            &seal(g, &f, &k.dropbox, "third today"),
            one.clone(),
            grants.clone(),
            two_today,
        ),
        accept(&f),
    );
    // Counted by when they were applied: requests applied over a day ago don't count, whatever their own `at`.
    let yesterday = applied_row(&[
        (gs.clone(), NOW - 30 * HOUR, NOW - 25 * HOUR),
        (gs.clone(), NOW - DAY - MIN, NOW - DAY),
    ]);
    let f = fields(
        g,
        "next day",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "applied over a day ago counts for nothing",
        open(
            &seal(g, &f, &k.dropbox, "next day"),
            one.clone(),
            grants.clone(),
            yesterday,
        ),
        accept(&f),
    );
    let f = fields(
        &k.limited,
        "limited",
        NOW - MIN,
        "seen",
        json!({"title": movie(), "value": true}),
    );
    add(
        "an op the limited grant allows",
        open(
            &seal(&k.limited, &f, &k.dropbox, "limited"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        accept(&f),
    );

    // Refused, in the order §5 checks.
    let valid = fields(
        g,
        "refused",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "sealed to a key the library does not hold",
        open(
            &seal(g, &valid, &k.stranger, "stranger"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("does_not_open"),
    );
    let sealed_other_aad = seal_raw(
        g,
        &serde_json::to_vec(&valid).unwrap(),
        &k.dropbox,
        OTHER_LIBRARY,
        "other aad",
    );
    add(
        "sealed for another library",
        open(
            &sealed_other_aad,
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("does_not_open"),
    );
    add(
        "not base64url",
        open(
            "not+base64/url",
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("does_not_open"),
    );
    add(
        "longer than den-edge's 4096 characters",
        open(
            &"A".repeat(4100),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("does_not_open"),
    );
    add(
        "a library with no drop-box key",
        open(
            &seal(g, &valid, &k.dropbox, "no key"),
            Value::Null,
            grants.clone(),
            applied.clone(),
        ),
        reject("does_not_open"),
    );
    let signature_only = b64url(
        &hpke_seal(
            &kem_public(&k.dropbox),
            REQUEST_INFO,
            LIBRARY.as_bytes(),
            &[7; 64],
            &eseed("sig only"),
        )
        .unwrap(),
    );
    add(
        "a signature and no message",
        open(
            &signature_only,
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("malformed"),
    );
    add(
        "a message that is not JSON",
        open(
            &seal_raw(g, b"watchlist_add 550", &k.dropbox, LIBRARY, "not json"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("malformed"),
    );
    let f = fields(
        &k.other,
        "unknown",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "a grant the library does not hold",
        open(
            &seal(&k.other, &f, &k.dropbox, "unknown"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("unknown_grant"),
    );
    let forged = seal_raw(
        &k.other,
        &serde_json::to_vec(&valid).unwrap(),
        &k.dropbox,
        LIBRARY,
        "forged",
    );
    add(
        "signed by another key in the grant's name",
        open(&forged, one.clone(), grants.clone(), applied.clone()),
        reject("bad_signature"),
    );
    let spaced = serde_json::to_string_pretty(&valid).unwrap();
    add(
        "a signed message that is not its own JCS",
        open(
            &seal_raw(g, spaced.as_bytes(), &k.dropbox, LIBRARY, "spaced"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("malformed"),
    );
    for (name, op, args) in [
        (
            "an argument v1 does not define",
            "watchlist_add",
            json!({"title": movie(), "note": "x"}),
        ),
        (
            "an episode of a film",
            "seen",
            json!({"title": movie(), "season": 1, "episode": 1, "value": true}),
        ),
        (
            "an episode with no season",
            "seen",
            json!({"title": series(), "episode": 1, "value": true}),
        ),
        (
            "an episode past 99999",
            "seen",
            json!({"title": series(), "season": 1, "episode": 100000, "value": true}),
        ),
        (
            "a rating outside the scale",
            "rate",
            json!({"title": movie(), "value": "seen"}),
        ),
        (
            "an op v1 does not define",
            "delete",
            json!({"title": movie()}),
        ),
        (
            "a title with id 0",
            "watchlist_add",
            json!({"title": {"type": "movie", "id": 0}}),
        ),
    ] {
        let f = fields(g, name, NOW - MIN, op, args);
        let sealed = seal_raw(
            g,
            &serde_json::to_vec(&f).unwrap(),
            &k.dropbox,
            LIBRARY,
            name,
        );
        add(
            name,
            open(&sealed, one.clone(), grants.clone(), applied.clone()),
            reject("malformed"),
        );
    }
    let mut elsewhere = valid.clone();
    elsewhere["library"] = json!(OTHER_LIBRARY);
    elsewhere["id"] = json!(id("elsewhere"));
    let sealed = seal_raw(
        g,
        &serde_json::to_vec(&elsewhere).unwrap(),
        &k.dropbox,
        LIBRARY,
        "elsewhere",
    );
    add(
        "a message for another library sealed for this one",
        open(&sealed, one.clone(), grants.clone(), applied.clone()),
        reject("wrong_library"),
    );
    let f = fields(
        &k.revoked,
        "revoked",
        NOW - 2 * HOUR,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "a revoked grant, even for a request made before the revocation",
        open(
            &seal(&k.revoked, &f, &k.dropbox, "revoked"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("revoked"),
    );
    let f = fields(
        g,
        "future",
        NOW + 5 * MIN + 1,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "more than 5 minutes ahead",
        open(
            &seal(g, &f, &k.dropbox, "future"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("from_future"),
    );
    let f = fields(
        g,
        "stale",
        NOW - 7 * DAY - 1,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "more than 7 days old",
        open(
            &seal(g, &f, &k.dropbox, "stale"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("stale"),
    );
    let f = fields(
        g,
        "replayed",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "an id already applied",
        open(
            &seal(g, &f, &k.dropbox, "replayed"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("replay"),
    );
    let f = fields(
        &k.limited,
        "not allowed",
        NOW - MIN,
        "rate",
        json!({"title": movie(), "value": "like"}),
    );
    add(
        "an op the grant does not allow",
        open(
            &seal(&k.limited, &f, &k.dropbox, "not allowed"),
            one.clone(),
            grants.clone(),
            applied.clone(),
        ),
        reject("op_not_allowed"),
    );
    let three_today = applied_row(&[
        (gs.clone(), NOW - HOUR, NOW - HOUR),
        (gs, NOW - MIN, NOW - MIN),
    ]);
    let f = fields(
        g,
        "fourth today",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "the fourth of a cap of 3 in a day",
        open(
            &seal(g, &f, &k.dropbox, "fourth today"),
            one.clone(),
            grants.clone(),
            three_today,
        ),
        reject("over_daily_cap"),
    );
    // A grant whose stored value is not its key's is no grant: unknown.
    let mut bad = grants.clone();
    let wrong = bad["values"][gid(&k.limited)].clone();
    bad["values"][gid(g)] = wrong;
    let f = fields(
        g,
        "bad grant",
        NOW - MIN,
        "watchlist_add",
        json!({"title": movie()}),
    );
    add(
        "a grant stored under another grant's id",
        open(
            &seal(g, &f, &k.dropbox, "bad grant"),
            one.clone(),
            bad,
            applied.clone(),
        ),
        reject("unknown_grant"),
    );
    cases
}

fn other_cases() -> Vec<Case> {
    let k = keys();
    let mut cases = Vec::new();
    let mut add = |name: &str, request: Value, expect: Value| {
        cases.push(Case {
            name: name.into(),
            request,
            expect,
        })
    };
    let public = kem_public(&k.dropbox);
    add(
        "assistant_keygen_dropbox: the seed is the X-Wing private key",
        json!({"op": "assistant_keygen_dropbox", "random": hex(&k.dropbox)}),
        json!({"ok": {"kid": key_id(&public), "public": b64url(&public),
            "setting": format!("dropbox.{}", key_id(&public)), "value": {"string": b64url(&k.dropbox)}}}),
    );
    add(
        "assistant_keygen_dropbox: 31 bytes",
        json!({"op": "assistant_keygen_dropbox", "random": hex(&k.dropbox[..31])}),
        json!({"error": "invalid_seed"}),
    );
    let both = dropbox_row(&[k.dropbox, k.dropbox2]);
    let later = kem_public(&k.dropbox2);
    add(
        "assistant_dropbox: the key with the latest stamp",
        json!({"op": "assistant_dropbox", "assistant": both}),
        json!({"ok": {"kid": key_id(&later), "public": b64url(&later)}}),
    );
    add(
        "assistant_dropbox: none",
        json!({"op": "assistant_dropbox", "assistant": row("assistant", json!({}))}),
        json!({"ok": null}),
    );
    let gpub = grant_public(&k.grant);
    let value = json!({"string": json!({"cap": 3, "client": "Claude", "createdAt": NOW - 30 * DAY,
        "ops": ["rate", "seen", "watchlist_add", "watchlist_remove"], "pk": b64url(&gpub), "revokedAt": null,
        "v": 1}).to_string()});
    add(
        "assistant_keygen_grant: ops sorted and deduplicated",
        json!({"op": "assistant_keygen_grant", "random": hex(&k.grant), "client": "Claude",
            "ops": ["watchlist_add", "watchlist_remove", "seen", "rate", "seen"], "cap": 3, "now": NOW - 30 * DAY}),
        json!({"ok": {"grant": gid(&k.grant), "public": b64url(&gpub), "secret": b64url(&k.grant),
            "setting": gid(&k.grant), "value": value}}),
    );
    for (name, client, ops, cap, error) in [
        (
            "an empty client name",
            "",
            json!(["seen"]),
            3,
            "invalid_client",
        ),
        (
            "a client name with a bidi override",
            "Cla\u{202e}ude",
            json!(["seen"]),
            3,
            "invalid_client",
        ),
        ("no ops", "Claude", json!([]), 3, "invalid_ops"),
        (
            "an op v1 does not define",
            "Claude",
            json!(["delete"]),
            3,
            "invalid_ops",
        ),
        ("a cap of 0", "Claude", json!(["seen"]), 0, "invalid_cap"),
        (
            "a cap over 1000",
            "Claude",
            json!(["seen"]),
            1001,
            "invalid_cap",
        ),
    ] {
        add(
            &format!("assistant_keygen_grant: {name}"),
            json!({"op": "assistant_keygen_grant", "random": hex(&k.grant), "client": client, "ops": ops,
                "cap": cap, "now": NOW}),
            json!({"error": error}),
        );
    }
    let revoked_at = |v: &Value| -> Value {
        let mut object: Value = serde_json::from_str(value["string"].as_str().unwrap()).unwrap();
        object["revokedAt"] = v.clone();
        json!({"string": object.to_string()})
    };
    add(
        "assistant_revoke: sets revokedAt",
        json!({"op": "assistant_revoke", "grant": gid(&k.grant), "value": value, "now": NOW}),
        json!({"ok": {"value": revoked_at(&json!(NOW))}}),
    );
    add(
        "assistant_revoke: an earlier revocation stands",
        json!({"op": "assistant_revoke", "grant": gid(&k.grant), "value": revoked_at(&json!(NOW - DAY)),
            "now": NOW}),
        json!({"ok": {"value": revoked_at(&json!(NOW - DAY))}}),
    );
    add(
        "assistant_revoke: under another grant's id",
        json!({"op": "assistant_revoke", "grant": gid(&k.limited), "value": value, "now": NOW}),
        json!({"error": "invalid_grant"}),
    );
    // Merge: a, b and c of one grant; the earliest revocation, the latest stamp. A malformed version ranks below.
    let a = setting(value.clone(), 5);
    let b = setting(revoked_at(&json!(2000)), 7);
    let c = setting(revoked_at(&json!(1500)), 6);
    let merged = setting(revoked_at(&json!(1500)), 7);
    let grants = |v: Value| row("assistant-grants", json!({gid(&k.grant): v}));
    add(
        "merge: assistant grants keep the earliest revocation and the latest stamp",
        json!({"op": "merge", "a": grants(merge_of(&[&a, &b])), "b": grants(c.clone())}),
        json!({"ok": grants(merged.clone())}),
    );
    add(
        "merge: an assistant grant beats a malformed version with a later stamp",
        json!({"op": "merge", "a": grants(a.clone()), "b": grants(setting(json!({"string": "{}"}), 9))}),
        json!({"ok": grants(a.clone())}),
    );
    // Prune: entries whose request is over 8 days old.
    let g = gid(&k.grant);
    let applied = row(
        "assistant-applied",
        json!({
            id("p1"): setting(applied_value(&g, NOW - 8 * DAY - 1, NOW - 2 * DAY), NOW - 2 * DAY),
            id("p2"): setting(applied_value(&g, NOW - 8 * DAY, NOW - 2 * DAY), NOW - 2 * DAY),
            id("p3"): setting(json!({"string": "not json"}), NOW),
        }),
    );
    let mut remove = vec![id("p1"), id("p3")];
    remove.sort();
    add(
        "assistant_prune: requests over 8 days old, and malformed entries",
        json!({"op": "assistant_prune", "applied": applied, "now": NOW}),
        json!({"ok": {"remove": remove}}),
    );
    add(
        "assistant_open: a library id that is not one",
        json!({"op": "assistant_open", "library": "LIBRARY", "sealed": "", "now": NOW}),
        json!({"error": "invalid_library"}),
    );
    cases
}

/// The setting `a ⊔ b`, computed through `merge` itself, so a three-way case is built from the op it tests.
fn merge_of(versions: &[&Value]) -> Value {
    let id = gid(&keys().grant);
    let grants = |v: &Value| row("assistant-grants", json!({id.clone(): v}));
    let mut out = versions[0].clone();
    for v in &versions[1..] {
        out = ok(&json!({"op": "merge", "a": grants(&out), "b": grants(v)}))["values"][&id].clone();
    }
    out
}

fn cases() -> Vec<Case> {
    let mut all = open_cases();
    all.extend(other_cases());
    all
}

fn check(case: &Case) {
    let response = call(&case.request);
    assert_eq!(response["version"], 1);
    if let Some(error) = case.expect.get("error") {
        assert_eq!(&response["error"], error, "{}", case.name);
    } else {
        assert_eq!(
            response["ok"], case.expect["ok"],
            "{}: {response}",
            case.name
        );
    }
}

#[test]
fn assistant_cases() {
    let all = cases();
    assert!(all.len() > 50, "{}", all.len());
    for case in &all {
        check(case);
    }
}

/// The grants merge is a join over every ordering and grouping of three versions, malformed ones included.
#[test]
fn grant_merge_is_a_join() {
    let k = keys();
    let id = gid(&k.grant);
    let base = grant_value(&k.grant, "Claude", json!(["seen"]), 3, None);
    let revoked = |at: u64| grant_value(&k.grant, "Claude", json!(["seen"]), 3, Some(at));
    let versions = [
        setting(base.clone(), 5),
        setting(revoked(2000), 7),
        setting(revoked(1500), 6),
        setting(json!({"string": "{}"}), 9),
        setting(json!(null), 8),
        setting(
            grant_value(&k.limited, "ChatGPT", json!(["seen"]), 3, None),
            10,
        ),
    ];
    let grants = |v: &Value| row("assistant-grants", json!({id.clone(): v}));
    let merge = |a: &Value, b: &Value| {
        ok(&json!({"op": "merge", "a": grants(a), "b": grants(b)}))["values"][&id].clone()
    };
    for a in &versions {
        assert_eq!(&merge(a, a), a);
        for b in &versions {
            assert_eq!(merge(a, b), merge(b, a));
            for c in &versions {
                assert_eq!(merge(&merge(a, b), c), merge(a, &merge(b, c)));
            }
        }
    }
}

// ---- den-spec vectors

fn spec_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("DEN_SPEC_DIR") {
        let dir = PathBuf::from(dir).join("vectors");
        return dir.is_dir().then_some(dir);
    }
    let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../den-spec/vectors")
        .canonicalize()
        .ok()?;
    sibling.is_dir().then_some(sibling)
}

const SUB: &str = "8f14e45fceea167a5a36dedd4bea2543";
/// A den-edge refresh token is `<sid>.<secret>`, the secret unpadded base64url of 32 random bytes.
fn refresh_secret() -> Vec<u8> {
    bytes("refresh", 32)
}

/// The keys, one request, one token claim and one wrap, byte for byte: what den-mcp and den-edge build.
fn fixed() -> Value {
    let k = keys();
    let key = |secret: &[u8; 32]| {
        let public = kem_public(secret);
        json!({"seed": hex(secret), "public": b64url(&public), "kid": key_id(&public)})
    };
    let grant = |secret: &[u8; 32]| json!({"seed": hex(secret), "public": b64url(&grant_public(secret)), "id": gid(secret)});
    let f = fields(
        &k.grant,
        "fixed",
        NOW - MIN,
        "seen",
        json!({"title": series(), "season": 1, "episode": 2,
        "value": true}),
    );
    let message = serde_json::to_vec(&f).unwrap();
    let sealed = seal(&k.grant, &f, &k.dropbox, "fixed");
    let grant_key = GrantKey::from_secret(&k.grant);
    let claim = seal_claim(&kem_public(&k.mcp), SUB, &grant_key, &eseed("claim")).unwrap();
    let nonce: [u8; 12] = bytes("nonce", 12).try_into().unwrap();
    let wrapped = wrap(&refresh_secret(), SUB, &grant_key, &nonce);
    json!({
        "library": LIBRARY,
        "now": NOW,
        "keys": {
            "dropbox": key(&k.dropbox),
            "dropbox2": key(&k.dropbox2),
            "stranger": key(&k.stranger),
            "mcp": key(&k.mcp),
            "grant": grant(&k.grant),
            "limited": grant(&k.limited),
            "revoked": grant(&k.revoked),
            "other": grant(&k.other),
        },
        "request": {
            "fields": f,
            "message": String::from_utf8(message.clone()).unwrap(),
            "signed": hex(&[SIGN_CONTEXT, &message].concat()),
            "signature": hex(&ed25519_sign(&k.grant, &message)),
            "eseed": hex(&eseed("fixed")),
            "sealed": sealed,
            "sealedLength": sealed.len(),
        },
        "claim": {
            "sub": SUB,
            "grant": gid(&k.grant),
            "eseed": hex(&eseed("claim")),
            "claim": claim,
            "claimLength": claim.len(),
        },
        "wrap": {
            "session": SUB,
            "refreshToken": format!("{SUB}.{}", b64url(&refresh_secret())),
            "refreshSecret": hex(&refresh_secret()),
            "key": hex(&*wrap_key(&refresh_secret(), SUB)),
            "nonce": hex(&nonce),
            "wrapped": wrapped,
        },
    })
}

fn case_json(case: &Case) -> Value {
    let mut out = json!({"name": case.name, "request": case.request});
    for (k, v) in case.expect.as_object().unwrap() {
        out[k] = v.clone();
    }
    out
}

/// Writes `vectors/assistant-v1.json` into `DEN_SPEC_DIR`, every case checked first.
#[test]
#[ignore]
fn write_assistant_vectors() {
    let dir = spec_dir().expect("DEN_SPEC_DIR");
    let all = cases();
    for case in &all {
        check(case);
    }
    let file = json!({
        "version": 1,
        "spec": "wire/assistant-v1.md",
        "notes": [
            "Generated by den-core's crates/den-sync/tests/assistant.rs (`write_assistant_vectors`), where every case is also asserted.",
            "Every random input is SHA-256 of \"den/assistant/vectors/<label>/<i>\" for i = 0, 1, … concatenated and cut to length: key seeds (32 bytes), request ids (`id/<label>`, 16 bytes), X-Wing eseeds (`eseed/<label>`, 64 bytes), the wrap nonce (`nonce`, 12) and the refresh secret (`refresh`, 32).",
            "fixed: byte-exact outputs for den-mcp and den-edge — keys from seeds, one request (message, signed bytes, signature, sealed), one token claim and one wrap. Base64url is unpadded; hex is lowercase.",
            "cases: one den-core `evaluate` request each, with its exact `ok` or `error`.",
            "Official vectors for the primitives (X-Wing, HPKE-PQ) are checked in den-core's crates/den-assistant/tests/official.rs."
        ],
        "suite": {"mode": 0, "kem": 0x647a, "kdf": 1, "aead": 2,
            "requestInfo": "den/assistant/v1", "tokenInfo": "den/assistant/token/v1",
            "wrapInfo": "den/assistant/wrap/v1", "signContext": "den/assistant/sig/v1\u{0}"},
        "fixed": fixed(),
        "cases": all.iter().map(case_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        dir.join("assistant-v1.json"),
        serde_json::to_string_pretty(&file).unwrap() + "\n",
    )
    .unwrap();
}

#[test]
fn den_spec_assistant_vectors() {
    let Some(dir) = spec_dir() else {
        if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") {
            eprintln!("SKIP: den-spec absent and DEN_SPEC_OPTIONAL=1");
            return;
        }
        panic!("den-spec/vectors not found — check out den-spec beside this repo, set DEN_SPEC_DIR, or set DEN_SPEC_OPTIONAL=1 to skip deliberately.");
    };
    let text = std::fs::read_to_string(dir.join("assistant-v1.json"))
        .expect("den-spec/vectors/assistant-v1.json");
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["fixed"], fixed(), "the fixed outputs moved");
    let spec = file["cases"].as_array().unwrap();
    let ours: Vec<Value> = cases().iter().map(case_json).collect();
    assert_eq!(spec.len(), ours.len());
    for (theirs, mine) in spec.iter().zip(&ours) {
        assert_eq!(theirs, mine, "{}", mine["name"]);
        let response = call(&theirs["request"]);
        match theirs.get("error") {
            Some(error) => assert_eq!(&response["error"], error, "{}", theirs["name"]),
            None => assert_eq!(response["ok"], theirs["ok"], "{}", theirs["name"]),
        }
    }
    // The binding contract carries a sample of the same cases, unchanged.
    let fixture: Value = serde_json::from_str(include_str!("fixtures/policy-v1.json")).unwrap();
    let sample: Vec<&Value> = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| {
            c["request"]["op"]
                .as_str()
                .is_some_and(|op| op.starts_with("assistant_"))
                || c["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("merge: assistant"))
        })
        .collect();
    assert!(
        sample.len() >= 6,
        "{} assistant cases in policy-v1.json",
        sample.len()
    );
    for case in sample {
        assert!(
            spec.contains(case),
            "{} is not a den-spec case",
            case["name"]
        );
    }
}
