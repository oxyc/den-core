//! Allocation evidence for the season projection boundary. One test owns this binary so the counting allocator has
//! no concurrent test noise. The repeated and batched paths hold equivalent answers over the same 100 registers.

use serde_json::{json, Map, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    ALLOCATED.fetch_add(by, Relaxed);
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

fn fixture() -> (Value, Value) {
    let title = json!({
        "format": 4,
        "kind": "title",
        "title": {"type": "tv", "id": 1399},
        "episodesReset": [1_700_000_000_000_i64, 0, "a1b2c3d4e5f60718"]
    });
    let episodes: Map<String, Value> = (1..=100_i64)
        .map(|episode| {
            let at = 1_700_000_000_000_i64 + episode * 1_000;
            (
                episode.to_string(),
                json!({
                    "progress": {"value": if episode % 3 == 0 { 0.42 } else { 1.0 },
                        "at": [at, 0, "a1b2c3d4e5f60718"], "viewing": episode % 4},
                    "imported": false,
                    "plays": {"0": at},
                    "cleared": null
                }),
            )
        })
        .collect();
    let season = json!({
        "format": 4,
        "kind": "season",
        "title": {"type": "tv", "id": 1399},
        "season": 1,
        "seasonReset": null,
        "episodes": episodes
    });
    (title, season)
}

fn measure(run: impl FnOnce() -> Vec<String>) -> (usize, usize, Vec<String>) {
    let before_live = LIVE.load(Relaxed);
    let before_allocated = ALLOCATED.load(Relaxed);
    PEAK.store(before_live, Relaxed);
    let answers = run();
    (
        ALLOCATED.load(Relaxed) - before_allocated,
        PEAK.load(Relaxed) - before_live,
        answers,
    )
}

#[test]
fn batching_reduces_allocated_bytes_and_peak_heap() {
    let (title, season) = fixture();
    let now = 1_700_000_200_000_i64;
    let singles: Vec<String> = (1..=100)
        .map(|episode| {
            json!({"op": "episode_state_v4", "title": title, "season": season,
                "episode": episode, "now": now})
            .to_string()
        })
        .collect();
    let batch =
        json!({"op": "season_state_v4", "title": title, "season": season, "now": now}).to_string();

    let (single_allocated, single_peak, single_answers) = measure(|| {
        singles
            .iter()
            .map(|request| den_sync::evaluate(request))
            .collect()
    });
    let (batch_allocated, batch_peak, mut batch_answers) =
        measure(|| vec![den_sync::evaluate(&batch)]);

    let batch_answer: Value = serde_json::from_str(&batch_answers.pop().unwrap()).unwrap();
    for (index, answer) in single_answers.iter().enumerate() {
        let answer: Value = serde_json::from_str(answer).unwrap();
        assert_eq!(batch_answer["ok"][(index + 1).to_string()], answer["ok"]);
    }
    eprintln!(
        "100 episodes: repeated allocated {single_allocated} bytes, peak {single_peak} bytes; batch allocated {batch_allocated} bytes, peak {batch_peak} bytes"
    );
    assert!(batch_allocated < single_allocated / 4);
    assert!(batch_peak < single_peak);
}
