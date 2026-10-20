//! A test binary that measures without `nrese_exec::measure::Measured` as its global
//! allocator: on purpose, as a measured test can forget it. It must fail, not pass with
//! nothing counted. Here, not in nrese-exec, whose own test binaries install it (its unit
//! tests) or can't see `measure` (its integration tests are built without the feature).

use std::alloc::{GlobalAlloc, Layout, System};

use nrese_exec::Budget;
use nrese_exec::measure::{Measured, alone, measure, measure_within};

/// Every way into a measurement panics, naming the allocator to install; also after a
/// `Measured` of the test's own was used directly, which is no global allocator (Sol,
/// measure-pool r1: that passed a held 4 KiB as 0 bytes).
#[test]
fn measuring_without_the_allocator_panics() {
    let own = Measured(System);
    let layout = Layout::new::<u64>();
    // SAFETY: a layout of non-zero size, allocated and freed by the same allocator.
    unsafe {
        let block = own.alloc(layout);
        assert!(!block.is_null());
        own.dealloc(block, layout);
    }
    let held = || vec![1u8; 1 << 20].len();
    let budget = Budget::unlimited();
    let ways: [(&str, &dyn Fn()); 3] = [
        ("measure", &|| {
            let _ = measure(held);
        }),
        ("measure_within", &|| {
            let _ = measure_within(&budget, held);
        }),
        ("alone", &|| {
            let _ = alone();
        }),
    ];
    for (way, run) in ways {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
            .expect_err(&format!("{way} measured without the allocator"));
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains("#[global_allocator]") && message.contains("Measured"),
            "{way}: {message}"
        );
    }
}
