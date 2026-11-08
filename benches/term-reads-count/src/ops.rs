//! The counted ops. Each draws its input from the seed and prepares it outside the window
//! (nothing is interned, synced or maintained in the background inside it), runs inside the
//! window over the committed snapshot taken after the load, and returns its rows. What it
//! produces is kept until the window has closed (dropping results isn't the op's work) and
//! then checked against what the store was built from: a wrong answer fails the run.
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
//! snapshot into its own part of the results; the threads start before the window opens and
//! end after it closes. The reopened store's commit (`takeover`) appends to its WAL, unsynced:
//! `instructions:u` counts only its user-space part.

use std::collections::{HashMap, HashSet};
use std::sync::Barrier;

use nrese_engine::{
    EncodedQuad, InferredSubset, QuadPattern, ReadModel, Snapshot, TermId, TermView, Transaction,
};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Quad, Term};

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
            let drawn: Vec<usize> = (0..1_000_000).map(|_| rng.below(TERMS)).collect();
            let ids: Vec<TermId> = drawn.iter().map(|&i| store.ids[i]).collect();
            let mut decoded: Vec<Option<Term>> =
                std::iter::repeat_with(|| None).take(ids.len()).collect();
            parallel(
                threads,
                snapshot,
                &ids,
                &mut decoded,
                window,
                |snapshot, ids, out| {
                    for (slot, &id) in out.iter_mut().zip(ids) {
                        *slot = snapshot.decode(id);
                    }
                },
            )?;
            let right = drawn
                .iter()
                .zip(&decoded)
                .filter(|&(&i, term)| term.as_ref() == Some(&store.terms[i]));
            expect("decoded right", right.count(), ids.len())?
        }
        Op::Lookup => {
            // Even rows a stored term and its id, odd ones a term of the dictionary's kinds the
            // store lacks.
            let (terms, want): (Vec<Term>, Vec<Option<TermId>>) = (0..200_000)
                .map(|i| match i % 2 {
                    0 => {
                        let n = rng.below(TERMS);
                        (store.terms[n].clone(), Some(store.ids[n]))
                    }
                    _ => {
                        let kind = rng.below(store::DICTIONARY_TERMS);
                        (store::term(kind, true, &mut rng), None)
                    }
                })
                .unzip();
            let mut found = vec![None; terms.len()];
            parallel(
                threads,
                snapshot,
                &terms,
                &mut found,
                window,
                |snapshot, terms, out| {
                    for (slot, term) in out.iter_mut().zip(terms) {
                        *slot = snapshot.lookup(term.as_ref());
                    }
                },
            )?;
            let right = want
                .iter()
                .zip(&found)
                .filter(|(want, found)| want == found);
            expect("looked up right", right.count(), terms.len())?
        }
        Op::WithViews => {
            let drawn: Vec<Vec<usize>> = (0..1_000)
                .map(|_| (0..1_024).map(|_| rng.below(TERMS)).collect())
                .collect();
            let batches: Vec<Vec<TermId>> = drawn
                .iter()
                .map(|batch| batch.iter().map(|&i| store.ids[i]).collect())
                .collect();
            let mut seen: Vec<Vec<usize>> =
                batches.iter().map(|batch| vec![0; batch.len()]).collect();
            parallel(
                threads,
                snapshot,
                &batches,
                &mut seen,
                window,
                |snapshot, batches, out| {
                    for (seen, batch) in out.iter_mut().zip(batches) {
                        snapshot.with_views(|view| {
                            for (slot, &id) in seen.iter_mut().zip(batch) {
                                *slot = view(id).map_or(0, view_key);
                            }
                        });
                    }
                },
            )?;
            // A view borrows the dictionary only while `with_views` runs, and copying it out
            // inside the window would add work the op doesn't do. So the same ids are viewed
            // again after the window, each view compared field by field with its term and
            // with what the counted pass kept of it.
            let right = snapshot.with_views(|view| {
                drawn
                    .iter()
                    .flatten()
                    .zip(batches.iter().flatten())
                    .zip(seen.iter().flatten())
                    .filter(|&((&i, &id), &seen)| {
                        view(id).is_some_and(|view| {
                            view_key(view) == seen && same_view(view, &store.terms[i])
                        })
                    })
                    .count()
            });
            expect("viewed right", right, 1_000 * 1_024)?
        }
        Op::DecodeQuad => {
            let quads: Vec<EncodedQuad> = snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
                .collect();
            expect("scanned", quads.len(), QUADS)?;
            let mut decoded: Vec<Option<Quad>> =
                std::iter::repeat_with(|| None).take(quads.len()).collect();
            parallel(
                threads,
                snapshot,
                &quads,
                &mut decoded,
                window,
                |snapshot, quads, out| {
                    for (slot, &quad) in out.iter_mut().zip(quads) {
                        *slot = snapshot.decode_quad(quad);
                    }
                },
            )?;
            let terms: HashMap<TermId, &Term> =
                store.ids.iter().copied().zip(&store.terms).collect();
            let right = quads.iter().zip(&decoded).filter(|(quad, decoded)| {
                decoded
                    .as_ref()
                    .is_some_and(|decoded| same_quad(&terms, quad, decoded))
            });
            expect("decoded right", right.count(), QUADS)?
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
            expect("changed", changed, 180_000)?;
            let asserted = |quad: &EncodedQuad| transaction.contains_in(ReadModel::Asserted, quad);
            if transaction.pending() != (70_000, 70_000) {
                return Err(format!(
                    "insert_remove: pending {:?}, not 70,000 each",
                    transaction.pending()
                ));
            }
            all("inserted", &inserted[10_000..], true, asserted)?;
            all(
                "inserted, then removed",
                &inserted[..10_000],
                false,
                asserted,
            )?;
            all("absent", absent, false, asserted)?;
            all("removed", &deleted[10_000..], false, asserted)?;
            all("removed, then inserted", &deleted[..10_000], true, asserted)?;
            all("kept", kept, true, asserted)?;
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
            let after = store.engine.snapshot();
            all("taken over, asserted", &quads, true, |quad| {
                after.contains_in(ReadModel::Asserted, quad)
            })?;
            all("taken over, inferred", &quads, false, |quad| {
                after.contains_in(ReadModel::Inferred, quad)
            })?;
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
            let asserted = |quad: &EncodedQuad| made.contains_in(ReadModel::Asserted, quad);
            let inferred = |quad: &EncodedQuad| made.contains_in(ReadModel::Inferred, quad);
            all("asserted", &pending.asserted, true, asserted)?;
            all("deleted", &pending.deleted, false, asserted)?;
            all("inferred", &pending.inferred, true, inferred)?;
            all("taken over", &pending.asserted[9_000..], false, inferred)?;
            // Every pending entry: the asserted inserts and deletes, the inferred inserts.
            30_000
        }
        Op::PendingOver => {
            let pending = pending_changes(store, seed)?;
            let view = pending
                .transaction
                .base()
                .with_inferred_subset(InferredSubset::Without(pending.without.clone()));
            let over = window.count(|| pending.transaction.pending_over(&view, &pending.hidden))?;
            // Gone: the view's 1,000, the 1,000 asserted now and the 1,000 hidden.
            counts("pending over", &over, [QUADS, INFERRED - 3_000])?;
            let asserted = |quad: &EncodedQuad| over.contains_in(ReadModel::Asserted, quad);
            let inferred = |quad: &EncodedQuad| over.contains_in(ReadModel::Inferred, quad);
            all("asserted", &pending.asserted, true, asserted)?;
            all("deleted", &pending.deleted, false, asserted)?;
            all("pending inferred", &pending.inferred, false, inferred)?;
            all("taken over", &pending.asserted[9_000..], false, inferred)?;
            all("left out of the view", &pending.without, false, inferred)?;
            all("hidden", &pending.hidden, false, inferred)?;
            // What it reads: the asserted inserts and deletes, and the hidden statements (it
            // leaves the pending inferred inserts out).
            21_000
        }
    };
    Ok(rows as u64)
}

/// `read` over `items` into `out`, both split into one part per thread. The threads start
/// before the window opens and end after it closes, so neither is counted.
fn parallel<T: Sync, O: Send>(
    threads: usize,
    snapshot: &Snapshot,
    items: &[T],
    out: &mut [O],
    window: &mut Window,
    read: impl Fn(&Snapshot, &[T], &mut [O]) + Sync,
) -> Result<(), String> {
    let size = items.len().div_ceil(threads);
    let parts: Vec<(&[T], &mut [O])> = items.chunks(size).zip(out.chunks_mut(size)).collect();
    let [start, done, end] = [(); 3].map(|()| Barrier::new(parts.len() + 1));
    std::thread::scope(|scope| {
        let readers: Vec<_> = parts
            .into_iter()
            .map(|(part, out)| {
                let snapshot = snapshot.clone();
                let (start, done, end, read) = (&start, &done, &end, &read);
                scope.spawn(move || {
                    start.wait();
                    read(&snapshot, part, out);
                    done.wait();
                    end.wait();
                })
            })
            .collect();
        let counted = window.count(|| {
            start.wait();
            done.wait();
        });
        end.wait();
        for reader in readers {
            reader.join().expect("a reader panicked");
        }
        counted
    })
}

/// A view's kind and text length, as one number: what the counted pass keeps of each view,
/// for the check's second pass to match.
fn view_key(view: TermView<'_>) -> usize {
    let kind = match view {
        TermView::Iri(_) => 1,
        TermView::String(_) => 2,
        TermView::LangString { .. } => 3,
        TermView::Typed { .. } => 4,
        TermView::Triple => 5,
        TermView::BlankNode(_) => 6,
    };
    view.str().map_or(0, str::len) * 8 + kind
}

/// Whether `view` shows `term`, every field the view has: text, language tag, base direction
/// and datatype. A triple term's view has no parts (the `decode` op checks them).
fn same_view(view: TermView<'_>, term: &Term) -> bool {
    let literal = match (view, term) {
        (TermView::Iri(iri), Term::NamedNode(term)) => return iri == term.as_str(),
        (TermView::BlankNode(id), Term::BlankNode(term)) => return id == term.as_str(),
        (TermView::Triple, Term::Triple(_)) => return true,
        (_, Term::Literal(literal)) => literal,
        _ => return false,
    };
    let plain = literal.datatype() == xsd::STRING;
    match view {
        TermView::String(value) => {
            plain && literal.language().is_none() && value == literal.value()
        }
        TermView::LangString {
            value,
            language,
            direction,
        } => {
            literal.language() == Some(language)
                && literal.direction() == direction
                && value == literal.value()
        }
        TermView::Typed { value, datatype } => {
            literal.language().is_none()
                && !plain
                && datatype == literal.datatype().as_str()
                && value == literal.value()
        }
        _ => false,
    }
}

/// Whether `decoded` is `quad`'s terms, by the terms the store was built from.
fn same_quad(terms: &HashMap<TermId, &Term>, quad: &EncodedQuad, decoded: &Quad) -> bool {
    let is = |id: TermId, term: Term| terms.get(&id) == Some(&&term);
    let graph = match &decoded.graph_name {
        GraphName::DefaultGraph => quad.graph == TermId::DEFAULT_GRAPH,
        GraphName::NamedNode(graph) => is(quad.graph, Term::NamedNode(graph.clone())),
        GraphName::BlankNode(_) => false,
    };
    graph
        && is(quad.subject, decoded.subject.clone().into())
        && is(quad.predicate, Term::NamedNode(decoded.predicate.clone()))
        && is(quad.object, decoded.object.clone())
}

/// A transaction's pending changes, the same for both pending ops: 9,000 new asserted quads
/// and 1,000 inferred ones asserted (`asserted`, those last), 10,000 asserted quads removed
/// and 10,000 new inferred statements. Besides, two more sets of 1,000 inferred statements,
/// for `pending_over`'s view to leave out and to hide. All drawn from the seed.
struct Pending<'s> {
    transaction: Transaction<'s>,
    asserted: Vec<EncodedQuad>,
    deleted: Vec<EncodedQuad>,
    inferred: Vec<EncodedQuad>,
    without: Vec<EncodedQuad>,
    hidden: Vec<EncodedQuad>,
}

fn pending_changes(store: &Store, seed: u64) -> Result<Pending<'_>, String> {
    let mut rng = Rng::new(seed, 100);
    let mut taken = HashSet::new();
    let mut asserted = store.fresh(9_000, false, &mut taken, &mut rng);
    let inferred = store.fresh(10_000, true, &mut taken, &mut rng);
    let deleted: Vec<EncodedQuad> = rng
        .distinct(10_000, QUADS)
        .into_iter()
        .map(|i| store.asserted[i])
        .collect();
    let picked: Vec<EncodedQuad> = rng
        .distinct(3_000, INFERRED)
        .into_iter()
        .map(|i| store.inferred[i])
        .collect();
    asserted.extend_from_slice(&picked[..1_000]);
    let mut transaction = store.engine.transaction();
    let mut changed = 0;
    for &quad in &asserted {
        changed += usize::from(transaction.insert_encoded(quad));
    }
    for &quad in &deleted {
        changed += usize::from(transaction.remove_encoded(quad));
    }
    for &quad in &inferred {
        changed += usize::from(transaction.insert_inferred(quad.into()));
    }
    expect("pending changes", changed, 30_000)?;
    let mut without = picked[1_000..2_000].to_vec();
    without.sort_unstable();
    Ok(Pending {
        transaction,
        asserted,
        deleted,
        inferred,
        without,
        hidden: picked[2_000..].to_vec(),
    })
}

/// Each of `quads` is (`want`) or isn't in what `contains` reads.
fn all(
    what: &str,
    quads: &[EncodedQuad],
    want: bool,
    contains: impl Fn(&EncodedQuad) -> bool,
) -> Result<(), String> {
    match quads.iter().filter(|quad| contains(quad) != want).count() {
        0 => Ok(()),
        wrong => Err(format!("{what}: {wrong} of {} wrong", quads.len())),
    }
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
