//! The dictionary's lock order (the module docs of [`super`]): two schedules that
//! deadlocked when a thread took `inner` before an index's lock, or ran a vector search's
//! `accept` under the vector index's lock (SYN-22: B-103 and the vector callback).
//!
//! The schedules are set up from the locks' own states (a queued writer makes `try_read`
//! fail; a held guard makes `is_locked` true), with no hooks in the dictionary. A deadlock
//! shows as a call that doesn't return by [`DEADLINE`]; its thread is left blocked.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use nrese_rdf::{Literal, NamedNode, Term};
use parking_lot::RwLock;

use super::{Dictionary, unlimited};
use crate::term::text::TextIndex;
use crate::term::vectors::VectorIndex;
use crate::term::{TextQuery, VectorQuery, VectorStrategy};

/// How long a call that should return may take: far above any run, the answer check's bound.
const DEADLINE: Duration = Duration::from_secs(10);

/// How long a step waits for a lock state only the parent tree reaches (on the fix the
/// schedule goes on without it).
const STEP: Duration = Duration::from_secs(1);

/// `f` on a thread of its own; its result arrives on the receiver.
fn spawn<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let _ = sender.send(f());
    });
    receiver
}

/// Waits until `state` holds or `limit` passes; whether it held.
fn until(limit: Duration, state: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !state() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

fn text(dictionary: &Dictionary) -> &RwLock<TextIndex> {
    &dictionary.text
}

fn iri_text(dictionary: &Dictionary) -> &RwLock<TextIndex> {
    &dictionary.iri_text
}

fn vectors(dictionary: &Dictionary) -> &RwLock<VectorIndex> {
    &dictionary.vectors
}

fn words(text: &str) -> TextQuery {
    TextQuery {
        text: text.to_owned(),
        all_words: false,
        prefix: false,
        stem: None,
    }
}

fn vector_literal(values: &[f32]) -> Term {
    Literal::new_typed_literal(
        nrese_vector::lexical(values),
        NamedNode::new_unchecked(nrese_vector::DATATYPE),
    )
    .into()
}

/// One index's three parties: A ([`Dictionary::stats`]) reads `inner` and the index; B,
/// a search, extends the index by `entry` (the index's write lock, then `inner`'s read);
/// C interns a new term (`inner`'s write lock). With `inner` taken before the index (the
/// parent tree's `stats`), the schedule closes a cycle: A holds `inner` and waits for the
/// index, which B holds while it waits for `inner` behind C, queued behind A.
fn stats_extension_and_intern<T>(
    lock: for<'d> fn(&'d Dictionary) -> &'d RwLock<T>,
    entry: Term,
    search: fn(&Dictionary) -> usize,
) -> Result<(), String> {
    let dictionary = Arc::new(Dictionary::default());
    dictionary.intern(entry.as_ref());
    let fresh: Term = Literal::new_simple_literal("a term interned meanwhile").into();
    // 1. The test reads the index: B passes its coverage check and queues for the write lock.
    let gate = lock(&dictionary).read();
    let b = {
        let dictionary = Arc::clone(&dictionary);
        spawn(move || search(&dictionary))
    };
    if !until(DEADLINE, || lock(&dictionary).try_read().is_none()) {
        return Err("the search never asked for the index's write lock".into());
    }
    // 2. A: on the parent it holds `inner`'s read while it waits for the index (behind B).
    let a = {
        let dictionary = Arc::clone(&dictionary);
        spawn(move || dictionary.stats().terms)
    };
    until(STEP, || dictionary.inner.is_locked());
    // 3. C: queued for `inner`'s write lock where A holds its read; else done at once.
    let interned = Arc::new(AtomicBool::new(false));
    let c = {
        let dictionary = Arc::clone(&dictionary);
        let interned = Arc::clone(&interned);
        let fresh = fresh.clone();
        spawn(move || {
            let id = dictionary.intern(fresh.as_ref());
            interned.store(true, Ordering::SeqCst);
            id
        })
    };
    until(STEP, || {
        interned.load(Ordering::SeqCst) || dictionary.inner.try_read().is_none()
    });
    // 4. The index goes to B.
    drop(gate);
    let found = b
        .recv_timeout(DEADLINE)
        .map_err(|_| "the search (index extension) didn't return")?;
    let terms = a
        .recv_timeout(DEADLINE)
        .map_err(|_| "stats didn't return")?;
    let id = c
        .recv_timeout(DEADLINE)
        .map_err(|_| "intern didn't return")?;
    if found != 1 {
        return Err(format!("the search found {found} entries, not the one"));
    }
    if !(1..=2).contains(&terms) {
        return Err(format!("stats counted {terms} terms, not 1 or 2"));
    }
    if dictionary.lookup(fresh.as_ref()) != Some(id) {
        return Err("the interned term isn't found by its id".into());
    }
    Ok(())
}

/// `stats`, an index's extension and `intern` never wait on each other in a cycle, for
/// each index `stats` reads (B-103: `stats` took `inner`, then the indexes; extensions take
/// their index, then `inner`).
#[test]
fn stats_an_index_extension_and_intern_never_wait_on_each_other() {
    let failures: Vec<String> = [
        (
            "text",
            stats_extension_and_intern(
                text,
                Literal::new_simple_literal("quick fox").into(),
                |dictionary| dictionary.text_search(&words("fox")).len(),
            ),
        ),
        (
            "iri_text",
            stats_extension_and_intern(
                iri_text,
                NamedNode::new_unchecked("http://example.org/QuickFox").into(),
                |dictionary| dictionary.iri_search(&words("fox")).len(),
            ),
        ),
        (
            "vectors",
            stats_extension_and_intern(vectors, vector_literal(&[1.0, 2.0]), |dictionary| {
                let query = VectorQuery::new(vec![1.0, 2.0], 5);
                dictionary
                    .vector_search(&query, dictionary.len(), &|_| true, unlimited())
                    .unwrap()
                    .0
                    .len()
            }),
        ),
    ]
    .into_iter()
    .filter_map(|(index, outcome)| outcome.err().map(|error| format!("{index}: {error}")))
    .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A vector search's `accept` may intern a vector and search again: it runs under no lock of
/// the index, which the nested search extends. The outer search answers from the vectors it
/// started with (8), the nested one from those and the new one (9). Before, `accept` ran
/// under the index's read lock, and the nested search waited for its write lock forever.
#[test]
fn a_vector_search_s_accept_may_intern_and_search_again() {
    for strategy in [VectorStrategy::Exact, VectorStrategy::Approximate] {
        let dictionary = Arc::new(Dictionary::default());
        for i in 0..8 {
            dictionary.intern(vector_literal(&[i as f32, 1.0]).as_ref());
        }
        let outcome = {
            let dictionary = Arc::clone(&dictionary);
            spawn(move || {
                let mut query = VectorQuery::new(vec![1.0, 1.0], 3);
                query.strategy = strategy;
                let nested_space = AtomicUsize::new(0);
                let entered = AtomicBool::new(false);
                let known = dictionary.len();
                let (hits, report) = dictionary
                    .vector_search(
                        &query,
                        known,
                        &|_| {
                            if !entered.swap(true, Ordering::SeqCst) {
                                dictionary.intern(vector_literal(&[100.0, 100.0]).as_ref());
                                let (nested, report) = dictionary
                                    .vector_search(&query, dictionary.len(), &|_| true, unlimited())
                                    .unwrap();
                                assert_eq!(nested.len(), 3, "{strategy:?}: nested hits");
                                nested_space.store(report.space, Ordering::SeqCst);
                            }
                            true
                        },
                        unlimited(),
                    )
                    .unwrap();
                (hits.len(), report.space, nested_space.into_inner())
            })
        };
        let (hits, space, nested_space) = outcome.recv_timeout(DEADLINE).unwrap_or_else(|_| {
            panic!("{strategy:?}: the search with a nested search didn't return")
        });
        assert_eq!(
            (hits, space, nested_space),
            (3, 8, 9),
            "{strategy:?}: hits, the outer space and the nested space"
        );
    }
}
