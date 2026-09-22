//! The den-spec contract test: read `vectors/store-v2.store` and check it against `store-v2.json` —
//! and `store-v1.*`, the frozen last store-v1 output, which this reader still accepts.
//!
//! This is the mechanism that keeps a Python writer in den-dataset and this Rust reader in step. Without
//! it the spec drifted from the writer unnoticed — it claimed the content hash was xxHash64 when the
//! writer used blake2b-64, which would have made every store we ship unreadable by a reader that believed
//! the document.
//!
//! The fixture lives in den-spec. When it is not checked out these tests FAIL — see `fixture_or_fail!`.

use den_store::{Store, NONE_U32};
use std::path::PathBuf;

/// `den-spec/vectors/`, as a sibling checkout or via `DEN_SPEC_DIR`.
fn spec_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("DEN_SPEC_DIR") {
        // Checked, not trusted: a DEN_SPEC_DIR pointing nowhere used to return Some and then fall out
        // of the read below as a skip, so a mistyped path reported five passes.
        let dir = PathBuf::from(dir).join("vectors");
        return dir.is_dir().then_some(dir);
    }
    let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../den-spec/vectors")
        .canonicalize()
        .ok()?;
    sibling.is_dir().then_some(sibling)
}

fn fixture(stem: &str) -> Option<(Vec<u8>, serde_json::Value)> {
    let dir = spec_dir()?;
    let store = std::fs::read(dir.join(format!("{stem}.store"))).ok()?;
    let expected = std::fs::read_to_string(dir.join(format!("{stem}.json"))).ok()?;
    Some((store, serde_json::from_str(&expected).ok()?))
}

/// The fixture, or a FAILURE.
///
/// This used to `return` when den-spec was absent, on the reasoning that skipping beats a false pass.
/// It was itself the false pass: `cargo test` swallows stderr without `--nocapture`, so
/// `DEN_SPEC_DIR=/nonexistent cargo test` printed "5 passed" and enforced nothing. A contract test that
/// cannot find its contract has not verified anything, and must say so the only way a test can.
///
/// `DEN_SPEC_OPTIONAL=1` is the deliberate escape, for a checkout that genuinely has no den-spec. It has
/// to be set on purpose, which is the whole difference.
macro_rules! fixture_or_fail {
    () => {
        fixture_or_fail!("store-v2")
    };
    ($stem:literal) => {
        match fixture($stem) {
            Some(pair) => pair,
            // `== "1"`, not `is_ok()`: `DEN_SPEC_OPTIONAL=0`, set to turn skipping OFF, would
            // otherwise turn it on.
            None if std::env::var("DEN_SPEC_OPTIONAL").as_deref() == Ok("1") => {
                eprintln!("SKIP: den-spec absent and DEN_SPEC_OPTIONAL=1");
                return;
            }
            None => panic!(
                "den-spec/vectors/{}.* not found — this test verifies the format contract and \
                 cannot do so without it. Check out den-spec beside this repo, set DEN_SPEC_DIR, or set \
                 DEN_SPEC_OPTIONAL=1 to skip deliberately.",
                $stem
            ),
        }
    };
}

#[test]
fn header_matches_the_spec_vectors() {
    let (bytes, expected) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("the fixture opens");
    let header = &expected["header"];

    assert_eq!(store.rows() as u64, header["rowCount"].as_u64().unwrap());
    assert_eq!(
        store.dataset_version(),
        header["datasetVersion"].as_str().unwrap()
    );
    assert_eq!(bytes.len() as u64, header["bytes"].as_u64().unwrap());
    assert_eq!(
        den_store::FORMAT_VERSION as u64,
        header["formatVersion"].as_u64().unwrap(),
        "this build reads a different format version than the fixture was written with"
    );
    assert_eq!(
        store.format_version() as u64,
        header["formatVersion"].as_u64().unwrap()
    );
}

/// store-v2's one change: `franchise` is a list, every series in the facts' order, most specific first.
///
/// store-v1 kept the first alone, and 219 real titles are in more than one series — some meet their
/// siblings only through the second (oxyc/den-atlas#43). The fixture's movie:1 is in two, written
/// Q114 then Q105, so a reader that sorted them, or kept only one, fails here.
#[test]
fn franchise_is_every_series_in_order() {
    let (bytes, expected) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    assert_eq!(store.format_version(), 2);
    let franchises = store.franchises().expect("franchise_v / franchise_o");
    assert!(
        store.per_row::<u32>("franchise").is_err(),
        "store-v2 has no single-valued franchise column"
    );
    for row in expected["rows"].as_array().unwrap() {
        let i = row["row"].as_u64().unwrap() as usize;
        let want: Vec<u32> = row["franchise"]
            .as_array()
            .unwrap_or_else(|| panic!("{} states its franchise list", row["key"]))
            .iter()
            .map(|q| q.as_u64().unwrap() as u32)
            .collect();
        assert_eq!(
            franchises.get(den_store::Row(i)),
            want.as_slice(),
            "{} franchise",
            row["key"]
        );
    }
    assert!(franchises.get(den_store::Row(usize::MAX)).is_empty());
}

/// A store published before store-v2 still opens, and its single `franchise` column reads as a list
/// of zero or one — so atlas can be deployed before the dataset that writes v2, and a rollback of the
/// dataset does not take it down.
#[test]
fn a_store_v1_file_reads_its_franchise_as_a_list_of_one() {
    let (bytes, _) = fixture_or_fail!("store-v1");
    let store = Store::open(&bytes).expect("a v1 store is still read");
    assert_eq!(store.format_version(), 1);
    let franchises = store.franchises().expect("v1's franchise column");
    // movie:1 was written with Q105, and the other two with none.
    let row = |media, id| store.row_of(media, id).unwrap().expect("in the fixture");
    assert_eq!(franchises.get(row(0, 1)), &[105]);
    assert!(franchises.get(row(0, 2)).is_empty(), "u32::MAX is none");
    assert!(franchises.get(row(1, 10)).is_empty());
    assert!(franchises.get(den_store::Row(store.rows())).is_empty());
    assert!(
        store.list::<u32>("franchise_v", "franchise_o").is_err(),
        "a v1 store has no list"
    );
}

#[test]
fn a_flipped_bit_is_refused() {
    let (bytes, _) = fixture_or_fail!();
    let mut corrupt = bytes.clone();
    // Past the header, so the hash is the only thing that can catch it — which is the point: a
    // structural validator accepted 339 of 400 such flips and answered 77 of them wrongly.
    let at = corrupt.len() / 2;
    corrupt[at] ^= 0x01;
    assert!(
        matches!(
            Store::open(&corrupt),
            Err(den_store::StoreError::Corrupt { .. })
        ),
        "a single flipped bit past the header must be refused, not read"
    );
}

#[test]
fn rows_are_sorted_by_packed_key_and_findable() {
    let (bytes, expected) = fixture_or_fail!();
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
    assert!(
        keys.windows(2).all(|w| w[0] < w[1]),
        "keys must be strictly ascending"
    );
}

/// `card_poster` is store-v1's one OPTIONAL section, and this pins both halves of that.
///
/// The producer stopped writing it: a poster path is licensed vendor content and the store is a public
/// artifact (oxyc/den#118). So a reader must answer "no posters" rather than fail — and the failure that
/// gates this is not hypothetical. den-atlas read the section alongside the title and the year with `?`,
/// which meant a store without it produced no cards at all, and a caller that turned the error into an
/// empty map: browse rows empty, search with no display titles, `/health` green.
///
/// The second half matters as much: the paths are not merely unreferenced, they are NOT IN THE FILE. The
/// writer was handed `posterPath` for two of these three titles, and dropping the section while still
/// interning the strings would have left both in the published bytes with nothing pointing at them.
#[test]
fn a_store_without_the_optional_poster_section_reads_as_having_no_posters() {
    let (bytes, _) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    assert!(
        store.per_row::<u32>("card_poster").is_err(),
        "the fixture still carries card_poster, so this contract is untested"
    );
    // `votes` went the same way and for the same reason, but it is NOT optional — it is gone. A reader
    // that ordered rows by it needs a source of its own; den-atlas joins IMDb's public dump on `imdb`.
    assert!(
        store.per_row::<u32>("votes").is_err(),
        "the fixture still carries votes, so nothing here proves a reader can do without it"
    );
    // Everything else about a card is unaffected by its absence.
    assert_eq!(
        store.per_row::<u32>("card_title").unwrap().len(),
        store.rows()
    );
    assert_eq!(
        store.per_row::<i16>("card_year").unwrap().len(),
        store.rows()
    );

    let haystack = String::from_utf8_lossy(&bytes);
    for path in ["/alpha.jpg", "/gamma.jpg"] {
        assert!(
            !haystack.contains(path),
            "{path} survived in the published bytes"
        );
    }
}

#[test]
fn cards_scores_and_genres_read_as_the_vectors_say() {
    let (bytes, expected) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    let strings = store.strings().expect("string table");
    let titles = store.per_row::<u32>("card_title").unwrap();
    let years = store.per_row::<i16>("card_year").unwrap();
    let intensity = store.per_row::<u16>("score_intensity").unwrap();
    let genres = store.list::<u32>("genres_v", "genres_o").unwrap();

    for row in expected["rows"].as_array().unwrap() {
        let i = row["row"].as_u64().unwrap() as usize;
        let key = row["key"].as_str().unwrap();

        assert_eq!(
            strings.get(titles[i]),
            row["cardTitle"].as_str(),
            "{key} card title"
        );
        assert!(
            row.get("cardPoster").is_none(),
            "{key}: the vectors still describe a poster, so this reader is out of step with the spec"
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
            let got: Vec<u64> = den_store::Row(i)
                .pipe(|r| genres.get(r))
                .iter()
                .map(|&g| g as u64)
                .collect();
            let want: Vec<u64> = want.iter().map(|g| g.as_u64().unwrap()).collect();
            assert_eq!(
                got, want,
                "{key} genres — one Q-id can map to several TMDB ids"
            );
        }
    }
}

/// The entity table, and the aliases people search reads as well as the name.
///
/// A reader that indexes only `ent_name` answers "Cyrus Actor" with nothing while "Cy Actor" works —
/// which on the real corpus is 118,958 names across 64,075 of 162,812 entities. The aliases are STRINGS
/// here, not hashes: folding is the reader's own `name_key`, and a writer producing those hashes would be
/// a second copy of that algorithm.
#[test]
fn entity_aliases_resolve() {
    let (bytes, expected) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    let strings = store.strings().expect("strings");
    let qids = store.column::<u32>("ent_qid").expect("ent_qid");
    let names = store.column::<u32>("ent_name").expect("ent_name");
    let tmdb = store.column::<u32>("ent_tmdb").expect("ent_tmdb");
    let aliases = store
        .list_of::<u32>("ent_alias_v", "ent_alias_o", qids.len())
        .expect("ent_alias");

    for want in expected["entities"].as_array().unwrap() {
        let qid = want["qid"].as_u64().unwrap() as u32;
        let at = qids.iter().position(|&q| q == qid).expect("entity present");
        assert_eq!(strings.get(names[at]), want["name"].as_str(), "Q{qid} name");
        match want["tmdbPersonId"].as_u64() {
            Some(id) => assert_eq!(u64::from(tmdb[at]), id, "Q{qid} tmdb person id"),
            None => assert_eq!(tmdb[at], NONE_U32, "Q{qid} has no tmdb person id"),
        }
        let got: Vec<&str> = aliases
            .get(den_store::Row(at))
            .iter()
            .filter_map(|&id| strings.get(id))
            .collect();
        let want: Vec<&str> = want["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(got, want, "Q{qid} aliases");
    }
}

#[test]
fn a_facts_only_row_reads_as_absent_not_as_zero() {
    let (bytes, expected) = fixture_or_fail!();
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
    assert_eq!(
        strings.get(primary[i]),
        None,
        "and it resolves to nothing, not to a string"
    );
    assert!(makers.get(den_store::Row(i)).is_empty(), "no makers");
    assert_eq!(
        strings.get(NONE_U32),
        None,
        "the absent sentinel never resolves"
    );
}

/// `released` is DAYS SINCE 1970-01-01, with its precision in a column of its own.
///
/// This is the fix that started the rework — the column was 100% sentinel because the corpus value is
/// `{date, precision}` and the writer read it as an int. The fixture pinned the right answer and nothing
/// asserted it, which is how a headline fix goes unprotected. More than half of real dated rows are
/// year-precision, so a reader that ignores the precision column dates them all to 1 January.
#[test]
fn released_is_days_since_epoch_with_its_precision() {
    let (bytes, expected) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    let released = store.per_row::<i32>("released").expect("released");
    let precision = store.per_row::<u8>("released_prec").expect("released_prec");

    for row in expected["rows"].as_array().unwrap() {
        let i = row["row"].as_u64().unwrap() as usize;
        let key = row["key"].as_str().unwrap();
        match row["released"].as_i64() {
            Some(want) => {
                assert_eq!(released[i] as i64, want, "{key} released");
                if let Some(prec) = row["releasedPrecision"].as_u64() {
                    assert_eq!(precision[i] as u64, prec, "{key} precision");
                }
            }
            None if row.get("released").is_some_and(|v| v.is_null()) => {
                assert_eq!(released[i], i32::MIN, "{key} has no date and must say so");
            }
            None => {}
        }
    }
}

/// The 12 facet axes, in order, and a declined one reading as ABSENT.
///
/// Swapping two axes, or treating `does-not-apply` as a value, are both invisible without this: the
/// store would still be structurally perfect and every row would still have twelve entries.
#[test]
fn facets_keep_their_axis_order_and_declines_are_absent() {
    let (bytes, expected) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    let strings = store.strings().expect("strings");
    let values = store.column::<u32>("facet_v").expect("facet_v");
    let confs = store.column::<u8>("facet_c").expect("facet_c");
    let axes = den_store::FACET_AXES.len();

    for row in expected["rows"].as_array().unwrap() {
        let i = row["row"].as_u64().unwrap() as usize;
        let key = row["key"].as_str().unwrap();

        if let Some(want) = row.get("facets").and_then(|f| f.as_object()) {
            for (name, pair) in want {
                let axis = den_store::FACET_AXES
                    .iter()
                    .position(|a| a == name)
                    .unwrap_or_else(|| panic!("{name} is not a facet axis"));
                let at = i * axes + axis;
                let pair = pair.as_array().expect("[value, confidence]");
                assert_eq!(
                    strings.get(values[at]),
                    pair[0].as_str(),
                    "{key} {name} — a wrong axis order shows up here and nowhere else"
                );
                assert_eq!(
                    confs[at] as u64,
                    pair[1].as_u64().unwrap(),
                    "{key} {name} confidence"
                );
            }
        }
        for name in row
            .get("facetsAbsent")
            .and_then(|a| a.as_array())
            .unwrap_or(&vec![])
        {
            let name = name.as_str().unwrap();
            let axis = den_store::FACET_AXES
                .iter()
                .position(|a| a == &name)
                .unwrap();
            assert_eq!(
                values[i * axes + axis],
                NONE_U32,
                "{key} {name} was declined and must be absent, not a value"
            );
        }
    }
}

/// The vectors, re-ordered from their own labels-file order into the store's sorted-key order.
///
/// The fixture generator's own comment says getting this wrong "is invisible in normal use": every row
/// would still hold 1024 plausible bytes, just the wrong title's. The fixture builds each row from a
/// known fill, so a shift of even one row is caught.
#[test]
fn vectors_are_reordered_into_key_order() {
    let (bytes, expected) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    let plot = store.column::<i8>("vec_plot").expect("vec_plot");
    let has_premise = store
        .per_row::<u8>("vec_premise_has")
        .expect("vec_premise_has");
    let premise = store.column::<i8>("vec_premise").expect("vec_premise");
    let dims = plot.len() / store.rows();

    for row in expected["rows"].as_array().unwrap() {
        let i = row["row"].as_u64().unwrap() as usize;
        let key = row["key"].as_str().unwrap();
        let span = &plot[i * dims..(i + 1) * dims];

        if row["hasPlotVector"].as_bool() == Some(true) {
            assert!(
                span.iter().any(|&b| b != 0),
                "{key} should carry a plot vector"
            );
        } else {
            assert!(
                span.iter().all(|&b| b == 0),
                "{key} has none, so its row must be zero-filled"
            );
        }

        let want = row["hasPremiseVector"].as_bool() == Some(true);
        assert_eq!(has_premise[i] == 1, want, "{key} premise flag");
        let span = &premise[i * dims..(i + 1) * dims];
        assert_eq!(
            span.iter().any(|&b| b != 0),
            want,
            "{key} premise vector must agree with its own flag"
        );
    }
}

/// A column read at the wrong element width must be REFUSED, not silently reinterpreted.
///
/// This is the one the audit demonstrated on a real store: `column::<u16>("card_title")` returned
/// 95,236 elements with `first = 64060` and no error, because half of a `u32` is a perfectly aligned,
/// whole `u16`. Alignment and length checks cannot see it; only the width the writer declared can.
#[test]
fn a_column_read_at_the_wrong_width_is_refused() {
    let (bytes, _) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");

    assert!(
        store.column::<u32>("card_title").is_ok(),
        "the declared width reads"
    );
    assert!(
        matches!(
            store.column::<u16>("card_title"),
            Err(den_store::StoreError::WidthMismatch { .. })
        ),
        "a u32 column read as u16 must be an error, not twice as many wrong numbers"
    );
    assert!(
        matches!(
            store.column::<u8>("keys"),
            Err(den_store::StoreError::WidthMismatch { .. })
        ),
        "and the same for the key column, which addresses everything else"
    );
}

/// A row number nothing can describe yields an empty span rather than panicking.
#[test]
fn an_impossible_row_is_empty_not_a_panic() {
    let (bytes, _) = fixture_or_fail!();
    let store = Store::open(&bytes).expect("opens");
    let makers = store.list::<u32>("makers_v", "makers_o").expect("makers");

    // `row.0 + 1` overflowed here and panicked in a debug build, in a crate that forbids unsafe and
    // documents this as returning empty for a row it cannot describe.
    assert!(makers.get(den_store::Row(usize::MAX)).is_empty());
    assert!(makers.get(den_store::Row(store.rows() + 10)).is_empty());
}

/// Tiny helper so the genre assert above reads in one line.
trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl Pipe for den_store::Row {}
