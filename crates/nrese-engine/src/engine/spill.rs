//! External sorting for bulk loads with bounded memory, as QLever's index builder does it.
//!
//! A bulk load whose quads outgrow its budget ([`DurabilityConfig::bulk_load_memory`]
//! (crate::DurabilityConfig)) hands them over in chunks to a spilling thread, which writes
//! each as it is: its new terms have provisional ids until the load finishes
//! ([`crate::term::pending`]), and sorted by those a chunk would be out of order once
//! renumbered. At the end each chunk is read back, renumbered and sorted in every
//! permutation of the smallest layout that holds it, each permutation written packed
//! ([`PackedKeys`], 6 to 12 bytes per key) to its own file, one chunk at a time
//! ([`Spill::sort_raw`]). Then each permutation is the merge of its chunk files, read
//! through memory maps, with duplicates dropped. A chunk without named graphs gives the graph-first permutations of
//! the quad layout (if another chunk needs that layout) from its graph-last ones, the
//! graph moved to the front of each key: they sort alike when every graph is the default.
//! The merged permutation is packed on the fly and goes into the checkpoint before the
//! next is merged. So the load's quads take the budget plus one packed permutation,
//! whatever the data's size, at the cost of writing and reading each key twice more.
//!
//! The spilling thread sorts on a thread pool of its own. The loader's threads, which add
//! the quads from the global pool, wait for it when the next chunk is full: a sort on
//! the global pool could wait for those very threads.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::JoinHandle;

use crate::index::Layout;
use crate::index::keys::PackedKeys;
use crate::index::run::PermutationBuilder;
use crate::mapped;
use crate::quad::{EncodedQuad, Key, Permutation};

/// The directory spilled chunks go to, in the store's directory. A load that ended without
/// removing it (a crash) leaves it behind; opening the store removes it.
pub(crate) const SPILL_DIR: &str = "bulk-spill";

/// Quads concatenated into one array, each batch freed once copied.
fn concat(batches: Vec<Vec<EncodedQuad>>) -> Vec<EncodedQuad> {
    let mut quads: Vec<EncodedQuad> = Vec::with_capacity(batches.iter().map(Vec::len).sum());
    for batch in batches {
        quads.extend_from_slice(&batch);
    }
    quads
}

/// Chunks spilled to disk: as written (raw, until [`Spill::sort_raw`]), then per chunk one
/// packed file per permutation of its layout.
pub(crate) struct Spill {
    dir: PathBuf,
    /// The layout of each sorted chunk.
    chunks: Vec<Layout>,
    /// The raw chunks' files, in the order they came.
    raw: Vec<PathBuf>,
}

impl Spill {
    /// A new, empty spill directory under `root` (an old one is removed first).
    fn create(root: &Path) -> std::io::Result<Self> {
        let dir = root.join(SPILL_DIR);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            chunks: Vec::new(),
            raw: Vec::new(),
        })
    }

    /// Writes `quads` as they are (32 bytes each, little-endian components).
    fn write_raw(&mut self, quads: &[EncodedQuad]) -> std::io::Result<()> {
        let path = self.dir.join(format!("raw-{:05}.quads", self.raw.len()));
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&path)?);
        for quad in quads {
            for component in quad.components() {
                out.write_all(&component.to_le_bytes())?;
            }
        }
        out.into_inner()
            .map_err(|error| error.into_error())?
            .sync_all()?;
        self.raw.push(path);
        Ok(())
    }

    /// Reads each raw chunk back, renumbers its quads with `renumber` and writes it sorted
    /// ([`Self::write_chunk`]), one chunk at a time; the raw files go.
    pub(crate) fn sort_raw(
        &mut self,
        renumber: &dyn Fn(&mut [EncodedQuad]),
    ) -> std::io::Result<()> {
        for path in std::mem::take(&mut self.raw) {
            let bytes = fs::read(&path)?;
            let mut quads: Vec<EncodedQuad> = bytes
                .as_chunks::<32>()
                .0
                .iter()
                .map(|raw| {
                    let word = |i: usize| {
                        u64::from_le_bytes(raw[8 * i..8 * i + 8].try_into().expect("8 bytes"))
                    };
                    EncodedQuad::from_components([word(0), word(1), word(2), word(3)])
                })
                .collect();
            drop(bytes);
            renumber(&mut quads);
            self.write_chunk(quads)?;
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// The layout of the merged quads: the quad layout if a chunk has a named graph.
    pub(crate) fn layout(&self) -> Layout {
        match self.chunks.contains(&Layout::Quads) {
            true => Layout::Quads,
            false => Layout::DefaultGraph,
        }
    }

    fn path(&self, chunk: usize, permutation: Permutation) -> PathBuf {
        self.dir
            .join(format!("chunk-{chunk:05}-{}.keys", permutation as usize))
    }

    /// Sorts `quads` in every permutation of the smallest layout that holds them and writes
    /// each, packed.
    fn write_chunk(&mut self, quads: Vec<EncodedQuad>) -> std::io::Result<()> {
        let layout = Layout::holding(&quads);
        let chunk = self.chunks.len();
        let mut builder = PermutationBuilder::planned(quads, layout.permutations());
        for &permutation in layout.permutations() {
            let packed = builder.packed(permutation);
            let file = File::create(self.path(chunk, permutation))?;
            let mut out = BufWriter::with_capacity(1 << 20, file);
            packed.write(Some(0), &mut |bytes| out.write_all(bytes))?;
            out.into_inner()
                .map_err(|error| error.into_error())?
                .sync_all()?;
        }
        self.chunks.push(layout);
        Ok(())
    }

    /// The keys of `permutation` (of [`layout`](Self::layout)) over all chunks, in order,
    /// duplicates dropped, packed. The files stay until the spill is dropped: a graph-last
    /// permutation's also answers its graph-first one.
    pub(crate) fn merged(&self, permutation: Permutation) -> std::io::Result<PackedKeys> {
        let mut cursors = Vec::with_capacity(self.chunks.len());
        for (chunk, layout) in self.chunks.iter().enumerate() {
            let (file, rotate) = match layout.permutations().contains(&permutation) {
                true => (permutation, false),
                false => match permutation.graph_last() {
                    Some(last) => (last, true),
                    None => {
                        return Err(std::io::Error::other(format!(
                            "chunk {chunk} has no permutation {permutation:?}"
                        )));
                    }
                },
            };
            let path = self.path(chunk, file);
            let map = mapped::map(&path)?;
            let (keys, _) = PackedKeys::read(&map, Some(0), Some(&map), false)
                .map_err(|error| std::io::Error::other(format!("{}: {error}", path.display())))?;
            cursors.push(Cursor::new(keys, rotate));
        }
        Ok(PackedKeys::from_sorted_iter(Merge::new(cursors)))
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Keys of one chunk file in order, a block decoded at a time; with `rotate`, each key's
/// last component (the graph) moved to the front.
struct Cursor {
    keys: PackedKeys,
    rotate: bool,
    next: usize,
    block: Vec<Key>,
    at: usize,
}

impl Cursor {
    fn new(keys: PackedKeys, rotate: bool) -> Self {
        Self {
            keys,
            rotate,
            next: 0,
            block: Vec::with_capacity(crate::index::keys::BLOCK),
            at: 0,
        }
    }

    fn pop(&mut self) -> Option<Key> {
        if self.at == self.block.len() {
            if self.next == self.keys.len() {
                return None;
            }
            let end = (self.next / crate::index::keys::BLOCK + 1) * crate::index::keys::BLOCK;
            let end = end.min(self.keys.len());
            self.block.clear();
            self.keys.decode_range(self.next, end, &mut self.block);
            if self.rotate {
                for key in &mut self.block {
                    key.rotate_right(1);
                }
            }
            self.next = end;
            self.at = 0;
        }
        self.at += 1;
        Some(self.block[self.at - 1])
    }
}

/// The k-way merge of sorted cursors, without duplicates.
struct Merge {
    cursors: Vec<Cursor>,
    heap: BinaryHeap<Reverse<(Key, usize)>>,
    last: Option<Key>,
}

impl Merge {
    fn new(mut cursors: Vec<Cursor>) -> Self {
        let heap = cursors
            .iter_mut()
            .enumerate()
            .filter_map(|(i, cursor)| Some(Reverse((cursor.pop()?, i))))
            .collect();
        Self {
            cursors,
            heap,
            last: None,
        }
    }
}

impl Iterator for Merge {
    type Item = Key;

    fn next(&mut self) -> Option<Key> {
        loop {
            let Reverse((key, i)) = self.heap.pop()?;
            if let Some(next) = self.cursors[i].pop() {
                self.heap.push(Reverse((next, i)));
            }
            if self.last != Some(key) {
                self.last = Some(key);
                return Some(key);
            }
        }
    }
}

/// The spilling thread of a bulk load, started with its first chunk.
pub(super) struct Spiller {
    chunks: SyncSender<Vec<Vec<EncodedQuad>>>,
    thread: JoinHandle<std::io::Result<Spill>>,
}

impl Spiller {
    pub(super) fn start(root: &Path) -> std::io::Result<Self> {
        let mut spill = Spill::create(root)?;
        let threads = std::thread::available_parallelism().map_or(4, usize::from);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("nrese-spill-{i}"))
            .build()
            .map_err(std::io::Error::other)?;
        // One chunk waits while the last is written: a full chunk in the sender's hands
        // blocks it.
        let (chunks, received) = sync_channel::<Vec<Vec<EncodedQuad>>>(0);
        let thread = std::thread::Builder::new()
            .name("nrese-spill".into())
            .spawn(move || {
                for batches in received {
                    pool.install(|| spill.write_raw(&concat(batches)))?;
                    pool.broadcast(|_| crate::memory::release_thread());
                    crate::memory::release_thread();
                }
                Ok(spill)
            })?;
        Ok(Self { chunks, thread })
    }

    /// Hands a full chunk over, waiting while the last one is written. `false` if the
    /// thread stopped on an error ([`finish`](Self::finish) returns it).
    pub(super) fn send(&self, batches: Vec<Vec<EncodedQuad>>) -> bool {
        self.chunks.send(batches).is_ok()
    }

    /// Waits for the last chunk to be written; the chunks, or the first error.
    pub(super) fn finish(self) -> std::io::Result<Spill> {
        drop(self.chunks);
        self.thread
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("the spilling thread panicked")))
    }
}

/// Removes what a load that didn't finish left in `root`.
pub(crate) fn remove_leftovers(root: &Path) -> std::io::Result<()> {
    let dir = root.join(SPILL_DIR);
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TermId;

    fn quad(g: u64, s: u64, p: u64, o: u64) -> EncodedQuad {
        EncodedQuad {
            graph: TermId::from_raw(g),
            subject: TermId::from_raw(s),
            predicate: TermId::from_raw(p),
            object: TermId::from_raw(o),
        }
    }

    /// Overlapping chunks merge into each permutation sorted and without duplicates, the
    /// same as sorting everything at once: in the default-graph layout while every chunk
    /// is in the default graph, in the quad layout once one has a named graph (the others'
    /// graph-first permutations then come from their graph-last ones).
    #[test]
    fn merged_chunks_equal_one_sort() {
        for named in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut spill = Spill::create(dir.path()).unwrap();
            let mut all: Vec<EncodedQuad> = (0..5000u64)
                .map(|i| quad(0, 1 + i % 97, 1 + i % 7, 1 + (i * 31) % 1009))
                .collect();
            for chunk in all.chunks(1700) {
                let mut chunk = chunk.to_vec();
                // Duplicates across chunks.
                chunk.extend_from_slice(&all[..50]);
                spill.write_chunk(chunk).unwrap();
            }
            if named {
                let graphs: Vec<EncodedQuad> = (0..300u64)
                    .map(|i| quad(2000 + i % 3, 1 + i % 97, 1 + i % 7, 5 + i))
                    .chain(all[..20].iter().copied())
                    .collect();
                spill.write_chunk(graphs.clone()).unwrap();
                all.extend(graphs);
            }
            let layout = spill.layout();
            assert_eq!(layout == Layout::Quads, named);
            let mut builder = PermutationBuilder::new(all);
            for &permutation in layout.permutations() {
                let merged = spill.merged(permutation).unwrap();
                let expected = builder.packed(permutation);
                assert_eq!(merged.len(), expected.len(), "{permutation:?}");
                assert!(
                    (0..merged.len()).all(|i| merged.get(i) == expected.get(i)),
                    "{permutation:?}"
                );
            }
            drop(spill);
            assert!(!dir.path().join(SPILL_DIR).exists());
        }
    }
}
