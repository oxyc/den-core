//! Opening a projection (§15) part by part: `open_projection_compact` against `open_projection`, every refusal, and
//! the memory the streaming open needs for a large library.

use den_assistant::*;
use serde_json::{json, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts live and peak heap bytes, for the memory test.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Every test here takes this first, so the memory test counts only its own allocations.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

const LIBRARY: &str = "4c1b7a0e9d3f2c8b5a6e1d0f7c3b9a2e";
const GRANT: &str = "d7047d7faa4c6f77e1919c132bb6bd9f";
const KEY: [u8; 32] = [7; 32];

/// The size of den-core's large synthetic library — 5,000 on the watchlist, 500 in Continue Watching, 50,000 plays —
/// with ids and times that compress about as badly as a real library's.
fn large_view() -> Value {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        x >> 11
    };
    let now = 1_790_000_000_000u64;
    let watchlist: Vec<Value> = (0..5_000)
        .map(|_| json!({"title": {"type": "movie", "id": next() % 1_000_000}, "addedAt": now - next() % 1_000_000_000}))
        .collect();
    let continuing: Vec<Value> = (0..500)
        .map(|_| {
            json!({"title": {"type": "tv", "id": next() % 300_000}, "action": "resume", "season": next() % 10,
                "episode": next() % 30, "fraction": 0.123_456_789_012_345_67, "at": now - next() % 1_000_000_000})
        })
        .collect();
    let seen: Vec<Value> = (0..50_000)
        .map(|_| {
            json!({"title": {"type": "tv", "id": next() % 300_000}, "season": next() % 10, "episode": next() % 30,
                "at": now - next() % 10_000_000_000})
        })
        .collect();
    json!({"v": 1, "library": LIBRARY, "grant": GRANT, "at": now, "head": 4711,
        "watchlist": watchlist, "continue": continuing, "seen": seen})
}

fn sealed(view: &Value, random: u8) -> SealedProjection {
    seal_projection(&KEY, view, &[random; 32]).unwrap()
}

#[test]
fn the_compact_open_is_the_open_without_the_lists_parsed() {
    let _serial = serial();
    let view = large_view();
    let s = sealed(&view, 1);
    assert!(s.parts.len() > 1, "{} parts", s.parts.len());
    let compact = open_projection_compact(&KEY, LIBRARY, GRANT, &s.set, &s.parts).unwrap();
    assert_eq!(&*compact, &projection_plaintext(&view).unwrap());
    assert_eq!(
        open_projection(&KEY, LIBRARY, GRANT, &s.set, &s.parts).unwrap(),
        view
    );
}

#[test]
fn every_refusal_holds_for_both_opens() {
    let _serial = serial();
    let view = large_view();
    let s = sealed(&view, 1);
    let other = sealed(&view, 2);
    let n = s.parts.len();
    let mut mixed = s.parts.clone();
    mixed[1] = other.parts[1].clone();
    let mut swapped = s.parts.clone();
    swapped.swap(0, 1);
    let mut extra = s.parts.clone();
    extra.push(s.parts[n - 1].clone());
    let cases: Vec<(&str, [u8; 32], &str, &str, &str, Vec<String>)> = vec![
        (
            "a part of another publish",
            KEY,
            LIBRARY,
            GRANT,
            &s.set,
            mixed,
        ),
        (
            "another publish's set id",
            KEY,
            LIBRARY,
            GRANT,
            &other.set,
            s.parts.clone(),
        ),
        ("parts out of order", KEY, LIBRARY, GRANT, &s.set, swapped),
        (
            "a part missing",
            KEY,
            LIBRARY,
            GRANT,
            &s.set,
            s.parts[..n - 1].to_vec(),
        ),
        ("an extra part", KEY, LIBRARY, GRANT, &s.set, extra),
        ("no parts", KEY, LIBRARY, GRANT, &s.set, vec![]),
        (
            "the wrong key",
            [8; 32],
            LIBRARY,
            GRANT,
            &s.set,
            s.parts.clone(),
        ),
        (
            "another library",
            KEY,
            "9e2a7b3c4d5e6f708192a3b4c5d6e7f8",
            GRANT,
            &s.set,
            s.parts.clone(),
        ),
        (
            "another grant",
            KEY,
            LIBRARY,
            "00112233445566778899aabbccddeeff",
            &s.set,
            s.parts.clone(),
        ),
        (
            "a set id that is not one",
            KEY,
            LIBRARY,
            GRANT,
            "set",
            s.parts.clone(),
        ),
        (
            "a part that is not base64url",
            KEY,
            LIBRARY,
            GRANT,
            &s.set,
            vec!["not+base64".into()],
        ),
    ];
    for (name, key, library, grant, set, parts) in cases {
        assert!(
            open_projection_compact(&key, library, grant, set, &parts).is_err(),
            "{name}"
        );
        assert!(
            open_projection(&key, library, grant, set, &parts).is_err(),
            "{name}"
        );
    }
}

/// Seals arbitrary deflated bytes as a one-part set, to refuse what `seal_projection` never makes.
fn one_part(deflated: &[u8]) -> (String, Vec<String>) {
    use aes_gcm::aead::{Aead, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit};
    let set = "00112233445566778899aabbccddeeff".to_owned();
    let nonce = [1u8; 12];
    let ct = Aes256Gcm::new((&KEY).into())
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: deflated,
                aad: &projection_aad(LIBRARY, GRANT, &set, 0, 1),
            },
        )
        .unwrap();
    (set, vec![b64url(&[&nonce[..], &ct].concat())])
}

#[test]
fn malformed_plaintexts_are_refused() {
    let _serial = serial();
    let small = json!({"v": 1, "library": LIBRARY, "grant": GRANT, "at": 1, "head": 1, "watchlist": [],
        "continue": [], "seen": []});
    let plain = projection_plaintext(&small).unwrap();
    let deflated = miniz_oxide::deflate::compress_to_vec(&plain, 9);
    let (set, parts) = one_part(&deflated);
    assert_eq!(
        &*open_projection_compact(&KEY, LIBRARY, GRANT, &set, &parts).unwrap(),
        &plain
    );
    let newer = String::from_utf8(plain.clone())
        .unwrap()
        .replace("\"v\":1", "\"v\":2");
    let trailing = [&deflated[..], b"x"].concat();
    let bomb = miniz_oxide::deflate::compress_to_vec(&vec![b' '; MAX_PROJECTION_PLAINTEXT + 1], 9);
    for (name, deflated) in [
        (
            "a stream cut short",
            deflated[..deflated.len() / 2].to_vec(),
        ),
        ("bytes after the stream", trailing),
        ("not DEFLATE", vec![0xff; 64]),
        (
            "a newer version",
            miniz_oxide::deflate::compress_to_vec(newer.as_bytes(), 9),
        ),
        (
            "not JSON",
            miniz_oxide::deflate::compress_to_vec(b"watchlist", 9),
        ),
        ("past 64 MiB inflated", bomb),
    ] {
        let (set, parts) = one_part(&deflated);
        assert!(
            open_projection_compact(&KEY, LIBRARY, GRANT, &set, &parts).is_err(),
            "{name}"
        );
        assert!(
            open_projection(&KEY, LIBRARY, GRANT, &set, &parts).is_err(),
            "{name}"
        );
    }
}

/// The compact open's peak heap, past what the sealed parts already take, is about the plaintext it returns — not
/// the deflated set plus a tree of the lists.
#[test]
fn the_compact_open_needs_little_more_than_its_plaintext() {
    let _serial = serial();
    let view = large_view();
    let s = sealed(&view, 1);
    let plain_len = projection_plaintext(&view).unwrap().len();
    drop(view);
    let measure = |open: &dyn Fn()| {
        let base = LIVE.load(Ordering::SeqCst);
        PEAK.store(base, Ordering::SeqCst);
        open();
        PEAK.load(Ordering::SeqCst) - base
    };
    let compact = measure(&|| {
        open_projection_compact(&KEY, LIBRARY, GRANT, &s.set, &s.parts).unwrap();
    });
    let expanded = measure(&|| {
        open_projection(&KEY, LIBRARY, GRANT, &s.set, &s.parts).unwrap();
    });
    let sealed_len: usize = s.parts.iter().map(String::len).sum();
    eprintln!(
        "large projection: {} parts, {sealed_len} characters sealed, {plain_len} bytes of plaintext; \
         peak heap: compact open {compact} bytes, expanded open {expanded} bytes",
        s.parts.len()
    );
    // Growing by half again holds the old and the new buffer for a moment: at most 2.5 times the plaintext. The rest
    // is one part being opened, the inflater and its scratch.
    assert!(compact <= plain_len * 5 / 2 + 4 * MAX_PART, "{compact}");
    assert!(compact * 10 < expanded, "{compact} vs {expanded}");
}
