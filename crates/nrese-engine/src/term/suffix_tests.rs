//! Package 1's guards (`notes/2026-11-08-p1-prepared-install-design.md` §4): new-contract
//! tests of an API the pin doesn't have, so none is red there. Rebase interleavings (T3–T5)
//! are beside the rebase test in `durability/checkpoint.rs`, which builds the mapped bases.

use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded};
use nrese_rdf::{LiteralRef, TermRef};
use parking_lot::Mutex;

use super::{PrepareError, Suffix};
use crate::term::dictionary::Dictionary;
use crate::term::hash::key_hash;
use crate::term::pending::PROVISIONAL;
use crate::term::pieces::BLOCK;
use crate::term::slot::WriterGuard;

fn literal(text: &str) -> TermRef<'_> {
    LiteralRef::new_simple_literal(text).into()
}

/// The keys `texts` have as simple literals, as the dictionary encodes them.
fn keys_of(texts: &[String]) -> Vec<Vec<u8>> {
    let scratch = Dictionary::default();
    for text in texts {
        scratch.intern(literal(text));
    }
    scratch.export_keys(0, scratch.len())
}

fn texts(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{prefix} {i}")).collect()
}

/// A dictionary of `n` interned literals, and a suffix at its end holding `added` more, charged
/// to `budget`.
fn dictionary_and_suffix(n: usize, added: &[String], budget: &Arc<Budget>) -> (Dictionary, Suffix) {
    let dictionary = Dictionary::default();
    for text in texts("old", n) {
        dictionary.intern(literal(&text));
    }
    let mut suffix = Suffix::at(dictionary.len(), Arc::clone(budget));
    for key in keys_of(added) {
        suffix.push(&key, key_hash(&key)).unwrap();
    }
    (dictionary, suffix)
}

/// The writer slot a test holds: its guard proves it to the dictionary.
fn slot() -> Mutex<()> {
    Mutex::new(())
}

/// Every installed text is found at the id the suffix gave it, and decodes back.
fn assert_installed(dictionary: &Dictionary, start: u64, added: &[String], row: &str) {
    for (i, text) in added.iter().enumerate() {
        let id = dictionary
            .lookup(literal(text))
            .unwrap_or_else(|| panic!("{row}: {text} found"));
        assert_eq!(id.payload(), start + i as u64, "{row}: {text}'s id");
        assert_eq!(
            dictionary.decode(id),
            Some(nrese_rdf::Literal::new_simple_literal(text).into()),
            "{row}: {text} decodes"
        );
    }
}

/// T1. Installing a prepared suffix allocates nothing (the promise reserved the room; the keys'
/// `Arc`s are handed over, not copied), across the sizes that matter to the tail: one key, one
/// short of a block, a block, two blocks and three. Measured, alone.
#[test]
fn installing_a_prepared_suffix_allocates_nothing() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let budget = Arc::new(Budget::unlimited());
    for added in [1, BLOCK - 1, BLOCK, 2 * BLOCK + 3] {
        let new = texts("new", added);
        let (dictionary, suffix) = dictionary_and_suffix(100, &new, &budget);
        let slot = slot();
        let guard = WriterGuard::hold(slot.lock());
        let prepared = dictionary.prepare(&guard, &suffix).unwrap();
        let ((), held) = nrese_exec::measure::measure(|| prepared.install());
        assert_eq!(held, 0, "{added} keys: bytes allocated by the install");
        assert_installed(&dictionary, 100, &new, &format!("{added} keys"));
    }
}

/// T2. An install whose keys cross a block boundary of the live tail leaves the tail unsealed
/// and every key readable, by lookup, decode and a view taken between install and seal; the
/// seal then blocks it with every key where it was.
#[test]
fn an_install_across_a_block_boundary_reads_the_same_before_and_after_the_seal() {
    let budget = Arc::new(Budget::unlimited());
    let new = texts("new", 5);
    let (dictionary, suffix) = dictionary_and_suffix(BLOCK - 2, &new, &budget);
    let before = dictionary.export_keys(0, dictionary.len());
    let slot = slot();
    let guard = WriterGuard::hold(slot.lock());
    dictionary.prepare(&guard, &suffix).unwrap().install();
    assert_eq!(dictionary.tail_len(), BLOCK + 3, "the install doesn't seal");
    let start = (BLOCK - 2) as u64;
    assert_installed(&dictionary, start, &new, "unsealed");
    let between = dictionary.held_view();
    dictionary.seal(&guard);
    assert_eq!(dictionary.tail_len(), 3, "a block sealed, three keys left");
    assert_installed(&dictionary, start, &new, "sealed");
    let all = dictionary.export_keys(0, dictionary.len());
    assert_eq!(all[..before.len()], before[..], "the earlier entries");
    for (i, key) in all.iter().enumerate() {
        assert_eq!(
            &between.key(i as u64),
            key,
            "entry {i} through the view taken unsealed"
        );
    }
}

/// T6. A prepared install dropped (its publication failed) gives its promise up and changes no
/// entry; the next preparation succeeds.
#[test]
fn a_prepared_install_dropped_leaves_the_dictionary_as_it_was() {
    let budget = Arc::new(Budget::unlimited());
    let new = texts("new", 7);
    let (dictionary, suffix) = dictionary_and_suffix(10, &new, &budget);
    let before = dictionary.export_keys(0, dictionary.len());
    let slot = slot();
    let guard = WriterGuard::hold(slot.lock());
    let prepared = dictionary.prepare(&guard, &suffix).unwrap();
    assert_eq!(dictionary.promised(), 7);
    drop(prepared);
    assert_eq!(dictionary.promised(), 0, "the promise given up");
    assert_eq!(
        dictionary.export_keys(0, dictionary.len()),
        before,
        "no entry added"
    );
    assert!(dictionary.lookup(literal(&new[0])).is_none());
    dictionary.prepare(&guard, &suffix).unwrap().install();
    assert_installed(&dictionary, 10, &new, "prepared again");
}

/// T7 and T8. A suffix that doesn't begin at the dictionary's length (something else added an
/// entry) or whose ids would reach bulk loads' provisional bit is refused before anything is
/// reserved.
#[test]
fn a_diverged_or_overlong_suffix_is_refused() {
    let budget = Arc::new(Budget::unlimited());
    let new = texts("new", 2);
    let (dictionary, suffix) = dictionary_and_suffix(10, &new, &budget);
    dictionary.intern(literal("a writer the slot didn't exclude"));
    let slot = slot();
    let guard = WriterGuard::hold(slot.lock());
    let refused = dictionary.prepare(&guard, &suffix).err();
    assert_eq!(refused, Some(PrepareError::Diverged { start: 10, len: 11 }));
    assert_eq!(dictionary.promised(), 0);
    let mut far = Suffix::at(PROVISIONAL - 1, Arc::clone(&budget));
    for key in keys_of(&new) {
        far.push(&key, key_hash(&key)).unwrap();
    }
    assert_eq!(
        dictionary.prepare(&guard, &far).err(),
        Some(PrepareError::Full)
    );
    let mut at_the_bit = Suffix::at(PROVISIONAL - 2, Arc::clone(&budget));
    for key in keys_of(&new) {
        at_the_bit.push(&key, key_hash(&key)).unwrap();
    }
    // Ids up to PROVISIONAL − 1: in range, so only the length differs.
    assert!(matches!(
        dictionary.prepare(&guard, &at_the_bit).err(),
        Some(PrepareError::Diverged { .. })
    ));
}

/// T10 and T14. A frozen view answers exactly the entries there were when it was taken, after
/// later pushes; many small freezes make many chunks, each entry still at its id.
#[test]
fn a_frozen_view_answers_the_entries_it_was_taken_with() {
    let budget = Arc::new(Budget::unlimited());
    let keys = keys_of(&texts("k", 1_000));
    let mut suffix = Suffix::at(50, Arc::clone(&budget));
    let mut views = Vec::new();
    for key in &keys {
        suffix.push(key, key_hash(key)).unwrap();
        views.push(suffix.freeze().unwrap());
    }
    for (n, view) in views.iter().enumerate() {
        assert_eq!(view.len(), n as u64 + 1);
        assert_eq!(
            view.key(50 + n as u64 + 1),
            None,
            "view {n}: no later entry"
        );
        assert_eq!(view.key(49), None, "view {n}: nothing below its start");
    }
    let last = views.last().unwrap();
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(last.key(50 + i as u64), Some(&key[..]), "entry {i}");
        assert_eq!(
            suffix.key(50 + i as u64),
            Some(&key[..]),
            "entry {i} in the suffix"
        );
    }
}

/// T11 (refusal). A push past the suffix's budget is refused with nothing changed: its length,
/// its keys, and the budget's charge as before.
#[test]
fn a_refused_push_changes_nothing() {
    let budget = Arc::new(Budget::new(4_096));
    let mut suffix = Suffix::at(0, Arc::clone(&budget));
    let short = keys_of(&texts("short", 3));
    for key in &short {
        suffix.push(key, key_hash(key)).unwrap();
    }
    let used = budget.used();
    let long = vec![b'x'; 8_192];
    let refused: Result<u64, BudgetExceeded> = suffix.push(&long, key_hash(&long));
    assert!(refused.is_err());
    assert_eq!(suffix.len(), 3);
    assert_eq!(budget.used(), used, "the refused push's charges released");
    for (i, key) in short.iter().enumerate() {
        assert_eq!(suffix.key(i as u64), Some(&key[..]));
    }
}

/// T11 (charges) and T13. What a suffix and its frozen views allocate is charged before it is
/// allocated, long and empty keys included, and released only when its last holder drops: the
/// suffix dropped first, then the view. Measured, alone.
#[test]
fn a_suffix_and_its_views_hold_only_what_they_charged() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let budget = Arc::new(Budget::unlimited());
    // The inputs, made before the measurement: only the suffix's own allocations are measured.
    let long = vec![b'y'; 1 << 20];
    let more: Vec<Vec<u8>> = (0..(BLOCK + 5))
        .map(|i| format!("key {i}").into_bytes())
        .collect();
    let ((), excess) = nrese_exec::measure::measure_within(&budget, || {
        let mut suffix = Suffix::at(0, Arc::clone(&budget));
        for key in [&b""[..], &b"a"[..], &long[..], &b"abcdefghi"[..]] {
            suffix.push(key, key_hash(key)).unwrap();
        }
        let first = suffix.freeze().unwrap();
        for key in &more {
            suffix.push(key, key_hash(key)).unwrap();
        }
        let second = suffix.freeze().unwrap();
        drop(suffix);
        assert!(
            budget.used() > 1 << 20,
            "the views keep their chunks charged"
        );
        drop(first);
        assert_eq!(second.key(2), Some(&long[..]));
        drop(second);
    });
    assert_eq!(excess, 0, "bytes held beyond the charges");
    assert_eq!(
        budget.used(),
        0,
        "every charge released with its last holder"
    );
}

/// T12. A reader running across an install and its seal reads only the old length or the new
/// one (the install is one step under the write guard), and once it reads the new one finds
/// every installed entry at its id.
#[test]
fn readers_during_an_install_and_its_seal_see_whole_entries() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let budget = Arc::new(Budget::unlimited());
    let new = texts("new", BLOCK + 100);
    let old = (BLOCK - 10) as u64;
    let (dictionary, suffix) = dictionary_and_suffix(BLOCK - 10, &new, &budget);
    let (dictionary, done, reads) = (
        Arc::new(dictionary),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
    );
    let reader = {
        let (dictionary, done, reads) = (
            Arc::clone(&dictionary),
            Arc::clone(&done),
            Arc::clone(&reads),
        );
        let new = new.clone();
        std::thread::spawn(move || {
            let (mut saw_old, mut saw_new) = (false, false);
            loop {
                // Read the flag first: a read after it saw the install's end.
                let last = done.load(Ordering::Acquire);
                let len = dictionary.len();
                assert!(len == old || len == old + new.len() as u64, "length {len}");
                if len == old {
                    saw_old = true;
                } else {
                    saw_new = true;
                    for (i, text) in new.iter().enumerate() {
                        let id = dictionary.lookup(literal(text));
                        assert_eq!(id.map(|id| id.payload()), Some(old + i as u64), "{text}");
                    }
                }
                reads.fetch_add(1, Ordering::Release);
                if last {
                    return (saw_old, saw_new);
                }
            }
        })
    };
    // The reader reads the old length before the install starts.
    while reads.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    let slot = slot();
    let guard = WriterGuard::hold(slot.lock());
    dictionary.prepare(&guard, &suffix).unwrap().install();
    dictionary.seal(&guard);
    done.store(true, Ordering::Release);
    assert_eq!(
        reader.join().unwrap(),
        (true, true),
        "read before and after"
    );
    assert_installed(&dictionary, old, &new, "after");
}

/// T15 (the seal condition, `>= BLOCK`). An ordinary push onto the tail an install left unsealed,
/// longer than a block, seals every full block at once and leaves fewer than a block; every
/// entry, installed and pushed, stays at its id.
#[test]
fn a_push_onto_an_unsealed_tail_seals_every_full_block() {
    let budget = Arc::new(Budget::unlimited());
    let new = texts("new", 2 * BLOCK + 7);
    let (dictionary, suffix) = dictionary_and_suffix(5, &new, &budget);
    let slot = slot();
    let guard = WriterGuard::hold(slot.lock());
    dictionary.prepare(&guard, &suffix).unwrap().install();
    assert_eq!(dictionary.tail_len(), 2 * BLOCK + 12, "unsealed");
    let pushed = dictionary.intern(literal("pushed after the install"));
    assert_eq!(pushed.payload(), (2 * BLOCK + 12) as u64);
    assert_eq!(dictionary.tail_len(), 13, "two blocks sealed, 13 keys left");
    assert_installed(&dictionary, 5, &new, "after the push sealed");
    assert_eq!(
        dictionary.lookup(literal("pushed after the install")),
        Some(pushed)
    );
    assert_eq!(
        dictionary.lookup(literal("old 4")).map(|id| id.payload()),
        Some(4)
    );
}
