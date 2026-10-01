//! The dictionary's key hash: fixed for all time, because checkpoints store hash tables.
//!
//! A checkpoint holds the dictionary's hash table ready to map ([`crate::mapped`]), so the
//! hash of a key must not change with a library update or between machines, as a seeded
//! or crate-defined hasher may. This one is defined here, in the style of wyhash: 16 bytes
//! per step folded through a 64 × 64 → 128-bit multiply, the length mixed in, so keys that
//! differ only in trailing zero bytes still differ.
//!
//! Changing it means a new checkpoint format.

const P0: u64 = 0xa076_1d64_78bd_642f;
const P1: u64 = 0xe703_7ed1_a0b4_28db;
const P2: u64 = 0x8ebc_6af0_9c88_c6e3;

#[inline]
fn mum(a: u64, b: u64) -> u64 {
    let product = u128::from(a) * u128::from(b);
    (product as u64) ^ ((product >> 64) as u64)
}

#[inline]
fn word(bytes: &[u8]) -> u64 {
    let mut padded = [0u8; 8];
    padded[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(padded)
}

/// The hash of a dictionary key.
#[inline]
pub(crate) fn key_hash(bytes: &[u8]) -> u64 {
    let len = bytes.len() as u64;
    let mut seed = P0 ^ len.wrapping_mul(P1);
    let (chunks, rest) = bytes.as_chunks::<16>();
    for chunk in chunks {
        let (a, b) = chunk.split_at(8);
        seed = mum(word(a) ^ P1, word(b) ^ seed);
    }
    let (a, b) = rest.split_at(rest.len().min(8));
    mum(P1 ^ len, mum(word(a) ^ P2, word(b) ^ seed))
}

#[cfg(test)]
mod tests {
    use super::key_hash;

    /// The values are part of the checkpoint format: they must never change.
    #[test]
    fn the_hash_is_fixed() {
        // Computed independently (a Python transcription of the algorithm).
        let expected: [(&[u8], u64); 5] = [
            (b"", 0x18c7_fcc6_51d1_587e),
            (b"Ihttp://example.org/a", 0x0c40_cb1d_97ed_cade),
            (b"Sx", 0x34f9_3a80_0311_de89),
            (b"Sx\0", 0x48fc_ad7b_45a5_ea07),
            (
                b"Ihttp://dbpedia.org/resource/Berlin",
                0xcdaa_55cb_2a6f_77fd,
            ),
        ];
        for (key, hash) in expected {
            assert_eq!(key_hash(key), hash, "{key:?}");
        }
        assert_ne!(key_hash(b""), key_hash(b"\0"));
    }

    #[test]
    fn hashes_spread_over_buckets() {
        // 100,000 IRI-like keys into 2^16 buckets: close to the expected occupancy.
        let mut used = vec![false; 1 << 16];
        for i in 0..100_000u32 {
            let key = format!("Ihttp://example.org/resource/{i}");
            used[(key_hash(key.as_bytes()) & 0xffff) as usize] = true;
        }
        let occupied = used.iter().filter(|&&u| u).count() as f64 / used.len() as f64;
        // 1 - e^(-100000/65536) = 0.782
        assert!((0.76..0.80).contains(&occupied), "{occupied}");
    }
}
