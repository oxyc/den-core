//! `LibraryColumns` against today's per-document ops. Home, Continue, History, the watchlist and the newest stamp are
//! the clients' folds; den-edge's `libraryColumns.test.ts` holds them to the web's own code over the per-document ops.
//! Here: the assistant's lists against `assistant_projection` over the documents, `pending` against
//! `pending_targets_v4`, `newest` against den-edge's `documentNewest`, an edited index against a rebuilt one, and the
//! stateless op against the index. The boundary-cost comparison is `#[ignore]`d; run it with
//! `cargo test --release -p den-sync --test library_columns -- --ignored --nocapture`.

use den_sync::{evaluate, ColumnsLayouts, LibraryColumns};
use serde_json::{json, Value};
use std::time::Instant;

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 86_400_000;
const D: &str = "a1b2c3d4e5f60718";

fn st(t: i64) -> Value {
    json!([t, 0, D])
}

/// 1,000 films and 1,000 series with four four-episode seasons each: den-edge's `libraryFactStore.bench.ts` household.
fn household() -> Vec<Value> {
    let stamp = |t: i64| json!([t, 0, "bench"]);
    let mut documents = Vec::new();
    for index in 0..1_000i64 {
        documents.push(json!({
            "format": 4, "kind": "title", "title": {"type": "movie", "id": 20_000 + index},
            "status": {"value": if index % 2 == 1 { "watched" } else { "watchlist" }, "at": stamp(NOW - index)},
            "addedAt": NOW - 10_000 - index,
        }));
        documents.push(json!({
            "format": 4, "kind": "title", "title": {"type": "tv", "id": 30_000 + index},
            "status": {"value": "watched", "at": stamp(NOW - index)},
            "addedAt": NOW - 10_000 - index,
        }));
        for season in 1..=4i64 {
            let episodes: serde_json::Map<String, Value> = (0..4i64)
                .map(|episode| {
                    let when = NOW - 100_000 + index * 20 + season * 4 + episode;
                    (
                        (episode + 1).to_string(),
                        json!({"progress": {"value": 1, "at": stamp(when), "viewing": 0},
                               "imported": false, "plays": {"0": when}, "cleared": null}),
                    )
                })
                .collect();
            documents.push(json!({
                "format": 4, "kind": "season", "title": {"type": "tv", "id": 30_000 + index},
                "season": season, "seasonReset": null, "episodes": episodes,
            }));
        }
    }
    documents
}

/// Every reading the lists decide on: resets, imported and cleared plays, timeless flags, dismissals, deletions,
/// future stamps, Specials, a series with no title document, a film's plays, ratings, unknown members.
fn varied() -> Vec<Value> {
    let h = |n: i64| NOW - n * 3_600_000;
    let future = NOW + 3 * DAY;
    let ep = |progress: Value, extra: Value| {
        let mut e = json!({"progress": progress, "imported": false, "plays": {}, "cleared": null});
        for (k, v) in extra.as_object().unwrap() {
            e[k] = v.clone();
        }
        e
    };
    vec![
        json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 550},
            "status": {"value": "watchlist", "at": st(h(10))}, "addedAt": h(10),
            "reaction": {"value": "seen", "at": st(h(10))}, "futureMember": {"at": st(h(1))}}),
        json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 13},
            "status": {"value": "watchlist", "at": st(h(11))}, "addedAt": h(11),
            "reaction": {"value": "love", "at": st(h(11))}, "deleted": {"value": true, "at": st(h(1))}}),
        json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 603},
            "status": {"value": "watched", "at": st(h(9))}, "resume": {"value": 1, "at": st(h(9)), "viewing": 0},
            "reaction": {"value": "love", "at": st(h(9))}, "watch": {"plays": {"0": h(9), "1": h(2)}, "cleared": null}}),
        json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 27205},
            "status": {"value": "watched", "at": [0, 1, ""]}, "reaction": {"value": "like", "at": [0, 1, ""]}}),
        json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 680},
            "status": {"value": "inProgress", "at": st(h(5))},
            "resume": {"value": 0.4, "at": st(h(5)), "viewing": 0, "seconds": 2000, "playing": false}}),
        json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 681},
            "status": {"value": "inProgress", "at": st(h(6))}, "resume": {"value": 0.2, "at": st(h(6)), "viewing": 0},
            "dismissed": {"value": true, "at": st(h(3))}}),
        json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 682},
            "status": {"value": "inProgress", "at": st(future)}, "resume": {"value": 0.5, "at": st(future), "viewing": 0},
            "addedAt": h(1)}),
        json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": 1399},
            "status": {"value": "inProgress", "at": st(h(6))}, "reaction": {"value": "dislike", "at": st(h(4))}}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 1399}, "season": 1, "seasonReset": null,
        "episodes": {
            "1": ep(json!({"value": 1, "at": st(h(7)), "viewing": 0}), json!({"plays": {"0": h(7)}})),
            "2": ep(json!({"value": 0.5, "at": st(h(6)), "viewing": 0, "seconds": 900}), json!({})),
            "10": ep(json!({"value": 1, "at": st(h(8)), "viewing": 0}), json!({"plays": {"0": h(8)}})),
        }}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 1399}, "season": 0, "seasonReset": null,
            "episodes": {"1": ep(json!({"value": 1, "at": st(h(2)), "viewing": 0}), json!({"plays": {"0": h(2)}}))}}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 1396}, "season": 1, "seasonReset": null,
            "episodes": {"1": {"imported": true, "plays": {(h(5) - (1i64 << 53)).to_string(): h(5)}, "cleared": null}}}),
        // Watched, then reset after its first season: what came before no longer counts.
        json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": 11},
            "status": {"value": "watched", "at": st(h(40))}, "episodesReset": st(h(25)), "addedAt": h(90)}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 11}, "season": 1, "seasonReset": null,
        "episodes": {
            "1": ep(json!({"value": 1, "at": st(h(30)), "viewing": 0}), json!({"plays": {"0": h(30)}})),
            "2": {"imported": true, "plays": {}, "cleared": null},
        }}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 11}, "season": 2, "seasonReset": null,
        "episodes": {
            "1": ep(json!({"value": 1, "at": st(h(20)), "viewing": 0}), json!({"plays": {"0": h(20)}})),
            "2": ep(json!({"value": 1, "at": st(h(20)), "viewing": 0}), json!({"plays": {"0": h(20)}})),
        }}),
        // Known only by imported bits, a season reset, a later un-watch; dismissed after its last activity.
        json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": 12},
            "status": {"value": "none", "at": [0, 0, ""]}, "dismissed": {"value": true, "at": st(h(1))}, "addedAt": h(70)}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 12}, "season": 3, "seasonReset": st(h(1)),
            "episodes": {"4": {"imported": true, "plays": {}, "cleared": null}}}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 12}, "season": 1, "seasonReset": null,
        "episodes": {
            "7": {"imported": true, "plays": {}, "cleared": null},
            "8": ep(json!({"value": 0, "at": st(h(1)), "viewing": 2}), json!({})),
        }}),
        // A deleted series, and one played in the future.
        json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": 14},
            "status": {"value": "watched", "at": st(h(40))}, "deleted": {"value": true, "at": st(h(35))}}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 14}, "season": 1, "seasonReset": null,
            "episodes": {"1": ep(json!({"value": 1, "at": st(h(30)), "viewing": 0}), json!({"plays": {"0": h(30)}}))}}),
        json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 15}, "season": 1, "seasonReset": null,
            "episodes": {"1": ep(json!({"value": 1, "at": st(future), "viewing": 0}), json!({"plays": {"0": future}}))}}),
    ]
}

fn layouts() -> Value {
    json!({
        "1396": {"seasons": [{"season": 1, "episodes": 3}], "last_aired": {"season": 1, "episode": 3}},
        "1399": {"seasons": [{"season": 0, "episodes": 2}, {"season": 1, "episodes": 10}, {"season": 2, "episodes": 8}],
            "last_aired": {"season": 2, "episode": 1}},
        "11": {"seasons": [{"season": 1, "episodes": 2}, {"season": 2, "episodes": 2}], "last_aired": null},
        "12": {"seasons": [{"season": 1, "episodes": 8}, {"season": 3, "episodes": 6}], "last_aired": null},
    })
}

fn index(documents: &[Value]) -> LibraryColumns {
    let mut columns = LibraryColumns::new();
    columns.upsert_text(&json!(documents).to_string()).unwrap();
    columns
}

fn ok(answer: &str) -> Value {
    let mut envelope: Value = serde_json::from_str(answer).unwrap();
    assert_eq!(envelope["version"], 1, "{answer}");
    envelope["ok"].take()
}

/// Every list of the index as JSON.
fn lists(columns: &LibraryColumns, now: i64) -> Value {
    let layouts = ColumnsLayouts::read(&layouts()).unwrap();
    json!({
        "home": columns.home(now).to_json(),
        "history": columns.history(now).to_json(),
        "watchlist": columns.watchlist(now).to_json(),
        "continue": columns.continuing(now, &layouts, None).to_json(),
        "newest": columns.newest(now).to_json(),
        "assistant": columns.assistant(now, &layouts),
    })
}

#[test]
fn the_stateless_op_answers_as_the_index() {
    for documents in [household(), varied()] {
        let answer = ok(&evaluate(
            &json!({"op": "library_columns", "documents": documents, "layouts": layouts(), "now": NOW,
                "queries": ["home", "history", "watchlist", "continue", "newest", "assistant"]})
            .to_string(),
        ));
        assert_eq!(answer, lists(&index(&documents), NOW));
    }
}

/// `assistant_projection` from the index's lists equals it from the documents: the same digest over the expanded
/// lists, the same counts.
#[test]
fn the_assistant_lists_are_the_projections() {
    for (documents, now) in [
        (household(), NOW),
        (varied(), NOW),
        (varied(), NOW + 4 * DAY),
    ] {
        let request = |extra: Value| {
            let mut r = json!({"op": "assistant_projection", "library": "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e",
                "head": 1, "random": "00".repeat(32), "now": now});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            ok(&evaluate(&r.to_string()))
        };
        let from_documents = request(json!({"documents": documents, "layouts": layouts()}));
        let layouts = ColumnsLayouts::read(&layouts()).unwrap();
        let from_index = request(json!({"lists": index(&documents).assistant(now, &layouts)}));
        assert_eq!(from_index, from_documents);
        assert!(from_documents["counts"]["seen"].as_u64().unwrap() > 0);
    }
    // The varied library reaches every list.
    let counts = ok(&evaluate(
        &json!({"op": "assistant_projection", "library": "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e", "head": 1,
            "random": "00".repeat(32), "now": NOW, "documents": varied(), "layouts": layouts()})
        .to_string(),
    ))["counts"]
        .clone();
    for list in ["watchlist", "continue", "seen", "rated"] {
        assert!(counts[list].as_u64().unwrap() > 0, "{list}: {counts}");
    }
}

#[test]
fn pending_is_pending_targets_v4() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/policy-v4.json")).unwrap();
    let mut compared = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let request = &case["request"];
        if request["op"] != "pending_targets_v4" || case.get("ok").is_none() {
            continue;
        }
        let mut columns = LibraryColumns::new();
        columns
            .upsert_text(&request["documents"].to_string())
            .unwrap();
        let pending: Value = serde_json::from_str(
            &columns
                .pending(&request["deliver"], request["now"].as_i64().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(pending, case["ok"], "{}", case["name"]);
        compared += 1;
    }
    // A latch closing over two batches of list removals, as `library_v4.rs` drives it.
    let mut documents = Vec::new();
    for (from, at) in [(1u64, 3000), (101, 6000)] {
        for id in from..from + 21 {
            documents.push(json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": id},
                "status": {"value": "watchlist", "at": st(1000)}, "deleted": {"value": true, "at": st(at)}}));
            documents.push(json!({"format": 4, "kind": "delivery", "provider": "simkl", "account": "4812736",
                "title": {"type": "movie", "id": id}, "entries": {"list": ["in", st(1000), [2, id, D]]}}));
        }
    }
    let deliver = json!({"provider": "simkl", "account": "4812736", "since": st(500), "removals_sent": [],
        "removals": {"approved": st(4000)}});
    let expected = ok(&evaluate(
        &json!({"op": "pending_targets_v4", "documents": documents, "deliver": deliver, "now": 8000})
            .to_string(),
    ));
    let pending: Value =
        serde_json::from_str(&index(&documents).pending(&deliver, 8000).unwrap()).unwrap();
    assert_eq!(pending, expected);
    assert_eq!(pending["removals"], json!({"held": st(6000)}));
    assert!(compared >= 1);
}

/// den-edge `documentNewest` over every document, the zero stamp to start from.
fn newest_oracle(documents: &[Value], now: i64) -> (Value, Option<i64>) {
    fn visit(v: &Value, now: i64, latest: &mut Value, reconsider: &mut Option<i64>) {
        match v {
            Value::Array(a)
                if a.len() == 3 && a[0].is_i64() && a[1].is_i64() && a[2].is_string() =>
            {
                let t = a[0].as_i64().unwrap();
                if t <= now + DAY {
                    let key = |s: &Value| {
                        (
                            s[0].as_i64().unwrap(),
                            s[1].as_i64().unwrap(),
                            s[2].as_str().unwrap().to_owned(),
                        )
                    };
                    if key(v) > key(latest) {
                        *latest = v.clone();
                    }
                } else {
                    *reconsider = Some(reconsider.map_or(t - DAY, |r| r.min(t - DAY)));
                }
            }
            Value::Array(a) => a.iter().for_each(|v| visit(v, now, latest, reconsider)),
            Value::Object(o) => o.values().for_each(|v| visit(v, now, latest, reconsider)),
            _ => {}
        }
    }
    let (mut latest, mut reconsider) = (json!([0, 0, ""]), None);
    for document in documents {
        visit(document, now, &mut latest, &mut reconsider);
    }
    (latest, reconsider)
}

#[test]
fn newest_is_the_webs_document_newest() {
    for documents in [household(), varied()] {
        let columns = index(&documents);
        for now in [NOW, NOW + 2 * DAY, NOW + 4 * DAY] {
            let (latest, stamp_reconsider) = newest_oracle(&documents, now);
            let answer = columns.newest(now).to_json();
            assert_eq!(
                json!([
                    answer["newest.t"][0],
                    answer["newest.c"][0],
                    answer["newest.d"][0]
                ]),
                json!([latest[0].as_f64(), latest[1].as_f64(), latest[2]]),
                "at {now}"
            );
            // The varied library's future play falls with its future stamps, so the two times agree.
            assert_eq!(columns.reconsider_at(now), stamp_reconsider, "at {now}");
        }
    }
}

/// A list asked at its reconsider time differs from the one before it, and is what a fresh index answers then.
#[test]
fn reconsider_at_is_when_a_list_next_changes() {
    let documents = varied();
    let columns = index(&documents);
    let at = columns.reconsider_at(NOW).unwrap();
    assert_eq!(at, NOW + 2 * DAY);
    assert_eq!(lists(&columns, NOW), lists(&columns, at - 1));
    assert_ne!(lists(&columns, at - 1), lists(&columns, at));
    assert_eq!(lists(&columns, at), lists(&index(&documents), at));
    assert_eq!(columns.reconsider_at(at), None);
}

#[test]
fn an_edited_index_answers_as_a_rebuilt_one() {
    let mut documents = varied();
    let mut columns = index(&documents);
    // Replaced in place: a film watched, a series reset; removed: a season.
    documents[0]["status"] = json!({"value": "watched", "at": st(NOW - 5)});
    documents[7]["episodesReset"] = st(NOW - 3_600_000 * 7);
    let removed = documents.remove(9);
    columns
        .upsert_text(&json!([documents[0], documents[7]]).to_string())
        .unwrap();
    assert!(columns.remove("season:tv:1399:0"));
    assert_eq!(removed["season"], 0);
    assert_eq!(lists(&columns, NOW), lists(&index(&documents), NOW));
    assert_eq!(
        columns
            .document("title:movie:550")
            .map(|t| serde_json::from_str::<Value>(t).unwrap()),
        Some(documents[0].clone()),
        "the document is held as it was given, unknown members and all"
    );
}

#[test]
fn a_decoded_row_is_held_without_its_document_coming_back() {
    let mut columns = LibraryColumns::new();
    let document = &varied()[0];
    let answer = columns.decode(document.to_string().as_bytes(), Some("title:movie:550"));
    assert_eq!(answer["status"], "document");
    assert!(answer.get("document").is_none());
    assert_eq!(columns.len(), 1);
    let unreadable = columns.decode(b" {}", None);
    assert_eq!(unreadable["status"], "unreadable");
    assert_eq!(columns.len(), 1);
}

fn median(mut samples: Vec<f64>) -> (f64, f64, f64) {
    samples.sort_by(f64::total_cmp);
    (
        samples[samples.len() / 2],
        samples[0],
        samples[samples.len() - 1],
    )
}

fn time(runs: usize, mut work: impl FnMut()) -> (f64, f64, f64) {
    work();
    median(
        (0..runs)
            .map(|_| {
                let start = Instant::now();
                work();
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect(),
    )
}

/// What Den Web asks today for Home then History: a `title_state` or `season_state_v4` per document, then an
/// `episode_mark` per projected episode, each a JSON request and answer.
fn per_document(documents: &[Value]) -> (usize, usize) {
    let (mut calls, mut bytes) = (0, 0);
    let mut ask = |request: Value| {
        let text = request.to_string();
        let answer = evaluate(&text);
        calls += 1;
        bytes += text.len() + answer.len();
        ok(&answer)
    };
    for document in documents {
        if document["kind"] == "title" {
            ask(json!({"op": "title_state", "title": document, "now": NOW}));
            continue;
        }
        let states = ask(json!({"op": "season_state_v4", "season": document, "now": NOW}));
        for (episode, state) in states.as_object().unwrap() {
            let at = state["watched_at"].as_i64().unwrap_or(0);
            let row = json!({"kind": "ep", "schema": 2, "title": document["title"], "season": document["season"],
                "episode": episode.parse::<u64>().unwrap(),
                "progress": {"value": if state["watched"] == true { 1 } else { 0 }, "viewing": 0, "at": [at, 0, ""]}});
            ask(json!({"op": "episode_mark", "row": row, "mark": null, "authoritative": true}));
        }
    }
    (calls, bytes)
}

#[test]
#[ignore]
fn boundary_cost_of_home_then_history() {
    let documents = household();
    let text = json!(documents).to_string();
    let (calls, bytes) = per_document(&documents);
    let json_ops = time(9, || {
        per_document(&documents);
    });
    let built = time(9, || {
        LibraryColumns::new().upsert_text(&text).unwrap();
    });
    let columns = index(&documents);
    let queries = time(9, || {
        columns.home(NOW);
        columns.history(NOW);
    });
    let layouts = ColumnsLayouts::default();
    let others = time(9, || {
        columns.continuing(NOW, &layouts, None);
        columns.watchlist(NOW);
        columns.newest(NOW);
    });
    let fmt = |(m, lo, hi): (f64, f64, f64)| format!("{m:.1} ms ({lo:.1}–{hi:.1})");
    println!(
        "columns-bench native: per-document JSON ops: {calls} calls, {} KiB, {}",
        bytes / 1024,
        fmt(json_ops)
    );
    println!(
        "columns-bench native: index build from {} KiB of document text: {}",
        text.len() / 1024,
        fmt(built)
    );
    println!(
        "columns-bench native: home + history from the index: {}",
        fmt(queries)
    );
    println!(
        "columns-bench native: continue + watchlist + newest from the index: {}",
        fmt(others)
    );
}
