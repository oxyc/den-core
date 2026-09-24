//! Reads `den-<version>.store` — den-spec `wire/store-v2.md`, and the `wire/store-v1.md` stores
//! published before it.
//!
//! Pure, like the rest of this workspace: the decoder takes a byte slice and returns borrowed views of
//! it. It never opens a file, maps memory, or allocates a copy of the data. The one impure step — mmap —
//! belongs to whoever owns the bytes: den-atlas maps the file and hands the slice here.
//!
//! That split is what lets one implementation serve both consumers. den-atlas is a native server;
//! the tuning playground is `wasm32-unknown-unknown` in a browser. `memmap2` compiles for neither the
//! browser nor tvOS, so a decoder that owned its mapping would force a second decoder to exist — and two
//! implementations of one binary format is precisely the drift this workspace exists to prevent.
//!
//! # Safety posture
//!
//! There is no `unsafe` in this crate. Every section is reached through `zerocopy`, which checks length
//! **and alignment** before handing back a typed slice. That matters more than it sounds: a content hash
//! catches corruption but says nothing about alignment, and an unaligned load happens to work on both
//! x86-64 and aarch64 — so the undefined behaviour would pass every test we could write.
//!
//! The hash is checked before any section is read, by [`Store::open`]. Measured on this format's
//! predecessor, a structural validator rejected 39 of 400 single-bit flips and let 77 return wrong
//! answers; a 64-bit content hash caught 400 of 400. Structure-checking is not integrity-checking.

#![forbid(unsafe_code)]

use core::fmt;
use zerocopy::{FromBytes, Immutable, KnownLayout};

/// The newest layout this decoder understands. A store declaring anything outside
/// `OLDEST_FORMAT_VERSION..=FORMAT_VERSION` is refused: a format change is a new version, never a
/// reinterpretation of the same bytes.
///
/// 2 made `franchise` a list (`franchise_v`/`franchise_o`). store-v1 differs in that column alone, so
/// it is still read, and [`Store::franchises`] answers the same question for both.
pub const FORMAT_VERSION: u32 = 2;
/// The oldest layout still read. store-v1 files stay readable so a reader can ship before the writer
/// does: the dataset is published separately, and a reader that refused v1 could not be deployed
/// until a v2 store existed.
pub const OLDEST_FORMAT_VERSION: u32 = 1;

const MAGIC: &[u8; 8] = b"DENSTOR1";
const ENDIAN_CHECK: u32 = 0x0102_0304;
const HEADER_BYTES: usize = 64;
const ENTRY_BYTES: usize = 32;
const NAME_BYTES: usize = 16;

/// Absent, for a `u32` id column.
pub const NONE_U32: u32 = u32::MAX;
/// Absent, for the `card_year` column.
pub const NONE_I16: i16 = i16::MIN;

/// The 12 facet axes, in the order `facet_v` and `facet_c` store them.
pub const FACET_AXES: [&str; 12] = [
    "era",
    "setting",
    "scope",
    "ending",
    "pacing",
    "chronology",
    "continuity",
    "conflict",
    "ensemble",
    "tone",
    "timespan",
    "archetype",
];

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    TooSmall {
        need: usize,
        got: usize,
    },
    BadMagic,
    /// A store written by a build that does not share this one's layout.
    Version {
        found: u32,
        expected: u32,
    },
    /// The writer's endianness marker did not survive the round trip.
    Endianness {
        found: u32,
    },
    /// The content hash did not match. The file is corrupt or truncated; nothing was read from it.
    Corrupt {
        found: u64,
        computed: u64,
    },
    SectionTable,
    /// A section's declared extent is not inside the file.
    SectionBounds {
        name: [u8; NAME_BYTES],
    },
    MissingSection(&'static str),
    /// A section exists but is not a whole number of its own elements, or is misaligned for them.
    BadSection {
        name: &'static str,
    },
    /// A column was asked for at a different element width than the writer declared. A `u32` column
    /// read as `u16` is aligned and whole, so nothing else catches it.
    WidthMismatch {
        name: &'static str,
        declared: u32,
        requested: u32,
    },
    /// A row count that does not agree with a section's length.
    RowMismatch {
        name: &'static str,
        rows: usize,
        found: usize,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall { need, got } => {
                write!(f, "store is {got} bytes, needs at least {need}")
            }
            Self::BadMagic => write!(f, "not a den store (bad magic)"),
            Self::Version { found, expected } => {
                write!(
                    f,
                    "store format version {found}, this build reads {OLDEST_FORMAT_VERSION} to \
                     {expected}"
                )
            }
            Self::Endianness { found } => {
                write!(f, "endianness marker {found:#x} — foreign writer")
            }
            Self::Corrupt { found, computed } => {
                write!(
                    f,
                    "content hash {found:#018x} but the bytes hash to {computed:#018x}"
                )
            }
            Self::SectionTable => write!(f, "section table does not fit in the file"),
            Self::SectionBounds { name } => {
                write!(f, "section {} extends past the end of the file", show(name))
            }
            Self::MissingSection(name) => write!(f, "store has no {name} section"),
            Self::BadSection { name } => {
                write!(f, "section {name} is misaligned or a partial element")
            }
            Self::WidthMismatch {
                name,
                declared,
                requested,
            } => write!(
                f,
                "section {name} holds {declared}-byte elements, read as {requested}-byte"
            ),
            Self::RowMismatch { name, rows, found } => {
                write!(f, "section {name} holds {found} elements for {rows} rows")
            }
        }
    }
}

fn show(name: &[u8; NAME_BYTES]) -> &str {
    let end = name.iter().position(|&b| b == 0).unwrap_or(NAME_BYTES);
    core::str::from_utf8(&name[..end]).unwrap_or("?")
}

#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
struct RawEntry {
    name: [u8; NAME_BYTES],
    offset: u64,
    length: u32,
    width: u32,
}

/// One title, as a row number. Every column is addressed by it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Row(pub usize);

/// A decoded store. Holds only the borrowed slice and its section table — opening one copies nothing.
///
/// `Debug` prints what identifies a store, never its contents: 124 MB in a test failure helps nobody.
pub struct Store<'a> {
    bytes: &'a [u8],
    entries: &'a [RawEntry],
    rows: usize,
    dataset_version: &'a str,
    format_version: u32,
}

impl<'a> Store<'a> {
    /// Verify and open. The content hash is checked here, before any section is addressed, so a caller
    /// that gets a `Store` back is holding bytes that have already been proven whole.
    pub fn open(bytes: &'a [u8]) -> Result<Self, StoreError> {
        if bytes.len() < HEADER_BYTES {
            return Err(StoreError::TooSmall {
                need: HEADER_BYTES,
                got: bytes.len(),
            });
        }
        if &bytes[..8] != MAGIC {
            return Err(StoreError::BadMagic);
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if !(OLDEST_FORMAT_VERSION..=FORMAT_VERSION).contains(&version) {
            return Err(StoreError::Version {
                found: version,
                expected: FORMAT_VERSION,
            });
        }
        let endian = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if endian != ENDIAN_CHECK {
            return Err(StoreError::Endianness { found: endian });
        }
        let declared = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let count = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
        let rows = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;

        let computed = hash(&bytes[HEADER_BYTES..]);
        if computed != declared {
            return Err(StoreError::Corrupt {
                found: declared,
                computed,
            });
        }

        let table_end = HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(ENTRY_BYTES)
                    .ok_or(StoreError::SectionTable)?,
            )
            .ok_or(StoreError::SectionTable)?;
        if table_end > bytes.len() {
            return Err(StoreError::SectionTable);
        }
        let entries = <[RawEntry]>::ref_from_bytes(&bytes[HEADER_BYTES..table_end])
            .map_err(|_| StoreError::SectionTable)?;

        for entry in entries {
            let end = entry.offset.checked_add(u64::from(entry.length));
            if end.is_none_or(|e| e > bytes.len() as u64) {
                return Err(StoreError::SectionBounds { name: entry.name });
            }
        }

        let raw_version = &bytes[32..48];
        let end = raw_version
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(raw_version.len());
        let dataset_version = core::str::from_utf8(&raw_version[..end]).unwrap_or("");

        Ok(Self {
            bytes,
            entries,
            rows,
            dataset_version,
            format_version: version,
        })
    }

    /// Titles in the store.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The layout the writer declared: 1 or 2.
    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    /// The `datasetVersion` the writer stamped, so a caller can check the store against a manifest.
    pub fn dataset_version(&self) -> &'a str {
        self.dataset_version
    }

    pub fn section(&self, name: &'static str) -> Result<&'a [u8], StoreError> {
        Ok(self.entry(name)?.0)
    }

    /// A section's bytes and the element width the writer declared for it.
    fn entry(&self, name: &'static str) -> Result<(&'a [u8], u32), StoreError> {
        let entry = self
            .entries
            .iter()
            .find(|e| show(&e.name) == name)
            .ok_or(StoreError::MissingSection(name))?;
        let start = entry.offset as usize;
        Ok((
            &self.bytes[start..start + entry.length as usize],
            entry.width,
        ))
    }

    /// A typed column, at the width the WRITER declared for it.
    ///
    /// `zerocopy` refuses a slice that is misaligned or not a whole number of elements — but a `u32`
    /// column read as `u16` is both aligned and whole, so it succeeded and returned twice as many
    /// wrong numbers. Measured on a real store: `column::<u16>("card_title")` returned 95,236 elements
    /// with `first = 64060`, no error. The section table carries `width` for exactly this, and nothing
    /// was reading it.
    pub fn column<T>(&self, name: &'static str) -> Result<&'a [T], StoreError>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        let (bytes, width) = self.entry(name)?;
        let want = core::mem::size_of::<T>() as u32;
        if width != want {
            return Err(StoreError::WidthMismatch {
                name,
                declared: width,
                requested: want,
            });
        }
        <[T]>::ref_from_bytes(bytes).map_err(|_| StoreError::BadSection { name })
    }

    /// A column with one element per title, refused if it does not have exactly that.
    pub fn per_row<T>(&self, name: &'static str) -> Result<&'a [T], StoreError>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        let column = self.column::<T>(name)?;
        if column.len() != self.rows {
            return Err(StoreError::RowMismatch {
                name,
                rows: self.rows,
                found: column.len(),
            });
        }
        Ok(column)
    }

    /// A `values`/`offsets` pair. Row *i* owns `values[offsets[i]..offsets[i+1]]`.
    /// A `values`/`offsets` pair indexed by something OTHER than a title row.
    ///
    /// `list` requires `len(rows) + 1` offsets, which is right for a per-title list and wrong for a list
    /// keyed by anything else — the entity table has its own length, and checking it against the title
    /// count refuses a perfectly correct section. `n` is what the caller expects, so the length is still
    /// checked rather than trusted.
    pub fn list_of<T>(
        &self,
        values: &'static str,
        offsets: &'static str,
        n: usize,
    ) -> Result<List<'a, T>, StoreError>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        let offsets_col = self.column::<u32>(offsets)?;
        if offsets_col.len() != n + 1 {
            return Err(StoreError::RowMismatch {
                name: offsets,
                rows: n + 1,
                found: offsets_col.len(),
            });
        }
        Ok(List {
            values: self.column::<T>(values)?,
            offsets: offsets_col,
        })
    }

    pub fn list<T>(
        &self,
        values: &'static str,
        offsets: &'static str,
    ) -> Result<List<'a, T>, StoreError>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        let offsets_col = self.column::<u32>(offsets)?;
        if offsets_col.len() != self.rows + 1 {
            return Err(StoreError::RowMismatch {
                name: offsets,
                rows: self.rows + 1,
                found: offsets_col.len(),
            });
        }
        Ok(List {
            values: self.column::<T>(values)?,
            offsets: offsets_col,
        })
    }

    /// The string dictionary. There is exactly one: every id in the store — a facet value, a genre, a
    /// country, a title, an entity name — indexes this table. A second id space would make a mismatched
    /// lookup return a wrong but valid string, silently.
    pub fn strings(&self) -> Result<Strings<'a>, StoreError> {
        Ok(Strings {
            blob: self.section("strings")?,
            offsets: self.column::<u32>("str_off")?,
        })
    }

    /// The row for a title, by binary search over `keys`. `media`: 0 = movie, 1 = tv.
    pub fn row_of(&self, media: u8, tmdb_id: u32) -> Result<Option<Row>, StoreError> {
        let keys = self.per_row::<u64>("keys")?;
        let want = (u64::from(media) << 32) | u64::from(tmdb_id);
        Ok(keys.binary_search(&want).ok().map(Row))
    }

    /// Every series (Wikidata P179) each title is part of, most specific first, as raw Q-id numbers —
    /// not entity indices: the entity table holds almost no franchises.
    ///
    /// The one column store-v1 and store-v2 lay out differently, so this is where the difference is
    /// absorbed and nowhere else: v2's `franchise_v`/`franchise_o` list, or v1's single `franchise`
    /// column read as a list of zero or one. Either way a row's answer is a borrowed slice.
    pub fn franchises(&self) -> Result<Franchises<'a>, StoreError> {
        if self.format_version >= 2 {
            Ok(Franchises::List(self.list("franchise_v", "franchise_o")?))
        } else {
            Ok(Franchises::Single(self.per_row("franchise")?))
        }
    }

    /// The iconic studios: production companies a viewer browses by, each with the entity ids of every
    /// credited Wikidata item that is the same studio. A title's studios are its `companies_v` entries
    /// found here.
    ///
    /// The `studio_*` sections are OPTIONAL, like `card_poster`: a store without them — any store-v1, and
    /// a store-v2 written before them — has no iconic studios, which is an empty answer and not an error.
    /// Present but malformed is still an error.
    pub fn iconic_studios(&self) -> Result<IconicStudios<'a>, StoreError> {
        let qids = match self.column::<u32>("studio_qid") {
            Ok(qids) => qids,
            Err(StoreError::MissingSection(_)) => return Ok(IconicStudios::default()),
            Err(e) => return Err(e),
        };
        let names = self.column::<u32>("studio_name")?;
        if names.len() != qids.len() {
            return Err(StoreError::RowMismatch {
                name: "studio_name",
                rows: qids.len(),
                found: names.len(),
            });
        }
        Ok(IconicStudios {
            qids,
            names,
            entities: self.list_of("studio_ent_v", "studio_ent_o", qids.len())?,
        })
    }

    /// A per-title list that is OPTIONAL: empty for every row when the store has no such section, and
    /// the usual checks when it does. Present in half (values without offsets) is malformed, not absent.
    fn optional_list(
        &self,
        values: &'static str,
        offsets: &'static str,
    ) -> Result<List<'a, u32>, StoreError> {
        match self.entry(offsets) {
            Err(StoreError::MissingSection(_)) => match self.entry(values) {
                Err(StoreError::MissingSection(_)) => Ok(List::default()),
                Err(e) => Err(e),
                Ok(_) => Err(StoreError::MissingSection(offsets)),
            },
            Err(e) => Err(e),
            Ok(_) => self.list(values, offsets),
        }
    }

    /// Each title's directors (Wikidata P57), as entity ids — one of the three credits `makers` is the
    /// union of. OPTIONAL: a store written before the role sections reads as none for every row.
    pub fn directors(&self) -> Result<List<'a, u32>, StoreError> {
        self.optional_list("directors_v", "directors_o")
    }

    /// Each title's creators (P170), as entity ids — mostly a series credit. OPTIONAL, as
    /// [`directors`](Self::directors).
    pub fn creators(&self) -> Result<List<'a, u32>, StoreError> {
        self.optional_list("creators_v", "creators_o")
    }

    /// Each title's screenwriters (P58), as entity ids. OPTIONAL, as [`directors`](Self::directors).
    pub fn writers(&self) -> Result<List<'a, u32>, StoreError> {
        self.optional_list("writers_v", "writers_o")
    }

    /// Each title's source authors: the authors (P50) of the works it is adapted from (P144), as entity
    /// ids in Q-id order. OPTIONAL, as [`directors`](Self::directors): a store written before them names
    /// no title's source author.
    pub fn source_authors(&self) -> Result<List<'a, u32>, StoreError> {
        self.optional_list("src_authors_v", "src_authors_o")
    }

    /// Each entity's IMDb person id (`nm…`, Wikidata P345) as a string id, [`NONE_U32`] for none,
    /// indexed like `ent_qid`. A join key for IMDb's own principals, never an entity's public id — that
    /// stays its Q-id.
    ///
    /// OPTIONAL: a store without `ent_imdb` answers an empty slice, so `get(entity)` is `None` for every
    /// entity. Present, it must have one entry per entity.
    pub fn entity_imdb_ids(&self) -> Result<&'a [u32], StoreError> {
        let ids = match self.column::<u32>("ent_imdb") {
            Ok(ids) => ids,
            Err(StoreError::MissingSection(_)) => return Ok(&[]),
            Err(e) => return Err(e),
        };
        let entities = self.column::<u32>("ent_qid")?.len();
        if ids.len() != entities {
            return Err(StoreError::RowMismatch {
                name: "ent_imdb",
                rows: entities,
                found: ids.len(),
            });
        }
        Ok(ids)
    }

    /// The awards each title won or was nominated for, grouped by ceremony (the Academy Awards, the
    /// Cannes Film Festival), and the ceremony table they index.
    ///
    /// The `award_*` and `ceremony_*` sections are OPTIONAL: a store without them has no awards, which
    /// is an empty answer and not an error. Present but malformed is still an error.
    pub fn awards(&self) -> Result<Awards<'a>, StoreError> {
        let qids = match self.column::<u32>("ceremony_qid") {
            Ok(qids) => qids,
            Err(StoreError::MissingSection(_)) => return Ok(Awards::default()),
            Err(e) => return Err(e),
        };
        let names = self.column::<u32>("ceremony_name")?;
        if names.len() != qids.len() {
            return Err(StoreError::RowMismatch {
                name: "ceremony_name",
                rows: qids.len(),
                found: names.len(),
            });
        }
        let ceremonies = self.list::<u32>("award_v", "award_o")?;
        let won = self.list::<u8>("award_w", "award_o")?;
        if won.values.len() != ceremonies.values.len() {
            return Err(StoreError::RowMismatch {
                name: "award_w",
                rows: ceremonies.values.len(),
                found: won.values.len(),
            });
        }
        Ok(Awards {
            qids,
            names,
            ceremonies,
            won,
        })
    }

    /// The tentative tier of the plot facets: per row and axis, an answer the writer's gates refused only
    /// as uncertain, with its probability. A cell is tentative only where `facet_v` is absent.
    ///
    /// `facet_tv` and `facet_tp` are OPTIONAL and written together: a store with neither has no tentative
    /// values, which is an empty answer and not an error. One without the other, or either not sized
    /// `rows × FACET_AXES.len()`, is an error.
    pub fn tentative_facets(&self) -> Result<TentativeFacets<'a>, StoreError> {
        let values = match self.column::<u32>("facet_tv") {
            Ok(values) => values,
            Err(StoreError::MissingSection(_)) => {
                return match self.entry("facet_tp") {
                    Err(StoreError::MissingSection(_)) => Ok(TentativeFacets::default()),
                    Err(e) => Err(e),
                    Ok(_) => Err(StoreError::MissingSection("facet_tv")),
                };
            }
            Err(e) => return Err(e),
        };
        let probabilities = self.column::<u8>("facet_tp")?;
        let cells = self.rows * FACET_AXES.len();
        for (name, found) in [
            ("facet_tv", values.len()),
            ("facet_tp", probabilities.len()),
        ] {
            if found != cells {
                return Err(StoreError::RowMismatch {
                    name,
                    rows: cells,
                    found,
                });
            }
        }
        Ok(TentativeFacets {
            values,
            probabilities,
        })
    }

    /// Each entity's traits as Wikidata states them — gender, citizenship, occupation, birth, death —
    /// indexed like `ent_qid`. A person Wikidata says nothing about has none; nothing is inferred.
    ///
    /// The ten `ent_gender_*`, `ent_citizen_*`, `ent_occupation_*`, `ent_born(_prec)` and
    /// `ent_died(_prec)` sections are OPTIONAL and written together: a store with none of them has no
    /// traits, which is an empty answer and not an error. Some of them, or any of the wrong length, is an
    /// error.
    pub fn person_traits(&self) -> Result<PersonTraits<'a>, StoreError> {
        if PERSON_TRAIT_SECTIONS
            .into_iter()
            .all(|name| matches!(self.entry(name), Err(StoreError::MissingSection(_))))
        {
            return Ok(PersonTraits::default());
        }
        let entities = self.column::<u32>("ent_qid")?.len();
        let per_entity = |name: &'static str, found: usize| {
            if found == entities {
                Ok(())
            } else {
                Err(StoreError::RowMismatch {
                    name,
                    rows: entities,
                    found,
                })
            }
        };
        let born = self.column::<i32>("ent_born")?;
        per_entity("ent_born", born.len())?;
        let born_prec = self.column::<u8>("ent_born_prec")?;
        per_entity("ent_born_prec", born_prec.len())?;
        let died = self.column::<i32>("ent_died")?;
        per_entity("ent_died", died.len())?;
        let died_prec = self.column::<u8>("ent_died_prec")?;
        per_entity("ent_died_prec", died_prec.len())?;
        Ok(PersonTraits {
            genders: self.list_of("ent_gender_v", "ent_gender_o", entities)?,
            citizenships: self.list_of("ent_citizen_v", "ent_citizen_o", entities)?,
            occupations: self.list_of("ent_occupation_v", "ent_occupation_o", entities)?,
            born,
            born_prec,
            died,
            died_prec,
        })
    }
}

impl<'a> Store<'a> {
    /// Where each person was born — the place (P19) and the country it is in (P17) — and each country's
    /// ISO 3166-1 alpha-2 code (P297), indexed like `ent_qid`.
    ///
    /// The five `ent_bplace_*`, `ent_bcountry_*` and `ent_iso` sections are OPTIONAL and written together:
    /// a store with none of them has no birthplaces and no codes, which is an empty answer and not an
    /// error. Some of them, or any of the wrong length, is an error.
    pub fn birthplaces(&self) -> Result<Birthplaces<'a>, StoreError> {
        if BIRTHPLACE_SECTIONS
            .into_iter()
            .all(|name| matches!(self.entry(name), Err(StoreError::MissingSection(_))))
        {
            return Ok(Birthplaces::default());
        }
        let entities = self.column::<u32>("ent_qid")?.len();
        let iso = self.column::<u32>("ent_iso")?;
        if iso.len() != entities {
            return Err(StoreError::RowMismatch {
                name: "ent_iso",
                rows: entities,
                found: iso.len(),
            });
        }
        Ok(Birthplaces {
            places: self.list_of("ent_bplace_v", "ent_bplace_o", entities)?,
            countries: self.list_of("ent_bcountry_v", "ent_bcountry_o", entities)?,
            iso,
        })
    }
}

/// The sections [`Store::birthplaces`] reads, all or none of which a store has.
const BIRTHPLACE_SECTIONS: [&str; 5] = [
    "ent_bplace_v",
    "ent_bplace_o",
    "ent_bcountry_v",
    "ent_bcountry_o",
    "ent_iso",
];

/// [`Store::birthplaces`]: per entity, where Wikidata says the person was born, and a country's code.
/// Every accessor takes an entity id and answers empty (or `None`) for one with none, or out of range.
#[derive(Default)]
pub struct Birthplaces<'a> {
    places: List<'a, u32>,
    countries: List<'a, u32>,
    iso: &'a [u32],
}

impl<'a> Birthplaces<'a> {
    /// Whether the store carries birthplaces at all.
    pub fn is_empty(&self) -> bool {
        self.iso.is_empty()
    }

    /// Places of birth (P19), as entity ids: a city, a village, now and then a country. Usually one.
    pub fn places(&self, entity: u32) -> &'a [u32] {
        self.places.get(Row(entity as usize))
    }

    /// The countries those places are in (P17 of each), as entity ids, deduplicated. Empty for a place
    /// Wikidata puts in no country; never inferred from citizenship.
    pub fn countries(&self, entity: u32) -> &'a [u32] {
        self.countries.get(Row(entity as usize))
    }

    /// String id of the entity's ISO 3166-1 alpha-2 code (`SE`), for a country a person's citizenship
    /// or birthplace names that has one. `None` for every other entity.
    pub fn iso(&self, entity: u32) -> Option<u32> {
        self.iso
            .get(entity as usize)
            .copied()
            .filter(|&id| id != NONE_U32)
    }
}

/// The sections [`Store::person_traits`] reads, all or none of which a store has.
const PERSON_TRAIT_SECTIONS: [&str; 10] = [
    "ent_gender_v",
    "ent_gender_o",
    "ent_citizen_v",
    "ent_citizen_o",
    "ent_occupation_v",
    "ent_occupation_o",
    "ent_born",
    "ent_born_prec",
    "ent_died",
    "ent_died_prec",
];

/// [`Store::tentative_facets`]: the tentative tier, dense `rows × FACET_AXES.len()` like `facet_v`.
#[derive(Default)]
pub struct TentativeFacets<'a> {
    values: &'a [u32],
    probabilities: &'a [u8],
}

/// One tentative answer.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Tentative {
    /// String id of the value.
    pub value: u32,
    /// Its probability in hundredths: from the answer's distribution, not `facet_c`'s self-reported
    /// confidence, and not comparable with it.
    pub probability: u8,
}

impl TentativeFacets<'_> {
    /// Whether the store carries the tier at all.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Row *i*'s tentative answer on axis *a* (an index into [`FACET_AXES`]), or `None` for none, an axis
    /// out of range, or a row out of range.
    pub fn get(&self, row: Row, axis: usize) -> Option<Tentative> {
        if axis >= FACET_AXES.len() {
            return None;
        }
        let at = row.0.checked_mul(FACET_AXES.len())?.checked_add(axis)?;
        let value = *self.values.get(at)?;
        (value != NONE_U32).then(|| Tentative {
            value,
            probability: self.probabilities.get(at).copied().unwrap_or(0),
        })
    }
}

/// [`Store::person_traits`]: per entity, what Wikidata states about the person. Every accessor takes an
/// entity id and answers empty (or `None`) for one with no such trait, or out of range.
#[derive(Default)]
pub struct PersonTraits<'a> {
    genders: List<'a, u32>,
    citizenships: List<'a, u32>,
    occupations: List<'a, u32>,
    born: &'a [i32],
    born_prec: &'a [u8],
    died: &'a [i32],
    died_prec: &'a [u8],
}

/// A birth or death date, as `released` stores a date.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct PersonDate {
    /// Days since 1970-01-01 in the proleptic Gregorian calendar; negative before it, reaching before
    /// the common era (year 0 is 1 BCE).
    pub days: i32,
    /// 0 day · 1 month · 2 year · 3 decade · 4 century: how much of `days` Wikidata asserts. A
    /// year-precision date is the 1st of January of that year and must not be shown as that day. A
    /// century is ⌈year / 100⌉ and nothing finer: Wikidata writes "20th century" as any year from 1901
    /// to 2000, so its year must not be bucketed into a decade.
    pub precision: u8,
}

impl<'a> PersonTraits<'a> {
    /// The entity's sex or gender (P21), as entity ids of the items Wikidata names — whatever values
    /// it holds, not only male and female. Usually one.
    pub fn genders(&self, entity: u32) -> &'a [u32] {
        self.genders.get(Row(entity as usize))
    }

    /// Countries of citizenship (P27), as entity ids.
    pub fn citizenships(&self, entity: u32) -> &'a [u32] {
        self.citizenships.get(Row(entity as usize))
    }

    /// Occupations (P106), as entity ids: actor, film director, screenwriter, singer.
    pub fn occupations(&self, entity: u32) -> &'a [u32] {
        self.occupations.get(Row(entity as usize))
    }

    /// Date of birth (P569).
    pub fn born(&self, entity: u32) -> Option<PersonDate> {
        date(self.born, self.born_prec, entity)
    }

    /// Date of death (P570). `None` for the living, and for a death Wikidata does not date.
    pub fn died(&self, entity: u32) -> Option<PersonDate> {
        date(self.died, self.died_prec, entity)
    }
}

fn date(days: &[i32], precision: &[u8], entity: u32) -> Option<PersonDate> {
    let i = entity as usize;
    let (&days, &precision) = (days.get(i)?, precision.get(i)?);
    (days != i32::MIN).then_some(PersonDate { days, precision })
}

/// [`Store::awards`]: per title, the ceremonies it won or was nominated at, and the ceremony table.
#[derive(Default)]
pub struct Awards<'a> {
    qids: &'a [u32],
    names: &'a [u32],
    ceremonies: List<'a, u32>,
    won: List<'a, u8>,
}

/// One ceremony a title was recognised at.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Award {
    /// Index into the ceremony table: [`Awards::ceremony`].
    pub ceremony: u32,
    /// Won at least one award there; `false` is nominated only.
    pub won: bool,
}

/// A ceremony or awarding body.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Ceremony {
    /// Its Wikidata item as a raw Q-id number.
    pub qid: u32,
    /// String id of its name.
    pub name: u32,
}

impl<'a> Awards<'a> {
    /// Ceremonies in the table.
    pub fn len(&self) -> usize {
        self.qids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.qids.is_empty()
    }

    /// Ceremony *i*, or `None` past the end.
    pub fn ceremony(&self, i: u32) -> Option<Ceremony> {
        let i = i as usize;
        Some(Ceremony {
            qid: *self.qids.get(i)?,
            name: *self.names.get(i)?,
        })
    }

    /// Row *i*'s awards, one per ceremony. Empty for a title with none, or a row out of range.
    pub fn get(&self, row: Row) -> impl Iterator<Item = Award> + 'a {
        let won = self.won.get(row);
        self.ceremonies
            .get(row)
            .iter()
            .zip(won)
            .map(|(&ceremony, &w)| Award {
                ceremony,
                won: w != 0,
            })
    }
}

/// [`Store::iconic_studios`], sorted by the studio's own Wikidata item.
#[derive(Default)]
pub struct IconicStudios<'a> {
    qids: &'a [u32],
    names: &'a [u32],
    entities: List<'a, u32>,
}

/// One iconic studio.
#[derive(Debug, PartialEq, Eq)]
pub struct IconicStudio<'a> {
    /// The studio's own Wikidata item as a raw Q-id number: the id a studio page is addressed by.
    pub qid: u32,
    /// String id of what a viewer calls it.
    pub name: u32,
    /// Entity ids of every credited item that is this studio.
    pub entities: &'a [u32],
}

impl<'a> IconicStudios<'a> {
    pub fn len(&self) -> usize {
        self.qids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.qids.is_empty()
    }

    /// Studio *i*, or `None` past the end.
    pub fn get(&self, i: usize) -> Option<IconicStudio<'a>> {
        Some(IconicStudio {
            qid: *self.qids.get(i)?,
            name: *self.names.get(i)?,
            entities: self.entities.get(Row(i)),
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = IconicStudio<'a>> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }
}

/// [`Store::franchises`]: a title's series, whichever layout the store has.
pub enum Franchises<'a> {
    /// store-v2 and later.
    List(List<'a, u32>),
    /// store-v1: one Q-id per row, [`NONE_U32`] for none.
    Single(&'a [u32]),
}

impl<'a> Franchises<'a> {
    /// Row *i*'s series, most specific first. Empty for a title in none, or a row out of range.
    pub fn get(&self, row: Row) -> &'a [u32] {
        match self {
            Self::List(list) => list.get(row),
            Self::Single(column) => match column.get(row.0..=row.0) {
                Some(one) if one[0] != NONE_U32 => one,
                _ => &[],
            },
        }
    }
}

impl fmt::Debug for Store<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("dataset_version", &self.dataset_version)
            .field("format_version", &self.format_version)
            .field("rows", &self.rows)
            .field("sections", &self.entries.len())
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// The section table, owned, so the bytes can be borrowed separately.
///
/// [`Store::open`] verifies the content hash, which costs about 1 ms per MB — fine once at load, far too
/// much per query. A consumer that owns the mapping (den-atlas owns an `Mmap`) cannot hold a `Store`
/// alongside it without a self-referential struct, so it holds one of these instead: built once, by
/// verifying, and thereafter [`view`](Self::view) hands out a `Store` for free.
///
/// The only constructor verifies, so a `StoreTable` cannot exist for bytes that were never checked.
pub struct StoreTable {
    entries: Vec<RawEntry>,
    rows: usize,
    dataset_version: String,
    format_version: u32,
    len: usize,
}

impl StoreTable {
    /// Verify the store and keep its table. Does not retain the bytes.
    pub fn open(bytes: &[u8]) -> Result<Self, StoreError> {
        let store = Store::open(bytes)?;
        Ok(Self {
            entries: store
                .entries
                .iter()
                .map(|e| RawEntry {
                    name: e.name,
                    offset: e.offset,
                    length: e.length,
                    width: e.width,
                })
                .collect(),
            rows: store.rows,
            dataset_version: store.dataset_version.to_owned(),
            format_version: store.format_version,
            len: bytes.len(),
        })
    }

    /// A `Store` over bytes this table was built from. Cheap: no hash, no parse.
    ///
    /// Panics if handed a different-length slice — that would mean the caller swapped the file under the
    /// table, and every section offset would then address the wrong thing quietly.
    pub fn view<'a>(&'a self, bytes: &'a [u8]) -> Store<'a> {
        assert_eq!(bytes.len(), self.len, "store bytes changed under its table");
        Store {
            bytes,
            entries: &self.entries,
            rows: self.rows,
            dataset_version: &self.dataset_version,
            format_version: self.format_version,
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn dataset_version(&self) -> &str {
        &self.dataset_version
    }
}

/// A variable-length column.
#[derive(Default)]
pub struct List<'a, T> {
    values: &'a [T],
    offsets: &'a [u32],
}

impl<'a, T> List<'a, T> {
    /// Row *i*'s span, or empty if the offsets do not describe one.
    pub fn get(&self, row: Row) -> &'a [T] {
        // checked_add: `row.0 + 1` overflows on `Row(usize::MAX)` and panics in a debug build, in a
        // crate that forbids unsafe and documents this as returning empty for a row it cannot describe.
        let Some(next) = row.0.checked_add(1) else {
            return &[];
        };
        let (Some(&from), Some(&to)) = (self.offsets.get(row.0), self.offsets.get(next)) else {
            return &[];
        };
        let (from, to) = (from as usize, to as usize);
        if from > to || to > self.values.len() {
            return &[];
        }
        &self.values[from..to]
    }
}

/// The one string table.
pub struct Strings<'a> {
    blob: &'a [u8],
    offsets: &'a [u32],
}

impl<'a> Strings<'a> {
    /// The string for an id. `None` for [`NONE_U32`], an out-of-range id, or non-UTF-8 bytes — a reader
    /// that cannot resolve an id must say so rather than substitute something plausible.
    pub fn get(&self, id: u32) -> Option<&'a str> {
        if id == NONE_U32 {
            return None;
        }
        let i = id as usize;
        let (&from, &to) = (self.offsets.get(i)?, self.offsets.get(i + 1)?);
        core::str::from_utf8(self.blob.get(from as usize..to as usize)?).ok()
    }

    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// BLAKE2b-64 over the body, matching the writer's `hashlib.blake2b(payload, digest_size=8)`.
fn hash(body: &[u8]) -> u64 {
    blake2b64(body)
}

// A minimal BLAKE2b restricted to an 8-byte digest: the only hash this crate needs, and vendoring one
// function keeps the dependency list at `zerocopy` alone for something that runs in a browser.
fn blake2b64(input: &[u8]) -> u64 {
    const IV: [u64; 8] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179,
    ];
    const SIGMA: [[usize; 16]; 12] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
        [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
        [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
        [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
        [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
        [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
        [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
        [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
        [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    ];

    let mut h = IV;
    h[0] ^= 0x0101_0000 ^ 8; // no key, 8-byte digest
    let mut counter: u128 = 0;

    let mut compress = |h: &mut [u64; 8], block: &[u8; 128], counter: u128, last: bool| {
        let mut m = [0u64; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u64::from_le_bytes(block[i * 8..i * 8 + 8].try_into().unwrap());
        }
        let mut v = [0u64; 16];
        v[..8].copy_from_slice(h);
        v[8..].copy_from_slice(&IV);
        v[12] ^= counter as u64;
        v[13] ^= (counter >> 64) as u64;
        if last {
            v[14] = !v[14];
        }
        for round in SIGMA.iter() {
            macro_rules! g {
                ($a:expr, $b:expr, $c:expr, $d:expr, $x:expr, $y:expr) => {
                    v[$a] = v[$a].wrapping_add(v[$b]).wrapping_add($x);
                    v[$d] = (v[$d] ^ v[$a]).rotate_right(32);
                    v[$c] = v[$c].wrapping_add(v[$d]);
                    v[$b] = (v[$b] ^ v[$c]).rotate_right(24);
                    v[$a] = v[$a].wrapping_add(v[$b]).wrapping_add($y);
                    v[$d] = (v[$d] ^ v[$a]).rotate_right(16);
                    v[$c] = v[$c].wrapping_add(v[$d]);
                    v[$b] = (v[$b] ^ v[$c]).rotate_right(63);
                };
            }
            g!(0, 4, 8, 12, m[round[0]], m[round[1]]);
            g!(1, 5, 9, 13, m[round[2]], m[round[3]]);
            g!(2, 6, 10, 14, m[round[4]], m[round[5]]);
            g!(3, 7, 11, 15, m[round[6]], m[round[7]]);
            g!(0, 5, 10, 15, m[round[8]], m[round[9]]);
            g!(1, 6, 11, 12, m[round[10]], m[round[11]]);
            g!(2, 7, 8, 13, m[round[12]], m[round[13]]);
            g!(3, 4, 9, 14, m[round[14]], m[round[15]]);
        }
        for i in 0..8 {
            h[i] ^= v[i] ^ v[i + 8];
        }
    };

    let mut chunks = input.chunks(128).peekable();
    if chunks.peek().is_none() {
        compress(&mut h, &[0u8; 128], 0, true);
    }
    while let Some(chunk) = chunks.next() {
        let mut block = [0u8; 128];
        block[..chunk.len()].copy_from_slice(chunk);
        let last = chunks.peek().is_none();
        counter += chunk.len() as u128;
        compress(&mut h, &block, counter, last);
    }
    h[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The writer hashes with `hashlib.blake2b(payload, digest_size=8)`. These vectors are that
    /// function's actual output, generated rather than recalled — a hand-written expectation is how a
    /// test comes to assert the wrong answer with total confidence.
    ///
    ///   python3 -c "import hashlib; print(list(hashlib.blake2b(b'', digest_size=8).digest()))"
    #[test]
    fn blake2b64_matches_the_writers_digest() {
        assert_eq!(
            blake2b64(b"").to_le_bytes(),
            [228, 166, 160, 87, 116, 121, 178, 180]
        );
        assert_eq!(
            blake2b64(b"abc").to_le_bytes(),
            [216, 187, 20, 216, 51, 213, 149, 89]
        );
        // A block boundary and one past it: the counter and the final-block flag are the easy things to
        // get wrong, and neither shows up on a short input.
        assert_eq!(
            blake2b64(&[0x61; 128]).to_le_bytes(),
            [240, 102, 67, 254, 156, 126, 24, 218]
        );
        assert_eq!(
            blake2b64(&[0x61; 129]).to_le_bytes(),
            [228, 98, 83, 91, 176, 197, 162, 153]
        );
    }

    /// A store holding just `sections` (name, element width, bytes), laid out and hashed as the writer
    /// does, for shapes the den-spec fixture cannot carry because they are malformed.
    fn build(rows: u32, sections: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
        let mut at = HEADER_BYTES + ENTRY_BYTES * sections.len();
        let (mut table, mut body) = (Vec::new(), Vec::new());
        for (name, width, bytes) in sections {
            let pad = (8 - at % 8) % 8;
            body.extend(core::iter::repeat_n(0u8, pad));
            at += pad;
            let mut padded = [0u8; NAME_BYTES];
            padded[..name.len()].copy_from_slice(name.as_bytes());
            table.extend(padded);
            table.extend((at as u64).to_le_bytes());
            table.extend((bytes.len() as u32).to_le_bytes());
            table.extend(width.to_le_bytes());
            body.extend(bytes);
            at += bytes.len();
        }
        let payload = [table, body].concat();
        let mut out = Vec::from(&MAGIC[..]);
        out.extend(FORMAT_VERSION.to_le_bytes());
        out.extend(ENDIAN_CHECK.to_le_bytes());
        out.extend(hash(&payload).to_le_bytes());
        out.extend((sections.len() as u32).to_le_bytes());
        out.extend(rows.to_le_bytes());
        out.extend([0u8; 32]);
        out.extend(payload);
        out
    }

    fn u32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// The optional sections read as empty when absent (the den-spec fixture tests that half) and as an
    /// error when present but malformed, which only a hand-built store can show.
    #[test]
    fn a_malformed_optional_section_is_an_error_not_empty() {
        // Values without their offsets: half a role list.
        let half = build(1, &[("directors_v", 4, u32s(&[0]))]);
        let store = Store::open(&half).unwrap();
        assert_eq!(
            store.directors().err(),
            Some(StoreError::MissingSection("directors_o"))
        );
        assert!(
            store.source_authors().unwrap().get(Row(0)).is_empty(),
            "absent"
        );
        assert!(store.writers().unwrap().get(Row(0)).is_empty(), "absent");

        // One IMDb id for two entities.
        let short = build(
            1,
            &[("ent_qid", 4, u32s(&[1, 2])), ("ent_imdb", 4, u32s(&[0]))],
        );
        assert_eq!(
            Store::open(&short).unwrap().entity_imdb_ids(),
            Err(StoreError::RowMismatch {
                name: "ent_imdb",
                rows: 2,
                found: 1
            })
        );

        // A won flag missing for one of two awards.
        let awards = build(
            1,
            &[
                ("award_v", 4, u32s(&[0, 1])),
                ("award_w", 1, vec![1]),
                ("award_o", 4, u32s(&[0, 2])),
                ("ceremony_qid", 4, u32s(&[19020, 1011547])),
                ("ceremony_name", 4, u32s(&[0, 1])),
            ],
        );
        assert!(matches!(
            Store::open(&awards).unwrap().awards(),
            Err(StoreError::RowMismatch {
                name: "award_w",
                ..
            })
        ));
    }

    /// The person-trait sections come as a set: some of them is an error, and so is a date column that
    /// does not have one entry per entity.
    #[test]
    fn malformed_person_traits_are_an_error_not_empty() {
        let offsets = u32s(&[0, 0, 0]);
        let days: Vec<u8> = [i32::MIN, -900_689]
            .iter()
            .flat_map(|d| d.to_le_bytes())
            .collect();
        let whole = |born: Vec<u8>| -> Vec<(&'static str, u32, Vec<u8>)> {
            vec![
                ("ent_qid", 4, u32s(&[1, 2])),
                ("ent_gender_v", 4, vec![]),
                ("ent_gender_o", 4, offsets.clone()),
                ("ent_citizen_v", 4, vec![]),
                ("ent_citizen_o", 4, offsets.clone()),
                ("ent_occupation_v", 4, vec![]),
                ("ent_occupation_o", 4, offsets.clone()),
                ("ent_born", 4, born),
                ("ent_born_prec", 1, vec![0xFF, 2]),
                ("ent_died", 4, days.clone()),
                ("ent_died_prec", 1, vec![0xFF, 2]),
            ]
        };

        let good = build(1, &whole(days.clone()));
        let store = Store::open(&good).unwrap();
        let traits = store.person_traits().expect("well formed");
        assert_eq!(traits.born(0), None, "i32::MIN is no date");
        assert_eq!(
            traits.born(1),
            Some(PersonDate {
                days: -900_689,
                precision: 2
            })
        );

        let short = build(1, &whole(days[..4].to_vec()));
        assert_eq!(
            Store::open(&short).unwrap().person_traits().err(),
            Some(StoreError::RowMismatch {
                name: "ent_born",
                rows: 2,
                found: 1
            })
        );

        let mut partial = whole(days.clone());
        partial.retain(|(name, _, _)| *name != "ent_died_prec");
        let partial = build(1, &partial);
        assert_eq!(
            Store::open(&partial).unwrap().person_traits().err(),
            Some(StoreError::MissingSection("ent_died_prec"))
        );
    }

    /// The birthplace sections come as a set: some of them is an error, and so is an `ent_iso` that does
    /// not have one entry per entity.
    #[test]
    fn malformed_birthplaces_are_an_error_not_empty() {
        let whole = |iso: Vec<u8>| -> Vec<(&'static str, u32, Vec<u8>)> {
            vec![
                ("ent_qid", 4, u32s(&[34, 1754])),
                ("ent_bplace_v", 4, u32s(&[1])),
                ("ent_bplace_o", 4, u32s(&[0, 0, 1])),
                ("ent_bcountry_v", 4, u32s(&[0])),
                ("ent_bcountry_o", 4, u32s(&[0, 0, 1])),
                ("ent_iso", 4, iso),
            ]
        };

        let good = build(1, &whole(u32s(&[7, NONE_U32])));
        let store = Store::open(&good).unwrap();
        let births = store.birthplaces().expect("well formed");
        assert_eq!(
            (births.places(1), births.countries(1)),
            (&[1][..], &[0][..])
        );
        assert_eq!(
            (births.iso(0), births.iso(1), births.iso(u32::MAX)),
            (Some(7), None, None)
        );

        let short = build(1, &whole(u32s(&[7])));
        assert_eq!(
            Store::open(&short).unwrap().birthplaces().err(),
            Some(StoreError::RowMismatch {
                name: "ent_iso",
                rows: 2,
                found: 1
            })
        );

        let mut partial = whole(u32s(&[7, NONE_U32]));
        partial.retain(|(name, _, _)| *name != "ent_bcountry_o");
        let partial = build(1, &partial);
        assert_eq!(
            Store::open(&partial).unwrap().birthplaces().err(),
            Some(StoreError::MissingSection("ent_bcountry_o"))
        );
    }

    /// The tentative tier comes as a pair sized like `facet_v`: half of it, or the wrong size, is an error.
    #[test]
    fn malformed_tentative_facets_are_an_error_not_empty() {
        let axes = FACET_AXES.len();
        let mut values = vec![NONE_U32; axes];
        values[3] = 7;
        let mut probabilities = vec![0u8; axes];
        probabilities[3] = 55;

        let good = build(
            1,
            &[
                ("facet_tv", 4, u32s(&values)),
                ("facet_tp", 1, probabilities.clone()),
            ],
        );
        let store = Store::open(&good).unwrap();
        let tier = store.tentative_facets().expect("well formed");
        assert_eq!(
            tier.get(Row(0), 3),
            Some(Tentative {
                value: 7,
                probability: 55
            })
        );
        assert_eq!(tier.get(Row(0), 0), None, "u32::MAX is no tentative value");
        assert_eq!(tier.get(Row(0), axes), None, "an axis out of range");
        assert_eq!(tier.get(Row(1), 3), None, "a row out of range");

        let half = build(1, &[("facet_tp", 1, probabilities.clone())]);
        assert_eq!(
            Store::open(&half).unwrap().tentative_facets().err(),
            Some(StoreError::MissingSection("facet_tv"))
        );
        let half = build(1, &[("facet_tv", 4, u32s(&values))]);
        assert_eq!(
            Store::open(&half).unwrap().tentative_facets().err(),
            Some(StoreError::MissingSection("facet_tp"))
        );

        let short = build(
            1,
            &[
                ("facet_tv", 4, u32s(&values)),
                ("facet_tp", 1, probabilities[..axes - 1].to_vec()),
            ],
        );
        assert_eq!(
            Store::open(&short).unwrap().tentative_facets().err(),
            Some(StoreError::RowMismatch {
                name: "facet_tp",
                rows: axes,
                found: axes - 1
            })
        );
    }

    #[test]
    fn refuses_a_store_that_is_not_one() {
        assert_eq!(
            Store::open(b"short").unwrap_err(),
            StoreError::TooSmall { need: 64, got: 5 }
        );
        let mut bytes = vec![0u8; 64];
        assert_eq!(Store::open(&bytes).unwrap_err(), StoreError::BadMagic);
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            Store::open(&bytes).unwrap_err(),
            StoreError::Version {
                found: 99,
                expected: FORMAT_VERSION
            }
        );
        // Both ends of the accepted range are exact: one past the newest and one before the oldest
        // are refused by their version, before anything else about the file is looked at.
        for found in [FORMAT_VERSION + 1, OLDEST_FORMAT_VERSION - 1] {
            bytes[8..12].copy_from_slice(&found.to_le_bytes());
            let err = Store::open(&bytes).unwrap_err();
            assert_eq!(
                err,
                StoreError::Version {
                    found,
                    expected: FORMAT_VERSION
                }
            );
            assert_eq!(
                err.to_string(),
                format!("store format version {found}, this build reads 1 to 2")
            );
        }
    }
}
