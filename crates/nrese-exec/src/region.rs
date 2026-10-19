//! Growable arrays in memory they map themselves, charged to a budget before they grow
//! (`Harness/notes/2026-10-19-pm-load-arena-design.md`): what a bulk load interns its
//! terms into, and what the dictionary keeps of them afterwards.
//!
//! A [`Region`] owns its pages' policy and their return to the system, which the global
//! allocator can't give an array: on Linux and Windows it reserves its bound's address
//! range once (`mmap` without backing, `VirtualAlloc` reserve), makes pages as it grows,
//! never moves, and unmaps the range in one call when it drops. Growing on many threads at
//! once through the allocator, arrays that double leave their old buffers with whichever
//! thread held them, in pages that stay resident: a load of DBpedia core held 408 MiB more
//! than main with 432 MiB less live (18 Oct 2026).
//!
//! - **Huge pages.** On Linux a region is advised `MADV_NOHUGEPAGE` while it grows: a
//!   transparent huge page makes 2 MiB resident for the first byte touched, and 256 shards
//!   growing at once each touch new ones. A region never advises huge pages: its holder
//!   settles it ([`Region::settle`]) into a [`Settled`], which has no way to grow or shrink,
//!   and only then is the part whose huge pages lie wholly inside its charged pages advised
//!   `MADV_HUGEPAGE`. The dictionary reads it for good, and queries over memory a load left
//!   on small pages ran 2 to 5 % slower (fleet pm-thp-attrib-1, 19 Oct 2026). Only the
//!   region's own range is advised: no process setting changes.
//! - **Charging.** Bytes are charged to the region's [`Reservation`] before pages are
//!   made for them and released after they are given back, at most [`STEP`] at a time;
//!   a region grows by an eighth of what it holds (a page at least) and never past the
//!   pages of its bound. Refused, a growth gives back what it made: the region is as it
//!   was. [`Region::shrink_to_fit`] gives back a step's unused pages.
//! - **Without a reserved range** (a Linux host with `vm.overcommit_memory = 2` refuses
//!   unbacked ones; Miri and other targets have none): a region maps what it holds and
//!   moves into a mapping twice the size to grow, charged before it is mapped.
//!   [`Region::moves`] says which way a region grows.
//! - **Zero-sized items and empty regions** map nothing; their items are at an aligned
//!   dangling address.
//! - **Measured tests** (`crate::measure`): a region reports its pages as blocks of the
//!   generation it was made in, after the system made them and after it took them back,
//!   so a measurement counts its memory and its reservation's charges, or neither.

use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::{Budget, BudgetExceeded, Reservation};

/// The most a region charges, or makes pages for, in one step.
pub const STEP: usize = 1 << 20;

/// Huge pages' size, for [`Region::settle`]'s advice (Linux on x86-64 and arm64 with
/// 4 KiB pages; with other page sizes the advice covers less, never more).
const HUGE: usize = 2 << 20;

/// A bound whose items can't be held: past `isize::MAX` bytes, or past what a page count
/// can round up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundTooLarge {
    pub bound: usize,
    pub item_bytes: usize,
}

impl std::fmt::Display for BoundTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a region of {} items of {} bytes can't be held",
            self.bound, self.item_bytes
        )
    }
}

impl std::error::Error for BoundTooLarge {}

/// A growable array of `T` in pages of its own, charged to `B` before it grows (module
/// docs).
pub struct Region<T: Copy, B: Deref<Target = Budget> = Arc<Budget>> {
    /// Before `held`: the pages are given back before their charge is released.
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
    /// grows; refused where the bound's bytes can't be held. Nothing is charged yet: a
    /// reserved range costs no memory until its pages are made.
    pub fn new(budget: B, bound: usize) -> Result<Self, BoundTooLarge> {
        Self::with(budget, bound, true)
    }

    /// [`Self::new`], reserving its range where `reserve`, else moving to grow.
    fn with(budget: B, bound: usize, reserve: bool) -> Result<Self, BoundTooLarge> {
        let page = sys::page();
        assert!(
            align_of::<T>() <= page,
            "a region's items are page-aligned at most"
        );
        let too_large = BoundTooLarge {
            bound,
            item_bytes: size_of::<T>(),
        };
        let bytes = bound
            .checked_mul(size_of::<T>())
            .filter(|&bytes| isize::try_from(bytes).is_ok())
            .and_then(|bytes| bytes.checked_next_multiple_of(page))
            .ok_or(too_large)?;
        Ok(Self {
            pages: Pages {
                map: Mapping::reserve(bytes, reserve),
                len: 0,
                bound,
                _items: PhantomData,
            },
            held: Reservation::new(budget),
        })
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

    /// Whether growing copies it (module docs).
    pub fn moves(&self) -> bool {
        self.pages.map.moving
    }

    /// Room for `additional` more items: their pages charged, then made. Refused, nothing
    /// changes. Past the bound it panics, as indexing past a slice does.
    pub fn reserve(&mut self, additional: usize) -> Result<(), BudgetExceeded> {
        let len = self.pages.len.checked_add(additional);
        let len = len.filter(|&len| len <= self.pages.bound);
        let len = len.expect("a region holds at most its bound");
        // Within the bound, whose bytes `new` checked.
        let need = len * size_of::<T>();
        let held = self.pages.map.held;
        if need <= held {
            return Ok(());
        }
        let page = sys::page();
        let step = (held / 8).clamp(page, STEP);
        let target = need
            .max(held.saturating_add(step))
            .min(self.pages.map.limit)
            .next_multiple_of(page);
        self.pages.map.grow(target, &mut self.held)
    }

    pub fn push(&mut self, item: T) -> Result<(), BudgetExceeded> {
        self.reserve(1)?;
        let len = self.pages.len;
        // SAFETY: `reserve` made room for `len + 1` items at `items()`, which is aligned
        // for `T` (a page-aligned mapping, or dangling for zero-sized items).
        unsafe { self.pages.items().add(len).write(item) };
        self.pages.len = len + 1;
        Ok(())
    }

    pub fn extend_from_slice(&mut self, items: &[T]) -> Result<(), BudgetExceeded> {
        self.reserve(items.len())?;
        let len = self.pages.len;
        // SAFETY: `reserve` made room for `items` past `len` at `items()`, aligned for
        // `T`; the region's pages are no part of `items`, which the caller borrows.
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
        // SAFETY: the first `len` items were written at `items()`, aligned for `T`;
        // `&mut self` lends them alone.
        unsafe { std::slice::from_raw_parts_mut(self.pages.items(), self.pages.len) }
    }

    /// Gives back the pages past its items, which a step made ahead.
    pub fn shrink_to_fit(&mut self) {
        let keep = (self.pages.len * size_of::<T>()).next_multiple_of(sys::page());
        self.pages.map.shrink(keep, &mut self.held);
    }

    /// Its items, final: the pages past them given back, and huge pages advised for the
    /// whole ones inside what is left (module docs). A settled region only reads.
    pub fn settle(mut self) -> Settled<T, B> {
        self.shrink_to_fit();
        self.pages.map.advise_settled();
        Settled {
            pages: self.pages,
            held: self.held,
        }
    }
}

// `as_atomic` views a page-aligned `u64` array as atomics; where `AtomicU64` needs more
// alignment than `u64` (32-bit x86) that is unsound for an item that isn't page-aligned,
// and such targets aren't supported: they don't build.
const _: () = assert!(align_of::<u64>() == align_of::<AtomicU64>());

impl<B: Deref<Target = Budget>> Region<u64, B> {
    /// Its items as atomics, for threads that write them in parallel.
    pub fn as_atomic(&mut self) -> &[AtomicU64] {
        let Some(base) = self.pages.map.base else {
            return &[];
        };
        // SAFETY: a mapping is page-aligned, so aligned for `AtomicU64`, which has the size
        // and alignment of `u64`; its first `len` items were written, and `&mut self` keeps
        // any other view of them away while these are lent.
        unsafe { std::slice::from_raw_parts(base.as_ptr().cast(), self.pages.len) }
    }
}

/// A region whose items are final ([`Region::settle`]): read, taken apart or dropped,
/// never grown or shrunk, so huge pages advised over it stay inside its charge.
///
/// ```
/// # use nrese_exec::{Budget, Region};
/// let budget = Budget::new(1 << 20);
/// let mut region = Region::<u64, &Budget>::new(&budget, 1000).unwrap();
/// region.push(7).unwrap();
/// let settled = region.settle();
/// assert_eq!((settled.len(), settled[0]), (1, 7));
/// ```
///
/// It has no way to grow:
///
/// ```compile_fail,E0599
/// # use nrese_exec::{Budget, Region};
/// let budget = Budget::new(1 << 20);
/// let mut settled = Region::<u64, &Budget>::new(&budget, 1000).unwrap().settle();
/// settled.push(7).unwrap();
/// ```
///
/// or to shrink, then grow again into pages its advice covered (Sol's review of eb6ca4fc):
///
/// ```compile_fail,E0599
/// # use nrese_exec::{Budget, Region};
/// let budget = Budget::new(1 << 20);
/// let mut settled = Region::<u64, &Budget>::new(&budget, 1000).unwrap().settle();
/// settled.shrink_to_fit();
/// ```
pub struct Settled<T: Copy, B: Deref<Target = Budget> = Arc<Budget>> {
    /// Before `held`, as in [`Region`].
    pages: Pages<T>,
    held: Reservation<B>,
}

impl<T: Copy, B: Deref<Target = Budget>> Settled<T, B> {
    /// The bytes held: what its pages were charged.
    pub fn charged(&self) -> usize {
        self.held.bytes()
    }

    pub fn as_slice(&self) -> &[T] {
        &self.pages
    }

    /// Its pages, and the reservation of their bytes, for a holder that keeps the charge
    /// until it drops the pages.
    pub fn into_parts(self) -> (Pages<T>, Reservation<B>) {
        (self.pages, self.held)
    }
}

impl<T: Copy, B: Deref<Target = Budget>> Deref for Settled<T, B> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.pages
    }
}

impl<T: Copy> Pages<T> {
    /// Where its items are: its mapping, or an address aligned for `T` where it maps
    /// nothing (zero-sized items, an empty region).
    fn items(&self) -> *mut T {
        match self.map.base {
            Some(base) if size_of::<T>() > 0 => base.as_ptr().cast(),
            _ => NonNull::<T>::dangling().as_ptr(),
        }
    }
}

impl<T: Copy> Deref for Pages<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: the first `len` items were written at `items()`, aligned for `T`; they
        // live as long as the mapping.
        unsafe { std::slice::from_raw_parts(self.items(), self.len) }
    }
}

// SAFETY: a region owns its mapping as a `Vec` owns its buffer: it is as `Send` and
// `Sync` as its items.
unsafe impl<T: Copy + Send> Send for Pages<T> {}
// SAFETY: as for `Send`; `&Pages` lends only shared slices.
unsafe impl<T: Copy + Sync> Sync for Pages<T> {}

/// A region's memory: `mapped` bytes at `base` (none: `None`), of which the first `held`
/// are made and charged; it grows to `limit` bytes at most.
struct Mapping {
    base: Option<NonNull<u8>>,
    mapped: usize,
    held: usize,
    limit: usize,
    /// It has no reserved range: it maps what it holds and moves to grow.
    moving: bool,
    #[cfg(any(test, feature = "measure"))]
    tag: u64,
}

impl Mapping {
    /// The address range for `limit` bytes (a whole number of pages) where `reserve` and
    /// the host allow it, without memory yet; else nothing, to map as it grows.
    fn reserve(limit: usize, reserve: bool) -> Self {
        let base = if limit > 0 && reserve {
            sys::reserve(limit)
        } else {
            None
        };
        if let Some(base) = base {
            sys::advise_huge(base, limit, false);
        }
        Self {
            base,
            mapped: if base.is_some() { limit } else { 0 },
            held: 0,
            limit,
            moving: limit > 0 && base.is_none(),
            #[cfg(any(test, feature = "measure"))]
            tag: crate::measure::tag(),
        }
    }

    /// `target` bytes made and charged (at most `limit`, a whole number of pages): each
    /// step charged first. Refused, it is as it was.
    fn grow(
        &mut self,
        target: usize,
        held: &mut Reservation<impl Deref<Target = Budget>>,
    ) -> Result<(), BudgetExceeded> {
        debug_assert!(target <= self.limit && target > self.held);
        if self.moving {
            return self.move_to(target.max(self.held * 2).min(self.limit), held);
        }
        let base = self.base.expect("a reserved range");
        let start = self.held;
        while self.held < target {
            let next = (self.held + STEP).min(target);
            if let Err(refused) = charge(held, next - self.held) {
                self.shrink(start, held);
                return Err(refused);
            }
            sys::commit(base, self.held, next);
            self.mapped(next - self.held);
            self.held = next;
        }
        Ok(())
    }

    /// Moves into a new mapping of `size` bytes, charged before it is mapped, the old one
    /// unmapped and its charge released after the items are copied.
    fn move_to(
        &mut self,
        size: usize,
        held: &mut Reservation<impl Deref<Target = Budget>>,
    ) -> Result<(), BudgetExceeded> {
        let mut charged = 0;
        while charged < size {
            let next = (charged + STEP).min(size);
            if let Err(refused) = charge(held, next - charged) {
                held.release(charged);
                return Err(refused);
            }
            charged = next;
        }
        let base = sys::map(size);
        sys::advise_huge(base, size, false);
        self.mapped(size);
        if let Some(old) = self.base {
            // SAFETY: both are mappings made by `sys`, apart, of at least `held` bytes.
            unsafe { std::ptr::copy_nonoverlapping(old.as_ptr(), base.as_ptr(), self.held) };
            sys::unmap(old, self.mapped);
            self.unmapped(self.mapped);
            held.release(self.mapped);
        }
        (self.base, self.mapped, self.held) = (Some(base), size, size);
        Ok(())
    }

    /// The pages past `keep` bytes given back, and their charge.
    fn shrink(&mut self, keep: usize, held: &mut Reservation<impl Deref<Target = Budget>>) {
        let (Some(base), false) = (self.base, self.moving) else {
            return;
        };
        if keep >= self.held {
            return;
        }
        sys::decommit(base, keep, self.held);
        self.unmapped(self.held - keep);
        held.release(self.held - keep);
        self.held = keep;
    }

    /// Huge pages advised for the whole ones inside what it holds: only for a
    /// [`Settled`] region, whose held bytes no longer change.
    fn advise_settled(&self) {
        let Some(base) = self.base else {
            return;
        };
        let start = base.as_ptr().addr();
        // The huge pages' addresses inside `start..start + held` (none where it holds less
        // than one whole).
        let first = start.next_multiple_of(HUGE);
        let end = (start + self.held) / HUGE * HUGE;
        if first < end {
            // SAFETY: `first - start` is within the mapping's held bytes.
            let at = unsafe { base.add(first - start) };
            sys::advise_huge(at, end - first, true);
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
        if let Some(base) = self.base {
            let made = if self.moving { self.mapped } else { self.held };
            sys::unmap(base, self.mapped);
            self.unmapped(made);
        }
    }
}

/// `bytes` charged to `held`, at most [`STEP`] (the tests see the largest).
fn charge(
    held: &mut Reservation<impl Deref<Target = Budget>>,
    bytes: usize,
) -> Result<(), BudgetExceeded> {
    debug_assert!(bytes <= STEP);
    #[cfg(test)]
    tests::LARGEST_CHARGE.with(|largest| largest.set(largest.get().max(bytes)));
    held.charge(bytes)
}

/// The system calls, the only unsafe code of regions besides the views of their items.
/// A failure to make or give back memory that was reserved and charged panics: it is the
/// system's, as an allocation failure is.
mod sys {
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Whether pages come from the global allocator (Miri, and targets other than Linux
    /// and Windows).
    #[cfg(any(test, feature = "measure"))]
    pub(super) const THROUGH_ALLOCATOR: bool =
        cfg!(any(miri, not(any(target_os = "linux", windows))));

    /// The system's page size, read once.
    pub(super) fn page() -> usize {
        static PAGE: AtomicUsize = AtomicUsize::new(0);
        match PAGE.load(Ordering::Relaxed) {
            0 => {
                let page = system_page();
                PAGE.store(page, Ordering::Relaxed);
                page
            }
            page => page,
        }
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    fn system_page() -> usize {
        // SAFETY: `sysconf` reads a system constant.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(page).expect("a page size")
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    pub(super) fn reserve(bytes: usize) -> Option<NonNull<u8>> {
        // SAFETY: a new anonymous private mapping at an address of the system's choosing;
        // no existing memory is affected. Unbacked (`MAP_NORESERVE`): pages are made as
        // they are touched; a host that refuses it returns `MAP_FAILED`.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        (base != libc::MAP_FAILED).then(|| NonNull::new(base.cast()).expect("mmap's address"))
    }

    /// Pages are made as they are touched.
    #[cfg(all(target_os = "linux", not(miri)))]
    pub(super) fn commit(_base: NonNull<u8>, _from: usize, _to: usize) {}

    #[cfg(all(target_os = "linux", not(miri)))]
    pub(super) fn decommit(base: NonNull<u8>, from: usize, to: usize) {
        // SAFETY: `from..to` lies in the mapping at `base` (the caller holds `to` bytes of
        // it), page-aligned, and holds no item: the pages read as zero if touched again.
        let done = unsafe {
            libc::madvise(
                base.as_ptr().add(from).cast(),
                to - from,
                libc::MADV_DONTNEED,
            )
        };
        assert_eq!(done, 0, "madvise(MADV_DONTNEED) of a region's own pages");
    }

    #[cfg(all(target_os = "linux", not(miri)))]
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

    #[cfg(all(target_os = "linux", not(miri)))]
    pub(super) fn unmap(base: NonNull<u8>, bytes: usize) {
        // SAFETY: `base` is a mapping of `bytes` made here, and nothing refers to it any
        // more: its region is gone, or moved out of it.
        let done = unsafe { libc::munmap(base.as_ptr().cast(), bytes) };
        assert_eq!(done, 0, "munmap of a region's mapping");
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    pub(super) fn advise_huge(base: NonNull<u8>, bytes: usize, on: bool) {
        let advice = if on {
            libc::MADV_HUGEPAGE
        } else {
            libc::MADV_NOHUGEPAGE
        };
        // SAFETY: advice on `bytes` of the region's own mapping from `base`; it changes how
        // its pages are backed, never their contents. A kernel without transparent huge
        // pages refuses it (`EINVAL`), which changes nothing.
        let _ = unsafe { libc::madvise(base.as_ptr().cast(), bytes, advice) };
    }

    #[cfg(all(windows, not(miri)))]
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
        VirtualFree,
    };

    #[cfg(all(windows, not(miri)))]
    fn system_page() -> usize {
        let mut info = windows_sys::Win32::System::SystemInformation::SYSTEM_INFO::default();
        // SAFETY: fills the structure it is given, which lives for the call.
        unsafe { windows_sys::Win32::System::SystemInformation::GetSystemInfo(&mut info) };
        info.dwPageSize as usize
    }

    #[cfg(all(windows, not(miri)))]
    pub(super) fn reserve(bytes: usize) -> Option<NonNull<u8>> {
        // SAFETY: reserves address space at an address of the system's choosing; no
        // existing memory is affected, nothing is committed.
        let base = unsafe { VirtualAlloc(std::ptr::null(), bytes, MEM_RESERVE, PAGE_READWRITE) };
        NonNull::new(base.cast())
    }

    #[cfg(all(windows, not(miri)))]
    pub(super) fn commit(base: NonNull<u8>, from: usize, to: usize) {
        // SAFETY: `from..to` lies in the range reserved at `base` (the caller grows to its
        // limit at most); committing pages makes them readable and zero, and affects no
        // other memory.
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
        // refers to it any more: its region is gone, or moved out of it.
        let done = unsafe { VirtualFree(base.as_ptr().cast(), 0, MEM_RELEASE) };
        assert_ne!(done, 0, "VirtualFree(MEM_RELEASE) of a region's range");
    }

    /// Windows has no transparent huge pages.
    #[cfg(all(windows, not(miri)))]
    pub(super) fn advise_huge(_base: NonNull<u8>, _bytes: usize, _on: bool) {}

    // Miri, and targets other than Linux and Windows: no range is reserved; a region maps
    // what it holds through the allocator and moves to grow.
    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    fn system_page() -> usize {
        4096
    }

    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    fn layout(bytes: usize) -> std::alloc::Layout {
        std::alloc::Layout::from_size_align(bytes, page()).expect("a page-aligned layout")
    }

    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    pub(super) fn reserve(_bytes: usize) -> Option<NonNull<u8>> {
        None
    }

    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    pub(super) fn commit(_base: NonNull<u8>, _from: usize, _to: usize) {
        unreachable!("nothing is reserved")
    }

    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    pub(super) fn decommit(_base: NonNull<u8>, _from: usize, _to: usize) {
        unreachable!("a moving region gives back nothing in place")
    }

    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    pub(super) fn map(bytes: usize) -> NonNull<u8> {
        // SAFETY: a layout of non-zero size: a region maps a page at least.
        let base = unsafe { std::alloc::alloc_zeroed(layout(bytes)) };
        NonNull::new(base).unwrap_or_else(|| std::alloc::handle_alloc_error(layout(bytes)))
    }

    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    pub(super) fn unmap(base: NonNull<u8>, bytes: usize) {
        // SAFETY: `base` was allocated by `map` with this layout and nothing refers to it.
        unsafe { std::alloc::dealloc(base.as_ptr(), layout(bytes)) };
    }

    #[cfg(any(miri, not(any(target_os = "linux", windows))))]
    pub(super) fn advise_huge(_base: NonNull<u8>, _bytes: usize, _on: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// The largest single charge a region made on this thread.
        pub(super) static LARGEST_CHARGE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// A region holds what was pushed, grows in place where the host lets it reserve its
    /// bound (Linux and Windows here) and moves elsewhere (Miri), is charged for its pages
    /// before they are made, gives back a step's spare pages, and returns every byte and
    /// every charge when it drops, through its parts too.
    #[test]
    fn a_region_is_charged_before_it_grows_and_returns_everything() {
        if !crate::measure::alone() {
            return;
        }
        let budget = Budget::new(64 << 20);
        let n: u64 = if cfg!(miri) { 5_000 } else { 200_000 };
        let ((), excess) = crate::measure::measure_within(&budget, || {
            let mut region: Region<u64, &Budget> = Region::new(&budget, 1 << 24).unwrap();
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
                assert_eq!(
                    budget.used(),
                    (region.len() * 8).next_multiple_of(sys::page())
                );
            }
            let settled = region.settle();
            assert_eq!(settled.charged(), budget.used());
            let (pages, held) = settled.into_parts();
            assert_eq!(pages[5], 10, "5, and 5 added through its atomic");
            drop(pages);
            drop(held);
        });
        assert_eq!((excess, budget.used()), (0, 0));
        // Refused, a region is as it was and holds nothing more.
        let page = sys::page();
        let small = Budget::new(page);
        let mut region: Region<u8, &Budget> = Region::new(&small, 1 << 20).unwrap();
        region.extend_from_slice(&[1; 100]).unwrap();
        let charged = region.charged();
        assert!(region.extend_from_slice(&vec![2; 2 * page]).is_err());
        assert_eq!((region.len(), region.as_slice()[99]), (100, 1));
        assert_eq!((region.charged(), small.used()), (charged, charged));
        drop(region);
        assert_eq!(small.used(), 0);
    }

    /// A settled region is resident within its charge: its huge pages are advised only
    /// where they lie wholly inside its pages, so collapsing every one the kernel lets
    /// (`khugepaged` does it in time, `MADV_COLLAPSE` at once) fills none past them.
    #[cfg(all(target_os = "linux", not(miri)))]
    #[test]
    fn a_settled_region_stays_resident_within_its_charge() {
        /// Linux 6.1's synchronous collapse (not in libc for every target).
        const MADV_COLLAPSE: libc::c_int = 25;
        let page = sys::page();
        let budget = Budget::new(64 << 20);
        let mut region: Region<u8, &Budget> = Region::new(&budget, 32 << 20).unwrap();
        // Past a few huge pages, ending inside one.
        region
            .extend_from_slice(&vec![1; (6 << 20) + 3 * page + 5])
            .unwrap();
        let settled = region.settle();
        let (base, mapped) = (settled.as_ptr().cast_mut(), settled.pages.map.mapped);
        for at in (base.addr().next_multiple_of(HUGE)..base.addr() + mapped).step_by(HUGE) {
            // SAFETY: advice on a huge page of the region's own mapping, which never
            // changes its contents; refused (outside the advised part), it does nothing.
            let _ = unsafe { libc::madvise(base.with_addr(at).cast(), HUGE, MADV_COLLAPSE) };
        }
        let mut resident = vec![0u8; mapped / page];
        // SAFETY: `resident` has a byte for each page of the mapping at `base`.
        let done = unsafe { libc::mincore(base.cast(), mapped, resident.as_mut_ptr()) };
        assert_eq!(done, 0, "mincore of the region's mapping");
        let resident = resident.iter().filter(|&&r| r & 1 != 0).count() * page;
        let charged = settled.charged();
        assert!(
            resident <= charged,
            "{resident} bytes resident, {charged} charged"
        );
        assert!(settled.iter().all(|&b| b == 1));
    }

    /// The boundaries (Sol's review of 6112f533): each row an edge a region must hold.
    #[test]
    fn a_region_holds_at_its_boundaries() {
        #[derive(Clone, Copy)]
        #[repr(align(64))]
        struct Aligned;
        let page = sys::page();
        let budget = Budget::new(64 << 20);
        // Empty: its views are aligned, in place and moving.
        for reserve in [true, false] {
            let mut empty: Region<u64, &Budget> = Region::with(&budget, 10, reserve).unwrap();
            assert!(empty.as_slice().is_empty() && empty.as_mut_slice().is_empty());
            assert!(empty.as_atomic().is_empty());
        }
        // Zero-sized items: no pages, no charge, an aligned address.
        let mut zst: Region<Aligned, &Budget> = Region::new(&budget, 10).unwrap();
        for _ in 0..10 {
            zst.push(Aligned).unwrap();
        }
        assert_eq!(
            (zst.len(), zst.as_slice().len(), zst.charged()),
            (10, 10, 0)
        );
        // A bound that isn't a whole number of pages or steps: growth stops at its pages.
        for bound in [10 * page, 10 * page + 1] {
            let mut tight: Region<u8, &Budget> = Region::new(&budget, bound).unwrap();
            for _ in 0..bound {
                tight.push(7).unwrap();
            }
            assert_eq!(tight.charged(), bound.next_multiple_of(page));
            tight.shrink_to_fit();
            assert!(tight.as_slice().iter().all(|&x| x == 7));
        }
        // A bound whose bytes can't be held is refused before anything is mapped.
        assert!(Region::<u64, &Budget>::new(&budget, usize::MAX).is_err());
        assert!(Region::<u64, &Budget>::new(&budget, usize::MAX / 8 + 1).is_err());
        assert!(Region::<u8, &Budget>::new(&budget, usize::MAX).is_err());
        // Bulk growth past a step, in place and moving: no charge past a step, every item
        // copied when it moves.
        let items: Vec<u64> = (0..(3 * STEP / 8) as u64).collect();
        for reserve in [true, false] {
            LARGEST_CHARGE.with(|largest| largest.set(0));
            let mut bulk: Region<u64, &Budget> =
                Region::with(&budget, items.len() * 2, reserve).unwrap();
            bulk.extend_from_slice(&items[..1000]).unwrap();
            bulk.extend_from_slice(&items).unwrap();
            assert_eq!(&bulk.as_slice()[1000..], &items[..]);
            assert_eq!(bulk.as_slice()[999], 999);
            assert!(LARGEST_CHARGE.with(std::cell::Cell::get) <= STEP);
            assert_eq!(budget.used(), bulk.charged());
        }
        assert_eq!(budget.used(), 0);
    }
}
