//! Sorting and deduplicating fixed-width id keys by radix over their significant bits:
//! the one sort kernel for bulk loads, permutations, the reasoner's runs and query
//! operators (performance.md §4; investigation §1 #9).
//!
//! A key is `N` ids compared lexicographically (a quad, a triple, a pair). Ids use few of
//! their 64 bits: LUBM's take 3–4 bytes each, and a predicate one. The kernel finds, per
//! component, the kind tags and payload ranges the keys span, packs each key into one or
//! two words holding only a tag's rank and the payload's offset per component (component
//! 0 highest, so the packed order is the keys' order), sorts the packed words by radix, and unpacks: one pass over main memory by the
//! most significant digit (up to 4,096 buckets of about 32 k values), then each bucket
//! by a least-significant-digit radix sort inside the cache, in parallel. A quad of
//! LUBM-1000 sorts as one 64-bit word instead of four.
//!
//! **In place.** The packed words and the radix sort's scratch both fit in the keys' own
//! buffer: packed keys of `W` words go to its end, the scratch to its start, which is
//! possible where `N >= 2W`. What packing overwrites before reading, the keys of the last
//! `W/N` of the buffer, is packed into a side vector first (a 16th of the buffer for
//! quads into one word), and unpacking does the same in reverse. So the kernel needs
//! about as little memory as an in-place comparison sort.
//!
//! **Deterministic.** The result is the sorted keys (deduplicated with
//! [`sort_dedup_keys`]), the same at any thread count: every pass distributes stably.
//!
//! Keys whose components span more than 64 bits together (two words measured slower than
//! a parallel comparison sort), and short inputs, are sorted by comparison instead.

use rayon::prelude::*;

/// Inputs shorter than this are sorted by comparison: the radix passes' fixed costs (a
/// histogram per pass) don't pay below it.
const RADIX_FROM: usize = 1 << 12;

/// Keys per chunk of a parallel pass.
const CHUNK: usize = 1 << 16;

/// Bits per radix digit.
const DIGIT: u32 = 8;
const BUCKETS: usize = 1 << DIGIT;

/// Sorts `keys` ascending (lexicographically by component).
pub fn sort_keys<const N: usize>(keys: &mut [[u64; N]]) {
    sort_inner(keys, false);
}

/// Sorts `keys` ascending and moves the distinct ones to the front; returns their number
/// (truncate a vector to it).
pub fn sort_dedup_keys<const N: usize>(keys: &mut [[u64; N]]) -> usize {
    sort_inner(keys, true)
}

/// What a sort did, for tests and lab counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// By comparison.
    Comparison,
    /// By radix over packed words of this many significant bits.
    Radix { bits: u32 },
}

/// How [`sort_keys`] would sort `keys`: by comparison, or by radix over how many bits.
pub fn method<const N: usize>(keys: &[[u64; N]]) -> Method {
    if keys.len() < RADIX_FROM || !likely_one_word(keys) {
        return Method::Comparison;
    }
    match Layout::of(keys) {
        Some(layout) if layout.words() == 1 && fits(N, 1) => Method::Radix { bits: layout.bits },
        _ => Method::Comparison,
    }
}

/// Whether keys of `n` words can be packed into `words` words with the scratch in place.
fn fits(n: usize, words: usize) -> bool {
    words > 0 && n >= 2 * words
}

/// Sorts (and with `dedup` deduplicates) `keys`; returns the number of keys kept, which
/// are at the front.
fn sort_inner<const N: usize>(keys: &mut [[u64; N]], dedup: bool) -> usize {
    if keys.len() < RADIX_FROM || !likely_one_word(keys) {
        return comparison(keys, dedup);
    }
    let Some(layout) = Layout::of(keys) else {
        return comparison(keys, dedup);
    };
    match layout.words() {
        0 => {
            // Every key the same.
            if dedup { keys.len().min(1) } else { keys.len() }
        }
        // Two-word keys sort faster by comparison (10 M of 116 bits: 99–177 ms against
        // 71–128); the radix pass for them stays for when that changes.
        1 if fits(N, 1) => radix::<N, 1>(keys, &layout, dedup),
        _ => comparison(keys, dedup),
    }
}

/// Keys sampled to decide whether the radix sort can pay ([`likely_one_word`]).
const SAMPLE: usize = 4096;

/// Whether an evenly spaced sample of `keys` packs into one word: if not, neither do the
/// keys, and the full scan for their layout is spared (it costs a tenth of a sort).
fn likely_one_word<const N: usize>(keys: &[[u64; N]]) -> bool {
    let step = (keys.len() / SAMPLE).max(1);
    let sample: Vec<[u64; N]> = keys.iter().step_by(step).copied().collect();
    Layout::of(&sample).is_some_and(|layout| layout.words() <= 1 && fits(N, 1))
}

fn comparison<const N: usize>(keys: &mut [[u64; N]], dedup: bool) -> usize {
    if keys.len() >= RADIX_FROM {
        keys.par_sort_unstable();
    } else {
        keys.sort_unstable();
    }
    if !dedup {
        return keys.len();
    }
    let mut kept = 0;
    for i in 0..keys.len() {
        if kept == 0 || keys[i] != keys[kept - 1] {
            keys[kept] = keys[i];
            kept += 1;
        }
    }
    kept
}

/// The bits of an id's payload; the 4 above them are its kind tag (`TermId`).
const PAYLOAD_BITS: u32 = 60;
const PAYLOAD: u64 = (1 << PAYLOAD_BITS) - 1;

/// How one key component is packed: the kind tags its ids have, ranked in order, and per
/// tag the smallest payload. A value packs as its tag's rank above its payload's offset
/// from that minimum: ids sort by tag, then payload, and so do the packed values. An
/// object column of IRIs and strings then takes one rank bit above the widest payload
/// range, not the 61 bits between the smallest IRI and the largest string.
#[derive(Clone, Copy)]
struct Component {
    /// Per tag: its rank among the tags present.
    rank: [u8; 16],
    /// Per rank: the tag.
    tag: [u8; 16],
    /// Per tag: the smallest payload.
    min: [u64; 16],
    /// Bits of the payload offsets, and of the whole packed component.
    payload_bits: u32,
    width: u32,
}

/// Per component of a key array, how it packs; the bits all take.
struct Layout<const N: usize> {
    components: [Component; N],
    bits: u32,
}

/// Per tag, the smallest and largest payload seen (`u64::MAX, 0` for none).
type Spans = [(u64, u64); 16];

impl<const N: usize> Layout<N> {
    fn of(keys: &[[u64; N]]) -> Option<Self> {
        keys.first()?;
        let none: [Spans; N] = [[(u64::MAX, 0); 16]; N];
        let spans = keys
            .par_chunks(CHUNK)
            .map(|chunk| {
                let mut spans = none;
                for key in chunk {
                    for (span, &value) in spans.iter_mut().zip(key) {
                        let (lo, hi) = &mut span[(value >> PAYLOAD_BITS) as usize];
                        let payload = value & PAYLOAD;
                        *lo = (*lo).min(payload);
                        *hi = (*hi).max(payload);
                    }
                }
                spans
            })
            .reduce(
                || none,
                |mut a, b| {
                    for (a, b) in a.iter_mut().zip(&b) {
                        for (a, b) in a.iter_mut().zip(b) {
                            *a = (a.0.min(b.0), a.1.max(b.1));
                        }
                    }
                    a
                },
            );
        let components = spans.map(|span| {
            let mut component = Component {
                rank: [0; 16],
                tag: [0; 16],
                min: [0; 16],
                payload_bits: 0,
                width: 0,
            };
            let mut present = 0u32;
            for (tag, &(lo, hi)) in span.iter().enumerate() {
                if lo > hi {
                    continue;
                }
                component.rank[tag] = present as u8;
                component.tag[present as usize] = tag as u8;
                component.min[tag] = lo;
                component.payload_bits = component.payload_bits.max(64 - (hi - lo).leading_zeros());
                present += 1;
            }
            let rank_bits = 32 - (present - 1).leading_zeros();
            component.width = rank_bits + component.payload_bits;
            component
        });
        Some(Self {
            components,
            bits: components.iter().map(|c| c.width).sum(),
        })
    }

    /// Words a packed key takes.
    fn words(&self) -> usize {
        self.bits.div_ceil(64) as usize
    }

    /// `key` packed into the low [`Self::bits`] bits of `W` words (word 0 the most
    /// significant), component 0 highest.
    #[inline]
    fn pack<const W: usize>(&self, key: &[u64; N]) -> [u64; W] {
        let mut packed = [0u64; W];
        let mut above = 0u32; // bits of the components before this one
        for (&value, c) in key.iter().zip(&self.components) {
            if c.width > 0 {
                above += c.width;
                let tag = (value >> PAYLOAD_BITS) as usize;
                let offset = (value & PAYLOAD) - c.min[tag];
                let packed_value = (u64::from(c.rank[tag]) << c.payload_bits) | offset;
                put::<W>(&mut packed, self.bits - above, c.width, packed_value);
            }
        }
        packed
    }

    /// The key `packed` was made from.
    #[inline]
    fn unpack<const W: usize>(&self, packed: &[u64; W]) -> [u64; N] {
        let mut key = [0u64; N];
        let mut above = 0u32;
        for (value, c) in key.iter_mut().zip(&self.components) {
            let (rank, offset) = if c.width > 0 {
                above += c.width;
                let packed_value = get::<W>(packed, self.bits - above, c.width);
                (
                    (packed_value >> c.payload_bits) as usize,
                    packed_value & ((1u64 << c.payload_bits) - 1),
                )
            } else {
                (0, 0)
            };
            let tag = c.tag[rank];
            *value = (u64::from(tag) << PAYLOAD_BITS) | (c.min[tag as usize] + offset);
        }
        key
    }
}

/// Writes the `w`-bit `value` with its lowest bit at bit `low` of the `W` words, word 0
/// the most significant.
#[inline]
fn put<const W: usize>(words: &mut [u64; W], low: u32, w: u32, value: u64) {
    let word = W - 1 - (low / 64) as usize;
    let shift = low % 64;
    words[word] |= value << shift;
    if shift + w > 64 {
        words[word - 1] |= value >> (64 - shift);
    }
}

/// The `w`-bit value with its lowest bit at bit `low` of the `W` words.
#[inline]
fn get<const W: usize>(words: &[u64; W], low: u32, w: u32) -> u64 {
    let word = W - 1 - (low / 64) as usize;
    let shift = low % 64;
    let mut value = words[word] >> shift;
    if shift + w > 64 {
        value |= words[word - 1] << (64 - shift);
    }
    if w == 64 {
        value
    } else {
        value & ((1u64 << w) - 1)
    }
}

/// The radix digit of `packed` at bit `shift` (a multiple of [`DIGIT`]).
#[inline]
fn digit<const W: usize>(packed: &[u64; W], shift: u32) -> usize {
    let word = W - 1 - (shift / 64) as usize;
    ((packed[word] >> (shift % 64)) as usize) & (BUCKETS - 1)
}

/// The radix sort of `keys` over `layout`'s packing into `W` words, in the keys' buffer.
fn radix<const N: usize, const W: usize>(
    keys: &mut [[u64; N]],
    layout: &Layout<N>,
    dedup: bool,
) -> usize {
    let n = keys.len();
    // Keys from `top` on lie where packed words go; they are packed aside first.
    let top = (N - W) * n / N;
    let aside: Vec<[u64; W]> = keys[top..].par_iter().map(|k| layout.pack(k)).collect();
    let flat = keys.as_flattened_mut();
    let (front, back) = flat.split_at_mut((N - W) * n);
    let (packed_low, _) = back.as_chunks_mut::<W>();
    // Keys before `top` end before the packed region: packed in parallel.
    {
        let (source, _) = front.as_chunks::<N>();
        packed_low[..top]
            .par_iter_mut()
            .zip(source[..top].par_iter())
            .for_each(|(out, key)| *out = layout.pack(key));
    }
    packed_low[top..].copy_from_slice(&aside);
    drop(aside);
    // The scratch: the buffer's first `W n` words, free now.
    let (scratch, _) = front[..W * n].as_chunks_mut::<W>();
    radix_sort::<W>(packed_low, scratch, layout.bits);
    let packed = packed_low;
    let len = if dedup { dedup_sorted(packed) } else { n };
    // Unpack: keys from `top` on are written over packed words, so theirs are kept aside.
    let aside: Vec<[u64; W]> = packed[top.min(len)..len].to_vec();
    let (keys, _) = front.as_chunks_mut::<N>();
    let below = top.min(len);
    keys[..below]
        .par_iter_mut()
        .zip(packed[..below].par_iter())
        .for_each(|(key, p)| *key = layout.unpack(p));
    let keys = flat.as_chunks_mut::<N>().0;
    keys[below..len]
        .par_iter_mut()
        .zip(aside.par_iter())
        .for_each(|(key, p)| *key = layout.unpack(p));
    len
}

/// Removes adjacent duplicates from sorted `values`; returns the number kept.
fn dedup_sorted<const W: usize>(values: &mut [[u64; W]]) -> usize {
    let mut kept = 0;
    for i in 0..values.len() {
        if kept == 0 || values[i] != values[kept - 1] {
            values[kept] = values[i];
            kept += 1;
        }
    }
    kept
}

/// Values a bucket of the first pass aims at: one bucket's in-cache sort (256 KiB of
/// one-word values).
const BUCKET_TARGET: usize = 1 << 15;

/// The most buckets of the first pass.
const MSD_BITS: u32 = 12;

/// Buckets at most this long are sorted by comparison.
const SMALL: usize = 48;

/// A bucket in the scratch and where it goes in the values.
type Halves<'a, const W: usize> = (&'a mut [[u64; W]], &'a mut [[u64; W]]);

/// Sorts `values` by their low `bits` bits, with `scratch` (as long): a pass over the
/// most significant digit into `scratch`, then each of its buckets, small enough for the
/// cache, by a least-significant-digit radix sort over the rest, back into `values`.
/// The first pass is the only one over main memory; buckets are sorted in parallel.
fn radix_sort<const W: usize>(values: &mut [[u64; W]], scratch: &mut [[u64; W]], bits: u32) {
    let n = values.len();
    let msd = ((n / BUCKET_TARGET)
        .max(2)
        .next_power_of_two()
        .trailing_zeros())
    .clamp(1, MSD_BITS)
    .min(bits);
    let low = bits - msd;
    let buckets = 1usize << msd;
    // Fixed-size chunks (as many pieces as the thread count can use, at most 64): a
    // stable distribution gives the same result for any chunking.
    let chunk = CHUNK.max(n.div_ceil(64));
    let top = |value: &[u64; W]| get::<W>(value, low, msd) as usize;
    let counts: Vec<Vec<usize>> = values
        .par_chunks(chunk)
        .map(|part| {
            let mut count = vec![0usize; buckets];
            for value in part {
                count[top(value)] += 1;
            }
            count
        })
        .collect();
    // Each (bucket, chunk)'s destination in `scratch`: buckets in order, chunks in order
    // within one. Cut `scratch` into those pieces, then hand each chunk its own.
    let mut pieces: Vec<Vec<&mut [[u64; W]]>> = (0..counts.len())
        .map(|_| Vec::with_capacity(buckets))
        .collect();
    let mut sizes = vec![0usize; buckets];
    let mut rest: &mut [[u64; W]] = scratch;
    for (b, size) in sizes.iter_mut().enumerate() {
        for (c, count) in counts.iter().enumerate() {
            let (piece, tail) = std::mem::take(&mut rest).split_at_mut(count[b]);
            pieces[c].push(piece);
            rest = tail;
            *size += count[b];
        }
    }
    values
        .par_chunks(chunk)
        .zip(pieces.into_par_iter())
        .for_each(|(part, mut pieces)| {
            let mut at = vec![0usize; buckets];
            for value in part {
                let b = top(value);
                pieces[b][at[b]] = *value;
                at[b] += 1;
            }
        });
    // Each bucket from `scratch` back into `values`, sorted on the low bits.
    let mut pairs: Vec<Halves<'_, W>> = Vec::with_capacity(buckets);
    let (mut from, mut to): (&mut [[u64; W]], &mut [[u64; W]]) = (scratch, values);
    for &size in &sizes {
        let (a, a_rest) = std::mem::take(&mut from).split_at_mut(size);
        let (b, b_rest) = std::mem::take(&mut to).split_at_mut(size);
        if size > 0 {
            pairs.push((a, b));
        }
        from = a_rest;
        to = b_rest;
    }
    pairs
        .into_par_iter()
        .for_each(|(bucket, out)| bucket_sort(bucket, out, low));
}

/// Sorts `from` on its low `bits` bits into `to` (as long; `from` is scratch afterwards):
/// a least-significant-digit radix sort whose histograms come from one read, digits all
/// values share skipped.
fn bucket_sort<const W: usize>(from: &mut [[u64; W]], to: &mut [[u64; W]], bits: u32) {
    let n = from.len();
    if n <= SMALL || bits == 0 {
        to.copy_from_slice(from);
        if bits > 0 {
            to.sort_unstable();
        }
        return;
    }
    let passes = bits.div_ceil(DIGIT) as usize;
    let mut counts = vec![[0usize; BUCKETS]; passes];
    for value in from.iter() {
        for (pass, count) in counts.iter_mut().enumerate() {
            count[digit(value, pass as u32 * DIGIT)] += 1;
        }
    }
    let mut in_to = false;
    for (pass, count) in counts.iter().enumerate() {
        if count.contains(&n) {
            continue; // one digit for all: this pass moves nothing
        }
        let shift = pass as u32 * DIGIT;
        let mut offset = [0usize; BUCKETS];
        let mut sum = 0;
        for (o, &c) in offset.iter_mut().zip(count.iter()) {
            *o = sum;
            sum += c;
        }
        let (src, dst): (&[[u64; W]], &mut [[u64; W]]) = if in_to {
            (&*to, &mut *from)
        } else {
            (&*from, &mut *to)
        };
        for value in src {
            let d = digit(value, shift);
            dst[offset[d]] = *value;
            offset[d] += 1;
        }
        in_to = !in_to;
    }
    if !in_to {
        to.copy_from_slice(from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 11
        }
    }

    /// Keys with components of the given widths above a common base (tags included).
    fn keys<const N: usize>(rng: &mut Rng, n: usize, widths: [u32; N]) -> Vec<[u64; N]> {
        (0..n)
            .map(|_| {
                std::array::from_fn(|c| {
                    let w = widths[c];
                    let base = (1u64 << 60) | (c as u64 * 1000);
                    let r = rng.next() ^ (rng.next() << 53);
                    if w == 0 {
                        base
                    } else if w >= 64 {
                        r
                    } else {
                        base.wrapping_add(r & ((1 << w) - 1))
                    }
                })
            })
            .collect()
    }

    /// LUBM-like quads whose objects mix kinds: IRIs (tag 1) and strings (tag 3), each
    /// with payloads of their own range; ids of tags far apart.
    fn mixed(rng: &mut Rng, n: usize) -> Vec<[u64; 4]> {
        (0..n)
            .map(|_| {
                let iri = |rng: &mut Rng, w: u32| (1u64 << 60) | (rng.next() & ((1 << w) - 1));
                let object = match rng.next() % 3 {
                    0 => (3u64 << 60) | (5_000_000 + rng.next() % (1 << 24)),
                    _ => iri(rng, 25),
                };
                [iri(rng, 25), iri(rng, 6), object, 0]
            })
            .collect()
    }

    fn check<const N: usize>(keys: Vec<[u64; N]>) {
        let mut expected = keys.clone();
        expected.sort_unstable();
        let mut sorted = keys.clone();
        sort_keys(&mut sorted);
        assert_eq!(sorted, expected, "{:?}", method(&keys));
        expected.dedup();
        let mut deduped = keys;
        let kept = sort_dedup_keys(&mut deduped);
        deduped.truncate(kept);
        assert_eq!(deduped, expected);
    }

    #[test]
    fn radix_sorts_equal_comparison_sorts() {
        let mut rng = Rng(3);
        for n in [0, 1, 2, 100, RADIX_FROM - 1, RADIX_FROM, 70_001, 200_000] {
            // Quads: one word (LUBM), two words, too wide; few distinct keys.
            check(keys(&mut rng, n, [22, 5, 23, 0]));
            check(keys(&mut rng, n, [40, 10, 50, 3]));
            check(keys(&mut rng, n, [64, 64, 60, 0]));
            check(keys(&mut rng, n, [3, 1, 2, 0]));
            check(keys(&mut rng, n, [0, 0, 0, 0]));
            // Triples and pairs.
            check(keys(&mut rng, n, [25, 6, 25]));
            check(keys(&mut rng, n, [30, 30]));
            check(keys(&mut rng, n, [40, 40]));
            // Components that straddle a word, and the full width of one.
            check(keys(&mut rng, n, [63, 1, 64, 0]));
            // Objects of two kinds.
            check(mixed(&mut rng, n));
        }
    }

    #[test]
    fn the_method_follows_the_bits_the_keys_use() {
        let mut rng = Rng(5);
        let lubm = keys(&mut rng, 10_000, [22, 5, 23, 0]);
        assert_eq!(method(&lubm), Method::Radix { bits: 50 });
        let wide = keys(&mut rng, 10_000, [40, 10, 50, 3]);
        assert_eq!(method(&wide), Method::Comparison, "two words");
        let pairs = keys(&mut rng, 10_000, [30, 30]);
        assert_eq!(method(&pairs), Method::Radix { bits: 60 });
        // Two kinds of objects: a rank bit above the wider payload range (25 bits), not
        // the 61 bits from the smallest IRI to the largest string.
        let mixed = mixed(&mut rng, 10_000);
        assert_eq!(method(&mixed), Method::Radix { bits: 25 + 6 + 26 });
    }

    #[test]
    fn sorts_are_the_same_at_any_thread_count() {
        let mut rng = Rng(9);
        let input = keys(&mut rng, 300_000, [22, 5, 23, 0]);
        let sorted: Vec<Vec<[u64; 4]>> = [1, 3, 8]
            .into_iter()
            .map(|threads| {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                let mut keys = input.clone();
                let kept = pool.install(|| sort_dedup_keys(&mut keys));
                keys.truncate(kept);
                keys
            })
            .collect();
        assert!(sorted.windows(2).all(|w| w[0] == w[1]));
    }

    /// The kernel against the comparison sort on a million LUBM-like quads:
    /// `cargo test --release -p nrese-exec --lib sort_bench -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn sort_bench() {
        let mut rng = Rng(11);
        for (name, widths) in [
            ("lubm", [24, 6, 25, 0]),
            ("mixed", [0; 4]),
            ("wide", [40, 14, 62, 0]),
        ] {
            let input = match name {
                "mixed" => mixed(&mut rng, 10_000_000),
                _ => keys(&mut rng, 10_000_000, widths),
            };
            for _ in 0..3 {
                let mut a = input.clone();
                let start = std::time::Instant::now();
                a.par_sort_unstable();
                let comparison = start.elapsed();
                let mut b = input.clone();
                let start = std::time::Instant::now();
                sort_keys(&mut b);
                let radix = start.elapsed();
                assert_eq!(a, b);
                eprintln!(
                    "{name}: comparison {comparison:?}, radix {radix:?} ({:?})",
                    method(&input)
                );
            }
        }
    }
}
