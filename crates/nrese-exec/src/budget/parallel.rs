//! The fallible parallel fill's scratch, within the destination's exact budget.

use super::{BudgetExceeded, ChargedVec, FillRange, Room, Slots, Written};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn try_fill<T: Send, E: Send + From<BudgetExceeded>>(
    room: &mut Room<'_, T>,
    n: usize,
    item: impl Fn(usize) -> Result<T, E> + Sync,
) -> Result<(), E> {
    let len = room.0.len();
    assert!(n <= room.0.capacity() - len, "past the room reserved");
    let slots = Slots(room.0.as_mut_ptr().wrapping_add(len));
    let part = n.div_ceil(4 * rayon::current_num_threads()).max(1);
    let count = n.div_ceil(part);
    // Both backings admitted before allocation and dispatch. Indexed collection writes
    // directly into this exact capacity; it has no reason to grow either vector.
    let mut ranges = ChargedVec::<FillRange<T, E>, _>::with_capacity(room.1, count)?;
    let mut complete = ChargedVec::<Written<T>, _>::with_capacity(room.1, count)?;
    let failed = AtomicBool::new(false);
    (0..count)
        .into_par_iter()
        .map(|p| {
            let mut written = Written::new(slots, p * part);
            for i in p * part..(p * part + part).min(n) {
                if failed.load(Ordering::Relaxed) {
                    break;
                }
                match item(i) {
                    Ok(value) => {
                        // SAFETY: Slots assigns this range alone the uninitialised slot i.
                        unsafe { slots.0.add(i).write(value) };
                        written.len += 1;
                    }
                    Err(error) => {
                        failed.store(true, Ordering::Relaxed);
                        return (written, Some((i, error)));
                    }
                }
            }
            (written, None)
        })
        .collect_into_vec(&mut ranges.items);
    let mut first: Option<(usize, E)> = None;
    for (written, error) in ranges.items.drain(..) {
        // One token per range, no more than count: complete's admitted capacity.
        complete.items.push(written);
        if let Some((i, error)) = error
            && first.as_ref().is_none_or(|(at, _)| i < *at)
        {
            first = Some((i, error));
        }
    }
    drop(ranges);
    if let Some((_, error)) = first {
        drop(complete);
        return Err(error);
    }
    let written = complete
        .items
        .drain(..)
        .fold(Written::new(slots, 0), Written::join);
    assert_eq!((written.start, written.len), (0, n), "every slot written");
    std::mem::forget(written);
    // SAFETY: every one of the n slots after the old items was written and handed over.
    unsafe { room.0.set_len(len + n) };
    Ok(())
}
