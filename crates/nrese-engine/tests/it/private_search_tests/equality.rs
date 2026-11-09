//! Equality classes: a private `sameAs` change, and the views of one unpublished version in
//! different equality modes.

use super::*;

/// Attack item 9: a private delta that changes `owl:sameAs` statements (one class split,
/// one joined) under readers of equality classes, expanded and canonical, with the base's
/// IRI corpus of `ex:g` warm: each IRI search equals an engine holding the same statements.
/// The mutant that skips the `sameAs` check is to be red here (DICT2 9 Nov 04:02, Q1 b).
#[test]
fn a_private_same_as_change_searches_its_own_classes() {
    let same_as_iri = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    // Interned in this order, so the first of each class is its representative.
    let names = ["rep", "alias", "other", "joined"];
    let setup = |split: bool, join: bool| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        engine.set_corpus_tuning(CorpusTuning {
            documents_per_probe: 1,
            ..CorpusTuning::DEFAULT
        });
        let mut tx = engine.transaction();
        let ids: Vec<TermId> = names
            .iter()
            .map(|name| tx.intern(ex(name).as_ref().into()))
            .collect();
        let same_as = tx.intern(same_as_iri.as_ref().into());
        let edge = |alias: usize, rep: usize| EncodedTriple {
            subject: ids[alias],
            predicate: same_as,
            object: ids[rep],
        };
        if !split {
            assert!(tx.insert_inferred(edge(1, 0)));
        }
        if join {
            assert!(tx.insert_inferred(edge(3, 2)));
        }
        for name in ["rep", "other"] {
            tx.insert(Quad::new(ex(name), ex("p"), ex("o"), ex("g")).as_ref());
        }
        tx.insert(Quad::new(ex("filler"), ex("p"), ex("o"), ex("g")).as_ref());
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        (engine, ids, same_as)
    };
    let mut failed = Vec::new();
    for canonical in [false, true] {
        let read = |snapshot: &Snapshot| match canonical {
            true => snapshot.with_canonical_equality(),
            false => snapshot.clone(),
        };
        let search = |snapshot: &Snapshot| {
            let g = snapshot.lookup(ex("g").as_ref().into()).unwrap();
            let scope = TextScope::Graphs(GraphSelector::Exact(g));
            let mut found = snapshot
                .iri_search_in(
                    ReadModel::Materialised,
                    &scope,
                    &query("rep alias other joined filler", false, false),
                    None,
                )
                .map(|found| decoded(snapshot, &found))
                .map_err(|e| format!("{e:?}"));
            if let Ok(found) = &mut found {
                found.sort_by(|a, b| a.0.cmp(&b.0));
            }
            found
        };
        let (truth, _, _) = setup(true, true);
        let want = search(&read(&truth.snapshot()));
        let (engine, ids, same_as) = setup(false, false);
        let published = engine.snapshot();
        // Warm: the base's corpus kept, so the private state could derive from it.
        search(&read(&published)).expect("the published search");
        let mut private = engine
            .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
            .unwrap();
        assert!(private.remove_inferred(EncodedTriple {
            subject: ids[1],
            predicate: same_as,
            object: ids[0],
        }));
        assert!(private.insert_inferred(EncodedTriple {
            subject: ids[3],
            predicate: same_as,
            object: ids[2],
        }));
        let got = search(&read(&private.pending_snapshot().unwrap()));
        if got != want || got.is_err() {
            failed.push(format!("canonical {canonical}: {got:?} != {want:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Attack item 9, DICT2's condition (b) (sol-loop r1 F1): a private removal of a `sameAs`
/// statement written representative first (`ex:rep owl:sameAs ex:alias`, `ex:rep` the
/// smaller id), asserted or inferred, under expanded and canonical readers, the base's IRI
/// corpus of the default graph warm. Its only IRI subject is `ex:rep`, so a derivation
/// probing the touched terms alone would keep `ex:alias` (no longer in a class, no longer
/// a subject): the private state walks instead. Each search equals an engine without the
/// statement. Red by mutation (the `sameAs` check skipped).
#[test]
fn a_private_removal_of_a_same_as_written_representative_first_drops_the_alias() {
    let same_as_iri = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let setup = |edge: bool, inferred: bool| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        engine.set_corpus_tuning(CorpusTuning {
            documents_per_probe: 1,
            ..CorpusTuning::DEFAULT
        });
        let mut tx = engine.transaction();
        // The representative interned first: the smaller id.
        let rep = tx.intern(ex("rep").as_ref().into());
        let alias = tx.intern(ex("alias").as_ref().into());
        let same_as = tx.intern(same_as_iri.as_ref().into());
        for subject in ["rep", "filler0", "filler1", "filler2"] {
            tx.insert(Quad::new(ex(subject), ex("p"), ex("o"), GraphName::DefaultGraph).as_ref());
        }
        let triple = EncodedTriple {
            subject: rep,
            predicate: same_as,
            object: alias,
        };
        if edge {
            match inferred {
                true => assert!(tx.insert_inferred(triple)),
                false => assert!(tx.insert_encoded(triple.in_default_graph())),
            }
        }
        tx.commit().unwrap();
        engine.set_equality(Some(same_as));
        (engine, triple)
    };
    let search = |snapshot: &Snapshot| {
        let scope = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
        let mut found = snapshot
            .iri_search_in(
                ReadModel::Materialised,
                &scope,
                &query("rep alias filler0", false, false),
                None,
            )
            .map(|found| decoded(snapshot, &found))
            .map_err(|e| format!("{e:?}"));
        if let Ok(found) = &mut found {
            found.sort_by(|a, b| a.0.cmp(&b.0));
        }
        found
    };
    let mut failed = Vec::new();
    for inferred in [false, true] {
        for canonical in [false, true] {
            let read = |snapshot: &Snapshot| match canonical {
                true => snapshot.with_canonical_equality(),
                false => snapshot.clone(),
            };
            let (truth, _) = setup(false, inferred);
            let want = search(&read(&truth.snapshot()));
            let (engine, triple) = setup(true, inferred);
            let published = engine.snapshot();
            search(&read(&published)).expect("the published search");
            let mut private = engine
                .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
                .unwrap();
            match inferred {
                true => assert!(private.remove_inferred(triple)),
                false => assert!(private.remove_encoded(triple.in_default_graph())),
            }
            let got = search(&read(&private.pending_snapshot().unwrap()));
            if got != want || got.is_err() {
                failed.push(format!(
                    "inferred {inferred}, canonical {canonical}: {got:?} != {want:?}"
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// R32-N2's own-slot corpora (DICT2 9 Nov 06:00; 3.0c's per-family test): the views of one
/// unpublished version (as stored, expanded, canonical) share its own slot, and never an
/// entry. A writer's pending snapshot and a private one, each over a base where
/// `ex:alias owl:sameAs ex:rep` is inferred and only `ex:rep` is a subject in `ex:g`, are
/// searched for IRIs in `ex:g` in every mode, in every order, twice: each answer equals an
/// engine with the same statements read in that mode.
#[test]
fn views_of_one_unpublished_version_never_share_a_corpus() {
    let same_as_iri = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let setup = |pending_label: bool| {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        engine.set_corpus_tuning(CorpusTuning {
            documents_per_probe: 1,
            ..CorpusTuning::DEFAULT
        });
        let mut tx = engine.transaction();
        let rep = tx.intern(ex("rep").as_ref().into());
        let alias = tx.intern(ex("alias").as_ref().into());
        let same_as = tx.intern(same_as_iri.as_ref().into());
        assert!(tx.insert_inferred(EncodedTriple {
            subject: alias,
            predicate: same_as,
            object: rep,
        }));
        for subject in ["rep", "filler0", "filler1"] {
            tx.insert(Quad::new(ex(subject), ex("p"), ex("o"), ex("g")).as_ref());
        }
        if pending_label {
            tx.insert(label("rep", "rep label", Some("g")).as_ref());
        }
        tx.commit().unwrap();
        (engine, same_as)
    };
    let modes = ["stored", "expanded", "canonical"];
    let read = |snapshot: &Snapshot, same_as, mode: &str| match mode {
        "stored" => snapshot.with_equality(None),
        "expanded" => snapshot.with_equality(Some(same_as)),
        _ => snapshot
            .with_equality(Some(same_as))
            .with_canonical_equality(),
    };
    let search = |snapshot: &Snapshot| {
        let g = snapshot.lookup(ex("g").as_ref().into()).unwrap();
        let mut found = snapshot
            .iri_search_in(
                ReadModel::Materialised,
                &TextScope::Graphs(GraphSelector::Exact(g)),
                &query("rep alias filler0", false, false),
                None,
            )
            .map(|found| decoded(snapshot, &found))
            .map_err(|e| format!("{e:?}"));
        if let Ok(found) = &mut found {
            found.sort_by(|a, b| a.0.cmp(&b.0));
        }
        found
    };
    let (truth, truth_same_as) = setup(true);
    let truth = truth.snapshot();
    let want: Vec<_> = modes
        .iter()
        .map(|mode| search(&read(&truth, truth_same_as, mode)))
        .collect();
    let orders = [[0, 1, 2], [2, 1, 0], [1, 0, 2]];
    let mut failed = Vec::new();
    for private in [false, true] {
        for order in orders {
            let (engine, same_as) = setup(false);
            let published = engine.snapshot();
            let mut tx = engine.transaction();
            let mut own = engine
                .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
                .unwrap();
            let pending = match private {
                false => {
                    tx.insert(label("rep", "rep label", Some("g")).as_ref());
                    tx.pending_snapshot()
                }
                true => {
                    own.try_insert(label("rep", "rep label", Some("g")).as_ref())
                        .unwrap();
                    own.pending_snapshot().unwrap()
                }
            };
            for round in 0..2 {
                for &at in &order {
                    let got = search(&read(&pending, same_as, modes[at]));
                    if got != want[at] || got.is_err() {
                        failed.push(format!(
                            "private {private}, order {order:?}, round {round}, {}: \
                             {got:?} != {:?}",
                            modes[at], want[at]
                        ));
                    }
                }
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
