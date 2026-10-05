//! Permutations derived from a sorted one by a stable partition instead of a sort.
//!
//! Keys sorted in order X, partitioned stably by one component c, are sorted in the order
//! T = c followed by X without c: among keys with the same c, X's order over the other
//! components is T's. With few distinct values of c (predicates, graphs), that is a
//! counting partition: one pass to count, one to scatter, instead of an O(n log n)
//! comparison sort of 32-byte keys. PSOG comes from SPOG and POSG from OSPG by
//! predicate; each graph-first permutation comes from its graph-last one by graph.
//!
//! What is partitioned are the keys' positions (4 bytes each), not the keys (32): the
//! packer gathers each block's keys through them ([`PackedKeys::from_sorted_gathered`]),
//! so a derived permutation holds an eighth of a copy of the keys, and positions
//! partition again for a permutation derived from a derived one.

use rayon::prelude::*;

use super::keys::PackedKeys;
use crate::quad::{Key, Permutation};

/// Distinct values of the partitioning component beyond which the caller sorts instead:
/// the counting tables grow with them (millions of named graphs).
const MAX_BUCKETS: usize = 1 << 16;

/// Keys per parallel part, at least.
const PART: usize = 1 << 15;

/// The position in `from`'s keys of the component `to` starts with, if `to` is that
/// component followed by `from`'s order without it (and differs from `from`).
pub(crate) fn derivable(from: Permutation, to: Permutation) -> Option<usize> {
    let (f, t) = (from.order(), to.order());
    if f == t {
        return None;
    }
    let at = f.iter().position(|&c| c == t[0])?;
    f.iter()
        .copied()
        .filter(|&c| c != t[0])
        .eq(t[1..].iter().copied())
        .then_some(at)
}

/// Keys stored in one permutation (`base`), visited in the order of another through their
/// positions, which sort them in that order.
pub(crate) struct Ordered<'k> {
    pub keys: &'k [Key],
    pub base: Permutation,
    /// The order the positions sort the keys in.
    pub order: Permutation,
    /// `None`: the keys' own order (`order == base`).
    pub positions: Option<&'k [u32]>,
}

impl Ordered<'_> {
    fn len(&self) -> usize {
        self.positions.map_or(self.keys.len(), <[u32]>::len)
    }

    fn position(&self, i: usize) -> usize {
        self.positions.map_or(i, |p| p[i] as usize)
    }

    /// The keys in `to` (derivable from this order), as positions: a stable partition on
    /// `to`'s first component. `None` if `to` isn't derivable, the keys don't fit `u32`
    /// positions, or that component has more than [`MAX_BUCKETS`] values (the caller
    /// sorts).
    pub(crate) fn partitioned(&self, to: Permutation) -> Option<Vec<u32>> {
        derivable(self.order, to)?;
        u32::try_from(self.keys.len()).ok()?;
        let n = self.len();
        // The component's place in the stored keys.
        let at = self.base.order().iter().position(|&c| c == to.order()[0])?;
        let value = |i: usize| self.keys[self.position(i)][at];
        let part = (n / (rayon::current_num_threads() * 4)).max(PART);
        let parts: Vec<std::ops::Range<usize>> =
            (0..n).step_by(part).map(|s| s..(s + part).min(n)).collect();
        // The distinct values, sorted: a key's bucket is its value's rank.
        let mut values: Vec<u64> = parts
            .par_iter()
            .map(|range| {
                let mut seen = hashbrown::HashSet::new();
                for i in range.clone() {
                    seen.insert(value(i));
                    if seen.len() > MAX_BUCKETS {
                        return None;
                    }
                }
                Some(seen.into_iter().collect::<Vec<u64>>())
            })
            .collect::<Option<Vec<_>>>()?
            .concat();
        values.par_sort_unstable();
        values.dedup();
        if values.len() > MAX_BUCKETS {
            return None;
        }
        let bucket = |i: usize| values.binary_search(&value(i)).expect("a seen value");
        // Each part's count per bucket, then each part's first output per bucket: buckets
        // in order, and within a bucket the parts in order, which keeps it stable.
        let counts: Vec<Vec<usize>> = parts
            .par_iter()
            .map(|range| {
                let mut count = vec![0usize; values.len()];
                for i in range.clone() {
                    count[bucket(i)] += 1;
                }
                count
            })
            .collect();
        let mut starts: Vec<Vec<usize>> = vec![vec![0; values.len()]; parts.len()];
        let mut next = 0;
        for b in 0..values.len() {
            for (p, count) in counts.iter().enumerate() {
                starts[p][b] = next;
                next += count[b];
            }
        }
        debug_assert_eq!(next, n);
        let mut out: Vec<u32> = Vec::with_capacity(n);
        let target = Target(out.as_mut_ptr());
        parts
            .par_iter()
            .zip(starts.into_par_iter())
            .for_each(|(range, mut cursor)| {
                let target = &target;
                for i in range.clone() {
                    let b = bucket(i);
                    // SAFETY: the parts' cursors cover disjoint ranges of `0..n` (counted
                    // above), each written once, within `out`'s capacity; positions fit
                    // `u32` (checked above).
                    unsafe { target.0.add(cursor[b]).write(self.position(i) as u32) };
                    cursor[b] += 1;
                }
            });
        // SAFETY: every position below `n` was written once by the scatter above.
        unsafe { out.set_len(n) };
        Some(out)
    }

    /// The keys in `order`, packed.
    pub(crate) fn packed(&self) -> PackedKeys {
        let (keys, base, to) = (self.keys, self.base, self.order);
        match self.positions {
            None => PackedKeys::from_sorted(keys),
            Some(positions) => PackedKeys::from_sorted_gathered(positions.len(), |range, out| {
                out.extend(
                    positions[range]
                        .iter()
                        .map(|&p| to.to_key(&base.key_to_quad(&keys[p as usize]))),
                );
            }),
        }
    }
}

/// The scatter's destination, shared by the parts, which write disjoint positions.
struct Target(*mut u32);

// SAFETY: the parts write disjoint positions of one allocation that outlives them.
unsafe impl Sync for Target {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quad::EncodedQuad;

    fn quads(n: usize, graphs: u64) -> Vec<EncodedQuad> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = |m: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % m
        };
        (0..n)
            .map(|_| EncodedQuad::from_components([next(500), next(7), next(900), next(graphs)]))
            .collect()
    }

    fn sorted(quads: &[EncodedQuad], permutation: Permutation) -> Vec<Key> {
        let mut keys: Vec<Key> = quads.iter().map(|q| permutation.to_key(q)).collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    #[test]
    fn the_derivable_pairs_are_the_partitions_by_a_leading_component() {
        use Permutation::*;
        assert_eq!(derivable(Spog, Psog), Some(1));
        assert_eq!(derivable(Ospg, Posg), Some(2));
        assert_eq!(derivable(Spog, Gspo), Some(3));
        assert_eq!(derivable(Posg, Gpos), Some(3));
        assert_eq!(derivable(Ospg, Gosp), Some(3));
        assert_eq!(derivable(Psog, Gpso), Some(3));
        // OSPG is SPOG partitioned by object; with many objects that falls back to a sort.
        assert_eq!(derivable(Spog, Ospg), Some(2));
        assert_eq!(derivable(Spog, Posg), None);
        assert_eq!(derivable(Posg, Psog), None);
        assert_eq!(derivable(Spog, Spog), None);
    }

    #[test]
    fn a_partition_equals_a_sort_for_every_derivable_pair_and_chain() {
        let mut derived_pairs = 0;
        for graphs in [1, 3, 50] {
            let quads = quads(100_000, graphs);
            for base in Permutation::ALL {
                let keys = sorted(&quads, base);
                let own = Ordered {
                    keys: &keys,
                    base,
                    order: base,
                    positions: None,
                };
                for to in Permutation::ALL {
                    let Some(positions) = own.partitioned(to) else {
                        continue;
                    };
                    derived_pairs += 1;
                    let derived = Ordered {
                        keys: &keys,
                        base,
                        order: to,
                        positions: Some(&positions),
                    };
                    let packed: Vec<Key> = derived.packed().iter().collect();
                    assert_eq!(packed, sorted(&quads, to), "{base:?} -> {to:?}");
                    // And one step further, from the derived positions.
                    for next in Permutation::ALL {
                        if let Some(further) = derived.partitioned(next) {
                            let again = Ordered {
                                keys: &keys,
                                base,
                                order: next,
                                positions: Some(&further),
                            };
                            let packed: Vec<Key> = again.packed().iter().collect();
                            assert_eq!(
                                packed,
                                sorted(&quads, next),
                                "{base:?} -> {to:?} -> {next:?}"
                            );
                        }
                    }
                }
            }
        }
        assert!(derived_pairs > 20, "{derived_pairs}");
    }

    #[test]
    fn many_distinct_values_leave_it_to_a_sort() {
        let quads = quads(200_000, 1 << 20);
        let keys = sorted(&quads, Permutation::Spog);
        let own = Ordered {
            keys: &keys,
            base: Permutation::Spog,
            order: Permutation::Spog,
            positions: None,
        };
        assert!(own.partitioned(Permutation::Gspo).is_none());
    }
}
