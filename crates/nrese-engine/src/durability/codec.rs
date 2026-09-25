//! Pure byte formats for WAL records and checkpoints. No I/O here: the shells in
//! [`wal`](super::wal) and [`checkpoint`](super::checkpoint) own the files.
//!
//! All integers are little-endian and fixed width. A WAL frame is
//! `len: u32 | crc32(payload): u32 | payload`, where the payload is
//!
//! ```text
//! revision u64 | dictionary_start u64
//! key_count u32 | (key_len u32, key bytes)*
//! insert_count u32 | (s p o g: 4 x u64)*
//! delete_count u32 | (s p o g: 4 x u64)*
//! ```

use crate::error::{EngineError, EngineResult};
use crate::quad::EncodedQuad;

/// Size of the `len | crc` frame header.
pub(crate) const FRAME_HEADER: usize = 8;
const QUAD_BYTES: usize = 32;

/// One committed transaction as logged: the terms it added to the dictionary (a contiguous
/// id range starting at `dictionary_start`) and its exact delta.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CommitRecord {
    pub revision: u64,
    pub dictionary_start: u64,
    pub keys: Vec<Vec<u8>>,
    pub inserts: Vec<EncodedQuad>,
    pub deletes: Vec<EncodedQuad>,
}

/// Exact payload size of `record` in bytes.
fn payload_len(record: &CommitRecord) -> u64 {
    let keys: u64 = record.keys.iter().map(|key| 4 + key.len() as u64).sum();
    let quads = (record.inserts.len() + record.deletes.len()) as u64 * QUAD_BYTES as u64;
    8 + 8 + 4 + keys + 4 + 4 + quads
}

/// Appends one framed record to `out`. Fails without writing anything if the payload does
/// not fit the 32-bit frame length (about 130 M quads); such loads go through the bulk
/// loader, which writes a checkpoint instead of a WAL record.
pub(crate) fn encode_record(record: &CommitRecord, out: &mut Vec<u8>) -> EngineResult<()> {
    let len = payload_len(record);
    let payload_len =
        u32::try_from(len).map_err(|_| EngineError::TransactionTooLarge { bytes: len })?;
    out.reserve(FRAME_HEADER + payload_len as usize);
    let frame_start = out.len();
    out.extend_from_slice(&[0; FRAME_HEADER]);
    let payload_start = out.len();
    put_u64(out, record.revision);
    put_u64(out, record.dictionary_start);
    put_u32(out, record.keys.len() as u32);
    for key in &record.keys {
        put_u32(out, key.len() as u32);
        out.extend_from_slice(key);
    }
    put_quads(out, &record.inserts);
    put_quads(out, &record.deletes);
    debug_assert_eq!((out.len() - payload_start) as u64, len);
    let crc = crc32fast::hash(&out[payload_start..]);
    out[frame_start..frame_start + 4].copy_from_slice(&payload_len.to_le_bytes());
    out[frame_start + 4..payload_start].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Result of decoding the frame at the start of a byte slice.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// A complete, checksummed record and the number of bytes it occupied.
    Record(CommitRecord, usize),
    /// No bytes left.
    End,
    /// An incomplete or checksum-failing frame: a torn write at the tail of the log, or
    /// corruption anywhere else. The caller decides which, based on the position.
    Invalid,
}

pub(crate) fn decode_frame(bytes: &[u8]) -> Frame {
    if bytes.is_empty() {
        return Frame::End;
    }
    let mut header = Reader::new(bytes);
    let (Some(len), Some(crc)) = (header.u32(), header.u32()) else {
        return Frame::Invalid;
    };
    let Some(payload) = bytes.get(FRAME_HEADER..FRAME_HEADER + len as usize) else {
        return Frame::Invalid;
    };
    if crc32fast::hash(payload) != crc {
        return Frame::Invalid;
    }
    match decode_payload(payload) {
        Some(record) => Frame::Record(record, FRAME_HEADER + len as usize),
        None => Frame::Invalid,
    }
}

fn decode_payload(payload: &[u8]) -> Option<CommitRecord> {
    let mut reader = Reader::new(payload);
    let revision = reader.u64()?;
    let dictionary_start = reader.u64()?;
    let key_count = reader.u32()?;
    let keys = (0..key_count)
        .map(|_| {
            let len = reader.u32()?;
            reader.bytes(len as usize).map(<[u8]>::to_vec)
        })
        .collect::<Option<_>>()?;
    let inserts = reader.quads()?;
    let deletes = reader.quads()?;
    reader.is_done().then_some(CommitRecord {
        revision,
        dictionary_start,
        keys,
        inserts,
        deletes,
    })
}

pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_quad(out: &mut Vec<u8>, quad: &EncodedQuad) {
    for component in quad.components() {
        put_u64(out, component);
    }
}

fn put_quads(out: &mut Vec<u8>, quads: &[EncodedQuad]) {
    put_u32(out, quads.len() as u32);
    for quad in quads {
        put_quad(out, quad);
    }
}

/// Bounds-checked little-endian cursor; every read returns `None` past the end.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some(slice)
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.bytes(4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.bytes(8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    pub(crate) fn quad(&mut self) -> Option<EncodedQuad> {
        Some(EncodedQuad::from_components([
            self.u64()?,
            self.u64()?,
            self.u64()?,
            self.u64()?,
        ]))
    }

    fn quads(&mut self) -> Option<Vec<EncodedQuad>> {
        let count = self.u32()? as usize;
        // Guard the allocation against corrupt counts.
        if count > self.remaining() / QUAD_BYTES {
            return None;
        }
        (0..count).map(|_| self.quad()).collect()
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub(crate) fn is_done(&self) -> bool {
        self.remaining() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{TermId, TermKind};

    fn sample() -> CommitRecord {
        let id = |n| TermId::new(TermKind::Iri, n);
        CommitRecord {
            revision: 7,
            dictionary_start: 3,
            keys: vec![b"Ihttp://a".to_vec(), b"Sb".to_vec()],
            inserts: vec![EncodedQuad::new(id(3), id(4), id(1), TermId::DEFAULT_GRAPH)],
            deletes: vec![EncodedQuad::new(id(0), id(1), id(2), id(9))],
        }
    }

    #[test]
    fn records_roundtrip() {
        let mut bytes = Vec::new();
        encode_record(&sample(), &mut bytes).unwrap();
        encode_record(&CommitRecord::default(), &mut bytes).unwrap();
        let Frame::Record(first, used) = decode_frame(&bytes) else {
            panic!("first frame")
        };
        assert_eq!(first, sample());
        assert_eq!(
            decode_frame(&bytes[used..]),
            Frame::Record(CommitRecord::default(), bytes.len() - used)
        );
        assert_eq!(decode_frame(&[]), Frame::End);
    }

    #[test]
    fn every_truncation_and_bit_flip_is_detected() {
        let mut bytes = Vec::new();
        encode_record(&sample(), &mut bytes).unwrap();
        for cut in 1..bytes.len() {
            assert_eq!(decode_frame(&bytes[..cut]), Frame::Invalid, "cut at {cut}");
        }
        for i in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[i] ^= 0x10;
            assert!(
                !matches!(decode_frame(&flipped), Frame::Record(ref r, _) if *r == sample()),
                "flip at {i}"
            );
        }
    }
}
