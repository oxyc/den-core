//! Library v4 (den-spec `wire/library-v4.md`) through `evaluate`, as both clients call it.
//!
//! Every case here is checked in code, and the same cases are the den-spec vectors (`vectors/library-v4.json`,
//! §15): `write_library_v4_vectors` (ignored) writes them, and `den_spec_library_v4_vectors` replays the file.

use den_sync::evaluate;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

const D: &str = "a1b2c3d4e5f60718";
const E: &str = "0f1e2d3c4b5a6978";
const IMPORT: i64 = 1 << 53;

fn st(t: i64) -> Value {
    json!([t, 0, D])
}

fn call(request: &Value) -> Value {
    serde_json::from_str(&evaluate(&request.to_string())).unwrap()
}

/// A value for an assertion message, cut short: some responses carry hundreds of KiB of padding.
fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 3000 {
        format!("{}…", &text[..text.floor_char_boundary(3000)])
    } else {
        text
    }
}

fn ok(request: &Value) -> Value {
    let response = call(request);
    assert!(
        response.get("error").is_none(),
        "{}\n→ {}",
        short(request),
        short(&response)
    );
    response["ok"].clone()
}

fn with(base: Value, fields: Value) -> Value {
    let mut out = base.as_object().unwrap().clone();
    out.extend(fields.as_object().unwrap().clone());
    Value::Object(out)
}

fn title(media: &str, id: u64, fields: Value) -> Value {
    with(
        json!({"format": 4, "kind": "title", "title": {"type": media, "id": id}}),
        fields,
    )
}

fn season(id: u64, number: u64, fields: Value) -> Value {
    with(
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": id}, "season": number}),
        fields,
    )
}

fn dlv(media: &str, id: u64, number: Option<u64>, entries: Value) -> Value {
    let mut doc = json!({"format": 4, "kind": "delivery", "provider": "simkl", "account": "4812736", "title": {"type": media, "id": id}, "entries": entries});
    if let Some(number) = number {
        doc["season"] = json!(number);
    }
    doc
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn deflate(text: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8];
    out.extend(miniz_oxide::deflate::compress_to_vec(text, 9));
    out
}

fn compressed(doc: &Value) -> String {
    base64(&deflate(doc.to_string().as_bytes()))
}

/// `{"$pad": {"length": n, "seed": s}}` is a string of `n` characters: SplitMix64 from `s`, each output's top six
/// bits indexing the base64url alphabet (incompressible past 6/8). `{"$pad": {"length": n, "char": c}}` repeats `c`.
fn expand(value: &Value) -> Value {
    match value {
        Value::Object(map) if map.len() == 1 && map.contains_key("$pad") => {
            let pad = &map["$pad"];
            let length = pad["length"].as_u64().unwrap() as usize;
            if let Some(c) = pad["char"].as_str() {
                return json!(c.repeat(length));
            }
            const A: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut state = pad["seed"].as_u64().unwrap();
            let text: String = (0..length)
                .map(|_| {
                    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                    let mut z = state;
                    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                    A[((z ^ (z >> 31)) >> 58) as usize] as char
                })
                .collect();
            json!(text)
        }
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), expand(v))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(expand).collect()),
        other => other.clone(),
    }
}

fn pad(length: u64, seed: u64) -> Value {
    json!({"$pad": {"length": length, "seed": seed}})
}

/// Every member of `subset` equals the response's, recursively for objects and element-wise for arrays of the same
/// length.
fn contains(actual: &Value, subset: &Value) -> bool {
    match (actual, subset) {
        (Value::Object(a), Value::Object(s)) => s
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|x| contains(x, v))),
        (Value::Array(a), Value::Array(s)) => {
            a.len() == s.len() && a.iter().zip(s).all(|(x, v)| contains(x, v))
        }
        _ => same(actual, subset),
    }
}

/// Equality on JCS, which is v4's equality: `1` and `1.0` are one number.
fn same(a: &Value, b: &Value) -> bool {
    canonical(a) == canonical(b)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Number(n)
            if n.as_f64()
                .is_some_and(|f| f.fract() == 0.0 && f.abs() < 9.007e15) =>
        {
            json!(n.as_f64().unwrap() as i64)
        }
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

enum Expect {
    Ok(Value),
    Error(&'static str),
    /// Members the result must hold; the rest (sizes, which depend on the compressor) is not pinned.
    Subset(Value),
}

struct Case {
    name: &'static str,
    section: &'static str,
    request: Value,
    expect: Expect,
}

fn case(name: &'static str, section: &'static str, request: Value, expect: Expect) -> Case {
    Case {
        name,
        section,
        request,
        expect,
    }
}

fn check(case: &Case) {
    let response = call(&expand(&case.request));
    match &case.expect {
        Expect::Ok(value) => {
            assert!(
                response.get("ok").is_some_and(|ok| same(ok, value)),
                "{}: expected {value}\n got {}",
                case.name,
                short(&response)
            );
        }
        Expect::Error(error) => {
            assert_eq!(
                response["error"],
                *error,
                "{}: {}",
                case.name,
                short(&response)
            );
        }
        Expect::Subset(subset) => {
            assert!(
                response.get("ok").is_some_and(|ok| contains(ok, subset)),
                "{}: expected ⊇ {}\n got {}",
                case.name,
                short(subset),
                short(&response)
            );
        }
    }
}

fn film() -> Value {
    title(
        "movie",
        550,
        json!({
            "status": {"value": "watched", "at": st(1789000000000)},
            "resume": {"value": 1, "at": st(1789000000000), "viewing": 1, "seconds": 8340},
            "reaction": {"value": "love", "at": [1789000100000i64, 0, E]},
            "deleted": {"value": false, "at": st(1788000000000)},
            "dismissed": {"value": false, "at": st(1788000000000)},
            "addedAt": 1788000000000i64,
            "watchedAt": 1760000000000i64,
            "watch": {"plays": {"-9005499254740992": 1700000000000i64, "0": 1760000000000i64, "1": 1789000000000i64}, "cleared": null}
        }),
    )
}

fn decoded(name: &str, document: &Value) -> Value {
    json!({"status": "document", "name": name, "document": document, "dropped": []})
}

fn unreadable(reason: &str) -> Expect {
    Expect::Ok(json!({"status": "unreadable", "reason": reason}))
}

fn decode(plaintext: String) -> Value {
    json!({"op": "doc_decode", "plaintext": plaintext})
}

/// `depth − 1` nested arrays around a number.
fn nested(depth: usize) -> Value {
    (1..depth).fold(json!(1), |inner, _| json!([inner]))
}

fn encoding_cases() -> Vec<Case> {
    let doc = film();
    let set_row = json!({"kind": "set", "schema": 2, "name": "prefs", "values": {}});
    let mut trailing = deflate(doc.to_string().as_bytes());
    trailing.push(0);
    let mut huge = b"{\"a\":\"".to_vec();
    huge.resize(8 * 1024 * 1024 + 1 - 2, b'x');
    huge.extend_from_slice(b"\"}");
    let mut exact = huge.clone();
    exact.remove(10);
    // The document object is level 1, so `x` holds depth − 1 levels of arrays.
    let deep = |depth: usize| with(doc.clone(), json!({"x": nested(depth)}));
    let lone =
        r#"{"format":4,"kind":"title","title":{"type":"movie","id":550},"x":"\ud800"}"#.to_owned();
    let newer = with(doc.clone(), json!({"format": 5, "future": {"a": 1}}));
    let padded = |length| with(doc.clone(), json!({"x": pad(length, 7)}));
    vec![
        case(
            "compressed plaintext reads",
            "§4 Plaintext",
            decode(compressed(&doc)),
            Expect::Ok(decoded("title:movie:550", &doc)),
        ),
        case(
            "a known-kind document in the JSON form reads",
            "§4 Plaintext",
            decode(base64(doc.to_string().as_bytes())),
            Expect::Ok(decoded("title:movie:550", &doc)),
        ),
        case(
            "a settings row reads as a JSON row",
            "§4 Plaintext",
            decode(base64(set_row.to_string().as_bytes())),
            Expect::Ok(
                json!({"status": "row", "name": "set:prefs", "legacy": false, "row": set_row}),
            ),
        ),
        case(
            "a settings row is never encoded as a document",
            "§4 Plaintext",
            json!({"op": "doc_encode", "document": set_row}),
            Expect::Error("unknown_kind"),
        ),
        case(
            "a v3 row reads as a legacy row, which runs the switch",
            "§3 v2 and v3 rows",
            decode(base64(
                json!({"kind":"rec","schema":2,"title":{"type":"movie","id":550}})
                    .to_string()
                    .as_bytes(),
            )),
            Expect::Subset(json!({"status": "row", "name": "rec:movie:550", "legacy": true})),
        ),
        case(
            "a byte after the final DEFLATE block is unreadable",
            "§4 Plaintext",
            decode(base64(&trailing)),
            unreadable("trailing_bytes"),
        ),
        case(
            "JSON with leading whitespace is unreadable",
            "§4 Plaintext",
            decode(base64(format!(" {doc}").as_bytes())),
            unreadable("leading_whitespace"),
        ),
        case(
            "another first byte is a newer framing",
            "§4 Plaintext",
            decode(base64(&[1, 2, 3])),
            Expect::Ok(json!({"status": "newer", "reason": "framing"})),
        ),
        case(
            "an empty plaintext is unreadable",
            "§4 Bounds",
            decode(String::new()),
            unreadable("empty"),
        ),
        case(
            "inflating past 8 MiB is unreadable",
            "§4 Bounds",
            decode(base64(&deflate(&huge))),
            unreadable("inflate_limit"),
        ),
        case(
            "8 MiB exactly inflates",
            "§4 Bounds",
            decode(base64(&deflate(&exact))),
            Expect::Subset(json!({"status": "row"})),
        ),
        case(
            "depth 32 reads",
            "§4 Bounds",
            decode(compressed(&deep(32))),
            Expect::Subset(json!({"status": "document"})),
        ),
        case(
            "depth 33 is unreadable",
            "§4 Bounds",
            decode(compressed(&deep(33))),
            unreadable("depth"),
        ),
        case(
            "a lone surrogate is unreadable",
            "§4 Bounds",
            decode(base64(lone.as_bytes())),
            unreadable("invalid_json"),
        ),
        case(
            "a name that does not match the row's is unreadable (HMAC mismatch)",
            "§4 Bounds",
            json!({"op": "doc_decode", "plaintext": compressed(&doc), "name": "title:movie:551"}),
            unreadable("identity"),
        ),
        case(
            "format 3 is unreadable",
            "§4 Bounds",
            decode(compressed(&with(doc.clone(), json!({"format": 3})))),
            unreadable("format"),
        ),
        case(
            "no format is unreadable",
            "§4 Bounds",
            decode(compressed(&{
                let mut d = doc.clone();
                d.as_object_mut().unwrap().remove("format");
                d
            })),
            unreadable("format"),
        ),
        case(
            "a non-integer format is unreadable",
            "§4 Bounds",
            decode(compressed(&with(doc.clone(), json!({"format": "4"})))),
            unreadable("format"),
        ),
        case(
            "a wrongly typed identity is unreadable",
            "§4 Bounds",
            decode(compressed(&with(
                doc.clone(),
                json!({"title": {"type": "movie", "id": "550"}}),
            ))),
            unreadable("identity"),
        ),
        case(
            "an unknown kind is a kept row",
            "§3 Rows of other kinds",
            decode(base64(
                json!({"kind": "zzz", "v": 1}).to_string().as_bytes(),
            )),
            Expect::Ok(
                json!({"status": "row", "name": null, "legacy": false, "row": {"kind": "zzz", "v": 1}}),
            ),
        ),
        case(
            "an extra member inside resume is kept and readable",
            "§4 Shape",
            decode(compressed(&with(
                doc.clone(),
                json!({"resume": {"value": 0.5, "at": st(1), "viewing": 0, "extra": [1]}}),
            ))),
            Expect::Subset(
                json!({"status": "document", "dropped": [], "document": {"resume": {"extra": [1]}}}),
            ),
        ),
        case(
            "a format 5 document is read",
            "§4 Newer rows",
            decode(compressed(&newer)),
            Expect::Ok(
                json!({"status": "newer", "reason": "format", "name": "title:movie:550", "document": newer, "dropped": []}),
            ),
        ),
        case(
            "a format 5 document is never written",
            "§4 Newer rows",
            json!({"op": "doc_encode", "document": newer}),
            Expect::Error("newer_format"),
        ),
        case(
            "a format 5 document is never merged",
            "§4 Newer rows",
            json!({"op": "doc_merge", "a": newer, "b": doc}),
            Expect::Error("newer_format"),
        ),
        case(
            "a §8 write over 224 KiB sealed is refused",
            "§4 Size",
            json!({"op": "doc_encode", "document": padded(240_000), "write": true}),
            Expect::Subset(json!({"too_large": true, "reason": "sealed", "cap": 229376})),
        ),
        case(
            "a merge or settle up to 256 KiB is accepted",
            "§4 Size",
            json!({"op": "doc_encode", "document": padded(240_000), "write": false}),
            Expect::Subset(json!({"name": "title:movie:550"})),
        ),
        case(
            "anything over 256 KiB sealed is refused",
            "§4 Size",
            json!({"op": "doc_encode", "document": padded(270_000), "write": false}),
            Expect::Subset(json!({"too_large": true, "reason": "sealed", "cap": 262144})),
        ),
        case(
            "a JCS over 8 MiB is refused",
            "§4 Size",
            json!({"op": "doc_encode", "document": with(doc.clone(), json!({"x": {"$pad": {"length": 8 * 1024 * 1024, "char": "a"}}}))}),
            Expect::Subset(json!({"too_large": true, "reason": "jcs"})),
        ),
        case(
            "doc_encode refuses a malformed part rather than send what does not decode back",
            "§4 Writers check themselves",
            json!({"op": "doc_encode", "document": with(doc.clone(), json!({"status": {"value": 1, "at": st(1)}}))}),
            Expect::Error("self_check:title:movie:550"),
        ),
        case(
            "doc_name of a season delivery document",
            "§3",
            json!({"op": "doc_name", "document": dlv("tv", 1399, Some(1), json!({}))}),
            Expect::Ok(json!("dlv:simkl:4812736:tv:1399:1")),
        ),
        case(
            "doc_name of a title delivery document",
            "§3",
            json!({"op": "doc_name", "document": dlv("movie", 550, None, json!({}))}),
            Expect::Ok(json!("dlv:simkl:4812736:movie:550")),
        ),
        case(
            "doc_name of a season document",
            "§3",
            json!({"op": "doc_name", "document": season(1399, 0, json!({}))}),
            Expect::Ok(json!("season:tv:1399:0")),
        ),
    ]
}

fn malformed_cases() -> Vec<Case> {
    let bad = season(
        1399,
        1,
        json!({"seasonReset": null, "episodes": {
            "1": {"progress": {"value": 1, "at": st(1000), "viewing": 0}, "imported": false, "plays": {"0": "soon"}, "cleared": null},
            "2": {"progress": {"value": 1, "at": st(1000), "viewing": 0}, "imported": false, "plays": {"0": 1000}, "cleared": null}
        }}),
    );
    let mut read = bad.clone();
    read["episodes"]["1"]
        .as_object_mut()
        .unwrap()
        .remove("plays");
    let receipts = dlv(
        "tv",
        1399,
        Some(1),
        json!({"1": ["w", "x"], "2": ["w", 0, 1000, st(1000), [2, 1, D]]}),
    );
    let mut kept = receipts.clone();
    kept["entries"].as_object_mut().unwrap().remove("1");
    let other_account = with(
        dlv("tv", 1399, Some(1), json!({})),
        json!({"account": "99"}),
    );
    vec![
        case(
            "a wrongly typed plays value is dropped and the rest read",
            "§4 Malformed parts",
            decode(compressed(&bad)),
            Expect::Ok(
                json!({"status": "document", "name": "season:tv:1399:1", "document": read, "dropped": [{"part": "episodes.1.plays", "reason": "malformed"}]}),
            ),
        ),
        case(
            "a malformed delivery entry is dropped",
            "§4 Malformed parts",
            decode(compressed(&receipts)),
            Expect::Ok(
                json!({"status": "document", "name": "dlv:simkl:4812736:tv:1399:1", "document": kept, "dropped": [{"part": "entries.1", "reason": "malformed"}]}),
            ),
        ),
        case(
            "its target is decided as having no receipt, and delivery is not paused",
            "§4 Malformed parts",
            json!({"op": "pending_targets_v4", "documents": [read, kept], "deliver": {"provider": "simkl", "account": "4812736", "since": st(500)}, "now": 2000}),
            Expect::Subset(
                json!({"commands": [{"kind": "watched", "key": "1", "baseline": false}], "held": []}),
            ),
        ),
        case(
            "a season document under a title's name is unreadable",
            "§3 Identity",
            json!({"op": "doc_decode", "plaintext": compressed(&read), "name": "title:tv:1399"}),
            unreadable("identity"),
        ),
        case(
            "a delivery document of another account is unreadable",
            "§3 Identity",
            json!({"op": "doc_decode", "plaintext": compressed(&other_account), "name": "dlv:simkl:4812736:tv:1399:1"}),
            unreadable("identity"),
        ),
        case(
            "an account id outside [0-9A-Za-z_-] is unreadable",
            "§3",
            decode(compressed(&with(
                dlv("tv", 1399, Some(1), json!({})),
                json!({"account": "48:12"}),
            ))),
            unreadable("identity"),
        ),
    ]
}

fn state_cases() -> Vec<Case> {
    let watched = json!({"progress": {"value": 1, "at": st(1000), "viewing": 0}, "imported": false, "plays": {"0": 1000}, "cleared": null});
    let reset_title = title("tv", 1399, json!({"episodesReset": st(2000)}));
    let imported =
        json!({"imported": true, "plays": {(3000 - IMPORT).to_string(): 3000}, "cleared": null});
    let hidden = json!({"watched": false, "resume": null, "viewing": 0, "plays": [], "first_play": null, "watched_at": null});
    vec![
        case(
            "a series reset in the title document hides a season's registers",
            "§7",
            json!({"op": "episode_state_v4", "title": reset_title, "season": season(1399, 1, json!({"seasonReset": null, "episodes": {"1": watched}})), "episode": "1", "now": 3000}),
            Expect::Ok(hidden.clone()),
        ),
        case(
            "a season reset hides its registers",
            "§7",
            json!({"op": "episode_state_v4", "title": null, "season": season(1399, 1, json!({"seasonReset": st(2000), "episodes": {"1": watched}})), "episode": 1, "now": 3000}),
            Expect::Ok(hidden),
        ),
        case(
            "an import after a reset stays visible",
            "§7",
            json!({"op": "episode_state_v4", "title": reset_title, "season": season(1399, 1, json!({"episodes": {"1": imported}})), "episode": "1", "now": 4000}),
            Expect::Ok(
                json!({"watched": true, "resume": null, "viewing": 0, "plays": [[3000 - IMPORT, 3000]], "first_play": 3000, "watched_at": 3000}),
            ),
        ),
        case(
            "a future progress stamp derives as timeless",
            "§5",
            json!({"op": "episode_state_v4", "season": season(1399, 1, json!({"episodes": {"1": {"progress": {"value": 1, "at": st(10 * 86_400_000), "viewing": 0}, "imported": false, "plays": {}, "cleared": null}}})), "episode": "1", "now": 1000}),
            Expect::Subset(json!({"watched": false, "viewing": 0})),
        ),
        case(
            "a missing register derives as nothing",
            "§7",
            json!({"op": "episode_state_v4", "season": null, "episode": "7", "now": 1000}),
            Expect::Subset(json!({"watched": false, "plays": []})),
        ),
        case(
            "a film's watched-at before its play lands",
            "§7",
            json!({"op": "film_state_v4", "title": title("movie", 550, json!({"status": {"value": "watched", "at": st(5000)}, "resume": {"value": 1, "at": st(5000), "viewing": 0}})), "now": 6000}),
            Expect::Ok(
                json!({"watched": true, "resume": {"value": 1, "at": st(5000), "viewing": 0}, "viewing": 0, "plays": [], "first_play": null, "watched_at": 5000}),
            ),
        ),
        case(
            "a film's plays come from its watch register",
            "§7",
            json!({"op": "film_state_v4", "title": film(), "now": 1789000000000i64}),
            Expect::Subset(
                json!({"watched": true, "viewing": 1, "watched_at": 1789000000000i64, "first_play": 1700000000000i64}),
            ),
        ),
        case(
            "a title document with no rec fields derives as none",
            "§10 v4 form",
            json!({"op": "film_state_v4", "title": title("movie", 550, json!({"watch": {"plays": {"0": 1000}, "cleared": null}})), "now": 2000}),
            Expect::Subset(json!({"watched": false, "plays": []})),
        ),
        case(
            "title_state reads a far-future stamp as timeless",
            "§5",
            json!({"op": "title_state", "title": title("movie", 550, json!({"status": {"value": "watched", "at": st(10 * 86_400_000)}})), "now": 1000}),
            Expect::Subset(
                json!({"status": {"value": "watched", "at": [0, 0, ""]}, "reaction": null}),
            ),
        ),
    ]
}

fn write(kind: Value, target: Value, title_doc: Value, seasons: Value) -> Value {
    json!({"op": "apply_write", "write": kind, "target": target, "title": title_doc, "seasons": seasons, "now": 1_800_000_000_000i64})
}

fn tv() -> Value {
    json!({"type": "tv", "id": 1399})
}

fn movie() -> Value {
    json!({"type": "movie", "id": 550})
}

fn register(fields: Value) -> Value {
    with(
        json!({"imported": false, "plays": {}, "cleared": null}),
        fields,
    )
}

fn one(episodes: Value) -> Value {
    json!([season(
        1399,
        1,
        json!({"seasonReset": null, "episodes": episodes})
    )])
}

fn docs(docs: Value) -> Expect {
    Expect::Ok(json!({"documents": docs}))
}

fn write_cases() -> Vec<Case> {
    let progress =
        |value: f64, t: i64, viewing: u64| json!({"value": value, "at": st(t), "viewing": viewing});
    let ep = |episodes: Value| season(1399, 1, json!({"seasonReset": null, "episodes": episodes}));
    let finished = register(json!({"progress": progress(1.0, 1000, 0), "plays": {"0": 1000}}));
    let imported_key = |at: i64| (at - IMPORT).to_string();
    let day = 1_699_920_000_000i64;
    let film_watched = title(
        "movie",
        550,
        json!({"status": {"value": "watched", "at": st(1000)}, "resume": progress(1.0, 1000, 0), "watch": {"imported": false, "plays": {"0": 1000}, "cleared": null}}),
    );
    let receipts = json!([dlv(
        "tv",
        1399,
        Some(1),
        json!({
            "1": ["w", 0, 1_700_000_005_000i64, st(1000), [2, 1, D]],
            "2": ["n", -1, 1_600_000_000_000i64, [0, 0, ""], [2, 2, D], [[-1, 1_600_000_000_000i64]]]
        })
    )]);
    vec![
        case(
            "a first progress write creates the season document",
            "§8 Playback",
            write(
                json!({"kind": "progress", "episode": [1, 1], "value": 0.5, "seconds": 600, "at": st(1000)}),
                tv(),
                Value::Null,
                json!([]),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"progress": {"value": 0.5, "at": st(1000), "viewing": 0, "seconds": 600}}))})
            )])),
        ),
        case(
            "reaching 0.95 adds the viewing's play",
            "§8 Playback",
            write(
                json!({"kind": "progress", "episode": [1, 1], "value": 0.96, "at": st(2000)}),
                tv(),
                Value::Null,
                one(json!({"1": register(json!({"progress": progress(0.5, 1000, 0)}))})),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"progress": progress(0.96, 2000, 0), "plays": {"0": 2000}}))})
            )])),
        ),
        case(
            "playing a finished episode starts the next viewing",
            "§8 Rewatch",
            write(
                json!({"kind": "progress", "episode": [1, 1], "value": 0.3, "at": st(3000)}),
                tv(),
                Value::Null,
                one(json!({"1": finished})),
            ),
            docs(json!([ep(
                json!({"1": with(finished.clone(), json!({"progress": progress(0.3, 3000, 1)}))})
            )])),
        ),
        case(
            "playback never writes 0 in a new viewing",
            "§8 Playback",
            write(
                json!({"kind": "progress", "episode": [1, 1], "value": 0, "at": st(3000)}),
                tv(),
                Value::Null,
                one(json!({"1": finished})),
            ),
            docs(json!([])),
        ),
        case(
            "a film finished by playing writes resume and its play in one document",
            "§8 Films",
            write(
                json!({"kind": "progress", "value": 0.97, "at": st(2000)}),
                movie(),
                title("movie", 550, json!({"resume": progress(0.5, 1000, 0)})),
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"resume": progress(0.97, 2000, 0), "status": {"value": "watched", "at": st(2000)}, "watch": {"imported": false, "plays": {"0": 2000}, "cleared": null}})
            )])),
        ),
        case(
            "mark watched writes nothing on an imported or watched episode",
            "§8 Mark watched",
            write(
                json!({"kind": "mark_watched", "episodes": [[1, 1], [1, 2], [1, 3]], "at": st(5000)}),
                tv(),
                Value::Null,
                one(
                    json!({"1": register(json!({"imported": true, "plays": {imported_key(1_000_000): 1_000_000}})), "2": finished}),
                ),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"imported": true, "plays": {imported_key(1_000_000): 1_000_000}})), "2": finished, "3": register(json!({"progress": progress(1.0, 5000, 0), "plays": {"0": 5000}}))})
            )])),
        ),
        case(
            "mark watched after a season reset writes the next viewing with a play",
            "§8 Mark watched",
            write(
                json!({"kind": "mark_watched", "episodes": [[1, 1]], "at": st(5000)}),
                tv(),
                Value::Null,
                json!([season(
                    1399,
                    1,
                    json!({"seasonReset": st(2000), "episodes": {"1": finished}})
                )]),
            ),
            docs(json!([season(
                1399,
                1,
                json!({"seasonReset": st(2000), "episodes": {"1": register(json!({"progress": progress(1.0, 5000, 1), "plays": {"0": 1000, "1": 5000}}))}})
            )])),
        ),
        case(
            "mark watched on a watched film writes nothing",
            "§8 Mark watched",
            write(
                json!({"kind": "mark_watched", "at": st(5000)}),
                movie(),
                film_watched.clone(),
                json!([]),
            ),
            docs(json!([])),
        ),
        case(
            "mark watched on a film writes status, resume and play in one document",
            "§8 Mark watched",
            write(
                json!({"kind": "mark_watched", "at": st(5000)}),
                movie(),
                Value::Null,
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watched", "at": st(5000)}, "resume": progress(1.0, 5000, 0), "watch": {"imported": false, "plays": {"0": 5000}, "cleared": null}})
            )])),
        ),
        case(
            "an un-watched film writes cleared before the bump, in one document",
            "§8 Un-watch",
            write(
                json!({"kind": "unwatch", "at": st(2000)}),
                movie(),
                film_watched.clone(),
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "none", "at": st(2000)}, "resume": progress(0.0, 2000, 1), "watch": {"imported": false, "plays": {"0": 1000}, "cleared": [0, st(2000)]}})
            )])),
        ),
        case(
            "an un-watched episode clears its viewing and starts the next",
            "§8 Un-watch",
            write(
                json!({"kind": "unwatch", "episodes": [[1, 1]], "at": st(2000)}),
                tv(),
                Value::Null,
                one(json!({"1": finished})),
            ),
            docs(json!([ep(
                json!({"1": with(finished.clone(), json!({"progress": progress(0.0, 2000, 1), "cleared": [0, st(2000)]}))})
            )])),
        ),
        case(
            "a season reset",
            "§8",
            write(
                json!({"kind": "season_reset", "season": 1, "at": st(2000)}),
                tv(),
                Value::Null,
                one(json!({"1": finished})),
            ),
            docs(json!([season(
                1399,
                1,
                json!({"seasonReset": st(2000), "episodes": {"1": finished}})
            )])),
        ),
        case(
            "a series reset",
            "§8",
            write(
                json!({"kind": "series_reset", "at": st(2000)}),
                tv(),
                Value::Null,
                json!([]),
            ),
            docs(json!([title(
                "tv",
                1399,
                json!({"episodesReset": st(2000)})
            )])),
        ),
        case(
            "status, list, reaction, delete and dismiss set the title document",
            "§8",
            write(
                json!({"kind": "title", "fields": {"status": "watchlist", "reaction": "like", "deleted": false, "dismissed": true}, "added_at": 2000, "at": st(2000)}),
                movie(),
                Value::Null,
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watchlist", "at": st(2000)}, "reaction": {"value": "like", "at": st(2000)}, "deleted": {"value": false, "at": st(2000)}, "dismissed": {"value": true, "at": st(2000)}, "addedAt": 2000})
            )])),
        ),
        case(
            "a write with a timeless stamp is refused",
            "§8",
            write(
                json!({"kind": "title", "fields": {"status": "watchlist"}, "at": [0, 0, D]}),
                movie(),
                Value::Null,
                json!([]),
            ),
            Expect::Error("timeless_write"),
        ),
        case(
            "a writer id that is not 16 hex is refused",
            "§8",
            write(
                json!({"kind": "series_reset", "at": [1000, 0, "NOT-HEX"]}),
                tv(),
                Value::Null,
                json!([]),
            ),
            Expect::Error("invalid_device"),
        ),
        case(
            "an import with no claim is written",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "plays": [1_700_000_000_123i64]}]}),
                tv(),
                Value::Null,
                json!([]),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"imported": true, "plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}}))})
            )])),
        ),
        case(
            "an import over real progress writes its plays and no imported",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "plays": [1_700_000_000_000i64]}]}),
                tv(),
                Value::Null,
                one(json!({"1": register(json!({"progress": progress(0.5, 1000, 0)}))})),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"progress": progress(0.5, 1000, 0), "plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}}))})
            )])),
        ),
        case(
            "an import under a newer reset writes no imported",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "plays": [1_700_000_000_000i64]}]}),
                tv(),
                title("tv", 1399, json!({"episodesReset": st(1_750_000_000_000)})),
                json!([]),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}}))})
            )])),
        ),
        case(
            "an import never writes progress, status or dismissed",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 2, "plays": []}]}),
                tv(),
                Value::Null,
                json!([]),
            ),
            docs(json!([ep(
                json!({"2": register(json!({"imported": true}))})
            )])),
        ),
        case(
            "a tracker play at a Den play's second is set aside",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "plays": [1_700_000_000_999i64]}]}),
                tv(),
                Value::Null,
                one(
                    json!({"1": register(json!({"progress": progress(1.0, 1_700_000_000_456, 0), "plays": {"0": 1_700_000_000_456i64}}))}),
                ),
            ),
            docs(json!([])),
        ),
        case(
            "a tracker play at the account's receipt watched-at is set aside",
            "§8 Episode import",
            json!({"op": "apply_write", "write": {"kind": "import_episodes", "provider": "simkl", "account": "4812736", "items": [{"season": 1, "episode": 1, "plays": [1_700_000_005_500i64]}]}, "target": tv(), "receipts": receipts, "now": 1_800_000_000_000i64}),
            docs(json!([ep(
                json!({"1": register(json!({"imported": true}))})
            )])),
        ),
        case(
            "a tracker play Den sent as an imported play is set aside",
            "§8 Episode import",
            json!({"op": "apply_write", "write": {"kind": "import_episodes", "provider": "simkl", "account": "4812736", "items": [{"season": 1, "episode": 2, "plays": [1_600_000_000_000i64]}]}, "target": tv(), "receipts": receipts, "now": 1_800_000_000_000i64}),
            docs(json!([ep(
                json!({"2": register(json!({"imported": true}))})
            )])),
        ),
        case(
            "a day-only play is written at the start of its UTC day",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "days": [{"day": day, "noon": day + 43_200_000}]}]}),
                tv(),
                Value::Null,
                json!([]),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"imported": true, "plays": {imported_key(day): day}}))})
            )])),
        ),
        case(
            "a day-only play matching a Den play at local noon is not written",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "days": [{"day": day, "noon": day + 39_600_000}]}]}),
                tv(),
                Value::Null,
                one(json!({"1": register(json!({"plays": {"0": day + 39_600_000}}))})),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"imported": true, "plays": {"0": day + 39_600_000}}))})
            )])),
        ),
        case(
            "a day-only play matching a quarter-hour Den play in range is not written",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "days": [{"day": day, "noon": day + 43_200_000}]}]}),
                tv(),
                Value::Null,
                one(json!({"1": register(json!({"plays": {"0": day + 22 * 3_600_000}}))})),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"imported": true, "plays": {"0": day + 22 * 3_600_000}}))})
            )])),
        ),
        case(
            "each Den play excuses one day-only play: consecutive days keep the other",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "days": [{"day": day + 86_400_000, "noon": day + 129_600_000}, {"day": day, "noon": day + 43_200_000}]}]}),
                tv(),
                Value::Null,
                one(json!({"1": register(json!({"plays": {"0": day + 22 * 3_600_000}}))})),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"imported": true, "plays": {imported_key(day + 86_400_000): day + 86_400_000, "0": day + 22 * 3_600_000}}))})
            )])),
        ),
        case(
            "a tracker play inside a former viewing window is written",
            "§8 Episode import",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "plays": [1_700_003_600_000i64]}]}),
                tv(),
                Value::Null,
                one(
                    json!({"1": register(json!({"progress": progress(1.0, 1_700_000_000_000, 0), "plays": {"0": 1_700_000_000_000i64}}))}),
                ),
            ),
            docs(json!([ep(
                json!({"1": register(json!({"progress": progress(1.0, 1_700_000_000_000, 0), "plays": {imported_key(1_700_003_600_000): 1_700_003_600_000i64, "0": 1_700_000_000_000i64}}))})
            )])),
        ),
        case(
            "a film import writes a timeless status and its plays",
            "§8 Film import",
            write(
                json!({"kind": "import_film", "plays": [1_700_000_000_000i64]}),
                movie(),
                Value::Null,
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watched", "at": [0, 1_700_000_000_000i64, ""]}, "watch": {"imported": false, "plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}, "cleared": null}})
            )])),
        ),
        case(
            "a film import loses to any real status but keeps its plays",
            "§8 Film import",
            write(
                json!({"kind": "import_film", "plays": [1_700_000_000_000i64]}),
                movie(),
                title(
                    "movie",
                    550,
                    json!({"status": {"value": "watchlist", "at": st(1000)}}),
                ),
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watchlist", "at": st(1000)}, "watch": {"imported": false, "plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}, "cleared": null}})
            )])),
        ),
        case(
            "a rating import with no time is c = 1",
            "§8 Title imports",
            write(
                json!({"kind": "import_rating", "reaction": "love", "rated_at": null}),
                movie(),
                Value::Null,
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"reaction": {"value": "love", "at": [0, 1, ""]}})
            )])),
        ),
        case(
            "an import never rewrites an equal imported value",
            "§8 Title imports",
            write(
                json!({"kind": "import_rating", "reaction": "love", "rated_at": 9000}),
                movie(),
                title(
                    "movie",
                    550,
                    json!({"reaction": {"value": "love", "at": [0, 1, ""]}}),
                ),
                json!([]),
            ),
            docs(json!([])),
        ),
        case(
            "a watchlist add import",
            "§8 Title imports",
            write(
                json!({"kind": "import_watchlist_add", "listed_at": 5}),
                movie(),
                Value::Null,
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watchlist", "at": [0, 5, ""]}})
            )])),
        ),
        case(
            "a watchlist removal is stamped above the field's c",
            "§8 Title imports",
            write(
                json!({"kind": "import_watchlist_remove", "previous_listed_at": 3}),
                movie(),
                title(
                    "movie",
                    550,
                    json!({"status": {"value": "watchlist", "at": [0, 5, ""]}}),
                ),
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "none", "at": [0, 6, ""]}})
            )])),
        ),
        case(
            "a watchlist removal of a film with an imported play keeps it watched",
            "§8 Title imports",
            write(
                json!({"kind": "import_watchlist_remove", "previous_listed_at": 9}),
                movie(),
                title(
                    "movie",
                    550,
                    json!({"status": {"value": "watchlist", "at": [0, 5, ""]}, "watch": {"plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}, "cleared": null}}),
                ),
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watched", "at": [0, 10, ""]}, "watch": {"plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}, "cleared": null}})
            )])),
        ),
        // Film import and watchlist add in both orders end at the film import's status, as their merge does
        // (the merge vector "a film import and a watchlist add merge to the film import").
        case(
            "a watchlist add after a film import writes nothing",
            "§8 Title imports",
            write(
                json!({"kind": "import_watchlist_add", "listed_at": 5}),
                movie(),
                title(
                    "movie",
                    550,
                    json!({"status": {"value": "watched", "at": [0, 1_700_000_000_000i64, ""]}}),
                ),
                json!([]),
            ),
            docs(json!([])),
        ),
        case(
            "a film import after a watchlist add takes the status",
            "§8 Title imports",
            write(
                json!({"kind": "import_film", "plays": [1_700_000_000_000i64]}),
                movie(),
                title(
                    "movie",
                    550,
                    json!({"status": {"value": "watchlist", "at": [0, 5, ""]}}),
                ),
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watched", "at": [0, 1_700_000_000_000i64, ""]}, "watch": {"imported": false, "plays": {imported_key(1_700_000_000_000): 1_700_000_000_000i64}, "cleared": null}})
            )])),
        ),
        case(
            "imports skip a deleted title",
            "§8 Title imports",
            write(
                json!({"kind": "import_rating", "reaction": "love", "rated_at": 5}),
                movie(),
                title(
                    "movie",
                    550,
                    json!({"deleted": {"value": true, "at": st(1)}}),
                ),
                json!([]),
            ),
            docs(json!([])),
        ),
    ]
}

fn simkl(since: i64) -> Value {
    json!({"provider": "simkl", "account": "4812736", "since": st(since)})
}

fn pending(documents: Value, deliver: Value, now: i64) -> Value {
    json!({"op": "pending_targets_v4", "documents": documents, "deliver": deliver, "now": now})
}

fn delivery_cases() -> Vec<Case> {
    let watched = title(
        "movie",
        550,
        json!({"status": {"value": "watched", "at": st(1000)}, "resume": {"value": 1, "at": st(1000), "viewing": 0}, "watch": {"plays": {"0": 1000}, "cleared": null}}),
    );
    let unwatched = title(
        "movie",
        550,
        json!({"status": {"value": "none", "at": st(1000)}, "resume": {"value": 0, "at": st(1000), "viewing": 1}, "watch": {"plays": {"0": 900}, "cleared": [0, st(1000)]}}),
    );
    let order = |n: u64| json!([2, n, D]);
    let gone = |id: u64| {
        title(
            "movie",
            id,
            json!({"status": {"value": "watchlist", "at": st(1000)}, "deleted": {"value": true, "at": st(3000)}}),
        )
    };
    let mut many = Vec::new();
    for id in 1..=21u64 {
        many.push(gone(id));
        many.push(dlv(
            "movie",
            id,
            None,
            json!({"list": ["in", st(1000), order(id)]}),
        ));
    }
    let remark = season(
        1399,
        1,
        json!({"episodes": {"1": {"progress": {"value": 1, "at": st(3000), "viewing": 1}, "imported": false, "plays": {"0": 1000, "1": 3000}, "cleared": [0, st(2000)]}}}),
    );
    let big_order = |seed: u64| json!([2, seed, pad(100_000, seed)]);
    let settle = |key: &str, seed: u64| json!({"key": key, "settle": ["w", 0, 1000, st(1000), big_order(seed)]});
    vec![
        case(
            "no receipt: a watched value stamped before since is caught up as baseline",
            "§9 Pending",
            pending(json!([watched]), simkl(2000), 3000),
            Expect::Subset(
                json!({"commands": [{"kind": "watched", "key": "watch", "document": "dlv:simkl:4812736:movie:550", "baseline": true, "p": 0, "watched_at": 1000}]}),
            ),
        ),
        case(
            "no receipt: a watched value stamped after since is pending",
            "§9 Pending",
            pending(json!([watched]), simkl(500), 3000),
            Expect::Subset(json!({"commands": [{"kind": "watched", "baseline": false}]})),
        ),
        case(
            "no receipt: an unwatched value before since settles silently",
            "§9 Pending",
            pending(json!([unwatched]), simkl(2000), 3000),
            Expect::Ok(json!({"commands": [], "settle": [
            {"document": "dlv:simkl:4812736:movie:550", "key": "list", "built_from": {"key": "list", "kind": "list", "value": "out", "stamp": st(1000)}},
            {"document": "dlv:simkl:4812736:movie:550", "key": "rating", "built_from": {"key": "rating", "kind": "rating", "value": null, "stamp": [0, 0, ""]}},
            {"document": "dlv:simkl:4812736:movie:550", "key": "watch", "built_from": {"key": "watch", "kind": "film", "value": "unwatched", "stamp": st(1000), "p": 1}}
        ], "held": [], "removals": null, "greatest_epoch": 0, "unverified": []})),
        ),
        case(
            "no receipt: an unwatched value after since is sent",
            "§9 Pending",
            pending(json!([unwatched]), simkl(500), 3000),
            Expect::Subset(
                json!({"commands": [{"kind": "unwatched", "key": "watch", "baseline": false}]}),
            ),
        ),
        case(
            "a receipt equal to the value: nothing pending",
            "§9 Pending",
            pending(
                json!([
                    watched,
                    dlv(
                        "movie",
                        550,
                        None,
                        json!({"watch": ["w", 0, 1000, st(1000), order(1)], "list": ["out", st(1000), order(2)], "rating": [null, [0, 0, ""], order(3)]})
                    )
                ]),
                simkl(500),
                3000,
            ),
            Expect::Subset(json!({"commands": [], "settle": [], "greatest_epoch": 2})),
        ),
        case(
            "a regression against its receipt is not pending",
            "§9 Pending",
            pending(
                json!([
                    watched,
                    dlv(
                        "movie",
                        550,
                        None,
                        json!({"watch": ["w", 1, 5000, st(5000), order(1)], "list": ["out", st(1000), order(2)], "rating": [null, [0, 0, ""], order(3)]})
                    )
                ]),
                simkl(500),
                3000,
            ),
            Expect::Subset(json!({"commands": [], "settle": []})),
        ),
        case(
            "a format 5 delivery document holds its targets",
            "§4 Newer rows",
            pending(
                json!([
                    watched,
                    with(dlv("movie", 550, None, json!({})), json!({"format": 5}))
                ]),
                simkl(500),
                3000,
            ),
            Expect::Subset(
                json!({"commands": [], "held": [{"key": "list", "reason": "newer_format"}, {"key": "rating", "reason": "newer_format"}, {"key": "watch", "reason": "newer_format"}]}),
            ),
        ),
        case(
            "more than 20 pending removals latch",
            "v3 §6 removals",
            pending(Value::Array(many), simkl(500), 4000),
            Expect::Subset(
                json!({"removals": "held", "commands": vec![json!({"kind": "list", "added": false, "removals_held": true}); 21]}),
            ),
        ),
        case(
            "an equal value at an unverified epoch is decided against the snapshot",
            "v3 §6 Unverified receipts",
            pending(
                json!([
                    watched,
                    dlv(
                        "movie",
                        550,
                        None,
                        json!({"watch": ["w", 0, 1000, st(1000), [3, 1, D]]})
                    )
                ]),
                with(simkl(500), json!({"unverified": [3]})),
                3000,
            ),
            Expect::Subset(
                json!({"commands": [{"kind": "watched", "key": "watch", "unverified": true, "baseline": true}]}),
            ),
        ),
        case(
            "an epoch two devices settled under becomes unverified, and a listed epoch no receipt holds is dropped",
            "v3 §6 Unverified receipts",
            pending(
                json!([
                    title("movie", 550, json!({"status": {"value": "watchlist", "at": st(1000)}})),
                    title("movie", 551, json!({"status": {"value": "watchlist", "at": st(1000)}})),
                    dlv("movie", 550, None, json!({"list": ["in", st(1000), [3, 1, "aaaaaaaaaaaaaaaa"]]})),
                    dlv("movie", 551, None, json!({"list": ["in", st(1000), [3, 1, "bbbbbbbbbbbbbbbb"]]}))
                ]),
                with(simkl(500), json!({"unverified": [5]})),
                4000,
            ),
            Expect::Subset(json!({
                "unverified": [3],
                "commands": [
                    {"kind": "list", "added": true, "document": "dlv:simkl:4812736:movie:550", "unverified": true},
                    {"kind": "list", "added": true, "document": "dlv:simkl:4812736:movie:551", "unverified": true}
                ]
            })),
        ),
        case(
            "a compaction removes one unreadable row, whatever the library's size",
            "§4 Unreadable rows",
            json!({"op": "compaction_guard", "unreadable": 1, "rows": 3}),
            Expect::Ok(json!({"compact": true})),
        ),
        case(
            "a compaction removes a few unreadable rows of a large library",
            "§4 Unreadable rows",
            json!({"op": "compaction_guard", "unreadable": 10, "rows": 100}),
            Expect::Ok(json!({"compact": true})),
        ),
        case(
            "a compaction refuses more than ten unreadable rows",
            "§4 Unreadable rows",
            json!({"op": "compaction_guard", "unreadable": 11, "rows": 100000}),
            Expect::Ok(json!({"compact": false, "reason": "too_many_rows"})),
        ),
        case(
            "a compaction refuses unreadable rows past a tenth of the log",
            "§4 Unreadable rows",
            json!({"op": "compaction_guard", "unreadable": 2, "rows": 19}),
            Expect::Ok(json!({"compact": false, "reason": "too_large_a_share"})),
        ),
        case(
            "receipts at one epoch from one device stay verified",
            "v3 §6 Unverified receipts",
            pending(
                json!([
                    title("movie", 550, json!({"status": {"value": "watchlist", "at": st(1000)}})),
                    dlv("movie", 550, None, json!({"list": ["in", st(1000), [3, 1, D]], "rating": [null, [0, 0, ""], [3, 2, D]]}))
                ]),
                simkl(500),
                4000,
            ),
            Expect::Subset(json!({"unverified": [], "commands": []})),
        ),
        case(
            "a re-mark after a delivered un-watch sends the un-watch first",
            "v3 §6 Un-watch then re-mark",
            pending(
                json!([
                    remark,
                    dlv(
                        "tv",
                        1399,
                        Some(1),
                        json!({"1": ["w", 0, 1000, st(1000), order(1)]})
                    )
                ]),
                simkl(500),
                4000,
            ),
            Expect::Subset(
                json!({"commands": [{"kind": "unwatched", "key": "1", "at": 2000, "p": 1, "step": "unwatch_then_remark"}]}),
            ),
        ),
        case(
            "settle keeps a [-1, I] element and drops the intent",
            "v3 §6 Intent",
            json!({"op": "settle_v4", "outcome": {"action": "send"}, "built_from": {"key": "1", "kind": "episode", "value": "watched", "stamp": st(3000), "p": 1, "watched_at": 3000}, "order": [2, 9, D], "entry": ["w", 0, 1000, st(1000), [2, 8, D], [[-1, 1000], [1, 3000]]]}),
            Expect::Ok(json!(["w", 1, 3000, st(3000), [2, 9, D], [[-1, 1000]]])),
        ),
        case(
            "settle with no entry",
            "v3 §6 Settling",
            json!({"op": "settle_v4", "outcome": {"action": "acknowledge"}, "built_from": {"key": "list", "kind": "list", "value": "in", "stamp": st(1000)}, "order": [2, 1, D], "entry": null}),
            Expect::Ok(json!(["in", st(1000), [2, 1, D]])),
        ),
        case(
            "a timeless baseline rating acknowledged by another bucket settles as the remote's",
            "v3 §6 Timeless values",
            json!({"op": "settle_v4", "outcome": {"action": "acknowledge", "remote_rating": 8}, "built_from": {"key": "rating", "kind": "rating", "value": "like", "stamp": [0, 5, ""]}, "order": [2, 1, D], "entry": null}),
            Expect::Ok(json!(["love", [0, 5, ""], [2, 1, D], 8])),
        ),
        case(
            "a settle that fits is written",
            "§9 Fit before sending",
            json!({"op": "delivery_write", "identity": {"provider": "simkl", "account": "4812736", "title": tv(), "season": 1}, "commands": [{"key": "1", "settle": ["w", 0, 1000, st(1000), [2, 1, D]]}]}),
            Expect::Ok(
                json!({"document": dlv("tv", 1399, Some(1), json!({"1": ["w", 0, 1000, st(1000), [2, 1, D]]})), "intent_document": null, "accepted": 1, "held": []}),
            ),
        ),
        case(
            "a command whose settle would not fit is held as receipt_full",
            "§9 Fit before sending",
            json!({"op": "delivery_write", "identity": {"provider": "simkl", "account": "4812736", "title": tv(), "season": 1}, "commands": [{"key": "1", "settle": ["w", 0, 1000, st(1000), [2, 1, pad(400_000, 1)]]}]}),
            Expect::Subset(
                json!({"accepted": 0, "held": [{"key": "1", "reason": "receipt_full"}]}),
            ),
        ),
        case(
            "settles that fit alone but not together hold from the first that does not fit",
            "§9 Fit before sending",
            json!({"op": "delivery_write", "identity": {"provider": "simkl", "account": "4812736", "title": tv(), "season": 1}, "commands": [settle("1", 11), settle("2", 12), settle("3", 13), settle("4", 14)]}),
            Expect::Subset(
                json!({"accepted": 2, "held": [{"key": "3", "reason": "receipt_full"}, {"key": "4", "reason": "receipt_full"}]}),
            ),
        ),
        case(
            "an intent then its settle",
            "v3 §6 Intent",
            json!({"op": "delivery_write", "document": dlv("tv", 1399, Some(1), json!({"1": ["w", 0, 1000, st(1000), [2, 1, D], [[-1, 900]]]})), "commands": [{"key": "1", "intent": {"sending": [[1, 3000]], "order": [2, 2, D]}, "settle": ["w", 1, 3000, st(3000), [2, 3, D], [[-1, 900]]]}]}),
            Expect::Subset(
                json!({"accepted": 1, "intent_document": {"entries": {"1": ["w", 0, 1000, st(1000), [2, 2, D], [[1, 3000], [-1, 900]]]}}, "document": {"entries": {"1": ["w", 1, 3000, st(3000), [2, 3, D], [[-1, 900]]]}}}),
            ),
        ),
        case(
            "decide is v3's",
            "v3 §6",
            json!({"op": "decide", "command": {"kind": "watched", "at": 1000, "current": true, "baseline": true, "episode": false, "added": false, "rating": null}, "remote": {"authoritative": true, "account_matches": true, "simkl": true, "watched": {"at": 900}, "listed": null, "rated": null, "any_title_watch": true, "unknown_or_newer_title_watch": false, "episodes_complete": true}}),
            Expect::Ok(json!({"action": "acknowledge", "reason": "already_seen"})),
        ),
    ]
}

fn write_back_cases() -> Vec<Case> {
    let log_doc = title(
        "movie",
        550,
        json!({"status": {"value": "watchlist", "at": st(1000)}}),
    );
    let held_doc = title(
        "movie",
        550,
        json!({"reaction": {"value": "love", "at": st(2000)}}),
    );
    let merged = title(
        "movie",
        550,
        json!({"status": {"value": "watchlist", "at": st(1000)}, "reaction": {"value": "love", "at": st(2000)}}),
    );
    let rec = json!({"kind": "rec", "schema": 2, "title": movie()});
    let log_dlv = dlv(
        "tv",
        1399,
        Some(1),
        json!({"1": ["w", 0, 1000, st(1000), [2, 1, D]]}),
    );
    let held_dlv = dlv(
        "tv",
        1399,
        Some(1),
        json!({"1": ["u", 1, null, st(3000), [3, 1, D]]}),
    );
    let big = with(
        held_doc.clone(),
        json!({"status": {"value": "watched", "at": st(5000)}, "x": pad(300_000, 3)}),
    );
    vec![
        case(
            "a held document equal to the log's is not written",
            "§11 Write-back",
            json!({"op": "write_back_v4", "documents": [log_doc], "kept": [], "log": [log_doc], "now": 3000}),
            Expect::Ok(json!({"writes": [], "dropped": [], "discarded": 0})),
        ),
        case(
            "a held document is merged with the log's and written",
            "§11 Write-back",
            json!({"op": "write_back_v4", "documents": [held_doc, rec], "kept": [], "log": [log_doc], "now": 3000}),
            Expect::Ok(
                json!({"writes": [{"name": "title:movie:550", "document": merged}], "dropped": [], "discarded": 1}),
            ),
        ),
        case(
            "kept ops are re-applied on the new log",
            "§11 Write-back",
            json!({"op": "write_back_v4", "documents": [], "kept": [{"target": movie(), "write": {"kind": "title", "fields": {"reaction": "like"}, "at": st(2500)}}], "log": [log_doc], "now": 3000}),
            Expect::Ok(
                json!({"writes": [{"name": "title:movie:550", "document": with(log_doc.clone(), json!({"reaction": {"value": "like", "at": st(2500)}}))}], "dropped": [], "discarded": 0}),
            ),
        ),
        case(
            "settled entries merge by settle order",
            "§11 Write-back",
            json!({"op": "write_back_v4", "documents": [held_dlv], "kept": [], "log": [log_dlv], "now": 3000}),
            Expect::Ok(
                json!({"writes": [{"name": "dlv:simkl:4812736:tv:1399:1", "document": held_dlv}], "dropped": [], "discarded": 0}),
            ),
        ),
        case(
            "a merge over 256 KiB leaves the log's version",
            "§11 Write-back",
            json!({"op": "write_back_v4", "documents": [big], "kept": [], "log": [log_doc], "now": 3000}),
            Expect::Ok(
                json!({"writes": [], "dropped": [{"name": "title:movie:550", "reason": "too_large"}], "discarded": 0}),
            ),
        ),
    ]
}

/// The cases of den-core#24, the review of the first implementation: each failed on it.
fn review_cases() -> Vec<Case> {
    let order = |n: u64| json!([2, n, D]);
    let progress =
        |value: f64, t: i64, viewing: u64| json!({"value": value, "at": st(t), "viewing": viewing});
    let film_watched = title(
        "movie",
        550,
        json!({"status": {"value": "watched", "at": st(1000)}, "resume": progress(1.0, 1000, 0), "watch": {"imported": false, "plays": {"0": 1000}, "cleared": null}}),
    );
    // An imported watch, unhidden by a play later than the season reset, replayed to 0.3 in viewing 1.
    let replayed_import = season(
        1399,
        1,
        json!({"seasonReset": st(2000), "episodes": {"1": {"progress": progress(0.3, 4000, 1), "imported": true, "plays": {(3000 - IMPORT).to_string(): 3000}, "cleared": null}}}),
    );
    let hidden_import = season(
        1399,
        1,
        json!({"seasonReset": st(2000), "episodes": {"1": {"imported": true, "plays": {(1000 - IMPORT).to_string(): 1000}, "cleared": null}}}),
    );
    let remarked_film = title(
        "movie",
        550,
        json!({"status": {"value": "watched", "at": st(3000)}, "resume": progress(1.0, 3000, 1), "watch": {"plays": {"0": 1000, "1": 3000}, "cleared": [0, st(2000)]}}),
    );
    let newer_title = with(
        title("movie", 550, json!({"status": "watched"})),
        json!({"format": 5}),
    );
    let other_film = title(
        "movie",
        551,
        json!({"status": {"value": "watched", "at": st(1000)}, "resume": progress(1.0, 1000, 0), "watch": {"plays": {"0": 1000}, "cleared": null}}),
    );
    let remarked_episode = register(
        json!({"progress": progress(1.0, 3000, 1), "plays": {"0": 1000, "1": 3000}, "cleared": [0, st(2000)]}),
    );
    let nine_plays: Map<String, Value> = (0..9).map(|k| (k.to_string(), json!(1000 + k))).collect();
    let eight_plays: Map<String, Value> = [0, 2, 3, 4, 5, 6, 7, 8]
        .iter()
        .map(|k| (k.to_string(), json!(1000 + k)))
        .collect();
    vec![
        case(
            "a timeless rating is not pending against a receipt with its own value stamp",
            "v3 §6 Timeless values",
            pending(
                json!([
                    title("movie", 550, json!({"reaction": {"value": "like", "at": [0, 5, ""]}})),
                    dlv("movie", 550, None, json!({"rating": ["love", [0, 5, ""], order(1), 9], "list": ["out", [0, 0, ""], order(2)]}))
                ]),
                simkl(500),
                3000,
            ),
            Expect::Subset(json!({"commands": [], "settle": []})),
        ),
        case(
            "an in-progress replay of an unhidden imported watch sends no un-watch against its receipt",
            "v3 §6 Targets and values",
            pending(
                json!([
                    replayed_import,
                    dlv("tv", 1399, Some(1), json!({"1": ["w", 0, 3000, [0, 0, ""], order(1)]}))
                ]),
                simkl(500),
                5000,
            ),
            Expect::Subset(json!({"commands": [], "settle": []})),
        ),
        case(
            "an in-progress replay of an unhidden imported watch sends no un-watch with no receipt",
            "v3 §6 Targets and values",
            pending(json!([replayed_import]), simkl(500), 5000),
            Expect::Subset(json!({"commands": []})),
        ),
        case(
            "an imported watch a reset hides is an un-watch",
            "v3 §6 Targets and values",
            pending(json!([hidden_import]), simkl(500), 5000),
            Expect::Subset(
                json!({"commands": [{"kind": "unwatched", "key": "1", "at": 2000, "p": 0}]}),
            ),
        ),
        case(
            "a replayed title write older than the stored field writes nothing",
            "§2 Idempotence",
            write(
                json!({"kind": "title", "fields": {"status": "watchlist"}, "at": st(1000)}),
                movie(),
                title("movie", 550, json!({"status": {"value": "watched", "at": st(2000)}})),
                json!([]),
            ),
            docs(json!([])),
        ),
        case(
            "a title write sets only the fields it is later than",
            "§2 Idempotence",
            write(
                json!({"kind": "title", "fields": {"status": "watchlist", "reaction": "like"}, "at": st(1500)}),
                movie(),
                title("movie", 550, json!({"status": {"value": "watched", "at": st(2000)}})),
                json!([]),
            ),
            docs(json!([title(
                "movie",
                550,
                json!({"status": {"value": "watched", "at": st(2000)}, "reaction": {"value": "like", "at": st(1500)}})
            )])),
        ),
        case(
            "a kept title write older than the log's field writes nothing back",
            "§11 Write-back",
            json!({"op": "write_back_v4", "documents": [], "kept": [{"target": movie(), "write": {"kind": "title", "fields": {"status": "watchlist"}, "at": st(1000)}}], "log": [title("movie", 550, json!({"status": {"value": "watched", "at": st(2000)}}))], "now": 3000}),
            Expect::Ok(json!({"writes": [], "dropped": [], "discarded": 0})),
        ),
        case(
            "a kept write to a newer-format document is held, never written",
            "§4 Newer rows",
            json!({"op": "write_back_v4", "documents": [], "kept": [{"target": movie(), "write": {"kind": "title", "fields": {"reaction": "love"}, "at": st(2500)}}], "log": [with(title("movie", 550, json!({"status": {"value": "watched", "at": st(1000)}})), json!({"format": 5}))], "now": 3000}),
            Expect::Ok(
                json!({"writes": [], "dropped": [{"kept": 0, "name": "title:movie:550", "reason": "newer_format"}], "discarded": 0}),
            ),
        ),
        case(
            "one kept write that cannot apply is dropped and the rest written back",
            "§11 Write-back",
            json!({"op": "write_back_v4", "documents": [], "kept": [{"target": movie(), "write": {"kind": "nonsense", "at": st(2500)}}, {"target": movie(), "write": {"kind": "title", "fields": {"reaction": "like"}, "at": st(2500)}}], "log": [], "now": 3000}),
            Expect::Ok(
                json!({"writes": [{"name": "title:movie:550", "document": title("movie", 550, json!({"reaction": {"value": "like", "at": st(2500)}}))}], "dropped": [{"kept": 0, "name": "title:movie:550", "reason": "invalid_write"}], "discarded": 0}),
            ),
        ),
        case(
            "playing a watched film starts one new viewing, in progress",
            "§8 Films",
            write(
                json!({"kind": "progress", "value": 0.3, "at": st(2000)}),
                movie(),
                film_watched.clone(),
                json!([]),
            ),
            docs(json!([with(
                film_watched.clone(),
                json!({"status": {"value": "inProgress", "at": st(2000)}, "resume": progress(0.3, 2000, 1)})
            )])),
        ),
        case(
            "the next tick stays in that viewing",
            "§8 Films",
            write(
                json!({"kind": "progress", "value": 0.4, "at": st(3000)}),
                movie(),
                with(
                    film_watched.clone(),
                    json!({"status": {"value": "inProgress", "at": st(2000)}, "resume": progress(0.3, 2000, 1)}),
                ),
                json!([]),
            ),
            docs(json!([with(
                film_watched.clone(),
                json!({"status": {"value": "inProgress", "at": st(2000)}, "resume": progress(0.4, 3000, 1)})
            )])),
        ),
        case(
            "a film's status is decided by the op, never sent with progress",
            "§8 Films",
            write(
                json!({"kind": "progress", "value": 0.3, "at": st(2000), "status": "inProgress"}),
                movie(),
                Value::Null,
                json!([]),
            ),
            Expect::Error("invalid_write:status"),
        ),
        case(
            "a reshaped format 5 title holds its own targets and no other",
            "§4 Newer rows",
            pending(json!([newer_title, other_film]), simkl(500), 3000),
            Expect::Subset(json!({
                "commands": [{"kind": "watched", "document": "dlv:simkl:4812736:movie:551"}],
                "held": [{"document": "dlv:simkl:4812736:movie:550", "key": "list", "reason": "newer_format"}, {"document": "dlv:simkl:4812736:movie:550", "key": "rating", "reason": "newer_format"}, {"document": "dlv:simkl:4812736:movie:550", "key": "watch", "reason": "newer_format"}]
            })),
        ),
        case(
            "a film re-marked after a delivered un-watch sends the un-watch first",
            "v3 §6 Un-watch then re-mark",
            pending(
                json!([
                    remarked_film,
                    dlv("movie", 550, None, json!({"watch": ["w", 0, 1000, st(1000), order(1)], "list": ["out", st(3000), order(2)], "rating": [null, [0, 0, ""], order(3)]}))
                ]),
                simkl(500),
                4000,
            ),
            Expect::Subset(
                json!({"commands": [{"kind": "unwatched", "key": "watch", "at": 2000, "p": 1, "step": "unwatch_then_remark"}]}),
            ),
        ),
        case(
            "a list removal with no receipt settles silently",
            "v3 §6 command table",
            pending(
                json!([title("movie", 550, json!({"status": {"value": "watchlist", "at": st(1000)}, "deleted": {"value": true, "at": st(3000)}}))]),
                simkl(500),
                4000,
            ),
            Expect::Ok(json!({"commands": [], "settle": [
                {"document": "dlv:simkl:4812736:movie:550", "key": "list", "built_from": {"key": "list", "kind": "list", "value": "gone", "stamp": st(3000)}}
            ], "held": [], "removals": null, "greatest_epoch": 0, "unverified": []})),
        ),
        case(
            "episode imports skip a deleted series",
            "v3 §7 Title imports",
            write(
                json!({"kind": "import_episodes", "items": [{"season": 1, "episode": 1, "plays": [1_700_000_000_000i64]}]}),
                tv(),
                title("tv", 1399, json!({"deleted": {"value": true, "at": st(1)}})),
                json!([]),
            ),
            docs(json!([])),
        ),
        case(
            "a replayed un-watch after a re-mark writes nothing",
            "§2 Idempotence",
            write(
                json!({"kind": "unwatch", "episodes": [[1, 1]], "at": st(2000)}),
                tv(),
                Value::Null,
                one(json!({"1": remarked_episode})),
            ),
            docs(json!([])),
        ),
        case(
            "un-watching an in-progress episode clears its resume point",
            "v3 §7 Un-watch",
            write(
                json!({"kind": "unwatch", "episodes": [[1, 1]], "at": st(2000)}),
                tv(),
                Value::Null,
                one(json!({"1": register(json!({"progress": progress(0.5, 1000, 0)}))})),
            ),
            docs(json!([season(
                1399,
                1,
                json!({"seasonReset": null, "episodes": {"1": register(json!({"progress": progress(0.0, 2000, 1), "cleared": [0, st(2000)]}))}})
            )])),
        ),
        case(
            "un-watching an episode with nothing to clear writes nothing",
            "v3 §7 Un-watch",
            write(
                json!({"kind": "unwatch", "episodes": [[1, 5]], "at": st(2000)}),
                tv(),
                Value::Null,
                json!([]),
            ),
            docs(json!([])),
        ),
        case(
            "decode keeps the 8-kept selection of plays, so every decoded document merges with itself unchanged",
            "§6",
            decode(compressed(&title(
                "movie",
                550,
                json!({"watch": {"plays": nine_plays, "cleared": null}}),
            ))),
            Expect::Ok(json!({
                "status": "document",
                "name": "title:movie:550",
                "document": title("movie", 550, json!({"watch": {"plays": eight_plays, "cleared": null}})),
                "dropped": [{"part": "watch.plays", "reason": "selection"}]
            })),
        ),
        case(
            "a format 5 document reads with its reason",
            "§4 Newer rows",
            decode(compressed(&with(film(), json!({"format": 5})))),
            Expect::Subset(json!({"status": "newer", "reason": "format"})),
        ),
    ]
}

fn cases() -> Vec<Case> {
    [
        encoding_cases(),
        malformed_cases(),
        state_cases(),
        write_cases(),
        delivery_cases(),
        write_back_cases(),
        review_cases(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

struct Merge {
    name: &'static str,
    a: Value,
    b: Value,
    c: Option<Value>,
    merged: Value,
}

fn merges() -> Vec<Merge> {
    let resume = |viewing: u64, t: i64| json!({"value": 0.5, "at": st(t), "viewing": viewing});
    let status = |t: i64| json!({"value": "watched", "at": st(t)});
    let episode = |fields: Value| register(fields);
    let m = |name, a, b, c, merged| Merge {
        name,
        a,
        b,
        c,
        merged,
    };
    vec![
        m("§6 counterexample: both groupings keep C's unknown set",
            title("movie", 550, json!({"resume": resume(2, 5), "ua": "A"})),
            title("movie", 550, json!({"resume": resume(1, 9), "ub": "B"})),
            Some(title("movie", 550, json!({"status": status(7), "uc": "C"}))),
            title("movie", 550, json!({"resume": resume(2, 5), "status": status(7), "uc": "C"}))),
        m("§6 counterexample, season form through progress.at",
            season(1399, 1, json!({"episodes": {"1": episode(json!({"progress": resume(2, 5)}))}, "ua": "A"})),
            season(1399, 1, json!({"episodes": {"1": episode(json!({"progress": resume(1, 9)}))}, "ub": "B"})),
            Some(season(1399, 1, json!({"seasonReset": st(7), "uc": "C"}))),
            season(1399, 1, json!({"episodes": {"1": episode(json!({"progress": resume(2, 5)}))}, "seasonReset": st(7), "uc": "C"}))),
        m("§6 counterexample, season form through cleared",
            season(1399, 1, json!({"episodes": {"1": episode(json!({"cleared": [2, st(5)]}))}, "ua": "A"})),
            season(1399, 1, json!({"episodes": {"1": episode(json!({"cleared": [1, st(9)]}))}, "ub": "B"})),
            Some(season(1399, 1, json!({"seasonReset": st(7), "uc": "C"}))),
            season(1399, 1, json!({"episodes": {"1": episode(json!({"cleared": [2, st(5)]}))}, "seasonReset": st(7), "uc": "C"}))),
        m("an unstamped version ranks below every stamp (a < b < z gives a)",
            title("movie", 550, json!({"status": status(9), "u": "a"})),
            title("movie", 550, json!({"u": "z"})),
            Some(title("movie", 550, json!({"status": status(5), "u": "b"}))),
            title("movie", 550, json!({"status": status(9), "u": "a"}))),
        m("an empty unknown set meets a non-empty one at equal stamps: the non-empty one",
            title("movie", 550, json!({"status": status(5), "u": "x"})),
            title("movie", 550, json!({"status": status(5)})),
            None,
            title("movie", 550, json!({"status": status(5), "u": "x"}))),
        m("register level: the non-empty unknown set",
            title("movie", 550, json!({"watch": {"plays": {"0": 1000}, "cleared": null, "r": 1}})),
            title("movie", 550, json!({"watch": {"plays": {"0": 900}, "cleared": null}})),
            None,
            title("movie", 550, json!({"watch": {"plays": {"0": 900}, "cleared": null, "r": 1}}))),
        m("invalid episode keys travel with the document-level unknown set",
            season(1399, 1, json!({"seasonReset": st(9), "episodes": {"01": {"a": 1}, "1": episode(json!({}))}})),
            season(1399, 1, json!({"seasonReset": st(5), "episodes": {"100000": {"b": 1}}})),
            None,
            season(1399, 1, json!({"seasonReset": st(9), "episodes": {"01": {"a": 1}, "1": episode(json!({}))}}))),
        m("delivery entries at settle orders 5, 3 and 7 give 7",
            dlv("tv", 1399, Some(1), json!({"1": ["w", 0, 1000, st(9000), [5, 0, D]]})),
            dlv("tv", 1399, Some(1), json!({"1": ["u", 1, null, st(99999), [3, 0, E]]})),
            Some(dlv("tv", 1399, Some(1), json!({"1": ["w", 2, 3000, st(1), [7, 0, D]]}))),
            dlv("tv", 1399, Some(1), json!({"1": ["w", 2, 3000, st(1), [7, 0, D]]}))),
        m("an extra member inside resume travels with the winning resume",
            title("movie", 550, json!({"resume": with(resume(1, 5), json!({"extra": 1}))})),
            title("movie", 550, json!({"resume": resume(0, 9)})),
            None,
            title("movie", 550, json!({"resume": with(resume(1, 5), json!({"extra": 1}))}))),
        m("the same imported play from two sources is one key; the least key and the 7 greatest are kept",
            title("movie", 550, json!({"watch": {"plays": {(1_700_000_000_000 - IMPORT).to_string(): 1_700_000_000_000i64, "0": 1, "1": 2, "2": 3, "3": 4}}})),
            title("movie", 550, json!({"watch": {"plays": {(1_700_000_000_000 - IMPORT).to_string(): 1_700_000_000_000i64, "4": 5, "5": 6, "6": 7, "7": 8}}})),
            None,
            title("movie", 550, json!({"watch": {"plays": {(1_700_000_000_000 - IMPORT).to_string(): 1_700_000_000_000i64, "1": 2, "2": 3, "3": 4, "4": 5, "5": 6, "6": 7, "7": 8}}}))),
        m("a film import and a watchlist add merge to the film import",
            title("movie", 550, json!({"status": {"value": "watched", "at": [0, 1_700_000_000_000i64, ""]}})),
            title("movie", 550, json!({"status": {"value": "watchlist", "at": [0, 5, ""]}})),
            None,
            title("movie", 550, json!({"status": {"value": "watched", "at": [0, 1_700_000_000_000i64, ""]}}))),
        m("a field only one version has is kept; addedAt the minimum, watchedAt the least non-null",
            title("movie", 550, json!({"addedAt": 300, "watchedAt": null, "episodesReset": null})),
            title("movie", 550, json!({"addedAt": 100, "watchedAt": 500, "dismissed": {"value": true, "at": st(1)}})),
            Some(title("movie", 550, json!({"watchedAt": 400, "episodesReset": st(3)}))),
            title("movie", 550, json!({"addedAt": 100, "watchedAt": 400, "episodesReset": st(3), "dismissed": {"value": true, "at": st(1)}}))),
    ]
}

fn check_merge(merge: &Merge) {
    let pair = |a: &Value, b: &Value| ok(&json!({"op": "doc_merge", "a": a, "b": b}));
    let mut versions = vec![merge.a.clone(), merge.b.clone()];
    versions.extend(merge.c.clone());
    let n = versions.len();
    for i in 0..n {
        assert!(
            same(&pair(&versions[i], &versions[i]), &versions[i]),
            "{} idempotent",
            merge.name
        );
        for j in 0..n {
            if i == j {
                continue;
            }
            if n == 2 {
                assert!(
                    same(&pair(&versions[i], &versions[j]), &merge.merged),
                    "{}\n got {}",
                    merge.name,
                    pair(&versions[i], &versions[j])
                );
                continue;
            }
            let k = 3 - i - j;
            let left = pair(&pair(&versions[i], &versions[j]), &versions[k]);
            let right = pair(&versions[i], &pair(&versions[j], &versions[k]));
            assert!(
                same(&left, &merge.merged),
                "{} (({i}{j}){k})\n got {left}",
                merge.name
            );
            assert!(
                same(&right, &merge.merged),
                "{} ({i}({j}{k}))\n got {right}",
                merge.name
            );
        }
    }
    assert!(
        same(&pair(&merge.merged, &merge.merged), &merge.merged),
        "{} idempotent",
        merge.name
    );
}

/// A v3 log through `base`: the switch's corpus (§15 *Switch*).
fn v3_log() -> Vec<Value> {
    let rec_movie = json!({"kind": "rec", "schema": 2, "title": movie(),
        "status": {"value": "watched", "at": st(5000)}, "resume": {"value": 1, "viewing": 0, "at": st(5000)},
        "reaction": {"value": "love", "at": st(5000)}, "deleted": {"value": false, "at": st(1000)},
        "dismissed": {"value": false, "at": st(1000)}, "episodesReset": null, "addedAt": 1000, "watchedAt": 5000, "extra": 1});
    let rec_tv = json!({"kind": "rec", "schema": 2, "title": tv(),
        "status": {"value": "watchlist", "at": st(2000)}, "resume": {"value": 0, "viewing": 0, "at": st(2000)},
        "reaction": {"value": null, "at": st(2000)}, "deleted": {"value": false, "at": st(2000)},
        "dismissed": {"value": false, "at": st(2000)}, "episodesReset": null, "addedAt": 2000, "watchedAt": null});
    let reg = |t: i64| json!({"progress": {"value": 1, "at": st(t), "viewing": 0}, "imported": false, "plays": {"0": t}, "cleared": null});
    let wat_film = json!({"kind": "wat", "schema": 3, "title": movie(), "season": 0, "block": 0, "seasonReset": null, "entries": {"0": {"imported": false, "plays": {"0": 5000}, "cleared": null, "later": "x"}, "1": {"imported": false, "plays": {}, "cleared": null}}});
    let wat0 = json!({"kind": "wat", "schema": 3, "title": tv(), "season": 1, "block": 0, "seasonReset": st(500), "entries": {"1": reg(3000), "2": reg(3100), "01": reg(1), "40": reg(1)}, "rowExtra": true});
    let wat1 = json!({"kind": "wat", "schema": 3, "title": tv(), "season": 1, "block": 1, "seasonReset": st(9_000_000), "entries": {"32": reg(3200)}});
    let wat_s2 = json!({"kind": "wat", "schema": 3, "title": tv(), "season": 2, "block": 0, "seasonReset": null, "entries": {"1": reg(6000)}});
    let snt_ep = json!({"kind": "snt", "schema": 3, "provider": "simkl", "account": "4812736", "target": "wat:tv:1399:1:0", "entries": {"1": ["w", 0, 3000, st(3000), [1, 1, D]], "2": ["n", -1, null, [0, 0, ""], [1, 2, D]]}});
    let snt_titles = json!({"kind": "snt", "schema": 3, "provider": "simkl", "account": "4812736", "shard": "3e0", "entries": {
        "rec:movie:550#watch": ["w", 0, 5000, st(5000), [1, 3, D], [[0, null, 4000]]],
        "rec:movie:550#list": ["b", "out", st(1000), [1, 4, D]],
        "bogus": ["out", st(1), [1, 5, D]]
    }});
    let deliver = json!({"kind": "set", "schema": 2, "name": "deliver:simkl:4812736", "values": {
        "since": {"value": {"string": serde_json::to_string(&st(1000)).unwrap()}, "at": st(1000)},
        "lease": {"value": {"strings": ["", "1"]}, "at": st(1000)},
        "seededThrough": {"value": {"int": 6}, "at": st(1000)},
        "seedBound": {"value": {"int": 3150}, "at": st(1000)}
    }});
    let prefs = json!({"kind": "set", "schema": 2, "name": "prefs", "values": {}});
    let ep = json!({"kind": "ep", "schema": 2, "title": tv(), "season": 1, "episode": 3, "progress": {"value": 1.0, "viewing": 0, "at": st(7000)}});
    let event_after = json!({"kind": "ep", "schema": 2, "title": tv(), "season": 1, "episode": 4, "progress": {"value": 1.0, "viewing": 0, "at": st(7100)}});
    let event_before = json!({"kind": "ep", "schema": 2, "title": tv(), "season": 1, "episode": 4, "progress": {"value": 0.0, "viewing": 0, "at": st(100)}});
    let event = ok(
        &json!({"op": "capture", "before": event_before, "after": event_after, "at": st(7100), "id": "e4"}),
    );
    let event_row = json!({"kind": "set", "schema": 2, "name": "tracker-event:e4", "values": {"event": {"value": {"string": event.to_string()}, "at": st(7100)}}});
    let unknown = json!({"kind": "zzz", "schema": 9});
    [
        rec_movie, rec_tv, wat_film, wat0, wat1, snt_ep, snt_titles, deliver, prefs, ep, event_row,
        unknown, wat_s2,
    ]
    .into_iter()
    .collect()
}

fn rec_row(media: &str, id: u64, status: &str, reaction: Value, t: i64, viewing: u64) -> Value {
    json!({"kind": "rec", "schema": 2, "title": {"type": media, "id": id},
        "status": {"value": status, "at": st(t)}, "resume": {"value": 0, "viewing": viewing, "at": st(t)},
        "reaction": {"value": reaction, "at": st(t)}, "deleted": {"value": false, "at": st(1000)},
        "dismissed": {"value": false, "at": st(1000)}, "episodesReset": null, "addedAt": 1000,
        "watchedAt": if status == "watched" { json!(t) } else { Value::Null }})
}

/// A v3 log whose title receipts are in the form both shipped v3 clients write them: an `snt` row with `target`
/// `rec:<type>:<id>` and entries keyed `watch` / `list` / `rating`. The account was seeded through seq 1, so every
/// target here is above `seededThrough`: a receipt the switch lost would be replaced by a seeded default.
fn title_receipt_log() -> Vec<Value> {
    let deliver = json!({"kind": "set", "schema": 2, "name": "deliver:simkl:4812736", "values": {
        "since": {"value": {"string": serde_json::to_string(&st(1000)).unwrap()}, "at": st(1000)},
        "lease": {"value": {"strings": ["", "1"]}, "at": st(1000)},
        "seededThrough": {"value": {"int": 1}, "at": st(1000)}
    }});
    let film_wat = |id: u64, register: Value| json!({"kind": "wat", "schema": 3, "title": {"type": "movie", "id": id}, "season": 0, "block": 0, "seasonReset": null, "entries": {"0": register}});
    let receipt = |target: &str, entries: Value| json!({"kind": "snt", "schema": 3, "provider": "simkl", "account": "4812736", "target": target, "entries": entries});
    vec![
        deliver,
        // Watched and rated, both delivered: nothing is pending. A lost receipt would send both again.
        rec_row("movie", 550, "watched", json!("love"), 5000, 0),
        film_wat(
            550,
            json!({"imported": false, "plays": {"0": 5000}, "cleared": null}),
        ),
        receipt(
            "rec:movie:550",
            json!({
                "watch": ["w", 0, 5000, st(5000), [1, 1, D]],
                "rating": ["love", st(5000), [1, 2, D]]
            }),
        ),
        // Un-watched after its watch was delivered: the un-watch is pending. A lost receipt would hide it.
        rec_row("movie", 551, "none", json!("like"), 6000, 1),
        film_wat(
            551,
            json!({"imported": false, "plays": {"0": 4000}, "cleared": [0, st(6000)]}),
        ),
        receipt(
            "rec:movie:551",
            json!({
                "watch": ["w", 0, 4000, st(4000), [1, 3, D]],
                "rating": ["like", st(6000), [1, 4, D]]
            }),
        ),
        // On the watchlist (delivered) and unrated since its rating was delivered: the rating removal is pending.
        // A lost receipt would hide the removal and send the list add again.
        rec_row("tv", 1399, "watchlist", Value::Null, 6000, 0),
        receipt(
            "rec:tv:1399",
            json!({
                "list": ["in", st(6000), [1, 5, D]],
                "rating": ["like", st(2000), [1, 6, D]],
                "watch": ["w", 0, 2000, st(2000), [1, 7, D]]
            }),
        ),
    ]
}

fn rows(log: &[Value]) -> Value {
    Value::Array(
        log.iter()
            .enumerate()
            .map(
                |(i, row)| json!({"k": format!("k{i:02}"), "seq": i + 1, "bytes": 200, "row": row}),
            )
            .collect(),
    )
}

struct Switch {
    name: &'static str,
    rows: Value,
    tamper: Option<(String, Value)>,
    form: Expect,
    dry_run: Option<Value>,
}

fn v4_form(rows: &Value) -> Value {
    call(&expand(
        &json!({"op": "v4_form", "rows": rows, "base": 20, "performer": D, "now": 10_000}),
    ))
}

fn switches() -> Vec<Switch> {
    let log = v3_log();
    let mut with_doc = log.clone();
    with_doc.push(title(
        "movie",
        550,
        json!({"reaction": {"value": "like", "at": st(8000)}}),
    ));
    let pre_v3 = vec![log[9].clone(), log[10].clone(), log[0].clone()];
    let snt_only = vec![log[0].clone(), log[7].clone()];
    let mut newer = log.clone();
    newer[3]["schema"] = json!(4);
    // Three registers of block 1 carrying ~100 KB of register-level unknowns each: one season document over 224 KiB.
    let mut too_large = log.clone();
    for (i, key) in ["33", "34", "35"].iter().enumerate() {
        too_large[4]["entries"][*key] = json!({"imported": false, "plays": {}, "cleared": null, "u": pad(100_000, i as u64 + 40)});
    }
    // The series' rec row written after seededThrough: its rating target is seeded as `null`, which shipped v3
    // (reading no seededThrough) instead sends as a removal. A pending-command difference, counted, not an abort.
    let mut late_rec = log.clone();
    let rec_tv = late_rec.remove(1);
    late_rec.push(rec_tv);
    let seeded_null = json!({"pass": true});
    // A register v3 cannot derive (a malformed play key), which v4_form reads with that `plays` dropped.
    let mut malformed = log.clone();
    malformed[3]["entries"]["2"]["plays"] = json!({"0": 3100, "x": 5});
    let title_receipts = title_receipt_log();
    let mut unplaceable = title_receipts.clone();
    unplaceable.push(json!({"kind": "snt", "schema": 3, "provider": "simkl", "account": "4812736", "target": "rec:person:5", "entries": {"list": ["in", st(2000), [1, 9, D]]}}));
    vec![
        Switch { name: "title receipts with a rec: target convert into the title's delivery document and the dry run passes", rows: rows(&title_receipts), tamper: None, form: Expect::Subset(json!({"counts": {"invalid_keys": 1}})), dry_run: Some(json!({"pass": true, "pending_differences": []})) },
        Switch { name: "a receipt row the switch cannot place aborts the dry run", rows: rows(&unplaceable), tamper: None, form: Expect::Subset(json!({"counts": {"receipt_dropped": 1}})), dry_run: Some(json!({"pass": false, "abort": [{"reason": "receipt_dropped", "row": "snt:simkl:4812736:rec:person:5"}]})) },
        Switch { name: "a delivered receipt the form lost aborts the dry run", rows: rows(&title_receipts), tamper: Some(("dlv:simkl:4812736:movie:551".into(), json!({"entries": {"watch": ["n", 0, null, [0, 0, ""], [0, 1, D]], "rating": ["like", st(6000), [1, 4, D]], "list": ["out", [0, 0, ""], [0, 2, D]]}}))), form: Expect::Subset(json!({})), dry_run: Some(json!({"pass": false, "abort": [{"reason": "receipt_dropped", "name": "dlv:simkl:4812736:movie:551", "key": "watch"}]})) },
        Switch { name: "a register v3 cannot derive is compared with its malformed member dropped on both sides", rows: rows(&malformed), tamper: None, form: Expect::Subset(json!({"rows": 10})), dry_run: Some(json!({"pass": true, "counts": {"malformed_reference": 1}})) },
        Switch { name: "a v3 corpus converts and passes the dry run", rows: rows(&log), tamper: None, form: Expect::Subset(json!({
            "keep": ["k07", "k08", "k11"],
            "counts": {"events_folded": 1, "film_wat_keys": 1, "invalid_keys": 3, "row_unknown_fields": 2},
            "rows": 10
        })), dry_run: Some(seeded_null) },
        Switch { name: "a no-receipt null rating above seededThrough is a counted pending difference and the switch passes", rows: rows(&late_rec), tamper: None, form: Expect::Subset(json!({"rows": 11})), dry_run: Some(json!({"pass": true, "counts": {"pending_differences": 1}, "pending_differences": [{"account": "simkl:4812736", "key": "tv:1399#rating", "v4": null}]})) },
        Switch { name: "a derived-state difference aborts", rows: rows(&log), tamper: Some(("season:tv:1399:1".into(), json!({"episodes": {"1": {"progress": {"value": 0.5, "at": st(3000), "viewing": 0}, "imported": false, "plays": {}, "cleared": null}}}))), form: Expect::Subset(json!({"rows": 10})), dry_run: Some(json!({"pass": false})) },
        Switch { name: "a log holding documents and v3 rows is merged", rows: rows(&with_doc), tamper: None, form: Expect::Subset(json!({"rows": 10})), dry_run: Some(json!({"pass": true})) },
        Switch { name: "a pre-v3 log is refused", rows: rows(&pre_v3), tamper: None, form: Expect::Error("pre_v3"), dry_run: None },
        Switch { name: "a v3 log with no wat row but a set:deliver row converts", rows: rows(&snt_only), tamper: None, form: Expect::Subset(json!({"keep": ["k01"]})), dry_run: Some(json!({"pass": true})) },
        Switch { name: "a v3 row above its schema fails", rows: rows(&newer), tamper: None, form: Expect::Error("newer_v3_row"), dry_run: None },
        Switch { name: "a season over 224 KiB fails", rows: rows(&too_large), tamper: None, form: Expect::Error("too_large:season:tv:1399:1"), dry_run: None },
    ]
}

fn run_switch(switch: &Switch) -> (Value, Option<Value>) {
    let response = v4_form(&switch.rows);
    match &switch.form {
        Expect::Error(error) => {
            assert_eq!(
                response["error"],
                *error,
                "{}: {}",
                switch.name,
                short(&response)
            );
            return (response, None);
        }
        Expect::Subset(subset) => assert!(
            contains(&response["ok"], subset),
            "{}: expected ⊇ {subset}\n got {}",
            switch.name,
            short(&response)
        ),
        Expect::Ok(value) => assert!(same(&response["ok"], value), "{}", switch.name),
    }
    let mut form = response["ok"].clone();
    if let Some((name, fields)) = &switch.tamper {
        for doc in form["documents"].as_array_mut().unwrap() {
            if doc["name"] == *name {
                let changed = with(doc["document"].clone(), fields.clone());
                let encoded = ok(&json!({"op": "doc_encode", "document": changed, "write": true}));
                doc["document"] = changed;
                doc["plaintext"] = encoded["plaintext"].clone();
            }
        }
    }
    let dry =
        ok(&json!({"op": "v4_dry_run", "rows": expand(&switch.rows), "form": form, "now": 10_000}));
    if let Some(expect) = &switch.dry_run {
        assert!(
            contains(&dry, expect),
            "{}: expected ⊇ {expect}\n got {dry}",
            switch.name
        );
    }
    (form, Some(dry))
}

#[test]
fn library_v4_cases() {
    for case in cases() {
        check(&case);
    }
}

/// Runs every review case and names each one that fails, rather than stopping at the first.
#[test]
fn review_cases_hold() {
    let failed: Vec<&str> = review_cases()
        .iter()
        .filter(|case| std::panic::catch_unwind(|| check(case)).is_err())
        .map(|case| case.name)
        .collect();
    assert!(failed.is_empty(), "failing: {failed:#?}");
}

/// The v3 names keep exactly v3's shape: a v4 request sent to one is refused, never read as the other version.
#[test]
fn v3_op_names_keep_the_v3_shape() {
    let refused = [
        json!({"op": "episode_state", "season": season(1399, 1, json!({})), "episode": "1", "now": 1}),
        json!({"op": "film_state", "title": film(), "now": 1}),
        json!({"op": "pending_targets", "documents": [], "deliver": simkl(1), "now": 1}),
        json!({"op": "write_back", "documents": [], "kept": [], "log": [], "now": 1}),
    ];
    for request in refused {
        assert_eq!(call(&request)["error"], "invalid_request", "{request}");
    }
    // `settle_v4` with no `entry` (an encoder that omits a nil optional) is still v4, read as "no entry".
    let built = json!({"key": "1", "kind": "episode", "value": "watched", "stamp": st(3000), "p": 1, "watched_at": 3000});
    let v4 = ok(
        &json!({"op": "settle_v4", "outcome": {"action": "send"}, "built_from": built, "order": [2, 9, D]}),
    );
    assert_eq!(v4, json!(["w", 1, 3000, st(3000), [2, 9, D]]));
}

/// v3 §6 `removals`: after an approval, a new batch of more than 20 closes the latch again on that batch alone. The
/// latch is stored beside the approval (`{"approved", "held"}`), so the removals approved earlier still go out.
#[test]
fn a_latch_closed_after_an_approval_holds_only_the_later_removals() {
    let batch = |from: u64, count: u64, at: i64| -> Vec<Value> {
        (from..from + count)
            .flat_map(|id| {
                [
                    title(
                        "movie",
                        id,
                        json!({"status": {"value": "watchlist", "at": st(1000)}, "deleted": {"value": true, "at": st(at)}}),
                    ),
                    dlv("movie", id, None, json!({"list": ["in", st(1000), [2, id, D]]})),
                ]
            })
            .collect()
    };
    let held_ids = |documents: &[Value], removals: Value| -> Vec<u64> {
        let answer = call(&json!({"op": "pending_targets_v4", "documents": documents,
            "deliver": with(simkl(500), json!({"removals": removals})), "now": 8000}))["ok"]
            .clone();
        let mut ids: Vec<u64> = answer["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["removals_held"] == json!(true))
            .map(|c| c["title"].as_str().unwrap()[12..].parse().unwrap())
            .collect();
        ids.sort();
        ids
    };
    // 21 approved at 4000, then 21 more: those close the latch by count and are held; the approved 21 are not.
    let mut documents = batch(1, 21, 3000);
    documents.extend(batch(101, 21, 6000));
    let approved = json!({"approved": st(4000)});
    assert_eq!(
        held_ids(&documents, approved),
        (101..122).collect::<Vec<_>>()
    );
    // The latch then stored beside the approval stays closed on what is left of the later batch, under 20 of them,
    // and still holds none of the approved ones.
    let mut documents = batch(1, 21, 3000);
    documents.extend(batch(101, 5, 6000));
    let closed = json!({"approved": st(4000), "held": st(7000)});
    assert_eq!(held_ids(&documents, closed), (101..106).collect::<Vec<_>>());
    // v3's bare "held", with no approval, holds every removal.
    assert_eq!(held_ids(&documents, json!("held")).len(), 26);
}

/// Blocker 5 of den-core#24: progress on a watched film, with no status from the client, moved to a new viewing on
/// every tick and turned each pass into a rewatch.
#[test]
fn film_playback_ticks_stay_in_one_viewing() {
    let mut film = title(
        "movie",
        550,
        json!({"status": {"value": "watched", "at": st(1000)}, "resume": {"value": 1, "at": st(1000), "viewing": 0}, "watch": {"plays": {"0": 1000}, "cleared": null}}),
    );
    for (value, t) in [(0.3, 2000), (0.4, 3000), (0.5, 4000)] {
        let out = ok(
            &json!({"op": "apply_write", "write": {"kind": "progress", "value": value, "at": st(t)}, "target": movie(), "title": film, "now": 10_000}),
        );
        film = out["documents"][0].clone();
    }
    assert_eq!(film["resume"]["viewing"], 1, "{film}");
    assert_eq!(film["status"]["value"], "inProgress", "{film}");
    let receipt = dlv(
        "movie",
        550,
        None,
        json!({"watch": ["w", 0, 1000, st(1000), [2, 1, D]], "list": ["out", st(1000), [2, 2, D]], "rating": [null, [0, 0, ""], [2, 3, D]]}),
    );
    let pass = ok(&pending(json!([film, receipt]), simkl(500), 10_000));
    assert_eq!(pass["commands"], json!([]), "{pass}");
}

/// serde_json parses about one shortest-form double in ten 1 ULP off without `float_roundtrip`, so a progress
/// fraction moved between the client's document and den-core's.
#[test]
fn progress_fractions_round_trip_exactly() {
    let mut state = 0x5eed_u64;
    let mut checked = 0;
    while checked < 20_000 {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        let x = ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64;
        if !(0.1..0.95).contains(&x) {
            continue;
        }
        let text = format!("{x}");
        let doc = format!(
            r#"{{"format":4,"kind":"title","title":{{"type":"movie","id":550}},"resume":{{"value":{text},"at":[1000,0,"{D}"],"viewing":0}}}}"#
        );
        let merged = evaluate(&format!(r#"{{"op":"doc_merge","a":{doc},"b":{doc}}}"#));
        assert!(
            merged.contains(&format!(r#""value":{text}"#)),
            "{text} → {merged}"
        );
        let written = evaluate(&format!(
            r#"{{"op":"apply_write","write":{{"kind":"progress","value":{text},"at":[2000,0,"{D}"]}},"target":{{"type":"movie","id":550}},"now":3000}}"#
        ));
        assert!(
            written.contains(&format!(r#""value":{text}"#)),
            "{text} → {written}"
        );
        checked += 1;
    }
    let known = evaluate(
        r#"{"op":"doc_merge","a":{"format":4,"kind":"title","title":{"type":"movie","id":550},"resume":{"value":0.9856906946328695,"at":[1,0,""],"viewing":0}},"b":{"format":4,"kind":"title","title":{"type":"movie","id":550}}}"#,
    );
    assert!(known.contains("0.9856906946328695"), "{known}");
}

#[test]
fn library_v4_merges() {
    for merge in merges() {
        check_merge(&merge);
    }
}

#[test]
fn library_v4_round_trips_every_switch_form() {
    for switch in switches() {
        let (form, _) = run_switch(&switch);
        for doc in form["documents"].as_array().into_iter().flatten() {
            let back = ok(
                &json!({"op": "doc_decode", "plaintext": doc["plaintext"], "name": doc["name"]}),
            );
            assert_eq!(back["status"], "document", "{}", doc["name"]);
            assert!(same(&back["document"], &doc["document"]), "{}", doc["name"]);
        }
    }
}

#[test]
fn library_v4_switches() {
    for switch in switches() {
        run_switch(&switch);
    }
}

#[test]
fn switch_details() {
    let form = ok(
        &json!({"op": "v4_form", "rows": rows(&v3_log()), "base": 20, "performer": D, "now": 10_000}),
    );
    let docs: Map<String, Value> = form["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().to_owned(),
                d["document"].clone(),
            )
        })
        .collect();
    // Blocks merged into one season document; seasonReset from block 0 only; the stray ep row and the v1 event
    // folded; invalid keys and the block-condition failure dropped.
    let s1 = &docs["season:tv:1399:1"];
    assert_eq!(s1["seasonReset"], st(500));
    let keys: Vec<&String> = s1["episodes"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["1", "2", "3", "32", "4"]);
    // The film's watch register keeps its register-level unknown; the rec's row-level unknown is dropped.
    let film = &docs["title:movie:550"];
    assert_eq!(film["watch"]["later"], "x");
    assert!(film.get("extra").is_none());
    // Receipts carried as stored; the `n` at −1 above seededThrough (6) is stored at 0; the shard's bogus key dropped.
    let receipts = &docs["dlv:simkl:4812736:tv:1399:1"]["entries"];
    assert_eq!(receipts["1"], json!(["w", 0, 3000, st(3000), [1, 1, D]]));
    assert_eq!(receipts["2"], json!(["n", 0, null, [0, 0, ""], [1, 2, D]]));
    // Block 0 takes the greater seq of its parts (the stray ep row at 10, the event at 11), so its targets with
    // no receipt are seeded: default entries, settle orders counted from 1 per account. Block 1 (seq 5) is not.
    assert_eq!(receipts["3"], json!(["n", 0, null, [0, 0, ""], [0, 1, D]]));
    assert_eq!(receipts["4"], json!(["n", 0, null, [0, 0, ""], [0, 2, D]]));
    assert!(receipts.get("32").is_none());
    let season2 = &docs["dlv:simkl:4812736:tv:1399:2"]["entries"];
    assert_eq!(season2["1"], json!(["n", 0, null, [0, 0, ""], [0, 3, D]]));
    // The film's targets were last written at seq 1 (rec) and 3 (wat), below seededThrough: nothing seeded.
    let film_receipts = &docs["dlv:simkl:4812736:movie:550"]["entries"];
    assert!(film_receipts.get("rating").is_none());
    assert_eq!(film_receipts["list"][0], "b");
    // The series' list and rating targets (rec at seq 2) are not above seededThrough either.
    assert!(docs.get("dlv:simkl:4812736:tv:1399").is_none());
    // No v2 or v3 row survives; settings and unknown kinds are kept by k.
    assert!(docs
        .keys()
        .all(|n| !n.starts_with("rec:") && !n.starts_with("wat:")));

    let dry =
        ok(&json!({"op": "v4_dry_run", "rows": rows(&v3_log()), "form": form, "now": 10_000}));
    assert_eq!(dry["pass"], true, "{dry}");
    assert_eq!(dry["pending_differences"], json!([]), "{dry}");
    // §9 known limit: one entry carries a `[p, null, T]` element.
    assert!(
        dry["counts"]["window_known_limit"].as_u64().unwrap() >= 1,
        "{dry}"
    );
}

/// Title receipts written as both shipped v3 clients write them (`target` `rec:<type>:<id>`) reach the title's
/// delivery document as stored, and v4's pending commands after the switch are the ones v3 had: built here the way
/// the clients build them (their targets from each `rec` row, receipts keyed `<target>#<key>`), not by the switch.
#[test]
fn switch_keeps_title_receipts() {
    let log = title_receipt_log();
    let form = ok(
        &json!({"op": "v4_form", "rows": rows(&log), "base": 20, "performer": D, "now": 10_000}),
    );
    let docs: Map<String, Value> = form["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().to_owned(),
                d["document"].clone(),
            )
        })
        .collect();
    let receipts = |name: &str| docs[&format!("dlv:simkl:4812736:{name}")]["entries"].clone();
    // Carried as stored. The films' list targets had no receipt, so seeding (above seededThrough) adds `out`.
    let seeded_out = |n: u64| json!(["out", [0, 0, ""], [0, n, D]]);
    assert_eq!(
        receipts("movie:550"),
        json!({"watch": ["w", 0, 5000, st(5000), [1, 1, D]], "rating": ["love", st(5000), [1, 2, D]], "list": seeded_out(1)})
    );
    assert_eq!(
        receipts("movie:551"),
        json!({"watch": ["w", 0, 4000, st(4000), [1, 3, D]], "rating": ["like", st(6000), [1, 4, D]], "list": seeded_out(2)})
    );
    // A series has no title watch target, so v3 never read that key: dropped and counted.
    assert_eq!(
        receipts("tv:1399"),
        json!({"list": ["in", st(6000), [1, 5, D]], "rating": ["like", st(2000), [1, 6, D]]})
    );
    assert_eq!(form["counts"]["invalid_keys"], 1, "{}", form["counts"]);
    assert!(form["counts"].get("receipt_dropped").is_none());

    let mut targets = Vec::new();
    let mut v3_receipts = Map::new();
    for row in &log {
        match row["kind"].as_str() {
            Some("rec") => {
                let (media, id) = (row["title"]["type"].as_str().unwrap(), &row["title"]["id"]);
                let target = format!("rec:{media}:{id}");
                let common = json!({"media": media, "id": id});
                if media == "movie" {
                    let watched = if row["status"]["value"] == "watched" {
                        "watched"
                    } else {
                        "unwatched"
                    };
                    targets.push(with(common.clone(), json!({"key": format!("{target}#watch"), "kind": "film",
                        "value": watched, "stamp": row["status"]["at"], "p": row["resume"]["viewing"],
                        "watched_at": row["watchedAt"]})));
                }
                let listed = if row["status"]["value"] == "watchlist" {
                    "in"
                } else {
                    "gone"
                };
                targets.push(with(
                    common.clone(),
                    json!({"key": format!("{target}#list"), "kind": "list",
                    "value": listed, "stamp": row["status"]["at"]}),
                ));
                let reaction = row["reaction"]["value"].as_str().unwrap_or("none");
                targets.push(with(
                    common,
                    json!({"key": format!("{target}#rating"), "kind": "rating",
                    "value": reaction, "stamp": row["reaction"]["at"]}),
                ));
            }
            Some("snt") => {
                for (key, entry) in row["entries"].as_object().unwrap() {
                    v3_receipts.insert(
                        format!("{}#{key}", row["target"].as_str().unwrap()),
                        entry.clone(),
                    );
                }
            }
            _ => {}
        }
    }
    let v3 = ok(
        &json!({"op": "pending_targets", "targets": targets, "receipts": v3_receipts, "since": st(1000), "now": 10_000}),
    );
    let documents: Vec<Value> = docs.values().cloned().collect();
    let v4 = ok(&pending(json!(documents), simkl(1000), 10_000));
    let key = |cmd: &Value, target: String| {
        json!([
            target,
            cmd["kind"],
            cmd["added"],
            cmd["rating"],
            cmd.get("p").cloned().unwrap_or(Value::Null)
        ])
    };
    let mut v3_set: Vec<Value> = v3
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            key(
                c,
                c["key"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("rec:")
                    .to_owned(),
            )
        })
        .collect();
    let mut v4_set: Vec<Value> = v4["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let doc = c["document"]
                .as_str()
                .unwrap()
                .trim_start_matches("dlv:simkl:4812736:");
            key(c, format!("{doc}#{}", c["key"].as_str().unwrap()))
        })
        .collect();
    v3_set.sort_by_key(|v| v.to_string());
    v4_set.sort_by_key(|v| v.to_string());
    assert_eq!(v4_set, v3_set, "v4 {v4}\nv3 {v3}");
    // The pending set this library has: the film's un-watch and the series' rating removal, nothing re-sent.
    let pending: Vec<&str> = v3_set.iter().map(|k| k[0].as_str().unwrap()).collect();
    assert_eq!(pending, ["movie:551#watch", "tv:1399#rating"], "{v3}");
}

#[test]
fn switch_fails_on_rows_and_bytes() {
    let mut log = v3_log();
    log.truncate(8);
    let mut many: Vec<Value> = (0..50_000)
        .map(|i| json!({"k": format!("u{i}"), "seq": i + 100, "bytes": 1, "row": {"kind": "zzz"}}))
        .collect();
    many.extend(rows(&log).as_array().unwrap().clone());
    assert_eq!(
        call(
            &json!({"op": "v4_form", "rows": many, "base": 60_000, "performer": D, "now": 10_000})
        )["error"],
        "too_many_rows"
    );
    assert_eq!(
        call(
            &json!({"op": "v4_form", "rows": rows(&log), "base": 20, "performer": D, "now": 10_000, "stored_cap": 1000})
        )["error"],
        "too_many_bytes"
    );
}

/// The boundary never panics (a WASM panic would take the page with it): every v4 op answers malformed input with
/// a JSON envelope.
#[test]
fn malformed_input_is_an_error_not_a_panic() {
    let junk = [
        Value::Null,
        json!(1),
        json!("x"),
        json!([]),
        json!({}),
        json!({"format": 4, "kind": "season", "title": tv(), "season": 1}),
        json!({"format": 4, "kind": "delivery", "provider": "simkl", "account": "1", "title": tv(), "season": 1}),
        json!({"format": 4, "kind": "title", "title": movie(), "watch": 5, "status": {"value": 1}}),
    ];
    let shapes = [
        json!({"op": "doc_decode", "plaintext": "J"}),
        json!({"op": "doc_encode", "document": "J"}),
        json!({"op": "doc_name", "document": "J"}),
        json!({"op": "doc_merge", "a": "J", "b": "J"}),
        json!({"op": "title_state", "title": "J", "now": 1}),
        json!({"op": "episode_state_v4", "title": "J", "season": "J", "episode": "1", "now": 1}),
        json!({"op": "film_state_v4", "title": "J", "now": 1}),
        json!({"op": "apply_write", "write": "J", "target": "J", "title": "J", "seasons": ["J"], "now": 1}),
        json!({"op": "apply_write", "write": {"kind": "mark_watched", "episodes": [[1, 1]], "at": st(5)}, "target": tv(), "seasons": ["J"], "now": 1}),
        json!({"op": "pending_targets_v4", "documents": ["J"], "deliver": "J", "now": 1}),
        json!({"op": "pending_targets_v4", "documents": ["J"], "deliver": simkl(1), "now": 1}),
        json!({"op": "settle_v4", "outcome": "J", "built_from": "J", "order": "J", "entry": "J"}),
        json!({"op": "delivery_write", "document": "J", "identity": "J", "commands": ["J"]}),
        json!({"op": "v4_form", "rows": [{"k": "a", "seq": 1, "row": "J"}], "base": 1, "performer": D, "now": 1}),
        json!({"op": "v4_dry_run", "rows": [{"row": "J"}], "form": "J", "now": 1}),
        json!({"op": "write_back_v4", "documents": ["J"], "kept": ["J"], "log": ["J"], "now": 1}),
    ];
    /// `shape` with every `"J"` replaced by `value`.
    fn fill(shape: &Value, value: &Value) -> Value {
        match shape {
            Value::String(s) if s == "J" => value.clone(),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), fill(v, value)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, value)).collect()),
            other => other.clone(),
        }
    }
    for shape in &shapes {
        for value in &junk {
            let response = call(&fill(shape, value));
            assert_eq!(response["version"], 1, "{shape} with {value}");
        }
    }
}

// ---- den-spec vectors -------------------------------------------------------------------------------------------

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

fn expect_json(expect: &Expect, result: &Value) -> (&'static str, Value) {
    match expect {
        Expect::Ok(_) => ("ok", result["ok"].clone()),
        Expect::Error(error) => ("error", json!(error)),
        Expect::Subset(subset) => ("ok_subset", subset.clone()),
    }
}

/// Writes `vectors/library-v4.json` into `DEN_SPEC_DIR` from the cases above, each checked first, and the small
/// exact ones into `fixtures/policy-v4.json`, the binding contract the native and WASM builds both replay.
#[test]
#[ignore]
fn write_library_v4_vectors() {
    let dir = spec_dir().expect("DEN_SPEC_DIR");
    let mut plain = Vec::new();
    let mut contract = Vec::new();
    for case in cases() {
        check(&case);
        let result = call(&expand(&case.request));
        let (key, value) = expect_json(&case.expect, &result);
        let text = case.request.to_string();
        if key != "ok_subset" && text.len() < 4000 && !text.contains("$pad") {
            contract.push(json!({"name": case.name, "request": case.request, key: value}));
        }
        plain.push(json!({"name": case.name, "section": case.section, "request": case.request, key: value}));
    }
    let merges: Vec<Value> = merges()
        .into_iter()
        .map(|m| {
            check_merge(&m);
            json!({"name": m.name, "a": m.a, "b": m.b, "c": m.c, "merged": m.merged})
        })
        .collect();
    let switches: Vec<Value> = switches()
        .into_iter()
        .map(|s| {
            run_switch(&s);
            let form = match &s.form {
                Expect::Error(error) => json!({"error": error}),
                Expect::Subset(subset) => json!({"ok_subset": subset}),
                Expect::Ok(value) => json!({"ok": value}),
            };
            json!({"name": s.name, "rows": s.rows, "base": 20, "performer": D, "now": 10_000,
                "form": form, "tamper": s.tamper.as_ref().map(|(n, f)| json!({"name": n, "fields": f})),
                "dry_run": s.dry_run})
        })
        .collect();
    let file = json!({
        "version": 1,
        "spec": "wire/library-v4.md",
        "notes": [
            "Generated by den-core's crates/den-sync/tests/library_v4.rs, where every case is also asserted.",
            "cases: one `evaluate` request each; `ok` and `error` are exact (compared as JCS), `ok_subset` lists the members the result must hold (sizes and compressed bytes, which depend on the compressor version, are not pinned).",
            "merge: every ordering and grouping of a, b and c (or both orders of a and b) merges to `merged`, and `merged` is a fixed point.",
            "switch: v4_form on `rows` (with base, performer, now), checked by `form`; then v4_dry_run on its output, after `tamper` re-encodes the named document with `fields` overlaid, must hold `dry_run`. Round trip: every document v4_form makes decodes back to itself.",
            "{\"$pad\": {\"length\": n, \"seed\": s}} stands for a string of n characters: SplitMix64 seeded with s (state += 0x9e3779b97f4a7c15; z = state; z = (z ^ z>>30) * 0xbf58476d1ce4e5b9; z = (z ^ z>>27) * 0x94d049bb133111eb; z ^= z>>31), each output's top six bits indexing the alphabet A–Z a–z 0–9 - _. {\"$pad\": {\"length\": n, \"char\": c}} repeats c.",
            "Plaintexts are base64url, unpadded. A plaintext starting 0x00 is raw DEFLATE; any compressor's output decodes the same."
        ],
        "cases": plain,
        "merge": merges,
        "switch": switches,
    });
    let text = serde_json::to_string_pretty(&file).unwrap() + "\n";
    std::fs::write(dir.join("library-v4.json"), text).unwrap();
    let fixture = json!({"version": 4, "cases": contract});
    let mut lines = vec![
        "{".to_owned(),
        "  \"version\": 4,".to_owned(),
        "  \"cases\": [".to_owned(),
    ];
    let cases = fixture["cases"].as_array().unwrap();
    for (i, case) in cases.iter().enumerate() {
        let comma = if i + 1 < cases.len() { "," } else { "" };
        lines.push(format!("    {case}{comma}"));
    }
    lines.extend(["  ]".to_owned(), "}".to_owned()]);
    std::fs::write(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy-v4.json"),
        lines.join("\n") + "\n",
    )
    .unwrap();
}

#[test]
fn den_spec_library_v4_vectors() {
    let Some(dir) = spec_dir() else {
        if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") {
            eprintln!("SKIP: den-spec absent and DEN_SPEC_OPTIONAL=1");
            return;
        }
        panic!("den-spec/vectors not found — check out den-spec beside this repo, set DEN_SPEC_DIR, or set DEN_SPEC_OPTIONAL=1 to skip deliberately.");
    };
    let text = std::fs::read_to_string(dir.join("library-v4.json")).unwrap_or_else(|_| {
        panic!(
            "den-spec/vectors/library-v4.json not found in {}",
            dir.display()
        )
    });
    let file: Value = serde_json::from_str(&text).unwrap();
    let mut count = 0;
    for case in file["cases"].as_array().unwrap() {
        let result = call(&expand(&case["request"]));
        let name = &case["name"];
        if let Some(expected) = case.get("ok") {
            assert!(
                result.get("ok").is_some_and(|ok| same(ok, expected)),
                "{name}: {result}"
            );
        } else if let Some(expected) = case.get("error") {
            assert_eq!(&result["error"], expected, "{name}");
        } else {
            assert!(
                result
                    .get("ok")
                    .is_some_and(|ok| contains(ok, &case["ok_subset"])),
                "{name}: {result}"
            );
        }
        count += 1;
    }
    for merge in file["merge"].as_array().unwrap() {
        check_merge(&Merge {
            name: "vector",
            a: merge["a"].clone(),
            b: merge["b"].clone(),
            c: Some(merge["c"].clone()).filter(|c| !c.is_null()),
            merged: merge["merged"].clone(),
        });
        count += 1;
    }
    for switch in file["switch"].as_array().unwrap() {
        let form = &switch["form"];
        let expect = if let Some(error) = form.get("error") {
            Expect::Error(Box::leak(
                error.as_str().unwrap().to_owned().into_boxed_str(),
            ))
        } else {
            Expect::Subset(form["ok_subset"].clone())
        };
        run_switch(&Switch {
            name: "vector",
            rows: switch["rows"].clone(),
            tamper: switch["tamper"]
                .as_object()
                .map(|t| (t["name"].as_str().unwrap().to_owned(), t["fields"].clone())),
            form: expect,
            dry_run: Some(switch["dry_run"].clone()).filter(|d| !d.is_null()),
        });
        count += 1;
    }
    assert!(count > 100, "only {count} vectors");
}
