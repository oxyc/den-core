//! `LibraryColumns::apply` / `apply_batch` / `merge` against the per-document ops they stand in for: for every
//! `apply_write`, `doc_merge` and `write_back_v4` case of the binding contract and for a random sequence of writes over
//! den-edge's big-library household, the index after the held-index op equals an index rebuilt from the documents the
//! old ops produced, and each document's bytes are `doc_encode`'s. The boundary-cost comparison is `#[ignore]`d; run it
//! with `cargo test --release -p den-sync --test columns_apply -- --ignored --nocapture`.

use den_sync::{evaluate, ColumnsApplied, ColumnsLayouts, LibraryColumns};
use serde_json::{json, Map, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

/// The system allocator, counting allocations and the bytes they asked for.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED.fetch_add(layout.size(), Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        ALLOCATED.fetch_add(size, Relaxed);
        System.realloc(ptr, layout, size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const D: &str = "a1b2c3d4e5f60718";
const NOW: i64 = 1_790_000_000_000;
const DAY: i64 = 86_400_000;

fn st(at: i64) -> Value {
    json!([at, 0, D])
}

fn call(request: &Value) -> Result<Value, String> {
    let mut answer: Value = serde_json::from_str(&evaluate(&request.to_string())).unwrap();
    match answer.get("error") {
        Some(error) => Err(error.as_str().unwrap().to_owned()),
        None => Ok(answer["ok"].take()),
    }
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

fn name(document: &Value) -> String {
    call(&json!({"op": "doc_name", "document": document}))
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

/// `doc_encode`'s plaintext, or `None` for `too_large`.
fn encode(document: &Value, write: bool) -> Option<String> {
    let encoded = call(&json!({"op": "doc_encode", "document": document, "write": write})).unwrap();
    encoded["plaintext"].as_str().map(str::to_owned)
}

fn index(documents: &[Value]) -> LibraryColumns {
    let mut columns = LibraryColumns::new();
    columns.upsert_text(&json!(documents).to_string()).unwrap();
    columns
}

/// Everything an index answers: its documents in order, and every list.
fn snapshot(columns: &LibraryColumns, now: i64) -> Value {
    let layouts = ColumnsLayouts::default();
    json!({
        "documents": serde_json::from_str::<Value>(&columns.documents_text()).unwrap(),
        "home": columns.home(now).to_json(),
        "history": columns.history(now).to_json(),
        "watchlist": columns.watchlist(now).to_json(),
        "continue": columns.continuing(now, &layouts, None).to_json(),
        "newest": columns.newest(now).to_json(),
    })
}

/// `documents` with each of `written` replacing the one of its name, or added after them: what a client holds after
/// upserting what the old ops answered.
fn overlaid(documents: &[Value], written: &[Value]) -> Vec<Value> {
    let mut out = documents.to_vec();
    for document in written {
        let n = name(document);
        match out.iter().position(|d| name(d) == n) {
            Some(i) => out[i] = document.clone(),
            None => out.push(document.clone()),
        }
    }
    out
}

/// The answer's documents against `doc_encode` of the old ops' documents, in order.
fn assert_bytes(applied: &ColumnsApplied, expected: &[Value], write: bool, case: &str) {
    let mut names = Vec::new();
    let mut too_large = Vec::new();
    for document in expected {
        match encode(document, write) {
            Some(plaintext) => names.push((name(document), plaintext)),
            None => too_large.push(name(document)),
        }
    }
    let ours: Vec<(String, String)> = applied
        .documents
        .iter()
        .map(|(n, bytes)| (n.clone(), base64(bytes)))
        .collect();
    assert_eq!(ours, names, "{case}: bytes");
    assert_eq!(applied.too_large, too_large, "{case}: too large");
}

fn fixture() -> Vec<Value> {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/policy-v4.json")).unwrap();
    fixture["cases"].as_array().unwrap().clone()
}

#[test]
fn apply_is_apply_write_for_every_contract_case() {
    let mut compared = 0;
    for case in fixture() {
        let request = &case["request"];
        if request["op"] != "apply_write" {
            continue;
        }
        let label = case["name"].as_str().unwrap();
        let now = request["now"].as_i64().unwrap();
        let mut held: Vec<Value> = request["title"]
            .as_object()
            .map(|_| request["title"].clone())
            .into_iter()
            .collect();
        for list in ["seasons", "receipts"] {
            held.extend(request[list].as_array().into_iter().flatten().cloned());
        }
        let mut columns = index(&held);
        let before = snapshot(&columns, now);
        let applied = columns
            .apply(&request["write"], &request["target"], now)
            .unwrap();
        if let Some(error) = case.get("error") {
            assert_eq!(applied.outcomes, vec![error.as_str().unwrap()], "{label}");
            assert!(applied.documents.is_empty(), "{label}");
            assert_eq!(
                snapshot(&columns, now),
                before,
                "{label}: a refusal holds nothing"
            );
        } else {
            let written = case["ok"]["documents"].as_array().unwrap();
            let outcome = if written.is_empty() {
                "unchanged"
            } else {
                "changed"
            };
            assert_eq!(applied.outcomes, vec![outcome], "{label}");
            assert_bytes(&applied, written, true, label);
            assert_eq!(
                snapshot(&columns, now),
                snapshot(&index(&overlaid(&held, written)), now),
                "{label}"
            );
        }
        compared += 1;
    }
    assert_eq!(compared, 55);
}

/// `merge` of each document an `apply_write` case wrote into the documents it was written over, and the contract's
/// `doc_merge` case.
#[test]
fn merge_is_doc_merge() {
    let mut compared = 0;
    for case in fixture() {
        let request = &case["request"];
        if request["op"] == "doc_merge" {
            let mut columns = index(&[request["a"].clone()]);
            assert_eq!(
                columns.merge(&request["b"]).err().as_deref(),
                case["error"].as_str(),
                "{}",
                case["name"]
            );
            compared += 1;
            continue;
        }
        if request["op"] != "apply_write" || case.get("ok").is_none() {
            continue;
        }
        let mut held: Vec<Value> = request["title"]
            .as_object()
            .map(|_| request["title"].clone())
            .into_iter()
            .collect();
        held.extend(request["seasons"].as_array().into_iter().flatten().cloned());
        for written in case["ok"]["documents"].as_array().unwrap() {
            let n = name(written);
            let mut columns = index(&held);
            let stored = held.iter().find(|d| name(d) == n);
            let expected =
                call(&json!({"op": "doc_merge", "a": stored.unwrap_or(written), "b": written}))
                    .unwrap();
            let applied = columns.merge(written).unwrap();
            let now = request["now"].as_i64().unwrap();
            if stored.is_some_and(|s| encode(s, false) == encode(&expected, false)) {
                assert_eq!(applied.outcomes, vec!["unchanged"]);
                assert!(applied.documents.is_empty());
            } else {
                assert_eq!(applied.outcomes, vec!["changed"]);
                assert_bytes(&applied, std::slice::from_ref(&expected), false, &n);
            }
            assert_eq!(
                snapshot(&columns, now),
                snapshot(&index(&overlaid(&held, &[expected])), now)
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 41);
}

/// `write_back_v4` as the index does it: the log's documents held, each held document merged in, the kept writes
/// applied as a batch. What is written back, and each kept write's drop reason, are the op's.
#[test]
fn merge_then_apply_is_write_back() {
    let mut compared = 0;
    for case in fixture() {
        let request = &case["request"];
        if request["op"] != "write_back_v4" {
            continue;
        }
        let label = case["name"].as_str().unwrap();
        let now = request["now"].as_i64().unwrap();
        let log = request["log"].as_array().unwrap();
        let mut columns = index(log);
        for document in request["documents"].as_array().unwrap() {
            // A held document the op discards, or one the log has in a newer format, is refused here.
            let _ = columns.merge(document);
        }
        let kept = request["kept"].as_array().cloned().unwrap_or_default();
        let applied = columns.apply_batch(&kept, now, false).unwrap();
        let ok = &case["ok"];
        for drop in ok["dropped"].as_array().unwrap() {
            if let Some(index) = drop["kept"].as_u64() {
                assert_eq!(applied.outcomes[index as usize], drop["reason"], "{label}");
            }
        }
        let writes: Vec<Value> = ok["writes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["document"].clone())
            .collect();
        let expected = index(&overlaid(log, &writes));
        let by_name = |columns: &LibraryColumns| -> BTreeMap<String, Value> {
            let documents: Vec<Value> = serde_json::from_str(&columns.documents_text()).unwrap();
            documents.into_iter().map(|d| (name(&d), d)).collect()
        };
        assert_eq!(by_name(&columns), by_name(&expected), "{label}");
        // A document a kept write changed carries the op's bytes.
        for (n, bytes) in &applied.documents {
            let document = writes.iter().find(|w| name(w) == *n).unwrap();
            assert_eq!(Some(base64(bytes)), encode(document, true), "{label}: {n}");
        }
        compared += 1;
    }
    assert_eq!(compared, 7);
}

#[test]
fn an_atomic_batch_with_a_refusal_holds_nothing() {
    let film = json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 550},
        "status": {"value": "watchlist", "at": st(NOW - DAY)}});
    let mut columns = index(std::slice::from_ref(&film));
    let before = snapshot(&columns, NOW);
    let target = json!({"type": "movie", "id": 550});
    let writes = [
        json!({"target": target, "write": {"kind": "mark_watched", "at": st(NOW)}}),
        json!({"target": target, "write": {"kind": "title", "fields": {"status": 1}, "at": st(NOW)}}),
    ];
    let applied = columns.apply_batch(&writes, NOW, true).unwrap();
    assert_eq!(applied.outcomes, vec!["changed", "invalid_field:status"]);
    assert!(applied.documents.is_empty());
    assert_eq!(snapshot(&columns, NOW), before);
    let applied = columns.apply_batch(&writes, NOW, false).unwrap();
    assert_eq!(applied.outcomes, vec!["changed", "invalid_field:status"]);
    assert_eq!(applied.documents.len(), 1);
    assert_ne!(snapshot(&columns, NOW), before);
}

/// Den Web's big-library fixture (`web/e2e/big-library.mjs`, as `delivery_memory.rs` builds it): 1,510 films and 380
/// series of three or four seasons.
fn household() -> Vec<Value> {
    let mut documents = Vec::new();
    for i in 0..1_510i64 {
        let at = NOW - (i + 2) * DAY / 3;
        let mut doc = json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 100_000 + i},
            "addedAt": at - 30 * DAY});
        if i % 9 == 0 {
            doc["status"] = json!({"value": "watchlist", "at": st(at)});
        } else if i % 50 == 1 {
            doc["status"] = json!({"value": "inProgress", "at": st(at)});
            doc["resume"] = json!({"value": 0.42, "at": st(at), "viewing": 0, "seconds": 2_400});
        } else {
            doc["status"] = json!({"value": "watched", "at": st(at)});
            doc["resume"] = json!({"value": 1, "at": st(at), "viewing": 0});
            doc["watch"] = json!({"plays": {"0": at}, "cleared": null});
        }
        if i % 4 == 0 {
            let value = ["like", "love", "dislike"][(i % 3) as usize];
            doc["reaction"] = json!({"value": value, "at": st(at)});
        }
        documents.push(doc);
    }
    for s in 0..380i64 {
        let id = 200_000 + s;
        let seasons = if s % 8 == 0 { 4 } else { 3 };
        let watching = s % 13 == 0;
        documents.push(json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": id},
            "status": {"value": if watching { "inProgress" } else { "watched" }, "at": st(NOW - s * DAY / 2)}}));
        for season in 1..=seasons {
            let episodes: Map<String, Value> = (1..=10i64)
                .map(|e| {
                    let at = NOW - DAY - (s * 40 + season * 10 + e) * 3_600_000;
                    let value = if watching && season == seasons && e == 10 {
                        json!(0.5)
                    } else {
                        json!(1)
                    };
                    let register = json!({"progress": {"value": value, "at": st(at), "viewing": 0},
                        "imported": false, "plays": {"0": at}, "cleared": null});
                    (e.to_string(), register)
                })
                .collect();
            documents.push(
                json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": id},
                "season": season, "seasonReset": null, "episodes": episodes}),
            );
        }
    }
    documents
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

/// A write of every kind, on a title the household holds or a new one, stamped mostly later than everything held,
/// sometimes earlier (a replay), sometimes refused.
fn random_write(rng: &mut Rng, step: i64) -> Value {
    let roll = rng.below(100);
    let t = if roll < 6 {
        NOW - rng.below(400) as i64 * DAY
    } else {
        NOW + step * 1_000 + rng.below(500) as i64
    };
    let device = match rng.below(200) {
        0 => "not-a-device",
        1 => "",
        _ => D,
    };
    let at = json!([
        if rng.below(150) == 0 { 0 } else { t },
        rng.below(3),
        device
    ]);
    let value = [0.0, 0.2, 0.5, 0.94, 0.95, 1.0][rng.below(6) as usize];
    let account = |write: &mut Value| {
        write["provider"] = json!("simkl");
        write["account"] = json!("4812736");
    };
    if rng.below(2) == 0 {
        let id = if rng.below(30) == 0 {
            900_000 + rng.below(20)
        } else {
            100_000 + rng.below(1_510)
        };
        let mut write = match rng.pick(&[
            "progress",
            "mark_watched",
            "unwatch",
            "title",
            "import_film",
            "import_rating",
            "import_watchlist_add",
            "import_watchlist_remove",
        ]) {
            "progress" => {
                json!({"kind": "progress", "value": value, "seconds": 600, "playing": rng.below(2) == 0})
            }
            "mark_watched" => json!({"kind": "mark_watched", "watched_at": t - 5_000}),
            "unwatch" => json!({"kind": "unwatch"}),
            "title" => {
                let field = rng.pick(&["status", "reaction", "deleted", "dismissed"]);
                let v = match field {
                    "status" => json!(rng.pick(&["watchlist", "none", "watched"])),
                    "reaction" => json!(rng.pick(&["like", "love", "dislike"])),
                    _ => json!(rng.below(2) == 0),
                };
                json!({"kind": "title", "fields": {field: v}, "added_at": t - DAY})
            }
            "import_film" => json!({"kind": "import_film", "plays": [t - DAY, t - 3 * DAY]}),
            "import_rating" => json!({"kind": "import_rating", "reaction": "love", "rated_at": t}),
            "import_watchlist_add" => json!({"kind": "import_watchlist_add", "listed_at": t}),
            kind => json!({"kind": kind, "previous_listed_at": t - DAY}),
        };
        if write["kind"].as_str().unwrap().starts_with("import_") {
            account(&mut write);
        } else {
            write["at"] = at;
        }
        return json!({"target": {"type": "movie", "id": id}, "write": write});
    }
    let id = if rng.below(30) == 0 {
        950_000 + rng.below(10)
    } else {
        200_000 + rng.below(380)
    };
    let (s, e) = (1 + rng.below(4), 1 + rng.below(12));
    let mut write = match rng.pick(&[
        "progress",
        "mark_watched",
        "unwatch",
        "season_reset",
        "series_reset",
        "title",
        "import_episodes",
    ]) {
        "progress" => {
            json!({"kind": "progress", "episode": [s, e], "value": value, "seconds": 300})
        }
        "mark_watched" => json!({"kind": "mark_watched", "episodes": [[s, e], [s, e % 12 + 1]]}),
        "unwatch" => json!({"kind": "unwatch", "episodes": [[s, e]]}),
        "season_reset" => json!({"kind": "season_reset", "season": s}),
        "series_reset" => json!({"kind": "series_reset"}),
        "title" => json!({"kind": "title", "fields": {"status": "watchlist"}, "added_at": t - DAY}),
        _ => {
            json!({"kind": "import_episodes", "items": [{"season": s, "episode": e, "plays": [t - DAY]},
            {"season": s % 4 + 1, "episode": e, "days": [{"day": (t - 2 * DAY) / DAY * DAY}]}]})
        }
    };
    if write["kind"] == "import_episodes" {
        account(&mut write);
    } else {
        write["at"] = at;
    }
    let mut op = json!({"target": {"type": "tv", "id": id}, "write": write});
    if rng.below(4) == 0 {
        op["if_newer"] = json!(true);
    }
    op
}

/// The TV's own test before it asks (`LibraryLog.latestHeld`): a state no later than its register and resets hold.
fn latest_held(title: Option<&Value>, season: Option<&Value>, episode: u64) -> i64 {
    let t = |s: &Value| s[0].as_f64().unwrap_or(0.0);
    let (null, register) = (
        Value::Null,
        season.map(|s| &s["episodes"][episode.to_string()]),
    );
    let register = register.unwrap_or(&null);
    let mut times = vec![
        t(&register["progress"]["at"]),
        t(&register["cleared"][1]),
        t(&season.unwrap_or(&null)["seasonReset"]),
        t(&title.unwrap_or(&null)["episodesReset"]),
    ];
    times.extend(
        register["plays"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, v)| v.as_f64()),
    );
    times.into_iter().fold(0.0, f64::max) as i64
}

/// The old way: documents held as JSON by the client, each write an `apply_write` call given the title and the season
/// documents it names (Den Web's `applyOps`), each changed document then a `doc_encode` call.
struct Client {
    documents: Vec<Value>,
    names: BTreeMap<String, usize>,
}

impl Client {
    fn new(documents: Vec<Value>) -> Self {
        let names = documents
            .iter()
            .enumerate()
            .map(|(i, d)| (name(d), i))
            .collect();
        Client { documents, names }
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.names.get(name).map(|i| &self.documents[*i])
    }

    fn put(&mut self, document: Value) -> String {
        let n = name(&document);
        match self.names.get(&n) {
            Some(i) => self.documents[*i] = document,
            None => {
                self.names.insert(n.clone(), self.documents.len());
                self.documents.push(document);
            }
        }
        n
    }

    /// One batch: each write's outcome, and the changed documents in the order first changed.
    fn apply(&mut self, ops: &[Value], now: i64) -> (Vec<String>, Vec<Value>) {
        let mut outcomes = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        for op in ops {
            let (target, write) = (&op["target"], &op["write"]);
            let key = format!("{}:{}", target["type"].as_str().unwrap(), target["id"]);
            let mut coordinates: Vec<(u64, u64)> = Vec::new();
            if let Some(c) = write["episode"].as_array() {
                coordinates.push((c[0].as_u64().unwrap(), c[1].as_u64().unwrap()));
            }
            for c in write["episodes"].as_array().into_iter().flatten() {
                coordinates.push((c[0].as_u64().unwrap(), c[1].as_u64().unwrap()));
            }
            let mut seasons: Vec<u64> = coordinates.iter().map(|c| c.0).collect();
            seasons.extend(
                write["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| i["season"].as_u64().unwrap()),
            );
            seasons.extend(write["season"].as_u64());
            seasons.sort_unstable();
            seasons.dedup();
            let title = self.get(&format!("title:{key}"));
            let held: Vec<(u64, &Value)> = seasons
                .iter()
                .filter_map(|s| Some((*s, self.get(&format!("season:{key}:{s}"))?)))
                .collect();
            if op["if_newer"] == true {
                let at = write["at"][0].as_i64().unwrap_or(0);
                let season = |s: u64| held.iter().find(|(n, _)| *n == s).map(|(_, d)| *d);
                if !coordinates
                    .iter()
                    .all(|(s, e)| at > latest_held(title, season(*s), *e))
                {
                    outcomes.push("not_newer".to_owned());
                    continue;
                }
            }
            let mut request = json!({"op": "apply_write", "write": write, "target": target, "now": now,
                "seasons": held.iter().map(|(_, d)| *d).collect::<Vec<_>>()});
            if let Some(title) = title {
                request["title"] = title.clone();
            }
            match call(&request) {
                Err(code) => outcomes.push(code),
                Ok(answer) => {
                    let documents = answer["documents"].as_array().unwrap();
                    outcomes.push(
                        if documents.is_empty() {
                            "unchanged"
                        } else {
                            "changed"
                        }
                        .to_owned(),
                    );
                    for document in documents {
                        let n = self.put(document.clone());
                        if !changed.contains(&n) {
                            changed.push(n);
                        }
                    }
                }
            }
        }
        let documents = changed
            .iter()
            .map(|n| self.get(n).unwrap().clone())
            .collect();
        (outcomes, documents)
    }
}

#[test]
fn a_random_sequence_over_the_big_library_is_the_old_ops() {
    let documents = household();
    let mut columns = index(&documents);
    let mut client = Client::new(documents);
    let mut rng = Rng(0x5eed_d0c5);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut step = 0;
    for batch in 0..30 {
        let size = [1, 7, 60, 200][batch % 4];
        let ops: Vec<Value> = (0..size)
            .map(|_| {
                step += 1;
                random_write(&mut rng, step)
            })
            .collect();
        let now = NOW + step * 1_000;
        let (outcomes, written) = client.apply(&ops, now);
        let applied = columns.apply_batch(&ops, now, false).unwrap();
        assert_eq!(applied.outcomes, outcomes, "batch {batch}: {ops:?}");
        assert_bytes(&applied, &written, true, &format!("batch {batch}"));
        for outcome in outcomes {
            *counts.entry(outcome).or_default() += 1;
        }
    }
    let now = NOW + step * 1_000;
    assert_eq!(
        snapshot(&columns, now),
        snapshot(&index(&client.documents), now)
    );
    assert_eq!(
        snapshot(&columns, now + 30 * DAY),
        snapshot(&index(&client.documents), now + 30 * DAY)
    );
    eprintln!("random sequence outcomes: {counts:?}");
    for outcome in [
        "changed",
        "unchanged",
        "not_newer",
        "timeless_write",
        "invalid_device",
    ] {
        assert!(
            counts.get(outcome).is_some_and(|n| *n > 0),
            "{outcome}: {counts:?}"
        );
    }
}

/// A TV's first sync of den#580's library (500 series of four four-episode seasons, 500 films): every episode and film
/// staged as a write over documents that start empty, in season order, as `LibraryLog.stageV4` and `filmDocument`
/// make them.
fn first_sync() -> Vec<Value> {
    let mut ops = Vec::new();
    for s in 0..500i64 {
        for season in 1..=4i64 {
            for e in 1..=4i64 {
                let at = st(NOW - (s * 100 + season * 10 + e) * 60_000);
                let write = if (s + e) % 7 == 0 {
                    json!({"kind": "progress", "episode": [season, e], "value": 0.4, "seconds": 900,
                        "playing": false, "at": at})
                } else {
                    json!({"kind": "mark_watched", "episodes": [[season, e]], "at": at})
                };
                ops.push(json!({"target": {"type": "tv", "id": 300_000 + s}, "write": write, "if_newer": true}));
            }
        }
    }
    for f in 0..500i64 {
        ops.push(json!({"target": {"type": "movie", "id": 400_000 + f},
            "write": {"kind": "mark_watched", "at": st(NOW - f * 60_000)}}));
    }
    ops
}

fn median(mut samples: Vec<f64>) -> (f64, f64, f64) {
    samples.sort_by(f64::total_cmp);
    (
        samples[samples.len() / 2],
        samples[0],
        samples[samples.len() - 1],
    )
}

/// Wall time (median, low, high of `runs`), and the allocations and bytes of one run.
fn measure(runs: usize, mut work: impl FnMut()) -> ((f64, f64, f64), usize, usize) {
    work();
    let (calls, bytes) = (ALLOCATIONS.load(Relaxed), ALLOCATED.load(Relaxed));
    work();
    let allocations = (
        ALLOCATIONS.load(Relaxed) - calls,
        ALLOCATED.load(Relaxed) - bytes,
    );
    let times = (0..runs)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    (median(times), allocations.0, allocations.1)
}

/// The JSON path as a client drives it: per write, the request's text with the documents it touches, `evaluate`, and
/// its answer parsed (Rust's parse here; the TV's `JSONValue` decode of the same text costs several times more), then
/// a `doc_encode` per changed document with its plaintext decoded.
fn json_path(ops: &[Value]) -> (usize, usize) {
    let mut documents: BTreeMap<String, Value> = BTreeMap::new();
    let mut changed: Vec<String> = Vec::new();
    let mut bytes = 0;
    for op in ops {
        let (target, write) = (&op["target"], &op["write"]);
        let key = format!("{}:{}", target["type"].as_str().unwrap(), target["id"]);
        let season = write["episode"][0]
            .as_u64()
            .or(write["episodes"][0][0].as_u64());
        let title = documents.get(&format!("title:{key}"));
        let seasons: Vec<&Value> = season
            .and_then(|s| documents.get(&format!("season:{key}:{s}")))
            .into_iter()
            .collect();
        let request = json!({"op": "apply_write", "write": write, "target": target, "now": NOW,
            "title": title, "seasons": seasons})
        .to_string();
        let answer = evaluate(&request);
        bytes += request.len() + answer.len();
        let mut answer: Value = serde_json::from_str(&answer).unwrap();
        for document in answer["ok"]["documents"].as_array_mut().unwrap().drain(..) {
            let n = match document["kind"].as_str().unwrap() {
                "title" => format!("title:{key}"),
                _ => format!("season:{key}:{}", document["season"]),
            };
            if !changed.contains(&n) {
                changed.push(n.clone());
            }
            documents.insert(n, document);
        }
    }
    for n in &changed {
        let request =
            json!({"op": "doc_encode", "document": documents[n], "write": true}).to_string();
        let answer = evaluate(&request);
        bytes += request.len() + answer.len();
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert!(answer["ok"]["plaintext"].is_string());
    }
    (ops.len() + changed.len(), bytes)
}

#[test]
#[ignore]
fn boundary_cost_of_a_first_sync() {
    let ops = first_sync();
    let text = json!(ops).to_string();
    let (calls, bytes) = json_path(&ops);
    let (json_time, json_allocs, json_bytes) = measure(5, || {
        json_path(&ops);
    });
    let (mut documents, mut plaintext) = (0, 0);
    let (batch_time, batch_allocs, batch_bytes) = measure(5, || {
        let mut columns = LibraryColumns::new();
        let applied = columns.apply_batch_text(&text, NOW, false).unwrap();
        documents = applied.documents.len();
        plaintext = applied
            .documents
            .iter()
            .map(|(_, p)| p.len())
            .sum::<usize>();
    });
    let mut columns = LibraryColumns::new();
    columns.apply_batch_text(&text, NOW, false).unwrap();
    let written: Vec<Value> = serde_json::from_str(&columns.documents_text()).unwrap();
    let (encode_time, encode_allocs, _) = measure(5, || {
        for document in &written {
            evaluate(&json!({"op": "doc_encode", "document": document, "write": true}).to_string());
        }
    });
    let fmt = |(m, lo, hi): (f64, f64, f64)| format!("{m:.0} ms ({lo:.0}–{hi:.0})");
    println!(
        "apply-bench native: of either, doc_encode of the {} documents: {}, {encode_allocs} allocations",
        written.len(),
        fmt(encode_time)
    );
    println!(
        "apply-bench native: JSON ops: {calls} calls, {:.1} MiB across, {}, {json_allocs} allocations ({:.0} MiB)",
        bytes as f64 / 1048576.0,
        fmt(json_time),
        json_bytes as f64 / 1048576.0
    );
    println!(
        "apply-bench native: apply_batch of {} writes ({} KiB) -> {documents} documents ({} KiB of plaintext): {}, {batch_allocs} allocations ({:.0} MiB)",
        ops.len(),
        text.len() / 1024,
        plaintext / 1024,
        fmt(batch_time),
        batch_bytes as f64 / 1048576.0
    );
}
