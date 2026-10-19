//! What the switch (`v4_form`, then `v4_dry_run`) holds at once, for a household the size of den-edge's big-library
//! fixture (`web/e2e/big-library.mjs`): 1,510 films and 380 series of three or four seasons. Den Web runs it in a wasm
//! module whose memory never shrinks, and runs it again on a v4 library whenever a stray v2 or v3 row lands in it
//! (`switchToV4(…, stray)`), so a live library goes through it, not only one still at v3.
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
const ACCOUNT: &str = "4812736";
const NOW: u64 = 1_790_000_000_000;
const DAY: u64 = 86_400_000;
const MIB: usize = 1 << 20;

fn st(at: u64) -> Value {
    json!([at, 0, D])
}

fn mb(n: usize) -> f64 {
    n as f64 / MIB as f64
}

/// A film of the fixture: its id, when it was last touched, its status and reaction.
fn films() -> impl Iterator<Item = (u64, u64, &'static str, Option<&'static str>)> {
    (0..1_510u64).map(|i| {
        let status = if i % 9 == 0 {
            "watchlist"
        } else if i % 50 == 1 {
            "inProgress"
        } else {
            "watched"
        };
        let reaction = (i % 4 == 0).then(|| ["like", "love", "dislike"][(i % 3) as usize]);
        (100_000 + i, NOW - (i + 2) * DAY / 3, status, reaction)
    })
}

fn resume(status: &str, at: u64) -> Value {
    match status {
        "inProgress" => json!({"value": 0.42, "at": st(at), "viewing": 0, "seconds": 2_400}),
        "watched" => json!({"value": 1, "at": st(at), "viewing": 0}),
        _ => json!({"value": 0, "at": st(at), "viewing": 0}),
    }
}

/// A series of the fixture's episodes, by season: ten each, every one watched but the last of a series being watched.
fn series() -> impl Iterator<Item = (u64, u64, bool, Vec<Map<String, Value>>)> {
    (0..380u64).map(|s| {
        let seasons = if s % 8 == 0 { 4 } else { 3 };
        let watching = s % 13 == 0;
        let episodes = (1..=seasons)
            .map(|season| {
                (1..=10u64)
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
                    .collect()
            })
            .collect();
        (200_000 + s, NOW - s * DAY / 2, watching, episodes)
    })
}

/// The household as a v4 library's rows, with one stray v2 `ep` row written into it.
fn v4_library() -> Vec<Value> {
    let mut rows = Vec::new();
    for (id, at, status, reaction) in films() {
        let mut doc = json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": id},
            "addedAt": at - 30 * DAY, "status": {"value": status, "at": st(at)}});
        if status != "watchlist" {
            doc["resume"] = resume(status, at);
        }
        if status == "watched" {
            doc["watch"] = json!({"plays": {"0": at}, "cleared": null});
        }
        if let Some(reaction) = reaction {
            doc["reaction"] = json!({"value": reaction, "at": st(at)});
        }
        rows.push(doc);
    }
    for (id, at, watching, seasons) in series() {
        rows.push(
            json!({"format": 4, "kind": "title", "title": {"type": "tv", "id": id},
            "status": {"value": if watching { "inProgress" } else { "watched" }, "at": st(at)}}),
        );
        for (season, episodes) in seasons.into_iter().enumerate() {
            rows.push(
                json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": id},
                "season": season + 1, "seasonReset": null, "episodes": episodes}),
            );
        }
    }
    rows.push(
        json!({"kind": "ep", "schema": 2, "title": {"type": "tv", "id": 200_001}, "season": 1,
        "episode": 3, "progress": {"value": 1.0, "viewing": 0, "at": st(NOW - 1_000)}}),
    );
    rows
}

/// The household as a v3 library's rows, connected to SIMKL.
fn v3_library() -> Vec<Value> {
    let since = serde_json::to_string(&st(NOW - DAY / 2)).unwrap();
    let mut rows = vec![
        json!({"kind": "set", "schema": 2, "name": format!("deliver:simkl:{ACCOUNT}"), "values": {
        "since": {"value": {"string": since}, "at": st(NOW - DAY)},
        "lease": {"value": {"strings": ["", "1"]}, "at": st(NOW - DAY)}}}),
    ];
    let rec = |media: &str, id: u64, status: &str, reaction: Value, at: u64| {
        json!({"kind": "rec", "schema": 2, "title": {"type": media, "id": id},
            "status": {"value": status, "at": st(at)}, "resume": resume(status, at),
            "reaction": {"value": reaction, "at": st(at)}, "deleted": {"value": false, "at": st(1000)},
            "dismissed": {"value": false, "at": st(1000)}, "episodesReset": null, "addedAt": at - 30 * DAY,
            "watchedAt": if status == "watched" { json!(at) } else { Value::Null }})
    };
    for (id, at, status, reaction) in films() {
        rows.push(rec("movie", id, status, json!(reaction), at));
        if status == "watched" {
            rows.push(json!({"kind": "wat", "schema": 3, "title": {"type": "movie", "id": id}, "season": 0,
                "block": 0, "seasonReset": null,
                "entries": {"0": {"imported": false, "plays": {"0": at}, "cleared": null}}}));
        }
    }
    for (id, at, watching, seasons) in series() {
        let status = if watching { "inProgress" } else { "watched" };
        rows.push(rec("tv", id, status, Value::Null, at));
        for (season, entries) in seasons.into_iter().enumerate() {
            rows.push(
                json!({"kind": "wat", "schema": 3, "title": {"type": "tv", "id": id},
                "season": season + 1, "block": 0, "seasonReset": null, "entries": entries}),
            );
        }
    }
    rows
}

/// One call's peak above what was live before it, and its answer.
fn measure(request: &str) -> (usize, Value) {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let answer = den_sync::evaluate(request);
    let peak = PEAK.load(Relaxed) - before;
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert!(answer.get("ok").is_some(), "{answer}");
    (peak, answer["ok"].clone())
}

/// `v4_form` then `v4_dry_run` on `log`, each held to `form` and `dry` times its request.
fn switch(name: &str, log: Vec<Value>, form: usize, dry: usize) -> Value {
    let rows: Vec<Value> = log
        .into_iter()
        .enumerate()
        .map(|(i, row)| json!({"k": format!("{:064x}", i + 1), "seq": i + 1, "bytes": 300, "row": row}))
        .collect();
    let request =
        json!({"op": "v4_form", "rows": rows, "base": rows.len(), "performer": D, "now": NOW})
            .to_string();
    let (peak, answer) = measure(&request);
    eprintln!(
        "{name} v4_form: request {:.1} MB, peak {:.1} MB",
        mb(request.len()),
        mb(peak)
    );
    assert!(
        peak <= form * request.len(),
        "{name}: v4_form held {:.1} MB at once for a {:.1} MB request",
        mb(peak),
        mb(request.len())
    );
    drop(request);
    let request = json!({"op": "v4_dry_run", "rows": rows, "form": answer, "now": NOW}).to_string();
    drop((rows, answer));
    let (peak, dry_run) = measure(&request);
    eprintln!(
        "{name} v4_dry_run: request {:.1} MB, peak {:.1} MB",
        mb(request.len()),
        mb(peak)
    );
    assert!(
        peak <= dry * request.len(),
        "{name}: v4_dry_run held {:.1} MB at once for a {:.1} MB request",
        mb(peak),
        mb(request.len())
    );
    dry_run
}

/// A v4 library's documents pass through the switch: each is found by name and read again when it is written or
/// checked, `v4_form` holds its answer's documents as their text, and the dry run the documents it decoded as theirs
/// (about 0.8 times its request). Before, the switch held the library as `Value`s
/// several times over (the request, the merged form, the answer, the decoded form): `v4_form` peaked at 55 times its
/// request and `v4_dry_run` at 29. A v3 library is still held as `Value`s to be compacted and converted (about 12 times
/// its text), and the dry run's v3 reference (`v3_targets`) reads it again; it was 75 and 69 times.
#[test]
fn the_switch_holds_a_big_library_in_bounded_memory() {
    let dry_run = switch("v4 library + stray row", v4_library(), 4, 2);
    assert_eq!(dry_run["pass"], true, "{dry_run}");
    // 1,510 films, 380 series and 1,188 seasons, the stray row folded into its season.
    assert_eq!(dry_run["counts"]["rows"], 3_078, "{dry_run}");
    let dry_run = switch("v3 library", v3_library(), 16, 20);
    assert_eq!(dry_run["pass"], true, "{dry_run}");
    assert_eq!(dry_run["counts"]["rows"], 3_079, "{dry_run}");
}
