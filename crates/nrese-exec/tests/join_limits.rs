//! A join over its row limit stops before it expands its matches: the pairs of an
//! equal-key group are charged before they are made (the review of 3 October 2026, P1).
//! Measured with a counting allocator, so this file is a test binary of its own.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use nrese_exec::IdTable;
use nrese_exec::join::{join, left_join};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded to the system allocator; the counters only observe.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes allocated at the peak of `work` beyond what was live before it.
fn peak_of(work: impl FnOnce()) -> usize {
    let before = CURRENT.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    work();
    PEAK.load(Ordering::Relaxed) - before
}

/// `rows` rows of one key value, sorted on it, with a second column.
fn one_key(rows: usize) -> IdTable {
    IdTable::from_columns(vec![vec![5; rows], (0..rows as u64).collect()]).assume_sorted_by(vec![0])
}

#[test]
fn joins_over_their_limit_stop_before_expanding_a_group() {
    // 2,000 × 2,000 rows of one key: 4 million pairs, 32 MB of row numbers if expanded.
    let (left, right) = (one_key(2_000), one_key(2_000));
    let peak = peak_of(|| {
        assert!(join(&left, &right, &[0], &[0], 1).is_err());
    });
    assert!(
        peak < 256 * 1024,
        "the inner join allocated {peak} bytes before refusing"
    );
    let peak = peak_of(|| {
        assert!(left_join(&left, &right, &[0], &[0], None, 1).is_err());
    });
    assert!(
        peak < 256 * 1024,
        "OPTIONAL allocated {peak} bytes before refusing"
    );
    // Within the limit the same joins give every pair.
    assert_eq!(
        join(&left, &right, &[0], &[0], usize::MAX).unwrap().len(),
        4_000_000
    );
    assert_eq!(
        left_join(&left, &right, &[0], &[0], None, usize::MAX)
            .unwrap()
            .len(),
        4_000_000
    );
}
