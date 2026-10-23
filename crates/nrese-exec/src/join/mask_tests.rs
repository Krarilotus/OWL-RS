//! One regression table for the assigned counted-work contract. The existing 400-case
//! independent definition oracle also runs the public checked entry point.

use super::*;
use crate::StopSource;

/// Spec: mask setup (even empty-left/filtered build) and rejected enumeration must
/// observe the original stop within POLL_UNITS work, before the closing finish check.
/// Finite disabled-progress controls may still return Cancelled at finish, so retain
/// the separate inner result. Every bound second key disagrees: no match is possible.
#[test]
fn masks_stop_inside_setup_and_rejected_work() {
    for phase in [
        "empty-left setup",
        "filtered build",
        "left-open",
        "right-open",
        "left rows",
    ] {
        let b = Arc::new(Budget::unlimited());
        let mut unrelated = Reservation::new(b.clone());
        unrelated.charge(13).unwrap();
        let rows = 3 * crate::POLL_UNITS as usize + 17;
        let left_rows = match phase {
            "empty-left setup" | "filtered build" => 0,
            "left rows" => rows,
            _ => 1,
        };
        let right_rows = if phase == "left rows" { 0 } else { rows };
        let right_open = matches!(phase, "filtered build" | "right-open");
        let left_key = if right_open { 9 } else { UNDEF };
        let right_key = if right_open { UNDEF } else { 9 };
        let left =
            IdTable::from_rows(2, std::iter::repeat_n(&[left_key, 1][..], left_rows), &b).unwrap();
        let right = IdTable::from_rows(2, std::iter::repeat_n(&[right_key, 2][..], right_rows), &b)
            .unwrap();
        let held = b.used();
        let mut found = vec![true; left.len()];
        let flag = Arc::new(AtomicBool::new(false));
        let source = StopSource::new(Some(flag.clone()), None);

        // Actual setup completes before enumeration is armed. Setup therefore cannot
        // make an enumeration row pass. The empty-left rows use actual fallible setup.
        let prepared = if matches!(phase, "left-open" | "right-open" | "left rows") {
            Some(Compatible::new(&right, &[0, 1], &[0, 1], &b).unwrap())
        } else {
            None
        };
        let mut work = Ticker::new(source).unwrap();
        if phase != "filtered build" {
            flag.store(true, AtomicOrdering::Relaxed);
        }
        let inner = if let Some(prepared) = &prepared {
            prepared.mask(&left, true, &mut found, &mut || work.tick(1))
        } else if phase == "filtered build" {
            // Observe the build's own result before the later count/collection passes;
            // a checkpoint in either later pass cannot hide an unchecked build scan.
            BuildTable::with_progress(
                &right,
                &[0, 1],
                &b,
                |r| right.get(r, 0) != UNDEF && right.get(r, 1) != UNDEF,
                &mut || work.tick(1),
                |next, rows, progress| {
                    initialize_chains(next, rows, progress)?;
                    flag.store(true, AtomicOrdering::Relaxed);
                    Ok(())
                },
            )
            .map(drop)
        } else {
            compatible_mask_work(
                &left,
                &right,
                &[0, 1],
                &[0, 1],
                true,
                &b,
                &mut found,
                &mut || work.tick(1),
                |next, rows, progress| {
                    initialize_chains(next, rows, progress)?;
                    Ok(())
                },
            )
        };
        let stopped_inside = inner == Err(Stop::Cancelled);
        // This deliberately preserves the real closing check in a no-op control.
        let outer = inner.and_then(|()| work.finish());
        assert_eq!(outer, Err(Stop::Cancelled), "{phase}: original typed stop");
        assert!(
            stopped_inside,
            "{phase}: mask must stop inside before finish"
        );
        drop(prepared);
        assert_eq!(b.used(), held, "{phase}: setup scratch must drop on error");
        drop(left);
        drop(right);
        assert_eq!(b.used(), 13, "{phase}: independent owner survives");
        drop(unrelated);
        assert_eq!(b.used(), 0);
    }
}
