//! What the Owner split's row partition (`profiles_migration_partition_rows`) holds at once, for a household the size
//! of den-edge's big-library fixture (`web/e2e/big-library.mjs`): 1,510 films and 380 series of three or four seasons.
//! Den Web runs it in a wasm module whose memory never shrinks, over every row of the household's library.
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
const NOW: u64 = 1_790_000_000_000;
const DAY: u64 = 86_400_000;
const MIB: usize = 1 << 20;

fn st(at: u64) -> Value {
    json!([at, 0, D])
}

/// The fixture's household as library-v4 documents (as `projection_memory.rs` builds it).
fn household() -> Vec<Value> {
    let mut documents = Vec::new();
    for i in 0..1_510u64 {
        let at = NOW - (i + 2) * DAY / 3;
        let mut doc = json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 100_000 + i},
            "addedAt": at - 30 * DAY, "status": {"value": "watched", "at": st(at)},
            "resume": {"value": 1, "at": st(at), "viewing": 0}, "watch": {"plays": {"0": at}, "cleared": null}});
        if i % 4 == 0 {
            doc["reaction"] = json!({"value": "like", "at": st(at)});
        }
        documents.push(doc);
    }
    for s in 0..380u64 {
        let id = 200_000 + s;
        documents.push(
            json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": id},
            "status": {"value": "watched", "at": st(NOW - s * DAY / 2)}}),
        );
        for season in 1..=if s % 8 == 0 { 4 } else { 3 } {
            let episodes: Map<String, Value> = (1..=10u64)
                .map(|e| {
                    let at = NOW - DAY - (s * 40 + season * 10 + e) * 3_600_000;
                    let register = json!({"progress": {"value": 1, "at": st(at), "viewing": 0},
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

/// The household's rows as the Owner split reads them: each sealed (here, its text standing in for the ciphertext,
/// which is about as long) and opened, with the household's preferences.
fn rows() -> Vec<Value> {
    let mut rows: Vec<Value> = household()
        .into_iter()
        .map(|document| {
            let request = json!({"op": "doc_name", "document": document}).to_string();
            let name: Value = serde_json::from_str(&den_sync::evaluate(&request)).unwrap();
            json!({"v": document.to_string(), "opened": {"status": "document", "name": name["ok"],
                "document": document, "dropped": []}})
        })
        .collect();
    rows.push(
        json!({"v": "sealed", "opened": {"status": "row", "name": "set:prefs", "legacy": false,
        "row": {"kind": "set", "schema": 2, "name": "prefs",
            "values": {"den.hideAnime": {"value": {"bool": true}, "at": st(NOW - DAY)}}}}}),
    );
    for (i, row) in rows.iter_mut().enumerate() {
        row["k"] = json!(format!("{:064x}", i + 1));
        row["seq"] = json!(i + 1);
    }
    rows
}

/// The partition holds its answer's projections as their text, and each row read only while it is partitioned. It
/// held the household as `Value`s, and copied every document into its answer as one: 117 MB for a 5.3 MB request.
#[test]
fn a_big_household_is_partitioned_in_bounded_memory() {
    let rows = rows();
    let request = json!({"op": "profiles_migration_partition_rows",
        "input": {"base": rows.len(), "ownerPolicyAt": st(NOW), "rows": rows}})
    .to_string();
    drop(rows);

    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let answer = den_sync::evaluate(&request);
    let peak = PEAK.load(Relaxed) - before;

    let answer: Value = serde_json::from_str(&answer).unwrap();
    let partition = &answer["ok"];
    assert_eq!(
        partition["owner"].as_array().map(Vec::len),
        Some(3_079),
        "{answer}"
    );
    assert_eq!(
        partition["accounting"].as_array().map(Vec::len),
        Some(3_079)
    );
    let mb = |n: usize| n as f64 / MIB as f64;
    eprintln!(
        "partition: request {:.1} MB, peak {:.1} MB",
        mb(request.len()),
        mb(peak)
    );
    // The answer's text (about half the request), the rows found by key, and one row read at a time.
    assert!(
        peak <= 2 * request.len(),
        "a partition held {:.1} MB at once for a {:.1} MB request",
        mb(peak),
        mb(request.len())
    );
}
