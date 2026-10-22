//! Format 12's fixed, bounded checkpoint header after the magic.
//!
//! The guard covers only revision, flags and clock floor (24 bytes), independently of
//! the optional full-file verification. An invalid time fact must not advance the clock.

use crate::durability::codec::{Reader, put_u32, put_u64};
use crate::durability::history::CommitTime;

pub(super) const MAGIC: &[u8; 8] = b"NRESECKC";
/// Flags bit: integer-derived literals are dictionary entries.
pub(super) const INTEGERS_IN_DICTIONARY: u64 = 1;
/// Flags bit: the dictionary's keys are compressed.
pub(super) const COMPRESSED_KEYS: u64 = 2;
/// Flags bit: the checkpoint has a known last-commit time.
const HISTORY_KNOWN: u64 = 4;

pub(super) struct Header {
    pub revision: u64,
    pub flags: u64,
    pub committed: CommitTime,
}

impl Header {
    /// Appends the 32 bytes after the magic, without flushing the checkpoint writer.
    pub fn write(out: &mut Vec<u8>, revision: u64, flags: u64, committed: CommitTime) {
        let start = out.len();
        put_u64(out, revision);
        let known = committed.known && (revision == 0 || committed.clock != 0);
        put_u64(out, flags | if known { HISTORY_KNOWN } else { 0 });
        put_u64(out, committed.clock);
        let guard = crc32fast::hash(&out[start..]);
        put_u32(out, guard);
        put_u32(out, 0);
    }

    /// Reads and guards exactly 32 bytes, even when full-file verification is disabled.
    pub fn read(reader: &mut Reader<'_>) -> Result<Self, &'static str> {
        let fields = reader.bytes(24).ok_or("truncated header")?;
        let mut values = Reader::new(fields);
        let revision = values.u64().expect("8 of 24 bytes");
        let flags = values.u64().expect("16 of 24 bytes");
        let clock = values.u64().expect("24 bytes");
        let guard = reader.u32().ok_or("truncated header")?;
        let reserved = reader.u32().ok_or("truncated header")?;
        if flags & !(INTEGERS_IN_DICTIONARY | COMPRESSED_KEYS | HISTORY_KNOWN) != 0 {
            return Err("unknown flags");
        }
        let committed = if reserved != 0 || crc32fast::hash(fields) != guard {
            CommitTime::unknown()
        } else {
            CommitTime {
                clock,
                known: flags & HISTORY_KNOWN != 0 && (revision == 0 || clock != 0),
            }
        };
        Ok(Self {
            revision,
            flags,
            committed,
        })
    }
}
