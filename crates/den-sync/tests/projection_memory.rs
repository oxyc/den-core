//! What an `assistant_projection` build holds at once, for a household the size of den-edge's big-library fixture
//! (`web/e2e/big-library.mjs`): 1,510 films and 380 series of three or four seasons, about 13,000 plays in Seen. Den
//! Web builds it in a wasm module whose memory never shrinks, so the peak of one build is what an iPhone tab keeps.
//!
//! One test in its own binary: the allocator counts every thread, and no other test runs beside it.

use serde_json::{json, Map, Value};
use std::alloc::{GlobalAlloc, Layout, System};
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
const LIBRARY: &str = "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e";
const NOW: u64 = 1_790_000_000_000;
const DAY: u64 = 86_400_000;
const MIB: usize = 1 << 20;

fn st(at: u64) -> Value {
    json!([at, 0, D])
}

fn ok(request: &Value) -> Value {
    let answer: Value = serde_json::from_str(&den_sync::evaluate(&request.to_string())).unwrap();
    answer["ok"].clone()
}

/// The fixture's household as library-v4 documents, with the series layouts a client knows.
fn household() -> (Vec<Value>, Map<String, Value>) {
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
    let mut layouts = Map::new();
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
                    // Each series watched in order: a later episode finished later.
                    let at = NOW - DAY - (s * 60 + 60 - season * 10 - e) * 3_600_000;
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
        let layout: Vec<Value> = (1..=seasons)
            .map(|season| json!({"season": season, "episodes": 10}))
            .collect();
        layouts.insert(
            id.to_string(),
            json!({"seasons": layout, "last_aired": null}),
        );
    }
    (documents, layouts)
}

/// One build's peak, above what was live before it, is held to twice the request it reads: the documents and layouts
/// are read one at a time, and the lists are held once, as rows (about 1.6 times the request). Holding the lists as
/// `Value`s took about 11 times; parsing the documents into `Value`s up front as well, about 36; holding the layouts
/// or the title documents as `Value`s again passes 2.
#[test]
fn a_big_household_builds_its_projection_in_bounded_memory() {
    let (documents, layouts) = household();
    let grant = "0123456789abcdef0123456789abcdef";
    let read = ok(&json!({"op": "assistant_keygen_read", "grant": grant,
        "random": "11".repeat(32), "client": "Claude", "now": NOW - DAY}));
    let reads = json!({"kind": "set", "schema": 2, "name": "assistant-read",
        "values": {grant: {"value": read["value"], "at": st(NOW - DAY)}}});
    let request = json!({"op": "assistant_projection", "library": LIBRARY, "documents": documents,
        "layouts": layouts, "head": 6_100, "read": reads, "random": "22".repeat(32), "now": NOW})
    .to_string();
    drop((documents, layouts));

    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let answer = den_sync::evaluate_assistant(&request);
    let peak = PEAK.load(Relaxed) - before;

    let answer: Value = serde_json::from_str(&answer).unwrap();
    let counts = &answer["ok"]["counts"];
    assert_eq!(
        answer["ok"]["publish"].as_array().map(Vec::len),
        Some(1),
        "{answer}"
    );
    assert_eq!(counts["watchlist"], 168);
    // 28 films in progress (three of every fiftieth are on the watchlist instead) and 30 series being watched.
    assert_eq!(counts["continue"], 28 + 30);
    assert!(counts["seen"].as_u64().unwrap() > 13_000, "{counts}");
    let mb = |n: usize| n as f64 / MIB as f64;
    eprintln!(
        "projection build: request {:.1} MB, peak {:.1} MB",
        mb(request.len()),
        mb(peak)
    );
    assert!(
        peak <= 2 * request.len(),
        "a build held {:.1} MB at once for a {:.1} MB request",
        mb(peak),
        mb(request.len())
    );
}
