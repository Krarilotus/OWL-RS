//! FSST-compressed dictionary keys (`store.vocabulary = "fsst"`; design §3 item 4,
//! docs/plan/2026-10-02-research-designs.md).
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

use std::sync::atomic::{AtomicU8, Ordering};

use fsst::{Compressor, Symbol};

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
const SAMPLE: usize = 1 << 16;
/// Bytes of one table on file: 255 symbols of 8 bytes, then their 255 lengths, then the
/// number of symbols in use.
const TABLE_BYTES: usize = 255 * 8 + 255 + 1;
/// Bytes of the two tables on file.
pub(crate) const CODEC_BYTES: usize = 2 * TABLE_BYTES;

/// The symbol tables of a vocabulary: one for IRIs, one for the rest.
pub(crate) struct Codec {
    iri: Compressor,
    other: Compressor,
}

impl Codec {
    /// Tables trained on a sample of `keys` (plain), rebuilt from their stored form.
    pub(crate) fn train(keys: &[Vec<u8>]) -> Self {
        let (iris, others): (Vec<&[u8]>, Vec<&[u8]>) = keys
            .iter()
            .map(|key| key.as_slice())
            .partition(|key| key.first() == Some(&TAG_IRI));
        let rest =
            |keys: &[&[u8]]| -> Vec<Vec<u8>> { keys.iter().map(|key| key[1..].to_vec()).collect() };
        let train = |keys: Vec<Vec<u8>>| {
            let refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
            Compressor::train(&refs)
        };
        let trained = Self {
            iri: train(rest(&iris)),
            other: train(rest(&others)),
        };
        let mut stored = Vec::with_capacity(CODEC_BYTES);
        trained.write(&mut stored);
        Self::read(&stored).expect("a table just written reads")
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
