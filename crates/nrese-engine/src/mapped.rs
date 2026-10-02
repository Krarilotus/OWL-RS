//! Read-only memory maps of checkpoint files: index data and dictionary text used in
//! place, not copied.
//!
//! A checkpoint's packed permutations and dictionary are most of a store's memory once
//! loaded. Mapped, they stay in the file: the OS reads the pages a query touches, keeps
//! them in its page cache while there is room, and can drop them again, as it does for
//! QLever's or LMDB's files. Restart maps the file instead of copying it.
//!
//! Safety rests on the files never changing while mapped: a checkpoint is written to a
//! temporary file, synced and renamed, never written again, and the engine holds the
//! directory's lock. Deleting a mapped file (an older checkpoint) is fine on Unix, where
//! the pages stay valid until unmapped; on Windows the deletion fails while it is mapped,
//! and the file is removed at a later checkpoint.

use std::fs::File;
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

/// A whole file, mapped read-only.
pub(crate) type Map = Arc<memmap2::Mmap>;

/// Maps `path` read-only.
pub(crate) fn map(path: &Path) -> std::io::Result<Map> {
    let file = File::open(path)?;
    // SAFETY: checkpoint files are immutable once renamed into place (module docs).
    let map = unsafe { memmap2::Mmap::map(&file)? };
    Ok(Arc::new(map))
}

/// Element types a file stores as their little-endian memory image.
///
/// # Safety
///
/// Every bit pattern of the type's size must be a valid value, and the type must have no
/// padding bytes (padding is spelled out as fields).
pub(crate) unsafe trait Plain: Copy + 'static {}
// SAFETY: integers are valid for every bit pattern and have no padding.
unsafe impl Plain for u8 {}
// SAFETY: as above.
unsafe impl Plain for u32 {}
// SAFETY: as above.
unsafe impl Plain for u64 {}

/// `len` values of `T` inside a mapped file.
pub(crate) struct Mapped<T: Plain> {
    map: Map,
    /// Byte offset of the first value, aligned for `T`.
    offset: usize,
    len: usize,
    _values: PhantomData<T>,
}

impl<T: Plain> Mapped<T> {
    /// `len` values at byte `offset` of `map`; `None` if they aren't aligned or in bounds,
    /// or the machine isn't little-endian (then the caller copies them).
    pub(crate) fn new(map: &Map, offset: usize, len: usize) -> Option<Self> {
        let end = offset.checked_add(len.checked_mul(size_of::<T>())?)?;
        let aligned = (map.as_ptr() as usize + offset).is_multiple_of(align_of::<T>());
        (cfg!(target_endian = "little") && aligned && end <= map.len()).then(|| Self {
            map: Arc::clone(map),
            offset,
            len,
            _values: PhantomData,
        })
    }
}

impl<T: Plain> Clone for Mapped<T> {
    fn clone(&self) -> Self {
        Self {
            map: Arc::clone(&self.map),
            offset: self.offset,
            len: self.len,
            _values: PhantomData,
        }
    }
}

impl<T: Plain> Deref for Mapped<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        // SAFETY: `new` checked bounds and alignment; the map lives as long as `self`; `T`
        // is valid for any bit pattern, and on a little-endian machine the file's bytes are
        // its in-memory representation.
        unsafe {
            std::slice::from_raw_parts(self.map.as_ptr().add(self.offset).cast::<T>(), self.len)
        }
    }
}

impl<T: Plain> std::fmt::Debug for Mapped<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Mapped<{}>({} at {})",
            std::any::type_name::<T>(),
            self.len,
            self.offset
        )
    }
}
