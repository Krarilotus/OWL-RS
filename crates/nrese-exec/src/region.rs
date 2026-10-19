//! Growable arrays in memory they map themselves, charged to a budget before they grow
//! (`Harness/notes/2026-10-19-pm-load-arena-design.md`): what a bulk load interns its
//! terms into, and what the dictionary keeps of them afterwards.
//!
//! A [`Region`] owns its pages' policy and their return to the system, which the global
//! allocator can't give an array: it maps a virtual range once (Linux and other Unix
//! `mmap`, Windows `VirtualAlloc` reserve), its pages are made as it grows and never move,
//! and dropping it unmaps the range in one call. Growing on many threads at once through
//! the allocator, arrays that double leave their old buffers with whichever thread held
//! them, in pages that stay resident: a load of DBpedia core held 408 MiB more than main
//! with 432 MiB less live (18 Oct 2026).
//!
//! - **Huge pages.** On Linux a region is advised `MADV_NOHUGEPAGE` while it grows: a
//!   transparent huge page makes 2 MiB resident for the first byte touched, and 256 shards
//!   growing at once each touch new ones. Once its holder says it is final
//!   ([`Region::settle`]) it is advised `MADV_HUGEPAGE`: the dictionary reads it for good,
//!   and queries over memory a load left on small pages ran 2 to 5 % slower (fleet
//!   pm-thp-attrib-1, 19 Oct 2026). This is the region's own range: no process setting
//!   changes. Windows has no transparent huge pages.
//! - **Charging.** Bytes are charged to the region's [`Reservation`] before pages are
//!   made for them, in steps of an eighth of what it holds (a page at least, 1 MiB at
//!   most), and released after they are unmapped. [`Region::shrink_to_fit`] gives back the
//!   step's unused pages.
//! - **Without overcommit** (Linux `vm.overcommit_memory = 2` refuses an unbacked range):
//!   a region maps what it holds, in steps of 64 MiB, and growth past a step copies it into
//!   a mapping of the next size. [`Region::moves`] says which way a region grows.
//! - **Measured tests** (`crate::measure`): a region reports its pages as blocks of the
//!   generation it was made in, so a measurement counts its memory and its reservation's
//!   charges, or neither.

use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::{Budget, BudgetExceeded, Reservation};

/// The pages a region maps and charges in (the system's smallest on every target here).
const PAGE: usize = 4096;

/// The most a region charges and maps in one step.
const STEP: usize = 1 << 20;

/// What a region that can't reserve its bound maps at a time.
const CHUNK: usize = if cfg!(miri) { 4 * PAGE } else { 64 << 20 };

/// A growable array of `T` in pages of its own, charged to `B` before it grows (module
/// docs).
pub struct Region<T: Copy, B: Deref<Target = Budget> = Arc<Budget>> {
    /// Before `held`: the pages are unmapped before their charge is released.
    pages: Pages<T>,
    held: Reservation<B>,
}

/// A region's pages and its items: the mapping, unmapped when this drops.
pub struct Pages<T: Copy> {
    map: Mapping,
    len: usize,
    /// The most items it may hold.
    bound: usize,
    _items: PhantomData<T>,
}

impl<T: Copy, B: Deref<Target = Budget>> Region<T, B> {
    /// An empty region for at most `bound` items, its pages charged to `budget` as it
    /// grows. Nothing is charged yet: a virtual range costs no memory until its pages are
    /// made.
    pub fn new(budget: B, bound: usize) -> Self {
        assert!(
            align_of::<T>() <= PAGE,
            "a region's items are page-aligned at most"
        );
        let bytes = bound
            .checked_mul(size_of::<T>())
            .and_then(|bytes| bytes.checked_next_multiple_of(PAGE))
            .unwrap_or(usize::MAX);
        Self {
            pages: Pages {
                map: Mapping::reserve(bytes),
                len: 0,
                bound,
                _items: PhantomData,
            },
            held: Reservation::new(budget),
        }
    }

    pub fn len(&self) -> usize {
        self.pages.len
    }

    pub fn is_empty(&self) -> bool {
        self.pages.len == 0
    }

    /// The bytes held: what its pages were charged.
    pub fn charged(&self) -> usize {
        self.held.bytes()
    }

    /// Whether growing copies it (a host that refuses unbacked ranges): see module docs.
    pub fn moves(&self) -> bool {
        self.pages.map.moves()
    }

    /// Room for `additional` more items: their pages charged, then made. Refused, nothing
    /// changes.
    pub fn reserve(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        let len = self.pages.len.checked_add(additional);
        let len = len.filter(|&len| len <= self.pages.bound);
        let len = len.expect("a region holds at most its bound");
        let need = len * size_of::<T>();
        let held = self.pages.map.held;
        if need <= held {
            return Ok(());
        }
        let step = (held / 8).clamp(PAGE, STEP);
        let target = need.max(held + step).next_multiple_of(PAGE);
        self.pages.map.grow(target, &mut self.held)
    }

    pub fn push(&mut self, item: T) -> Result<(), BudgetExceeded> {
        self.reserve(1)?;
        let len = self.pages.len;
        // SAFETY: `reserve` made room for `len + 1` items in the mapping, aligned for `T`.
        unsafe { self.pages.items().add(len).write(item) };
        self.pages.len = len + 1;
        Ok(())
    }

    pub fn extend_from_slice(&mut self, items: &[T]) -> Result<(), BudgetExceeded> {
        self.reserve(items.len())?;
        let len = self.pages.len;
        // SAFETY: `reserve` made room for `items` past `len`; the mapping is no slice of
        // `items`, which the caller borrows.
        unsafe {
            std::ptr::copy_nonoverlapping(items.as_ptr(), self.pages.items().add(len), items.len())
        };
        self.pages.len = len + items.len();
        Ok(())
    }

    pub fn as_slice(&self) -> &[T] {
        &self.pages
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: the first `len` items were written; `&mut self` lends them alone.
        unsafe { std::slice::from_raw_parts_mut(self.pages.items(), self.pages.len) }
    }

    /// Gives back the pages past its items, which a step made ahead.
    pub fn shrink_to_fit(&mut self) {
        let keep = (self.pages.len * size_of::<T>()).next_multiple_of(PAGE);
        self.pages.map.shrink(keep, &mut self.held);
    }

    /// Says its items are final: huge pages may back it from now on (module docs).
    pub fn settle(&mut self) {
        self.pages.map.advise_huge(true);
    }

    /// Its pages, and the reservation of their bytes, for a holder that keeps the charge
    /// until it drops the pages.
    pub fn into_parts(self) -> (Pages<T>, Reservation<B>) {
        (self.pages, self.held)
    }
}

impl<B: Deref<Target = Budget>> Region<u64, B> {
    /// Its items as atomics, for threads that write them in parallel.
    pub fn as_atomic(&mut self) -> &[AtomicU64] {
        // SAFETY: the mapping is page-aligned, so aligned for `AtomicU64`, which has the
        // size of `u64`; the first `len` items were written, and `&mut self` keeps any
        // other view of them away while these are lent.
        unsafe { std::slice::from_raw_parts(self.pages.items().cast(), self.pages.len) }
    }
}

impl<T: Copy> Pages<T> {
    fn items(&self) -> *mut T {
        self.map.base.as_ptr().cast()
    }
}

impl<T: Copy> Deref for Pages<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: the first `len` items were written; they live as long as the mapping.
        unsafe { std::slice::from_raw_parts(self.items(), self.len) }
    }
}

// SAFETY: a region owns its mapping as a `Vec` owns its buffer: it is as `Send` and
// `Sync` as its items.
unsafe impl<T: Copy + Send> Send for Pages<T> {}
// SAFETY: as for `Send`; `&Pages` lends only shared slices.
unsafe impl<T: Copy + Sync> Sync for Pages<T> {}

/// A mapped range: `reserved` bytes of address space from `base`, the first `held` of them
/// made (and charged).
struct Mapping {
    base: NonNull<u8>,
    reserved: usize,
    held: usize,
    /// It couldn't reserve its bound: it maps what it holds and moves to grow.
    chunked: bool,
    #[cfg(any(test, feature = "measure"))]
    tag: u64,
}

impl Mapping {
    /// The address space for `bytes`, without memory yet; where the host refuses it, an
    /// empty mapping that maps as it grows.
    fn reserve(bytes: usize) -> Self {
        let bytes = if bytes == usize::MAX {
            bytes
        } else {
            bytes.max(PAGE)
        };
        let (base, reserved, chunked) = match sys::reserve(bytes) {
            Some(base) => (base, bytes, false),
            None => (NonNull::dangling(), 0, true),
        };
        let map = Self {
            base,
            reserved,
            held: 0,
            chunked,
            #[cfg(any(test, feature = "measure"))]
            tag: crate::measure::tag(),
        };
        if !chunked {
            sys::advise_huge(map.base, map.reserved, false);
        }
        map
    }

    fn moves(&self) -> bool {
        self.chunked
    }

    /// `target` bytes made and charged: the charge first.
    fn grow(
        &mut self,
        target: usize,
        held: &mut Reservation<impl Deref<Target = Budget>>,
    ) -> Result<(), BudgetExceeded> {
        if !self.chunked {
            held.charge(target - self.held)?;
            sys::commit(self.base, self.held, target);
            self.mapped(target - self.held);
            self.held = target;
            return Ok(());
        }
        // A mapping of the next chunked size: charged beside the old one, the items
        // copied, the old one unmapped and its charge released.
        let size = target.max(self.held * 2).next_multiple_of(CHUNK);
        held.charge(size)?;
        let base = sys::map(size);
        sys::advise_huge(base, size, false);
        self.mapped(size);
        if self.reserved > 0 {
            // SAFETY: both are mappings of at least `held` bytes, made by `sys`, apart.
            unsafe { std::ptr::copy_nonoverlapping(self.base.as_ptr(), base.as_ptr(), self.held) };
            self.unmapped(self.reserved);
            sys::unmap(self.base, self.reserved);
            held.release(self.reserved);
        }
        (self.base, self.reserved, self.held) = (base, size, size);
        Ok(())
    }

    /// The pages past `keep` bytes given back, and their charge.
    fn shrink(&mut self, keep: usize, held: &mut Reservation<impl Deref<Target = Budget>>) {
        if self.chunked || keep >= self.held {
            return;
        }
        self.unmapped(self.held - keep);
        sys::decommit(self.base, keep, self.held);
        held.release(self.held - keep);
        self.held = keep;
    }

    fn advise_huge(&self, on: bool) {
        if self.reserved > 0 {
            sys::advise_huge(self.base, self.reserved, on);
        }
    }

    /// Pages made outside the global allocator, for measurements (where `sys` maps
    /// through the allocator, the allocator counts them itself).
    fn mapped(&self, _bytes: usize) {
        #[cfg(any(test, feature = "measure"))]
        if !sys::THROUGH_ALLOCATOR {
            crate::measure::mapped(_bytes, self.tag);
        }
    }

    fn unmapped(&self, _bytes: usize) {
        #[cfg(any(test, feature = "measure"))]
        if !sys::THROUGH_ALLOCATOR {
            crate::measure::unmapped(_bytes, self.tag);
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if self.reserved > 0 {
            let made = if self.chunked {
                self.reserved
            } else {
                self.held
            };
            self.unmapped(made);
            sys::unmap(self.base, self.reserved);
        }
    }
}

/// The system calls, the only unsafe code of regions besides the views of their items.
/// Each returns or panics: a failure to map memory that was reserved and charged is the
/// system's, as an allocation failure is.
mod sys {
    use std::ptr::NonNull;

    /// Whether pages come from the global allocator (Miri, and targets that are neither
    /// Unix nor Windows).
    #[cfg(any(test, feature = "measure"))]
    pub(super) const THROUGH_ALLOCATOR: bool = cfg!(any(miri, not(any(unix, windows))));

    #[cfg(all(unix, not(miri)))]
    pub(super) fn reserve(bytes: usize) -> Option<NonNull<u8>> {
        if bytes == usize::MAX {
            return None;
        }
        // SAFETY: a new anonymous private mapping at an address of the system's choosing;
        // no existing memory is affected. Unbacked (`MAP_NORESERVE`): pages are made as
        // they are touched; a host that refuses it returns `MAP_FAILED`.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes.max(super::PAGE),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        (base != libc::MAP_FAILED).then(|| NonNull::new(base.cast()).expect("mmap's address"))
    }

    /// Pages are made as they are touched.
    #[cfg(all(unix, not(miri)))]
    pub(super) fn commit(_base: NonNull<u8>, _from: usize, _to: usize) {}

    #[cfg(all(unix, not(miri)))]
    pub(super) fn decommit(base: NonNull<u8>, from: usize, to: usize) {
        // SAFETY: `from..to` lies in the mapping at `base`, page-aligned, and holds no item
        // (the caller keeps `from` bytes); the pages read as zero if touched again.
        let done = unsafe {
            libc::madvise(
                base.as_ptr().add(from).cast(),
                to - from,
                libc::MADV_DONTNEED,
            )
        };
        assert_eq!(done, 0, "madvise(MADV_DONTNEED) of a region's own pages");
    }

    #[cfg(all(unix, not(miri)))]
    pub(super) fn map(bytes: usize) -> NonNull<u8> {
        // SAFETY: a new anonymous private mapping, as in `reserve`, backed.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(base != libc::MAP_FAILED, "mmap of {bytes} charged bytes");
        NonNull::new(base.cast()).expect("mmap's address")
    }

    #[cfg(all(unix, not(miri)))]
    pub(super) fn unmap(base: NonNull<u8>, bytes: usize) {
        // SAFETY: `base` is a mapping of `bytes` (at least a page) made here, and nothing
        // refers to it any more: its region is gone.
        let done = unsafe { libc::munmap(base.as_ptr().cast(), bytes.max(super::PAGE)) };
        assert_eq!(done, 0, "munmap of a region's mapping");
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    pub(super) fn advise_huge(base: NonNull<u8>, bytes: usize, on: bool) {
        let advice = if on {
            libc::MADV_HUGEPAGE
        } else {
            libc::MADV_NOHUGEPAGE
        };
        // SAFETY: advice on the mapping at `base` alone; it changes how its pages are
        // backed, never their contents. A kernel without transparent huge pages refuses
        // it (`EINVAL`), which changes nothing.
        let _ = unsafe { libc::madvise(base.as_ptr().cast(), bytes.max(super::PAGE), advice) };
    }

    #[cfg(all(unix, not(target_os = "linux"), not(miri)))]
    pub(super) fn advise_huge(_base: NonNull<u8>, _bytes: usize, _on: bool) {}

    #[cfg(all(windows, not(miri)))]
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
        VirtualFree,
    };

    #[cfg(all(windows, not(miri)))]
    pub(super) fn reserve(bytes: usize) -> Option<NonNull<u8>> {
        if bytes == usize::MAX {
            return None;
        }
        // SAFETY: reserves address space at an address of the system's choosing; no
        // existing memory is affected, nothing is committed.
        let base = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                bytes.max(super::PAGE),
                MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        NonNull::new(base.cast())
    }

    #[cfg(all(windows, not(miri)))]
    pub(super) fn commit(base: NonNull<u8>, from: usize, to: usize) {
        // SAFETY: `from..to` lies in the range reserved at `base`; committing pages makes
        // them readable and zero, and affects no other memory.
        let made = unsafe {
            VirtualAlloc(
                base.as_ptr().add(from).cast(),
                to - from,
                MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        assert!(
            !made.is_null(),
            "VirtualAlloc(MEM_COMMIT) of {} charged bytes",
            to - from
        );
    }

    #[cfg(all(windows, not(miri)))]
    pub(super) fn decommit(base: NonNull<u8>, from: usize, to: usize) {
        // SAFETY: `from..to` lies in the committed part at `base`, page-aligned, and holds
        // no item (the caller keeps `from` bytes).
        let done = unsafe { VirtualFree(base.as_ptr().add(from).cast(), to - from, MEM_DECOMMIT) };
        assert_ne!(done, 0, "VirtualFree(MEM_DECOMMIT) of a region's own pages");
    }

    #[cfg(all(windows, not(miri)))]
    pub(super) fn map(bytes: usize) -> NonNull<u8> {
        // SAFETY: reserves and commits a new range, as `reserve` and `commit` do.
        let base = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                bytes,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        NonNull::new(base.cast()).unwrap_or_else(|| panic!("VirtualAlloc of {bytes} charged bytes"))
    }

    #[cfg(all(windows, not(miri)))]
    pub(super) fn unmap(base: NonNull<u8>, _bytes: usize) {
        // SAFETY: `base` is a range reserved here, released whole (size 0), and nothing
        // refers to it any more: its region is gone.
        let done = unsafe { VirtualFree(base.as_ptr().cast(), 0, MEM_RELEASE) };
        assert_ne!(done, 0, "VirtualFree(MEM_RELEASE) of a region's range");
    }

    #[cfg(all(windows, not(miri)))]
    pub(super) fn advise_huge(_base: NonNull<u8>, _bytes: usize, _on: bool) {}

    // Miri, and targets that are neither: no address space is reserved; the region maps
    // what it holds through the allocator and moves to grow.
    #[cfg(any(miri, not(any(unix, windows))))]
    fn layout(bytes: usize) -> std::alloc::Layout {
        std::alloc::Layout::from_size_align(bytes, super::PAGE).expect("a page-aligned layout")
    }

    #[cfg(any(miri, not(any(unix, windows))))]
    pub(super) fn reserve(_bytes: usize) -> Option<NonNull<u8>> {
        None
    }

    #[cfg(any(miri, not(any(unix, windows))))]
    pub(super) fn commit(_base: NonNull<u8>, _from: usize, _to: usize) {
        unreachable!("nothing is reserved")
    }

    #[cfg(any(miri, not(any(unix, windows))))]
    pub(super) fn decommit(_base: NonNull<u8>, _from: usize, _to: usize) {
        unreachable!("a moving region gives back nothing in place")
    }

    #[cfg(any(miri, not(any(unix, windows))))]
    pub(super) fn map(bytes: usize) -> NonNull<u8> {
        // SAFETY: a layout of non-zero size (a chunk at least).
        let base = unsafe { std::alloc::alloc_zeroed(layout(bytes)) };
        NonNull::new(base).unwrap_or_else(|| std::alloc::handle_alloc_error(layout(bytes)))
    }

    #[cfg(any(miri, not(any(unix, windows))))]
    pub(super) fn unmap(base: NonNull<u8>, bytes: usize) {
        // SAFETY: `base` was allocated by `map` with this layout and nothing refers to it.
        unsafe { std::alloc::dealloc(base.as_ptr(), layout(bytes)) };
    }

    #[cfg(any(miri, not(any(unix, windows))))]
    pub(super) fn advise_huge(_base: NonNull<u8>, _bytes: usize, _on: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A region holds what was pushed, grows in place where the host lets it reserve its
    /// bound (Linux and Windows here) and moves in chunks elsewhere (Miri), is charged for
    /// its pages before they are made, gives back a step's spare pages, and returns
    /// every byte and every charge when it drops, through its parts too.
    #[test]
    fn a_region_is_charged_before_it_grows_and_returns_everything() {
        if !crate::measure::alone() {
            return;
        }
        let budget = Budget::new(64 << 20);
        let n: u64 = if cfg!(miri) { 5_000 } else { 200_000 };
        let ((), excess) = crate::measure::measure_within(&budget, || {
            let mut region: Region<u64, &Budget> = Region::new(&budget, 1 << 24);
            assert_eq!(region.moves(), cfg!(miri));
            for i in 0..n {
                region.push(i).unwrap();
            }
            region.extend_from_slice(&[7; 1000]).unwrap();
            assert_eq!(region.len() as u64, n + 1000);
            assert_eq!(region.as_slice()[123], 123);
            region.as_mut_slice()[123] = 5;
            for (i, atomic) in region.as_atomic().iter().enumerate().take(10) {
                atomic.fetch_add(i as u64, std::sync::atomic::Ordering::Relaxed);
            }
            assert_eq!(region.as_slice()[9], 18);
            assert!(budget.used() >= region.len() * 8);
            region.shrink_to_fit();
            if !region.moves() {
                assert_eq!(budget.used(), (region.len() * 8).next_multiple_of(PAGE));
            }
            region.settle();
            let (pages, held) = region.into_parts();
            assert_eq!(pages[5], 10, "5, and 5 added through its atomic");
            drop(pages);
            drop(held);
        });
        assert_eq!((excess, budget.used()), (0, 0));
        // Refused, a region is unchanged and holds nothing more.
        let first = if cfg!(miri) { CHUNK } else { PAGE };
        let small = Budget::new(first);
        let mut region: Region<u8, &Budget> = Region::new(&small, 1 << 20);
        region.extend_from_slice(&[1; 100]).unwrap();
        assert!(region.extend_from_slice(&vec![2; 2 * first]).is_err());
        assert_eq!((region.len(), region.as_slice()[99]), (100, 1));
        drop(region);
        assert_eq!(small.used(), 0);
    }
}
