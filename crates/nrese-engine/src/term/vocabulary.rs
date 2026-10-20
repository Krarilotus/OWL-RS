//! FSST-compressed dictionary keys (`store.vocabulary = "fsst"`; design §3 item 4,
//! docs/design/research-designs.md).
//!
//! A checkpoint written with [`VocabularyEncoding::Fsst`] stores each key as its tag byte
//! followed by the rest compressed with FSST (Boncz, Neumann, Leis, VLDB 2020): one symbol
//! table for IRIs and one for every other kind, trained on a sample of the keys when the
//! checkpoint is written. On the office PC's stores the keys shrink to 39–53 % (Wikidata
//! lexemes 576 → 224 MiB); a key decodes in about 17 ns more than it copies, so it is a
//! setting, with [`VocabularyEncoding::Plain`] the default.
//!
//! Compression is deterministic for a symbol table, so a lookup compresses the probed key
//! once and compares compressed bytes; the hash table keeps the plain keys' hashes. Both
//! the writer and every reader compress with a compressor rebuilt from the stored table.

use std::ops::Deref;
use std::sync::atomic::{AtomicU8, Ordering};

use fsst::{Compressor, Symbol};
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

/// How checkpoints store the dictionary's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VocabularyEncoding {
    /// As they are: a key reads in place, without decoding.
    #[default]
    Plain,
    /// FSST-compressed (module docs).
    Fsst,
}

impl VocabularyEncoding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Fsst => "fsst",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "plain" => Some(Self::Plain),
            "fsst" => Some(Self::Fsst),
            _ => None,
        }
    }
}

static ENCODING: AtomicU8 = AtomicU8::new(0);

/// How checkpoints written from now on store the dictionary's keys (for the process, as
/// [`crate::set_index_encoding`]). Checkpoints already written are read as they are.
pub fn set_vocabulary_encoding(encoding: VocabularyEncoding) {
    ENCODING.store(encoding as u8, Ordering::Relaxed);
}

pub(crate) fn vocabulary_encoding() -> VocabularyEncoding {
    match ENCODING.load(Ordering::Relaxed) {
        0 => VocabularyEncoding::Plain,
        _ => VocabularyEncoding::Fsst,
    }
}

const TAG_IRI: u8 = b'I';
/// Keys sampled to train a table: spread over the dictionary (FSST trains on a few dozen
/// KiB of them in any case).
pub(crate) const SAMPLE: usize = 1 << 16;
/// Bytes of one table on file: 255 symbols of 8 bytes, then their 255 lengths, then the
/// number of symbols in use.
const TABLE_BYTES: usize = 255 * 8 + 255 + 1;
/// Bytes of the two tables on file.
pub(crate) const CODEC_BYTES: usize = 2 * TABLE_BYTES;
/// The most a codec holds while it is trained and while it lives, besides the keys it is
/// trained on: FSST's counters, candidate heap and sample of the keys, and both tables, built
/// and rebuilt from their stored form. FSST's (0.6.0, pinned) sizes, whatever the keys and
/// however many (at most twice [`SAMPLE`]): 5.36 MiB at the largest (the most keys, each
/// empty: FSST copies the list of them), measured by the checkpoint's
/// `the_dictionary_section_holds_what_it_charged`, which fails past it.
const TRAINING_BYTES: usize = 6 << 20;
/// What a trained codec holds: its two tables (FSST's, 0.6.0), 327,977 bytes measured, by the
/// same test.
const CODEC_HELD: usize = 384 << 10;

/// A codec with the charge of its tables ([`CODEC_HELD`]), which it frees before the charge
/// is released, however its owner goes.
pub(crate) struct Trained<'b> {
    codec: Codec,
    _held: Reservation<&'b Budget>,
}

impl Trained<'_> {
    pub(crate) fn codec(&self) -> &Codec {
        &self.codec
    }
}

/// Keys to train a codec on, one after another, charged to a budget as they grow.
pub(crate) struct Sample<'b> {
    bytes: ChargedVec<u8, &'b Budget>,
    /// Where each key ends in `bytes`.
    ends: ChargedVec<usize, &'b Budget>,
}

impl<'b> Sample<'b> {
    pub(crate) fn new(budget: &'b Budget) -> Self {
        Self {
            bytes: ChargedVec::new(budget),
            ends: ChargedVec::new(budget),
        }
    }

    /// `key` (plain) appended, its room charged first.
    pub(crate) fn push(&mut self, key: &[u8]) -> Result<(), BudgetExceeded> {
        self.bytes.extend_from_slice(key)?;
        self.ends.push(self.bytes.len())
    }

    fn keys(&self) -> impl Iterator<Item = &[u8]> + Clone {
        (0..self.ends.len()).map(|i| {
            let start = if i == 0 { 0 } else { self.ends[i - 1] };
            &self.bytes[start..self.ends[i]]
        })
    }
}

/// The symbol tables of a vocabulary: one for IRIs, one for the rest.
pub(crate) struct Codec {
    iri: Compressor,
    other: Compressor,
}

impl Codec {
    /// Tables trained on `keys` (plain), rebuilt from their stored form.
    #[cfg(test)]
    pub(crate) fn train(keys: &[Vec<u8>]) -> Self {
        let budget = Budget::unlimited();
        let mut sample = Sample::new(&budget);
        for key in keys {
            sample.push(key).expect("no limit");
        }
        Self::train_within(&sample, &budget)
            .expect("no limit")
            .codec
    }

    /// Tables trained on `sample` (plain keys), rebuilt from their stored form, everything
    /// it allocates charged to `budget` first: the keys' rests sorted to their tables, and
    /// FSST's training ([`TRAINING_BYTES`]); then the codec's tables ([`CODEC_HELD`]), held
    /// as long as it lives.
    pub(crate) fn train_within<'b>(
        sample: &Sample<'_>,
        budget: &'b Budget,
    ) -> Result<Trained<'b>, BudgetExceeded> {
        let mut held = Reservation::new(budget);
        held.charge(TRAINING_BYTES)?;
        let iri = |key: &&[u8]| key.first() == Some(&TAG_IRI);
        fn rest(key: &[u8]) -> &[u8] {
            key.get(1..).unwrap_or_default()
        }
        let iris = sample.keys().filter(iri).count();
        let iris = ChargedVec::collect(budget, iris, sample.keys().filter(iri).map(rest))?;
        let others = sample.ends.len() - iris.len();
        let others = ChargedVec::collect(
            budget,
            others,
            sample.keys().filter(|key| !iri(key)).map(rest),
        )?;
        // FSST trains on a vector: each paired with its charge, which a tuple releases after
        // it frees the vector.
        let (iris, others) = (iris.into_parts(), others.into_parts());
        let trained = Self {
            iri: Compressor::train(&iris.0),
            other: Compressor::train(&others.0),
        };
        drop((iris, others));
        let mut stored = Vec::with_capacity(CODEC_BYTES);
        trained.write(&mut stored);
        drop(trained);
        let codec = Self::read(&stored).expect("a table just written reads");
        // Rebuilt, it holds its tables only.
        drop(stored);
        held.resize(CODEC_HELD)?;
        Ok(Trained { codec, _held: held })
    }

    /// Every `count / SAMPLE`-th of `count` entries: the keys to train on.
    pub(crate) fn sample_indexes(count: u64) -> impl Iterator<Item = u64> {
        let step = (count / SAMPLE as u64).max(1);
        (0..count).step_by(step as usize)
    }

    fn table(&self, tag: u8) -> &Compressor {
        if tag == TAG_IRI {
            &self.iri
        } else {
            &self.other
        }
    }

    /// Appends `key` (plain) as stored: its tag, then the rest compressed.
    pub(crate) fn compress(&self, key: &[u8], out: &mut Vec<u8>) {
        let Some((&tag, rest)) = key.split_first() else {
            return;
        };
        out.push(tag);
        out.extend_from_slice(&self.table(tag).compress(rest));
    }

    /// [`Self::compress`] into room in `out` charged first: the tag, and twice the rest
    /// (FSST's largest output, every byte escaped).
    pub(crate) fn compress_within<B: Deref<Target = Budget>>(
        &self,
        key: &[u8],
        out: &mut ChargedVec<u8, B>,
    ) -> Result<(), BudgetExceeded> {
        let Some((&tag, rest)) = key.split_first() else {
            return Ok(());
        };
        out.fill(1 + 2 * rest.len(), |room| {
            room.push(tag);
            let at = room.len();
            // SAFETY: the room left holds twice the rest, the most FSST writes.
            let len = unsafe {
                self.table(tag)
                    .compress_into(rest, room.spare_capacity_mut())
            };
            // SAFETY: `compress_into` initialised `len` bytes of the room left.
            unsafe { room.set_len(at + len) };
        })
    }

    /// [`Self::decompress`] into room in `out` charged first: FSST's bound for the rest.
    pub(crate) fn decompress_within<B: Deref<Target = Budget>>(
        &self,
        stored: &[u8],
        out: &mut ChargedVec<u8, B>,
    ) -> Result<(), BudgetExceeded> {
        let Some((&tag, rest)) = stored.split_first() else {
            return Ok(());
        };
        let decompressor = self.table(tag).decompressor();
        out.fill(
            1 + decompressor.max_decompression_capacity(rest) + 8,
            |room| {
                room.push(tag);
                let at = room.len();
                let len = decompressor.decompress_into(rest, room.spare_capacity_mut());
                // SAFETY: `decompress_into` initialised `len` bytes of the room left.
                unsafe { room.set_len(at + len) };
            },
        )
    }

    /// Appends the plain key of `stored`.
    pub(crate) fn decompress(&self, stored: &[u8], out: &mut Vec<u8>) {
        let Some((&tag, rest)) = stored.split_first() else {
            return;
        };
        out.push(tag);
        let decompressor = self.table(tag).decompressor();
        out.reserve(decompressor.max_decompression_capacity(rest) + 8);
        let len = decompressor.decompress_into(rest, out.spare_capacity_mut());
        // SAFETY: `decompress_into` initialised `len` bytes of the spare capacity.
        unsafe { out.set_len(out.len() + len) };
    }

    /// The tables as stored (module docs of [`crate::durability::checkpoint`]).
    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        for table in [&self.iri, &self.other] {
            let symbols = table.symbol_table();
            let lengths = table.symbol_lengths();
            for i in 0..255 {
                let symbol = symbols.get(i).map_or(0, |s| s.to_u64());
                out.extend_from_slice(&symbol.to_le_bytes());
            }
            for i in 0..255 {
                out.push(lengths.get(i).copied().unwrap_or(0));
            }
            out.push(symbols.len() as u8);
        }
    }

    /// The tables [`Self::write`] stored; `None` if they are damaged.
    pub(crate) fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != CODEC_BYTES {
            return None;
        }
        let table = |bytes: &[u8]| -> Option<Compressor> {
            let count = usize::from(bytes[TABLE_BYTES - 1]);
            if count > 255 {
                return None;
            }
            let symbols: Vec<Symbol> = bytes[..255 * 8]
                .as_chunks::<8>()
                .0
                .iter()
                .take(count)
                .map(Symbol::from_slice)
                .collect();
            let lengths = bytes[255 * 8..255 * 8 + count].to_vec();
            if lengths.iter().any(|&n| !(1..=8).contains(&n)) {
                return None;
            }
            Some(Compressor::rebuild_from(symbols, lengths))
        };
        Some(Self {
            iri: table(&bytes[..TABLE_BYTES])?,
            other: table(&bytes[TABLE_BYTES..])?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_compress_as_stored() {
        let keys: Vec<Vec<u8>> = (0..2_000)
            .map(|i| match i % 3 {
                0 => format!("Ihttp://www.wikidata.org/entity/Q{i}").into_bytes(),
                1 => format!("Sa label of item number {i}").into_bytes(),
                _ => format!("Len\u{0}the label {i} in English").into_bytes(),
            })
            .chain([b"I".to_vec(), b"S".to_vec()])
            .collect();
        let codec = Codec::train(&keys);
        let mut stored_tables = Vec::new();
        codec.write(&mut stored_tables);
        let reread = Codec::read(&stored_tables).expect("tables read");
        let (mut plain, mut stored, mut again) = (0, 0, Vec::new());
        for key in &keys {
            let mut compressed = Vec::new();
            codec.compress(key, &mut compressed);
            // A reader's codec compresses alike (lookups compare compressed keys).
            again.clear();
            reread.compress(key, &mut again);
            assert_eq!(again, compressed);
            let mut decoded = Vec::new();
            reread.decompress(&compressed, &mut decoded);
            assert_eq!(&decoded, key);
            plain += key.len();
            stored += compressed.len();
        }
        assert!(stored * 2 < plain, "{stored} of {plain} bytes");
        assert!(Codec::read(&stored_tables[1..]).is_none());
    }
}
