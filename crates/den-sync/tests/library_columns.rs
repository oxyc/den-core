//! `library_columns`: whole-library lists from documents read once. The boundary-cost comparison is `#[ignore]`d;
//! run it with `cargo test --release -p den-sync --test library_columns -- --ignored --nocapture`.

use den_sync::{evaluate, LibraryColumns};
use serde_json::{json, Value};
use std::time::Instant;

const NOW: i64 = 1_800_000_000_000;

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

fn ok(answer: &str) -> Value {
    let mut envelope: Value = serde_json::from_str(answer).unwrap();
    assert_eq!(envelope["version"], 1, "{answer}");
    envelope["ok"].take()
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

#[test]
fn the_stateless_op_answers_as_the_index() {
    let documents = household();
    let mut columns = LibraryColumns::new();
    for document in documents.clone() {
        columns.upsert(document).unwrap();
    }
    let answer = ok(&evaluate(
        &json!({"op": "library_columns", "documents": documents, "queries": ["home", "history"], "now": NOW})
            .to_string(),
    ));
    assert_eq!(answer["home"], columns.home(NOW).unwrap());
    assert_eq!(answer["history"], columns.history(NOW).unwrap());
    // 500 watched films and 1,000 series with seen episodes.
    assert_eq!(answer["history"].as_array().unwrap().len(), 1_500);
}

#[test]
fn a_replaced_document_moves_only_its_own_answer() {
    let documents = household();
    let mut columns = LibraryColumns::new();
    for document in documents.clone() {
        columns.upsert(document).unwrap();
    }
    let before = columns.history(NOW).unwrap();
    // The first film, un-watched by a later status: it leaves history and nothing else moves.
    let mut film = documents[0].clone();
    film["status"] = json!({"value": "none", "at": [NOW, 0, "bench"]});
    columns.upsert(film).unwrap();
    let after = columns.history(NOW).unwrap();
    assert_eq!(
        after, before,
        "the first film was on the watchlist, so history is unchanged"
    );
    let mut watched = documents[12].clone(); // film 20_002, on the watchlist (six documents per index)
    watched["status"] = json!({"value": "watched", "at": [NOW + 1, 0, "bench"]});
    columns.upsert(watched).unwrap();
    let after = columns.history(NOW).unwrap();
    assert_eq!(after[0][0], "movie:20002");
    assert_eq!(after.as_array().unwrap().len(), 1_501);
    assert!(columns.remove("title:movie:20002"));
    assert_eq!(columns.history(NOW).unwrap(), before);
}

#[test]
#[ignore]
fn boundary_cost_of_home_then_history() {
    let documents = household();
    let texts: Vec<String> = documents.iter().map(Value::to_string).collect();
    let (calls, bytes) = per_document(&documents);
    let json_ops = time(9, || {
        per_document(&documents);
    });
    let build = || {
        let mut columns = LibraryColumns::new();
        for text in &texts {
            columns.upsert(serde_json::from_str(text).unwrap()).unwrap();
        }
        columns
    };
    let built = time(9, || {
        build();
    });
    let columns = build();
    let queries = time(9, || {
        columns.home(NOW).unwrap();
        columns.history(NOW).unwrap();
    });
    let request = json!({"op": "library_columns", "documents": documents, "queries": ["home", "history"], "now": NOW})
        .to_string();
    let answer_len = evaluate(&request).len();
    let stateless = time(9, || {
        evaluate(&request);
    });
    let answers =
        json!({"home": columns.home(NOW).unwrap(), "history": columns.history(NOW).unwrap()})
            .to_string();
    let fmt = |(m, lo, hi): (f64, f64, f64)| format!("{m:.1} ms ({lo:.1}–{hi:.1})");
    println!(
        "columns-bench native: per-document JSON ops: {calls} calls, {} KiB, {}",
        bytes / 1024,
        fmt(json_ops)
    );
    println!(
        "columns-bench native: index build from document text: {}",
        fmt(built)
    );
    println!(
        "columns-bench native: home + history from the index: {}, answer {} KiB",
        fmt(queries),
        answers.len() / 1024
    );
    println!(
        "columns-bench native: stateless library_columns: 1 call, {} KiB, {}",
        (request.len() + answer_len) / 1024,
        fmt(stateless)
    );
}
