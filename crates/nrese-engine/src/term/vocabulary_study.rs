//! What a compressed vocabulary would take, measured on a checkpoint's dictionary: for the
//! decision on the store-size gap to QLever (design §3 item 4,
//! docs/design/research-designs.md).
//!
//! ```text
//! NRESE_STUDY_CHECKPOINT=/path/checkpoint-….nck \
//!   cargo test --release -p nrese-engine --lib vocabulary_study -- --ignored --nocapture
//! ```
//!
//! It compares the keys as stored with FSST (one symbol table for all keys, and one each
//! for IRIs and for the other keys), the 8-byte end offsets with block offsets (one 8-byte
//! start per 64 entries and a 2-byte offset each, where the block's text fits), and the
//! time to decode a key, in id order and at random, against copying it.

use std::time::Instant;

use fsst::Compressor;

/// Keys per block of compact offsets.
const BLOCK: usize = 64;

fn mib(bytes: usize) -> f64 {
    bytes as f64 / 1_048_576.0
}

/// Bytes of block offsets for keys of `lengths`: an 8-byte start per block, then 2 bytes a
/// key where the block's text is below 64 KiB, else 4.
fn block_offsets(lengths: &[usize]) -> usize {
    lengths
        .chunks(BLOCK)
        .map(|block| {
            let text: usize = block.iter().sum();
            8 + block.len() * if text < 65_536 { 2 } else { 4 }
        })
        .sum()
}

struct Encoded {
    bytes: usize,
    /// Compressed keys, kept to time decoding.
    keys: Vec<Vec<u8>>,
}

fn encode(compressor: &Compressor, keys: &[&[u8]]) -> Encoded {
    let keys: Vec<Vec<u8>> = keys.iter().map(|key| compressor.compress(key)).collect();
    Encoded {
        bytes: keys.iter().map(Vec::len).sum(),
        keys,
    }
}

#[test]
#[ignore = "reads a checkpoint named by NRESE_STUDY_CHECKPOINT"]
fn vocabulary_study() {
    let Some(path) = std::env::var_os("NRESE_STUDY_CHECKPOINT") else {
        eprintln!("NRESE_STUDY_CHECKPOINT not set");
        return;
    };
    let (base, _) = crate::durability::checkpoint::map_written(std::path::Path::new(&path))
        .expect("checkpoint");
    let keys: Vec<&[u8]> = (0..base.len).map(|i| base.key(i)).collect();
    let text: usize = keys.iter().map(|k| k.len()).sum();
    let lengths: Vec<usize> = keys.iter().map(|k| k.len()).collect();
    let iri = |key: &&[u8]| key.first() == Some(&b'I');
    let (iris, others): (Vec<&[u8]>, Vec<&[u8]>) = keys.iter().copied().partition(iri);
    println!(
        "{} keys ({} IRIs), text {:.1} MiB ({:.1} B a key; IRIs {:.1} MiB), ends {:.1} MiB, \
         block offsets {:.1} MiB",
        keys.len(),
        iris.len(),
        mib(text),
        text as f64 / keys.len() as f64,
        mib(iris.iter().map(|k| k.len()).sum()),
        mib(keys.len() * 8),
        mib(block_offsets(&lengths)),
    );

    let started = Instant::now();
    let one = Compressor::train(&keys);
    let trained = started.elapsed();
    let started = Instant::now();
    let all = encode(&one, &keys);
    let compressed = started.elapsed();
    println!(
        "FSST, one table: {:.1} MiB ({:.0} %), trained in {:.2} s, compressed in {:.2} s \
         ({:.0} MB/s)",
        mib(all.bytes),
        100.0 * all.bytes as f64 / text as f64,
        trained.as_secs_f64(),
        compressed.as_secs_f64(),
        text as f64 / 1e6 / compressed.as_secs_f64(),
    );
    let by_iri = Compressor::train(&iris);
    let by_other = Compressor::train(&others);
    let split = encode(&by_iri, &iris).bytes + encode(&by_other, &others).bytes;
    println!(
        "FSST, a table for IRIs and one for the rest: {:.1} MiB ({:.0} %)",
        mib(split),
        100.0 * split as f64 / text as f64,
    );

    // Decoding as a store would: the compressed keys one after another in one arena, each
    // decoded into one reused buffer; every key in id order, then a million at random.
    let mut arena: Vec<u8> = Vec::with_capacity(all.bytes);
    let mut ends: Vec<usize> = Vec::with_capacity(all.keys.len());
    for key in &all.keys {
        arena.extend_from_slice(key);
        ends.push(arena.len());
    }
    let compressed_key = |i: usize| &arena[if i == 0 { 0 } else { ends[i - 1] }..ends[i]];
    let decompressor = one.decompressor();
    let mut buffer: Vec<std::mem::MaybeUninit<u8>> = vec![std::mem::MaybeUninit::uninit(); 1 << 20];
    let mut sink = 0usize;
    let started = Instant::now();
    for i in 0..all.keys.len() {
        sink += std::hint::black_box(decompressor.decompress_into(compressed_key(i), &mut buffer));
    }
    let sequential = started.elapsed().as_nanos() as f64 / all.keys.len() as f64;
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let picks: Vec<usize> = (0..1_000_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % all.keys.len() as u64) as usize
        })
        .collect();
    let started = Instant::now();
    for &i in &picks {
        sink += std::hint::black_box(decompressor.decompress_into(compressed_key(i), &mut buffer));
    }
    let random = started.elapsed().as_nanos() as f64 / picks.len() as f64;
    // The plain keys as the dictionary reads them: through the end offsets into the arena.
    let mut plain: Vec<u8> = Vec::with_capacity(1 << 20);
    let started = Instant::now();
    for i in 0..base.len {
        plain.clear();
        plain.extend_from_slice(base.key(i));
        sink += std::hint::black_box(&plain).len();
    }
    let copy_sequential = started.elapsed().as_nanos() as f64 / base.len as f64;
    let started = Instant::now();
    for &i in &picks {
        plain.clear();
        plain.extend_from_slice(base.key(i as u64));
        sink += std::hint::black_box(&plain).len();
    }
    let copy = started.elapsed().as_nanos() as f64 / picks.len() as f64;
    println!(
        "decode a key: {sequential:.0} ns in id order, {random:.0} ns at random; a plain key: \
         {copy_sequential:.0} ns in id order, {copy:.0} ns at random ({sink} bytes)"
    );
    let dictionary_now = text + keys.len() * 8;
    let dictionary_then = all.bytes + block_offsets(&lengths) + 255 * 9;
    println!(
        "keys and offsets: {:.1} MiB now, {:.1} MiB compressed ({:.0} %)",
        mib(dictionary_now),
        mib(dictionary_then),
        100.0 * dictionary_then as f64 / dictionary_now as f64,
    );
}
