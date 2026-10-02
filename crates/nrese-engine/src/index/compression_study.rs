//! What other encodings of the packed permutations would take, measured on a checkpoint:
//! for the decision on the store-size gap to QLever (docs/reviews/2026-10-02-suite-results.md).
//!
//! ```text
//! NRESE_STUDY_CHECKPOINT=/path/checkpoint-….nck \
//!   cargo test --release -p nrese-engine --lib compression_study -- --ignored --nocapture
//! ```
//!
//! Per block of keys and per key position it compares the current frame of reference
//! (tag and payload bit-packed relative to the block's minimum) with a palette (the
//! block's distinct values, each key an index into them) and, for the first position that
//! varies in the block (sorted there), deltas between neighbours; the cheaper one counts.
//! Headers and first keys are counted as they are now.

use super::keys::{BLOCK, PackedKeys};
use crate::quad::Key;

/// Bits for values `0..=max`.
fn bits(max: u64) -> u64 {
    u64::from(64 - max.leading_zeros())
}

/// The current encoding's bits for one position of `block`: tag and payload offsets.
fn frame_of_reference(values: &[u64]) -> u64 {
    let tags = values.iter().map(|v| v >> 60);
    let payloads = values.iter().map(|v| v & ((1 << 60) - 1));
    let tag_range = tags.clone().max().unwrap() - tags.min().unwrap();
    let payload_range = payloads.clone().max().unwrap() - payloads.min().unwrap();
    values.len() as u64 * (bits(tag_range) + bits(payload_range))
}

fn palette(values: &[u64]) -> u64 {
    let mut distinct = values.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() == 1 {
        return 0;
    }
    values.len() as u64 * bits(distinct.len() as u64 - 1) + 64 * distinct.len() as u64
}

fn deltas(values: &[u64]) -> Option<u64> {
    let largest = values
        .windows(2)
        .map(|w| w[1].checked_sub(w[0]))
        .collect::<Option<Vec<u64>>>()?
        .into_iter()
        .max()?;
    Some((values.len() as u64 - 1) * bits(largest))
}

#[derive(Default)]
struct Totals {
    keys: u64,
    /// Bits of the packed data now, and with the cheapest choice per position.
    now: u64,
    alternative: u64,
    /// The cheaper of frame of reference and palette only (keys stay O(1) to read).
    random_access: u64,
    /// How often each choice won: frame of reference, palette, deltas.
    wins: [u64; 3],
}

fn study(keys: &PackedKeys, totals: &mut Totals) {
    let mut block: Vec<Key> = Vec::with_capacity(BLOCK);
    for start in (0..keys.len()).step_by(BLOCK) {
        block.clear();
        keys.decode_range(start, (start + BLOCK).min(keys.len()), &mut block);
        totals.keys += block.len() as u64;
        let mut first_varying = true;
        for c in 0..4 {
            let values: Vec<u64> = block.iter().map(|k| k[c]).collect();
            let now = frame_of_reference(&values);
            totals.now += now;
            if now == 0 {
                continue;
            }
            let mut choices = [now, palette(&values), u64::MAX];
            totals.random_access += now.min(choices[1]);
            if first_varying {
                choices[2] = deltas(&values).unwrap_or(u64::MAX);
                first_varying = false;
            }
            let (best, cost) = choices
                .iter()
                .enumerate()
                .min_by_key(|(_, cost)| **cost)
                .unwrap();
            totals.alternative += cost;
            totals.wins[best] += 1;
        }
    }
}

#[test]
#[ignore = "reads a checkpoint named by NRESE_STUDY_CHECKPOINT"]
fn compression_study() {
    let Some(path) = std::env::var_os("NRESE_STUDY_CHECKPOINT") else {
        return;
    };
    let (_, stacks) = crate::durability::checkpoint::map_written(std::path::Path::new(&path))
        .expect("checkpoint");
    for (name, index) in ["asserted", "inferred"].iter().zip(&stacks) {
        for run in index.runs() {
            for &permutation in run.layout().permutations() {
                let mut totals = Totals::default();
                study(&run.permutation(permutation).keys, &mut totals);
                if totals.keys == 0 {
                    continue;
                }
                // Headers (72 bytes) and first keys (32 bytes) per block, as now.
                let blocks = totals.keys.div_ceil(BLOCK as u64);
                let fixed = blocks * (72 + 32);
                let per_key = |data_bits: u64| (data_bits / 8 + fixed) as f64 / totals.keys as f64;
                eprintln!(
                    "{name} {permutation:?}: {} keys, now {:.2} B/key, best of three {:.2} B/key, without deltas {:.2} B/key (data {:.2} -> {:.2}); wins FOR {} palette {} deltas {}",
                    totals.keys,
                    per_key(totals.now),
                    per_key(totals.alternative),
                    per_key(totals.random_access),
                    totals.now as f64 / 8.0 / totals.keys as f64,
                    totals.alternative as f64 / 8.0 / totals.keys as f64,
                    totals.wins[0],
                    totals.wins[1],
                    totals.wins[2],
                );
            }
        }
    }
}
