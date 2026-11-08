//! The counted ops. Each draws its input from the seed and prepares it outside the window
//! (nothing is interned, synced or maintained in the background inside it), runs inside the
//! window over the committed snapshot taken after the load, and returns its rows. What it
//! produces is kept until the window has closed (dropping results isn't the op's work) and
//! then checked against what the store was built from: a wrong answer fails the run.
//!
//! | Op | Rows | Through |
//! |---|---|---|
//! | `decode` | 1,000,000 ids (or `--rows`), a seeded draw | `Snapshot::decode` |
//! | `lookup` | 200,000 terms, half present, half absent | `Snapshot::lookup` |
//! | `with_views` | 1,000 batches of 1,024 ids (a row per id), each view's fields copied into a buffer made before the window | `Snapshot::with_views` |
//! | `decode_quad` | the 1,000,000 asserted quads, in a scan's order | `Snapshot::decode_quad` |
//! | `insert_remove` | 100,000 inserts and 100,000 removes, each 10 % no-ops and 10 % cancellations | `Transaction::insert_encoded`, `remove_encoded` |
//! | `takeover` | 10,000 inferred quads asserted, then the commit | `Transaction::insert_encoded`, `commit` |
//! | `pending_snapshot` | the 30,000 pending entries of one call over 10,000 asserted inserts (1,000 of them inferred quads), 10,000 deletes and 10,000 inferred inserts | `Transaction::pending_snapshot` |
//! | `pending_over` | the 21,000 entries one call reads over a view without 1,000 inferred quads: the same changes' asserted inserts and deletes, and 1,000 hidden | `Transaction::pending_over` |
//! | `noop` | one: the window around an empty body, at the op's threads | nothing |
//!
//! At 1 thread a read op runs on the main thread. At more, it splits its rows over `threads`
//! threads, each reading its own clone of the snapshot into its own part of the results; the
//! threads start before the window opens and end after it closes, and meet barriers inside it
//! (counted by `noop` too). The reopened store's commit (`takeover`) appends to its WAL,
//! unsynced: `instructions:u` counts only its user-space part.

use std::collections::{HashMap, HashSet};
use std::sync::Barrier;

use nrese_engine::{
    EncodedQuad, InferredSubset, QuadPattern, ReadModel, Snapshot, TermId, TermView, Transaction,
};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{BaseDirection, GraphName, Quad, Term};

use crate::store::{self, INFERRED, QUADS, Rng, Store, TERMS};
use crate::{Op, Window};

pub(crate) fn run(
    op: Op,
    threads: usize,
    rows: Option<usize>,
    store: &Store,
    seed: u64,
    window: &mut Window,
) -> Result<u64, String> {
    let mut rng = Rng::new(seed, 2 + op as u64);
    let snapshot = &store.snapshot;
    let rows = match op {
        Op::Decode => {
            let drawn: Vec<usize> = (0..rows.unwrap_or(1_000_000))
                .map(|_| rng.below(TERMS))
                .collect();
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
            // A view borrows the dictionary only while `with_views` runs: each reader copies
            // every field of each view it gets into its batch's buffer, made before the window
            // as large as the expected views need (the copy is counted with the op). After the
            // window each buffer is compared with the expected terms' encoding.
            let expected: Vec<Vec<u8>> = drawn
                .iter()
                .map(|batch| {
                    let mut encoded = Vec::new();
                    for &i in batch {
                        encode_term(&store.terms[i], &mut encoded);
                    }
                    encoded
                })
                .collect();
            let mut seen: Vec<Vec<u8>> = expected
                .iter()
                .map(|encoded| Vec::with_capacity(encoded.len()))
                .collect();
            #[cfg(feature = "inject")]
            let first_part = batches.as_ptr() as usize;
            parallel(
                threads,
                snapshot,
                &batches,
                &mut seen,
                window,
                |snapshot, part, out| {
                    #[cfg(feature = "inject")]
                    let mut injected = part.as_ptr() as usize != first_part;
                    for (seen, batch) in out.iter_mut().zip(part) {
                        snapshot.with_views(|view| {
                            for &id in batch {
                                let view = view(id);
                                #[cfg(feature = "inject")]
                                let view = view.map(|view| inject(view, &mut injected));
                                encode_view(view, seen);
                            }
                        });
                    }
                },
            )?;
            let right = expected
                .iter()
                .zip(&seen)
                .filter(|(want, seen)| want == seen);
            expect("viewed right (batches)", right.count(), batches.len())?;
            1_000 * 1_024
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
        Op::Noop => {
            let parts = vec![(); threads];
            parallel(
                threads,
                snapshot,
                &parts,
                &mut vec![(); threads],
                window,
                |_, _, _| {},
            )?;
            1
        }
    };
    Ok(rows as u64)
}

/// `read` over `items` into `out`, both split into one part per thread. The threads start
/// before the window opens and end after it closes, so neither is counted. A reader that
/// panics still meets the barriers, and the run fails with its message.
fn parallel<T: Sync, O: Send>(
    threads: usize,
    snapshot: &Snapshot,
    items: &[T],
    out: &mut [O],
    window: &mut Window,
    read: impl Fn(&Snapshot, &[T], &mut [O]) + Sync,
) -> Result<(), String> {
    if threads == 1 {
        // On the main thread, nothing to meet: the window holds the op alone.
        return window.count(|| read(snapshot, items, out));
    }
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
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        read(&snapshot, part, out)
                    }));
                    done.wait();
                    end.wait();
                    outcome.map_err(|panic| {
                        let message = panic
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| panic.downcast_ref::<String>().map(String::as_str));
                        format!("a reader panicked: {}", message.unwrap_or("(no message)"))
                    })
                })
            })
            .collect();
        let counted = window.count(|| {
            start.wait();
            done.wait();
        });
        end.wait();
        let read = readers.into_iter().try_for_each(|reader| {
            reader
                .join()
                .unwrap_or_else(|_| Err("a reader panicked at a barrier".into()))
        });
        counted.and(read)
    })
}

/// Test-only (`--features inject`, `tests/inject.rs`): the first IRI the first reader views,
/// replaced by another IRI of the same length, a wrong answer the run must refuse.
#[cfg(feature = "inject")]
fn inject<'v>(view: TermView<'v>, injected: &mut bool) -> TermView<'v> {
    match view {
        TermView::Iri(iri) if !*injected => {
            *injected = true;
            let mut wrong = iri.to_owned();
            let last = if wrong.ends_with('x') { 'y' } else { 'x' };
            wrong.pop();
            wrong.push(last);
            TermView::Iri(Box::leak(wrong.into_boxed_str()))
        }
        view => view,
    }
}

/// Every field of a view, appended to `out`: its kind, then each text with its length (value,
/// language tag and base direction, or datatype). A triple term's view has no parts (the
/// `decode` op checks them); no view is kind 0. [`encode_term`] writes a term the same way.
fn encode_view(view: Option<TermView<'_>>, out: &mut Vec<u8>) {
    match view {
        None => out.push(0),
        Some(TermView::Iri(iri)) => texts(out, 1, &[iri]),
        Some(TermView::String(value)) => texts(out, 2, &[value]),
        Some(TermView::LangString {
            value,
            language,
            direction,
        }) => {
            texts(out, 3, &[value, language]);
            out.push(direction_code(direction));
        }
        Some(TermView::Typed { value, datatype }) => texts(out, 4, &[value, datatype]),
        Some(TermView::Triple) => out.push(5),
        Some(TermView::BlankNode(id)) => texts(out, 6, &[id]),
    }
}

/// What [`encode_view`] writes for the view of `term`.
fn encode_term(term: &Term, out: &mut Vec<u8>) {
    match term {
        Term::NamedNode(iri) => texts(out, 1, &[iri.as_str()]),
        Term::Literal(literal) => match literal.language() {
            Some(language) => {
                texts(out, 3, &[literal.value(), language]);
                out.push(direction_code(literal.direction()));
            }
            None if literal.datatype() == xsd::STRING => texts(out, 2, &[literal.value()]),
            None => texts(out, 4, &[literal.value(), literal.datatype().as_str()]),
        },
        Term::Triple(_) => out.push(5),
        Term::BlankNode(id) => texts(out, 6, &[id.as_str()]),
    }
}

fn texts(out: &mut Vec<u8>, kind: u8, texts: &[&str]) {
    out.push(kind);
    for text in texts {
        out.extend_from_slice(&(text.len() as u32).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
    }
}

fn direction_code(direction: Option<BaseDirection>) -> u8 {
    match direction {
        None => 0,
        Some(BaseDirection::Ltr) => 1,
        Some(BaseDirection::Rtl) => 2,
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
