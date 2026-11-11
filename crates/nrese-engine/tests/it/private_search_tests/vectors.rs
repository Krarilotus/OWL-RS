//! Vectors: shared and own vectors in one exact top-k, and the global entry point's budget.

use super::*;

/// A vector search's answer decoded: each hit's lexical form and distance, and the
/// report's space and scan.
type VectorAnswer = Result<(Vec<(String, f32)>, usize, usize), String>;

fn vector_answer(
    snapshot: &Snapshot,
    query: &nrese_engine::VectorQuery,
    accept: &(dyn Fn(TermId) -> bool + Sync),
) -> VectorAnswer {
    let budget = Arc::new(nrese_exec::Budget::unlimited());
    let (hits, report) = snapshot
        .vector_search_in(
            ReadModel::Materialised,
            &TextScope::EVERY,
            query,
            accept,
            &budget,
        )
        .map_err(|e| format!("{e:?}"))?;
    let hits = hits
        .iter()
        .map(|&(id, distance)| match snapshot.decode(id) {
            Some(Term::Literal(l)) => (l.value().to_owned(), distance),
            other => panic!("{other:?}"),
        })
        .collect();
    Ok((hits, report.space, report.scanned))
}

/// Attack item 10: vectors. The base holds `[1, 0]` and `[0.6, 0.8]`; the private delta
/// adds `[0, 1]` (its own, at the same distance from `[1, 1]` as the base's `[1, 0]`), a
/// vector of a dimension no base vector has, and removes `[0.6, 0.8]`. Each search, exact,
/// equals an engine holding the same statements: hits by distance then id across shared
/// and own vectors (k on whole tie groups), `accept` refusing the own one, a dimension only
/// the delta has, and the report's space and scan: exactly shared plus own, which the
/// SPARQL caller widens k by (SRCH 9 Nov).
#[test]
fn a_private_snapshot_searches_its_vectors_and_the_bases() {
    let base = |engine: &Engine| {
        let mut tx = engine.transaction();
        tx.insert(label("a", "[1, 0]", None).as_ref());
        tx.insert(label("c", "[0.6, 0.8]", None).as_ref());
        tx.insert(label("a", "alpha", None).as_ref());
        tx.commit().unwrap();
    };
    let inserted = || vec![label("b", "[0, 1]", None), label("d", "[0, 0, 1]", None)];
    let removed = || vec![label("c", "[0.6, 0.8]", None)];
    let truth = {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        base(&engine);
        let mut tx = engine.transaction();
        for quad in inserted() {
            tx.insert(quad.as_ref());
        }
        for quad in removed() {
            tx.remove(quad.as_ref());
        }
        tx.commit().unwrap();
        engine.snapshot()
    };
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    for quad in inserted() {
        private.try_insert(quad.as_ref()).unwrap();
    }
    for quad in removed() {
        private.remove(quad.as_ref()).unwrap();
    }
    let pending = private.pending_snapshot().unwrap();
    let own_of = |snapshot: &Snapshot| {
        let own: Term = Literal::new_typed_literal(
            "[0, 1]",
            NamedNode::new_unchecked(nrese_engine::vector::DATATYPE),
        )
        .into();
        snapshot.lookup(own.as_ref())
    };
    let queries = [
        // k on whole tie groups (SRCH 9 Nov): which members of a group a cut keeps
        // follows the order offers arrive in, which an independent engine needn't share.
        ("tie", nrese_engine::VectorQuery::new(vec![1.0, 1.0], 2)),
        (
            "near own",
            nrese_engine::VectorQuery::new(vec![0.0, 1.0], 3),
        ),
        (
            "own dimension",
            nrese_engine::VectorQuery::new(vec![0.0, 0.0, 1.0], 2),
        ),
        (
            "no dimension",
            nrese_engine::VectorQuery::new(vec![1.0; 4], 2),
        ),
    ];
    let mut failed = Vec::new();
    for (name, query) in &queries {
        for refuse_own in [false, true] {
            let (got, want) = match refuse_own {
                false => (
                    vector_answer(&pending, query, &|_| true),
                    vector_answer(&truth, query, &|_| true),
                ),
                true => {
                    let (mine, theirs) = (own_of(&pending), own_of(&truth));
                    (
                        vector_answer(&pending, query, &|id| Some(id) != mine),
                        vector_answer(&truth, query, &|id| Some(id) != theirs),
                    )
                }
            };
            if got != want || got.is_err() {
                failed.push(format!(
                    "{name}, refuse own {refuse_own}: {got:?} != {want:?}"
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Sol-loop r1 B3: a private snapshot that reads within a request's budget (as a SPARQL
/// query's does), with equality expansion and a cold vector corpus, searched by the global
/// vector entry point within that same budget, answers: the search charges the budget
/// through the snapshot's own reads, not through a bridge that holds all its room. Red
/// before the fix: the bridge reserved the remainder and the expansion's reads were
/// refused.
#[test]
fn a_private_global_vector_search_within_the_readers_budget_answers() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(ex("rep").as_ref().into());
    let alias = tx.intern(ex("alias").as_ref().into());
    let same_as = tx.intern(
        NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs")
            .as_ref()
            .into(),
    );
    assert!(tx.insert_inferred(EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: rep,
    }));
    tx.insert(label("rep", "[1, 0]", None).as_ref());
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    private
        .try_insert(label("own", "[0, 1]", None).as_ref())
        .unwrap();
    let budget = Arc::new(nrese_exec::Budget::new(64 << 20));
    let reader = private
        .pending_snapshot()
        .unwrap()
        .reading_within(Arc::clone(&budget))
        .unwrap();
    let near = nrese_engine::VectorQuery::new(vec![0.0, 1.0], 2);
    let found = reader.vector_search(&near, &|_| true, &budget);
    assert!(found.is_ok(), "{:?}", found.as_ref().err());
    let (hits, _) = found.unwrap();
    assert_eq!(hits.len(), 2);
    assert!(reader.read_refused().is_none());
}
