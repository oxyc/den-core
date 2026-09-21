//! Reads `den-<version>.store` — den-spec `wire/store-v1.md`.
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

/// The layout this decoder understands. A store declaring anything else is refused: a format change is a
/// new version, never a reinterpretation of the same bytes.
pub const FORMAT_VERSION: u32 = 1;

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
    "era", "setting", "scope", "ending", "pacing", "chronology", "continuity", "conflict", "ensemble",
    "tone", "timespan", "archetype",
];

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    TooSmall { need: usize, got: usize },
    BadMagic,
    /// A store written by a build that does not share this one's layout.
    Version { found: u32, expected: u32 },
    /// The writer's endianness marker did not survive the round trip.
    Endianness { found: u32 },
    /// The content hash did not match. The file is corrupt or truncated; nothing was read from it.
    Corrupt { found: u64, computed: u64 },
    SectionTable,
    /// A section's declared extent is not inside the file.
    SectionBounds { name: [u8; NAME_BYTES] },
    MissingSection(&'static str),
    /// A section exists but is not a whole number of its own elements, or is misaligned for them.
    BadSection { name: &'static str },
    /// A column was asked for at a different element width than the writer declared. A `u32` column
    /// read as `u16` is aligned and whole, so nothing else catches it.
    WidthMismatch { name: &'static str, declared: u32, requested: u32 },
    /// A row count that does not agree with a section's length.
    RowMismatch { name: &'static str, rows: usize, found: usize },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall { need, got } => write!(f, "store is {got} bytes, needs at least {need}"),
            Self::BadMagic => write!(f, "not a den store (bad magic)"),
            Self::Version { found, expected } => {
                write!(f, "store format version {found}, this build reads {expected}")
            }
            Self::Endianness { found } => write!(f, "endianness marker {found:#x} — foreign writer"),
            Self::Corrupt { found, computed } => {
                write!(f, "content hash {found:#018x} but the bytes hash to {computed:#018x}")
            }
            Self::SectionTable => write!(f, "section table does not fit in the file"),
            Self::SectionBounds { name } => {
                write!(f, "section {} extends past the end of the file", show(name))
            }
            Self::MissingSection(name) => write!(f, "store has no {name} section"),
            Self::BadSection { name } => write!(f, "section {name} is misaligned or a partial element"),
            Self::WidthMismatch { name, declared, requested } => write!(
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
}

impl<'a> Store<'a> {
    /// Verify and open. The content hash is checked here, before any section is addressed, so a caller
    /// that gets a `Store` back is holding bytes that have already been proven whole.
    pub fn open(bytes: &'a [u8]) -> Result<Self, StoreError> {
        if bytes.len() < HEADER_BYTES {
            return Err(StoreError::TooSmall { need: HEADER_BYTES, got: bytes.len() });
        }
        if &bytes[..8] != MAGIC {
            return Err(StoreError::BadMagic);
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if version != FORMAT_VERSION {
            return Err(StoreError::Version { found: version, expected: FORMAT_VERSION });
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
            return Err(StoreError::Corrupt { found: declared, computed });
        }

        let table_end = HEADER_BYTES
            .checked_add(count.checked_mul(ENTRY_BYTES).ok_or(StoreError::SectionTable)?)
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
        let end = raw_version.iter().position(|&b| b == 0).unwrap_or(raw_version.len());
        let dataset_version = core::str::from_utf8(&raw_version[..end]).unwrap_or("");

        Ok(Self { bytes, entries, rows, dataset_version })
    }

    /// Titles in the store.
    pub fn rows(&self) -> usize {
        self.rows
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
        Ok((&self.bytes[start..start + entry.length as usize], entry.width))
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
            return Err(StoreError::WidthMismatch { name, declared: width, requested: want });
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
            return Err(StoreError::RowMismatch { name, rows: self.rows, found: column.len() });
        }
        Ok(column)
    }

    /// A `values`/`offsets` pair. Row *i* owns `values[offsets[i]..offsets[i+1]]`.
    pub fn list<T>(&self, values: &'static str, offsets: &'static str) -> Result<List<'a, T>, StoreError>
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
        Ok(List { values: self.column::<T>(values)?, offsets: offsets_col })
    }

    /// The string dictionary. There is exactly one: every id in the store — a facet value, a genre, a
    /// country, a title, an entity name — indexes this table. A second id space would make a mismatched
    /// lookup return a wrong but valid string, silently.
    pub fn strings(&self) -> Result<Strings<'a>, StoreError> {
        Ok(Strings { blob: self.section("strings")?, offsets: self.column::<u32>("str_off")? })
    }

    /// The row for a title, by binary search over `keys`. `media`: 0 = movie, 1 = tv.
    pub fn row_of(&self, media: u8, tmdb_id: u32) -> Result<Option<Row>, StoreError> {
        let keys = self.per_row::<u64>("keys")?;
        let want = (u64::from(media) << 32) | u64::from(tmdb_id);
        Ok(keys.binary_search(&want).ok().map(Row))
    }
}

impl fmt::Debug for Store<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("dataset_version", &self.dataset_version)
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
                .map(|e| RawEntry { name: e.name, offset: e.offset, length: e.length, width: e.width })
                .collect(),
            rows: store.rows,
            dataset_version: store.dataset_version.to_owned(),
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
pub struct List<'a, T> {
    values: &'a [T],
    offsets: &'a [u32],
}

impl<'a, T> List<'a, T> {
    /// Row *i*'s span, or empty if the offsets do not describe one.
    pub fn get(&self, row: Row) -> &'a [T] {
        // checked_add: `row.0 + 1` overflows on `Row(usize::MAX)` and panics in a debug build, in a
        // crate that forbids unsafe and documents this as returning empty for a row it cannot describe.
        let Some(next) = row.0.checked_add(1) else { return &[] };
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
        0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
        0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
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
        assert_eq!(blake2b64(b"").to_le_bytes(), [228, 166, 160, 87, 116, 121, 178, 180]);
        assert_eq!(blake2b64(b"abc").to_le_bytes(), [216, 187, 20, 216, 51, 213, 149, 89]);
        // A block boundary and one past it: the counter and the final-block flag are the easy things to
        // get wrong, and neither shows up on a short input.
        assert_eq!(blake2b64(&[0x61; 128]).to_le_bytes(), [240, 102, 67, 254, 156, 126, 24, 218]);
        assert_eq!(blake2b64(&[0x61; 129]).to_le_bytes(), [228, 98, 83, 91, 176, 197, 162, 153]);
    }

    #[test]
    fn refuses_a_store_that_is_not_one() {
        assert_eq!(Store::open(b"short").unwrap_err(), StoreError::TooSmall { need: 64, got: 5 });
        let mut bytes = vec![0u8; 64];
        assert_eq!(Store::open(&bytes).unwrap_err(), StoreError::BadMagic);
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            Store::open(&bytes).unwrap_err(),
            StoreError::Version { found: 99, expected: FORMAT_VERSION }
        );
    }
}
