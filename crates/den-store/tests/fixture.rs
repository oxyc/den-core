//! The den-spec contract test: read `vectors/store-v1.store` and check it against `store-v1.json`.
//!
//! This is the mechanism that keeps a Python writer in den-dataset and this Rust reader in step. Without
//! it the spec drifted from the writer unnoticed — it claimed the content hash was xxHash64 when the
//! writer used blake2b-64, which would have made every store we ship unreadable by a reader that believed
//! the document.
//!
//! The fixture lives in den-spec. When it is not checked out the test SKIPS rather than passes: a
//! contract test that silently reports success when the contract is absent is worse than no test.

use den_store::{Store, NONE_U32};
use std::path::PathBuf;

/// `den-spec/vectors/`, as a sibling checkout or via `DEN_SPEC_DIR`.
fn spec_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("DEN_SPEC_DIR") {
        return Some(PathBuf::from(dir).join("vectors"));
    }
    let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../den-spec/vectors")
        .canonicalize()
        .ok()?;
    sibling.is_dir().then_some(sibling)
}

fn fixture() -> Option<(Vec<u8>, serde_json::Value)> {
    let dir = spec_dir()?;
    let store = std::fs::read(dir.join("store-v1.store")).ok()?;
    let expected = std::fs::read_to_string(dir.join("store-v1.json")).ok()?;
    Some((store, serde_json::from_str(&expected).ok()?))
}

macro_rules! fixture_or_skip {
    () => {
        match fixture() {
            Some(pair) => pair,
            None => {
                eprintln!("SKIP: den-spec/vectors/store-v1.* not found — set DEN_SPEC_DIR");
                return;
            }
        }
    };
}

#[test]
fn header_matches_the_spec_vectors() {
    let (bytes, expected) = fixture_or_skip!();
    let store = Store::open(&bytes).expect("the fixture opens");
    let header = &expected["header"];

    assert_eq!(store.rows() as u64, header["rowCount"].as_u64().unwrap());
    assert_eq!(store.dataset_version(), header["datasetVersion"].as_str().unwrap());
    assert_eq!(bytes.len() as u64, header["bytes"].as_u64().unwrap());
    assert_eq!(
        den_store::FORMAT_VERSION as u64,
        header["formatVersion"].as_u64().unwrap(),
        "this build reads a different format version than the fixture was written with"
    );
}

#[test]
fn a_flipped_bit_is_refused() {
    let (bytes, _) = fixture_or_skip!();
    let mut corrupt = bytes.clone();
    // Past the header, so the hash is the only thing that can catch it — which is the point: a
    // structural validator accepted 339 of 400 such flips and answered 77 of them wrongly.
    let at = corrupt.len() / 2;
    corrupt[at] ^= 0x01;
    assert!(
        matches!(Store::open(&corrupt), Err(den_store::StoreError::Corrupt { .. })),
        "a single flipped bit past the header must be refused, not read"
    );
}

#[test]
fn rows_are_sorted_by_packed_key_and_findable() {
    let (bytes, expected) = fixture_or_skip!();
    let store = Store::open(&bytes).expect("opens");
    for row in expected["rows"].as_array().unwrap() {
        let media = row["media"].as_u64().unwrap() as u8;
        let tmdb = row["tmdbId"].as_u64().unwrap() as u32;
        let found = store.row_of(media, tmdb).expect("keys readable");
        assert_eq!(
            found.map(|r| r.0),
            Some(row["row"].as_u64().unwrap() as usize),
            "{} should be at the row the vectors say",
            row["key"].as_str().unwrap()
        );
    }
    // Movie rows precede tv rows because the media bit is the high half of the packed key.
    let keys = store.per_row::<u64>("keys").unwrap();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "keys must be strictly ascending");
}

#[test]
fn cards_scores_and_genres_read_as_the_vectors_say() {
    let (bytes, expected) = fixture_or_skip!();
    let store = Store::open(&bytes).expect("opens");
    let strings = store.strings().expect("string table");
    let titles = store.per_row::<u32>("card_title").unwrap();
    let posters = store.per_row::<u32>("card_poster").unwrap();
    let years = store.per_row::<i16>("card_year").unwrap();
    let intensity = store.per_row::<u16>("score_intensity").unwrap();
    let genres = store.list::<u32>("genres_v", "genres_o").unwrap();

    for row in expected["rows"].as_array().unwrap() {
        let i = row["row"].as_u64().unwrap() as usize;
        let key = row["key"].as_str().unwrap();

        assert_eq!(strings.get(titles[i]), row["cardTitle"].as_str(), "{key} card title");
        assert_eq!(
            strings.get(posters[i]),
            row["cardPoster"].as_str(),
            "{key} poster — None must read as None, not as a string"
        );
        if let Some(year) = row["cardYear"].as_i64() {
            assert_eq!(years[i] as i64, year, "{key} year");
        }
        // Scores are HUNDREDTHS of a 0..4 axis. 321 is 3.21; it is not twentieths, which is what
        // silently rounded half the corpus before this was measured.
        if let Some(want) = row.get("scores").and_then(|s| s["intensity"].as_u64()) {
            assert_eq!(intensity[i] as u64, want, "{key} intensity");
        }
        if let Some(want) = row.get("genres").and_then(|g| g.as_array()) {
            let got: Vec<u64> = den_store::Row(i).pipe(|r| genres.get(r)).iter().map(|&g| g as u64).collect();
            let want: Vec<u64> = want.iter().map(|g| g.as_u64().unwrap()).collect();
            assert_eq!(got, want, "{key} genres — one Q-id can map to several TMDB ids");
        }
    }
}

#[test]
fn a_facts_only_row_reads_as_absent_not_as_zero() {
    let (bytes, expected) = fixture_or_skip!();
    let store = Store::open(&bytes).expect("opens");
    let strings = store.strings().unwrap();
    let primary = store.per_row::<u32>("primary_genre").unwrap();
    let makers = store.list::<u32>("makers_v", "makers_o").unwrap();

    // The row that exists in facts and in no label or vector file. 89 of these are in the real corpus,
    // and they are what a reader is most likely to mishandle — an absent value must not read as row 0's.
    let row = expected["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "movie:2")
        .expect("the fixture has a facts-only row");
    let i = row["row"].as_u64().unwrap() as usize;

    assert_eq!(primary[i], NONE_U32, "no primary genre");
    assert_eq!(strings.get(primary[i]), None, "and it resolves to nothing, not to a string");
    assert!(makers.get(den_store::Row(i)).is_empty(), "no makers");
    assert_eq!(strings.get(NONE_U32), None, "the absent sentinel never resolves");
}

/// Tiny helper so the genre assert above reads in one line.
trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl Pipe for den_store::Row {}
