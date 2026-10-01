//! Merge laws (§6, §9, §15 *Merges*) over random documents: commutative, associative, idempotent, compared on JCS.

use super::{doc_merge, jcs};
use serde_json::{json, Map, Value};

/// SplitMix64: deterministic, so a failure names the seed that reproduces it.
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
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const DEVICES: [&str; 3] = ["a1b2c3d4e5f60718", "0f1e2d3c4b5a6978", ""];

fn stamp(rng: &mut Rng) -> Value {
    // Few distinct times, so equal stamps and JCS ties are common.
    json!([rng.below(6) as i64 * 1000, rng.below(2), rng.pick(&DEVICES)])
}

fn progress(rng: &mut Rng) -> Value {
    let mut p = json!({
        "value": *rng.pick(&[0.0, 0.5, 0.95, 1.0]),
        "at": stamp(rng),
        "viewing": rng.below(3),
    });
    if rng.chance(50) {
        p["seconds"] = json!(rng.below(3) * 100);
    }
    if rng.chance(15) {
        p["extra"] = json!(rng.below(3));
    }
    p
}

/// A pool of 30 plays — Den viewings and imported plays, one imported play reachable from two sources at
/// different milliseconds — from which each register keeps the selection a writer keeps.
fn plays(rng: &mut Rng) -> Value {
    let mut all: Vec<(i64, u64)> = Vec::new();
    for _ in 0..rng.below(12) {
        let index = rng.below(30) as i64;
        let entry = if index < 20 {
            (index, 1_000 + rng.below(3) * 500)
        } else {
            let at = 1_700_000_000_000u64 + (index as u64 - 20) * 1000;
            (at as i64 - (1i64 << 53), at)
        };
        all.push(entry);
    }
    Value::Object(super::merge::select(&mut all))
}

fn unknowns(rng: &mut Rng, out: &mut Map<String, Value>, prefix: &str) {
    for _ in 0..rng.below(3) {
        out.insert(
            format!("{prefix}{}", rng.below(3)),
            json!(rng.pick(&["a", "b", "z"])),
        );
    }
}

fn register(rng: &mut Rng) -> Value {
    let mut r = Map::new();
    if rng.chance(70) {
        r.insert("progress".into(), progress(rng));
    }
    if rng.chance(80) {
        r.insert("imported".into(), json!(rng.chance(30)));
    }
    if rng.chance(80) {
        r.insert("plays".into(), plays(rng));
    }
    if rng.chance(80) {
        let cleared = if rng.chance(50) {
            Value::Null
        } else {
            json!([rng.below(3), stamp(rng)])
        };
        r.insert("cleared".into(), cleared);
    }
    if rng.chance(25) {
        unknowns(rng, &mut r, "reg");
    }
    Value::Object(r)
}

fn stamped(rng: &mut Rng, values: &[Value]) -> Value {
    json!({"value": rng.pick(values).clone(), "at": stamp(rng)})
}

fn title(rng: &mut Rng) -> Value {
    let mut doc = json!({"format": 4, "kind": "title", "title": {"type": "movie", "id": 550}});
    let d = doc.as_object_mut().unwrap();
    if rng.chance(70) {
        d.insert(
            "status".into(),
            stamped(rng, &[json!("none"), json!("watchlist"), json!("watched")]),
        );
    }
    if rng.chance(60) {
        d.insert(
            "reaction".into(),
            stamped(rng, &[Value::Null, json!("like"), json!("love")]),
        );
    }
    for field in ["deleted", "dismissed"] {
        if rng.chance(40) {
            d.insert(field.into(), stamped(rng, &[json!(false), json!(true)]));
        }
    }
    if rng.chance(70) {
        d.insert("resume".into(), progress(rng));
    }
    if rng.chance(40) {
        let reset = if rng.chance(40) {
            Value::Null
        } else {
            stamp(rng)
        };
        d.insert("episodesReset".into(), reset);
    }
    if rng.chance(50) {
        d.insert("addedAt".into(), json!(rng.below(4) as i64 * 100));
    }
    if rng.chance(50) {
        let w = if rng.chance(30) {
            Value::Null
        } else {
            json!(rng.below(4) as i64 * 100)
        };
        d.insert("watchedAt".into(), w);
    }
    if rng.chance(60) {
        d.insert("watch".into(), register(rng));
    }
    unknowns(rng, d, "u");
    doc
}

fn season(rng: &mut Rng) -> Value {
    let mut episodes = Map::new();
    for _ in 0..rng.below(4) {
        let key = rng.pick(&["1", "2", "3", "01", "100000"]).to_string();
        episodes.insert(key, register(rng));
    }
    let mut doc = json!({"format": 4, "kind": "season", "title": {"type": "tv", "id": 1399}, "season": 1, "episodes": episodes});
    if rng.chance(70) {
        doc["seasonReset"] = if rng.chance(40) {
            Value::Null
        } else {
            stamp(rng)
        };
    }
    unknowns(rng, doc.as_object_mut().unwrap(), "u");
    doc
}

fn order(rng: &mut Rng) -> Value {
    // Settle orders deliberately unrelated to the value stamps' clocks (skewed devices).
    json!([rng.below(3), rng.below(3), rng.pick(&DEVICES[..2])])
}

fn delivery(rng: &mut Rng) -> Value {
    let mut entries = Map::new();
    for _ in 0..rng.below(4) {
        let key = rng.pick(&["1", "2", "x"]).to_string();
        let entry = match rng.below(3) {
            0 => json!(["w", rng.below(2), 1000, stamp(rng), order(rng)]),
            1 => json!(["u", 1, null, stamp(rng), order(rng)]),
            _ => json!(["n", 0, null, [0, 0, ""], order(rng), [[-1, 1000]]]),
        };
        entries.insert(key, entry);
    }
    let mut doc = json!({"format": 4, "kind": "delivery", "provider": "simkl", "account": "4812736", "title": {"type": "tv", "id": 1399}, "season": 1, "entries": entries});
    unknowns(rng, doc.as_object_mut().unwrap(), "u");
    doc
}

fn m(a: &Value, b: &Value) -> Value {
    doc_merge(a, b).unwrap()
}

fn laws(make: fn(&mut Rng) -> Value, label: &str) {
    for seed in 0..3000u64 {
        let mut rng = Rng(seed);
        let (a, b, c) = (make(&mut rng), make(&mut rng), make(&mut rng));
        let ab = m(&a, &b);
        assert!(
            jcs::same(&ab, &m(&b, &a)),
            "{label} commutative, seed {seed}"
        );
        assert!(
            jcs::same(&m(&ab, &c), &m(&a, &m(&b, &c))),
            "{label} associative, seed {seed}\n{a}\n{b}\n{c}"
        );
        let aa = m(&a, &a);
        assert!(
            jcs::same(&m(&aa, &a), &aa),
            "{label} idempotent, seed {seed}"
        );
        assert!(
            jcs::same(&m(&ab, &ab), &ab),
            "{label} idempotent, seed {seed}"
        );
        assert!(jcs::same(&m(&ab, &b), &ab), "{label} absorbs, seed {seed}");
    }
}

#[test]
fn title_documents_merge_as_a_join() {
    laws(title, "title");
}

#[test]
fn season_documents_merge_as_a_join() {
    laws(season, "season");
}

#[test]
fn delivery_documents_merge_as_a_join() {
    laws(delivery, "delivery");
}

#[test]
fn merging_a_written_document_with_itself_changes_nothing() {
    // A document as a writer writes it (normalized plays) is a fixed point, not only its self-merge.
    for seed in 0..500u64 {
        let mut rng = Rng(seed);
        for doc in [title(&mut rng), season(&mut rng), delivery(&mut rng)] {
            assert!(jcs::same(&m(&doc, &doc), &doc), "seed {seed}: {doc}");
        }
    }
}
