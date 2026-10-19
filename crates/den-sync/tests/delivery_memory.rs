//! What a `pending_targets_v4` pass holds at once, for a household the size of den-edge's big-library fixture
//! (`web/e2e/big-library.mjs`): 1,510 films and 380 series of three or four seasons, connected to SIMKL. Den Web
//! runs it in a wasm module whose memory never shrinks, so the peak of one pass is what its library Worker keeps.
//!
//! One test in its own binary: the allocator counts every thread, and no other test runs beside it.

use serde_json::{json, Map, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// The system allocator, counting the bytes live and the most live at once.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    PEAK.fetch_max(LIVE.fetch_add(by, Relaxed) + by, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grew(layout.size());
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        grew(layout.size());
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if size > layout.size() {
            grew(size - layout.size());
        } else {
            LIVE.fetch_sub(layout.size() - size, Relaxed);
        }
        System.realloc(ptr, layout, size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const D: &str = "a1b2c3d4e5f60718";
const ACCOUNT: &str = "4812736";
const NOW: u64 = 1_790_000_000_000;
const DAY: u64 = 86_400_000;
const MIB: usize = 1 << 20;

fn st(at: u64) -> Value {
    json!([at, 0, D])
}

fn ok(request: &Value) -> Value {
    let answer: Value = serde_json::from_str(&den_sync::evaluate(&request.to_string())).unwrap();
    assert!(answer.get("ok").is_some(), "{answer}");
    answer["ok"].clone()
}

/// The fixture's household as library-v4 documents (as `projection_memory.rs` builds it).
fn household() -> Vec<Value> {
    let mut documents = Vec::new();
    for i in 0..1_510u64 {
        let id = 100_000 + i;
        let at = NOW - (i + 2) * DAY / 3;
        let mut doc = json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": id},
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
    for s in 0..380u64 {
        let id = 200_000 + s;
        let seasons = if s % 8 == 0 { 4 } else { 3 };
        let watching = s % 13 == 0;
        documents.push(
            json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": id},
            "status": {"value": if watching { "inProgress" } else { "watched" },
                "at": st(NOW - s * DAY / 2)}}),
        );
        for season in 1..=seasons {
            let episodes: Map<String, Value> = (1..=10u64)
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
            documents.push(json!({"format": 4, "kind": "season",
                "title": {"type": "tv", "id": id}, "season": season, "seasonReset": null,
                "episodes": episodes}));
        }
    }
    documents
}

fn deliver(since: u64) -> Value {
    json!({"provider": "simkl", "account": ACCOUNT, "since": st(since), "removals_sent": []})
}

fn request(documents: &[Value], since: u64) -> String {
    json!({"op": "pending_targets_v4", "documents": documents, "deliver": deliver(since), "now": NOW})
        .to_string()
}

/// The delivery documents a pass's every target settled into: what a household that has delivered everything holds.
fn receipts(pending: &Value) -> Vec<Value> {
    let mut by_document: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let targets = pending["settle"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t, "acknowledge"))
        .chain(
            pending["commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| (c, "send")),
        );
    for (order, (target, action)) in targets.enumerate() {
        let settled = ok(&json!({"op": "settle_v4", "outcome": {"action": action},
            "built_from": target["built_from"], "order": [2, order + 1, D]}));
        let document = target["document"].as_str().unwrap().to_string();
        by_document
            .entry(document)
            .or_default()
            .push(json!({"key": target["key"], "settle": settled}));
    }
    by_document
        .into_iter()
        .map(|(name, commands)| {
            let parts: Vec<&str> = name.split(':').collect();
            let mut identity = json!({"provider": "simkl", "account": ACCOUNT,
                "title": {"type": parts[3], "id": parts[4].parse::<u64>().unwrap()}});
            if let Some(season) = parts.get(5) {
                identity["season"] = json!(season.parse::<u64>().unwrap());
            }
            ok(&json!({"op": "delivery_write", "identity": identity, "commands": commands}))
                ["document"]
                .clone()
        })
        .collect()
}

/// One pass's peak above what was live before it, the length of its answer, and the answer.
fn measure(request: &str) -> (usize, usize, Value) {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let answer = den_sync::evaluate(request);
    let peak = PEAK.load(Relaxed) - before;
    let length = answer.len();
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert!(answer.get("ok").is_some(), "{answer}");
    (peak, length, answer["ok"].clone())
}

fn mb(n: usize) -> f64 {
    n as f64 / MIB as f64
}

/// The pass that decides a whole library at once holds, besides its answer's text, a row for each command and settle;
/// a settled one holds next to nothing, since each document is read only when it is used. Before, every document was
/// parsed into a `Value` up front (about 15 times its text) and every command and settle held as one: the first pass
/// peaked at 150 MB for a 2.2 MB request, the settled one at 107 MB for 4 MB. Reading the documents lazily but holding
/// the answer as `Value`s still took 93 MB for the first.
#[test]
fn a_big_household_decides_its_delivery_in_bounded_memory() {
    let mut documents = household();
    // Connected after everything was watched: every target is a baseline command or settles silently.
    let first = request(&documents, NOW - DAY / 2);
    let (first_peak, first_answer, pending) = measure(&first);
    let commands = pending["commands"].as_array().unwrap().len();
    let settles = pending["settle"].as_array().unwrap().len();
    assert_eq!(
        (commands, settles),
        (13_710, 3_234),
        "every film's list, rating and watch, every series' list and rating, every episode"
    );
    eprintln!(
        "first pass: request {:.1} MB, answer {:.1} MB, peak {:.1} MB",
        mb(first.len()),
        mb(first_answer),
        mb(first_peak)
    );
    // The answer's text grows to up to twice its length, and the rows it is written from are about 1.4 times it.
    assert!(
        first_peak <= 4 * first_answer,
        "a first pass held {:.1} MB at once for a {:.1} MB answer",
        mb(first_peak),
        mb(first_answer)
    );

    // Everything delivered: each target's receipt is in its delivery document, and nothing is pending.
    documents.extend(receipts(&pending));
    drop(pending);
    let settled = request(&documents, NOW - DAY / 2);
    drop(documents);
    let (settled_peak, _, pending) = measure(&settled);
    assert_eq!(pending["commands"], json!([]), "{pending}");
    assert_eq!(pending["settle"], json!([]));
    assert_eq!(pending["greatest_epoch"], 2);
    eprintln!(
        "settled pass: request {:.1} MB, peak {:.1} MB",
        mb(settled.len()),
        mb(settled_peak)
    );
    // The documents found by name, and one read at a time.
    assert!(
        settled_peak <= settled.len() / 2,
        "a settled pass held {:.1} MB at once for a {:.1} MB request",
        mb(settled_peak),
        mb(settled.len())
    );
}
