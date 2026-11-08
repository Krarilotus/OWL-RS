//! The counted ops. Each draws its input from the seed and prepares it outside the window
//! (nothing is interned, synced or maintained in the background inside it), runs inside the
//! window over the committed snapshot taken after the load, checks its answer afterwards and
//! returns its rows.
//!
//! | Op | Rows | Through |
//! |---|---|---|
//! | `decode` | 1,000,000 ids, a seeded draw | `Snapshot::decode` |
//! | `lookup` | 200,000 terms, half present, half absent | `Snapshot::lookup` |
//! | `with_views` | 1,000 batches of 1,024 ids (a row per id) | `Snapshot::with_views` |
//! | `decode_quad` | the 1,000,000 asserted quads, in a scan's order | `Snapshot::decode_quad` |
//! | `insert_remove` | 100,000 inserts and 100,000 removes, each 10 % no-ops and 10 % cancellations | `Transaction::insert_encoded`, `remove_encoded` |
//! | `takeover` | 10,000 inferred quads asserted, then the commit | `Transaction::insert_encoded`, `commit` |
//! | `pending_snapshot` | the 30,000 pending entries of one call over 10,000 asserted inserts (1,000 of them inferred quads), 10,000 deletes and 10,000 inferred inserts | `Transaction::pending_snapshot` |
//! | `pending_over` | the 21,000 entries one call reads over a view without 1,000 inferred quads: the same changes' asserted inserts and deletes, and 1,000 hidden | `Transaction::pending_over` |
//!
//! The read ops split their rows over `threads` threads, each reading its own clone of the
//! snapshot; the threads start before the window opens and end after it closes. The reopened
//! store's commit (`takeover`) appends to its WAL, unsynced: `instructions:u` counts only its
//! user-space part.

use std::collections::HashSet;
use std::hint::black_box;
use std::sync::Barrier;

use nrese_engine::{
    EncodedQuad, InferredSubset, QuadPattern, ReadModel, Snapshot, TermId, Transaction,
};
use nrese_rdf::Term;

use crate::store::{self, INFERRED, QUADS, Rng, Store, TERMS};
use crate::{Op, Window};

pub(crate) fn run(
    op: Op,
    threads: usize,
    store: &Store,
    seed: u64,
    window: &mut Window,
) -> Result<u64, String> {
    let mut rng = Rng::new(seed, 2 + op as u64);
    let snapshot = &store.snapshot;
    let rows = match op {
        Op::Decode => {
            let ids: Vec<TermId> = (0..1_000_000)
                .map(|_| store.ids[rng.below(TERMS)])
                .collect();
            let decoded = parallel(threads, snapshot, &ids, window, |snapshot, ids| {
                ids.iter()
                    .filter(|&&id| black_box(snapshot.decode(id)).is_some())
                    .count()
            })?;
            expect("decoded", decoded, ids.len())?
        }
        Op::Lookup => {
            let terms: Vec<Term> = (0..200_000)
                .map(|i| match i % 2 {
                    0 => store.terms[rng.below(TERMS)].clone(),
                    _ => {
                        let kind = rng.below(store::DICTIONARY_TERMS);
                        store::term(kind, true, &mut rng)
                    }
                })
                .collect();
            let found = parallel(threads, snapshot, &terms, window, |snapshot, terms| {
                terms
                    .iter()
                    .filter(|term| black_box(snapshot.lookup(term.as_ref())).is_some())
                    .count()
            })?;
            expect("found", found, terms.len() / 2)?;
            terms.len()
        }
        Op::WithViews => {
            let batches: Vec<Vec<TermId>> = (0..1_000)
                .map(|_| (0..1_024).map(|_| store.ids[rng.below(TERMS)]).collect())
                .collect();
            let viewed = parallel(threads, snapshot, &batches, window, |snapshot, batches| {
                batches
                    .iter()
                    .map(|batch| {
                        snapshot.with_views(|view| {
                            batch
                                .iter()
                                .filter(|&&id| black_box(view(id)).is_some())
                                .count()
                        })
                    })
                    .sum()
            })?;
            expect("viewed", viewed, 1_000 * 1_024)?
        }
        Op::DecodeQuad => {
            let quads: Vec<EncodedQuad> = snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
                .collect();
            expect("scanned", quads.len(), QUADS)?;
            let decoded = parallel(threads, snapshot, &quads, window, |snapshot, quads| {
                quads
                    .iter()
                    .filter(|&&quad| black_box(snapshot.decode_quad(quad)).is_some())
                    .count()
            })?;
            expect("decoded", decoded, QUADS)?
        }
        Op::InsertRemove => {
            let base: Vec<EncodedQuad> = rng
                .distinct(90_000, QUADS)
                .into_iter()
                .map(|i| store.asserted[i])
                .collect();
            let new = store.fresh(90_000, false, &mut HashSet::new(), &mut rng);
            let (deleted, kept) = base.split_at(80_000);
            let (inserted, absent) = new.split_at(80_000);
            // (insert?, quad): removes of asserted quads and absent ones (no-ops); inserts of new
            // quads, of quads still asserted (no-ops) and of quads removed above (cancellations);
            // removes of quads inserted here (cancellations).
            let steps: Vec<(bool, EncodedQuad)> = [
                (false, deleted),
                (false, absent),
                (true, inserted),
                (true, kept),
                (true, &deleted[..10_000]),
                (false, &inserted[..10_000]),
            ]
            .into_iter()
            .flat_map(|(insert, quads)| quads.iter().map(move |&quad| (insert, quad)))
            .collect();
            let mut transaction = store.engine.transaction();
            let changed = window.count(|| {
                steps
                    .iter()
                    .filter(|&&(insert, quad)| match insert {
                        true => transaction.insert_encoded(quad),
                        false => transaction.remove_encoded(quad),
                    })
                    .count()
            })?;
            drop(transaction);
            expect("changed", changed, 180_000)?;
            steps.len()
        }
        Op::Takeover => {
            let quads: Vec<EncodedQuad> = rng
                .distinct(10_000, INFERRED)
                .into_iter()
                .map(|i| store.inferred[i])
                .collect();
            let mut transaction = store.engine.transaction();
            let (taken, summary) = window.count(|| {
                let taken = quads
                    .iter()
                    .filter(|&&quad| transaction.insert_encoded(quad))
                    .count();
                (taken, transaction.commit())
            })?;
            let summary = summary.map_err(|error| format!("commit: {error}"))?;
            let counts = [taken as u64, summary.inserted, summary.inferred_deleted];
            if counts != [quads.len() as u64; 3] {
                return Err(format!(
                    "takeover: asserted, inserted, inferred deleted {counts:?}"
                ));
            }
            quads.len()
        }
        Op::PendingSnapshot => {
            let pending = pending_changes(store, seed)?;
            let made = window.count(|| pending.transaction.pending_snapshot())?;
            counts(
                "pending snapshot",
                &made,
                [QUADS, INFERRED + 10_000 - 1_000],
            )?;
            // Every pending entry: the asserted inserts and deletes, the inferred inserts.
            30_000
        }
        Op::PendingOver => {
            let Pending {
                transaction,
                without,
                hidden,
            } = pending_changes(store, seed)?;
            let view = transaction
                .base()
                .with_inferred_subset(InferredSubset::Without(without));
            let over = window.count(|| transaction.pending_over(&view, &hidden))?;
            // Gone: the view's 1,000, the 1,000 asserted now and the 1,000 hidden.
            counts("pending over", &over, [QUADS, INFERRED - 3_000])?;
            // What it reads: the asserted inserts and deletes, and the hidden statements (it
            // leaves the pending inferred inserts out).
            21_000
        }
    };
    Ok(rows as u64)
}

/// `read` over `items`, split into one part per thread. The threads start before the window
/// opens and end after it closes, so neither is counted.
fn parallel<T: Sync>(
    threads: usize,
    snapshot: &Snapshot,
    items: &[T],
    window: &mut Window,
    read: impl Fn(&Snapshot, &[T]) -> usize + Sync,
) -> Result<usize, String> {
    let parts: Vec<&[T]> = items.chunks(items.len().div_ceil(threads)).collect();
    let [start, done, end] = [(); 3].map(|()| Barrier::new(parts.len() + 1));
    std::thread::scope(|scope| {
        let readers: Vec<_> = parts
            .iter()
            .map(|&part| {
                let snapshot = snapshot.clone();
                let (start, done, end, read) = (&start, &done, &end, &read);
                scope.spawn(move || {
                    start.wait();
                    let rows = read(&snapshot, part);
                    done.wait();
                    end.wait();
                    rows
                })
            })
            .collect();
        let counted = window.count(|| {
            start.wait();
            done.wait();
        });
        end.wait();
        let read = readers
            .into_iter()
            .map(|reader| reader.join().expect("a reader panicked"))
            .sum();
        counted.map(|()| read)
    })
}

/// A transaction's pending changes, the same for both pending ops: 9,000 new asserted quads
/// and 1,000 inferred ones asserted, 10,000 asserted quads removed and 10,000 new inferred
/// statements. Besides, two more sets of 1,000 inferred statements, for `pending_over`'s
/// view to leave out and to hide. All drawn from the seed.
struct Pending<'s> {
    transaction: Transaction<'s>,
    without: Vec<EncodedQuad>,
    hidden: Vec<EncodedQuad>,
}

fn pending_changes(store: &Store, seed: u64) -> Result<Pending<'_>, String> {
    let mut rng = Rng::new(seed, 100);
    let mut taken = HashSet::new();
    let asserted = store.fresh(9_000, false, &mut taken, &mut rng);
    let inferred = store.fresh(10_000, true, &mut taken, &mut rng);
    let deleted = rng.distinct(10_000, QUADS);
    let picked: Vec<EncodedQuad> = rng
        .distinct(3_000, INFERRED)
        .into_iter()
        .map(|i| store.inferred[i])
        .collect();
    let mut transaction = store.engine.transaction();
    let mut changed = 0;
    for &quad in asserted.iter().chain(&picked[..1_000]) {
        changed += usize::from(transaction.insert_encoded(quad));
    }
    for i in deleted {
        changed += usize::from(transaction.remove_encoded(store.asserted[i]));
    }
    for quad in inferred {
        changed += usize::from(transaction.insert_inferred(quad.into()));
    }
    expect("pending changes", changed, 30_000)?;
    let mut without = picked[1_000..2_000].to_vec();
    without.sort_unstable();
    Ok(Pending {
        transaction,
        without,
        hidden: picked[2_000..].to_vec(),
    })
}

fn expect(what: &str, got: usize, want: usize) -> Result<usize, String> {
    match got == want {
        true => Ok(got),
        false => Err(format!("{what}: {got}, not {want}")),
    }
}

fn counts(what: &str, snapshot: &Snapshot, want: [usize; 2]) -> Result<(), String> {
    let got = [ReadModel::Asserted, ReadModel::Inferred].map(|model| snapshot.len_in(model));
    match got == want.map(|n| n as u64) {
        true => Ok(()),
        false => Err(format!(
            "{what}: {got:?} asserted and inferred, not {want:?}"
        )),
    }
}
