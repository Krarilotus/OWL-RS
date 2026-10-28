//! The support graph sets and the closure that decides what their cap left open, when the
//! rules change (`Supports::use_rules`) between a read's steps.

use super::*;

const EX: &str = "http://example.com/";

/// A store with `C ⊑ D` in the default graph and `w a C` in 17 graphs (g00 to g16), its
/// OWL 2 RL closure and rules in place: `w a D` has 17 support sets, one past the default cap
/// of 16, which leaves g16's out (graphs in IRI order).
fn past_the_cap() -> crate::StoreService {
    let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let graphs: String = (0..17)
        .map(|g| format!("GRAPH <{EX}g{g:02}> {{ <{EX}w> a <{EX}C> }} "))
        .collect();
    let reasoner = nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::default());
    crate::MutationPipeline::new(Arc::new(store.clone()), Arc::new(reasoner))
        .execute_update(&crate::SparqlUpdateRequest::new(format!(
            "INSERT DATA {{ <{EX}C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}D> . \
             {graphs} }}"
        )))
        .unwrap();
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
        .unwrap();
    store.use_reasoning_rules(Some(nrese_reasoner::rulesets::Ruleset::Owl2Rl.into()));
    store
}

/// `w a D`, in `snapshot`'s ids, as an inferred statement.
fn w_a_d(snapshot: &Snapshot) -> EncodedQuad {
    let id = |iri: &str| {
        snapshot
            .lookup(nrese_rdf::NamedNode::new_unchecked(iri).as_ref().into())
            .unwrap()
    };
    EncodedQuad::new(
        id(&format!("{EX}w")),
        id("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        id(&format!("{EX}D")),
        TermId::DEFAULT_GRAPH,
    )
}

/// Spec: config.rs (`StoreConfig::support_sets`): a reader none of a statement's kept sets
/// fits is decided by the closure of its graphs, under the rules the sets come from. A
/// repository's rules can change at any time (its reasoning switched off: `use_rules(None)`),
/// also while a read runs; the read then answers by the rules its sets were computed with,
/// and nothing it makes is kept for the rules now in place (review r1 of O4, C1). g16's
/// reader, past the cap: its sets, then the rules switched off, then its view.
#[test]
fn a_rule_change_during_a_read_leaves_it_its_rules_and_keeps_nothing_of_it() {
    let store = past_the_cap();
    let supports = store.supports();
    let snapshot = store.read_snapshot(None).unwrap();
    let compilation = Compilation {
        hide_unnamed_classes: false,
        by_representatives: false,
        compact: false,
        cap: 16,
    };
    let working = Work {
        budget: Arc::new(Budget::unlimited()),
        cancellation: None,
    };
    let access = Arc::new(GraphAccess {
        graphs: vec![format!("{EX}g16")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    });
    // The read's steps as `Supports::view` takes them: its cell with the rules in place,
    // its sets, then (the rules switched off meanwhile) its view.
    let (_, generation) = supports.cell_for(&snapshot).unwrap();
    let sets = supports
        .compute(&snapshot, &generation, compilation, &working)
        .unwrap()
        .unwrap();
    supports.use_rules(None);
    let rules = generation.rules.as_ref().expect("the rules in place");
    let within = || supports.within(&snapshot, &access, rules, compilation, &working);
    let view = supports
        .build_view(
            &snapshot,
            &access,
            &sets,
            within,
            generation.epoch,
            &working,
        )
        .unwrap();
    assert!(
        view.contains_in(ReadModel::Inferred, &w_a_d(&snapshot)),
        "g16's reader derives w a D"
    );
    let state = supports.state();
    assert!(
        state.readers.is_empty() && state.computed.is_none(),
        "kept for the rules now in place"
    );
}

/// Spec: an update's `WHERE` reads what its requester's queries read ([`Reading`], review r1 of
/// PR #50, F1), under the update's cancellation (the orchestrator's case 3 of F1). Its read
/// source, cancelled while it makes the requester's view, fails as cancelled and keeps
/// nothing; a requester of every graph reads the data as stored, and has none.
#[test]
fn an_updates_read_source_stops_with_its_cancellation() {
    let store = past_the_cap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let context = store.update_context(&cancelled);
    let reader = crate::Requester::new(
        crate::ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}g16")],
            default_graph: true,
            inferred: true,
            inferred_by_support: true,
            ..GraphAccess::default()
        })),
        crate::WriteScope::All,
    );
    assert!(
        context
            .update_options(&crate::Requester::all(), None)
            .read
            .is_none()
    );
    let read = context
        .update_options(&reader, None)
        .read
        .expect("a reader by support reads its view");
    let before = store.support_statistics();
    let tx = store.engine().speculative();
    let state = nrese_sparql::ReadSource::state(&*read, &tx);
    assert!(
        matches!(state, Err(nrese_sparql::QueryEvaluationError::Cancelled)),
        "{:?}",
        state.map(|_| ())
    );
    assert_eq!(store.support_statistics(), before, "the view was made");
}

/// A seeded generator (splitmix64), for the property tests.
struct Seeded(u64);

impl Seeded {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
}

/// The spec of a pending read (D5; what `Reading::pending` gathered before it was delta-sized):
/// the pending snapshot, its inferred statements those of the reader's view of the base the
/// changes leave: a base-inferred one while the transaction still holds it as inferred, one the
/// view adds while no change asserts it where the reader reads it. Every statement scanned.
fn gathered(reading: &Reading, tx: &Transaction<'_>) -> Snapshot {
    if tx.pending() == (0, 0) && tx.inferred_pending() == (0, 0) {
        return reading.committed(tx.base()).unwrap();
    }
    if !reading.through_view(tx.base()) {
        return tx.pending_snapshot();
    }
    let view = reading.committed(tx.base()).unwrap();
    let mut kept: Vec<EncodedQuad> = view
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .filter(
            |quad| match tx.base().contains_in(ReadModel::Inferred, quad) {
                true => tx.contains_in(ReadModel::Inferred, quad),
                false => asserted(tx, &reading.access, triple(*quad)) != Asserted::InReach,
            },
        )
        .collect();
    kept.sort_unstable();
    tx.pending_snapshot()
        .with_inferred_only(&kept, &Arc::new(Budget::unlimited()))
        .unwrap()
}

/// A snapshot's asserted and inferred statements, sorted, each list checked against the count
/// the snapshot keeps of it.
fn statements(snapshot: &Snapshot) -> [Vec<EncodedQuad>; 2] {
    [ReadModel::Asserted, ReadModel::Inferred].map(|model| {
        let mut quads: Vec<EncodedQuad> = snapshot
            .quads_for_pattern_in(model, &QuadPattern::all())
            .collect();
        quads.sort_unstable();
        assert_eq!(
            snapshot.len_in(model),
            quads.len() as u64,
            "{model:?}: the count kept"
        );
        quads
    })
}

/// Spec (D5, review r1 of PR #50): a pending read costs its delta, and reads exactly what the
/// gathering read (`gathered`), by support and seeing every inference. Seeded stores of 10 to 90
/// subjects typed in a class chain (`C0 ⊑ … ⊑ C4`, in the default graph) across five graphs,
/// some memberships also asserted in a graph the reader can't read (its own graphs derive them:
/// the view adds them). Each store gets three rounds of changes, checked after each (a request's
/// later operations): inserts and deletes of memberships in every graph and the default graph,
/// among them base-inferred statements asserted now, view-added ones asserted where the reader
/// reads, and deletions; each kind must occur.
#[test]
fn a_pending_read_is_the_gathered_one() {
    use nrese_rdf::{GraphName, NamedNode, Quad};
    let iri = |local: String| NamedNode::new_unchecked(format!("urn:t:{local}"));
    let graph = |g: u64| match g {
        0 => GraphName::DefaultGraph,
        g => GraphName::NamedNode(iri(format!("g{g}"))),
    };
    let member = |s: u64, c: u64, g: u64| {
        Quad::new(
            iri(format!("s{s}")),
            NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            iri(format!("C{c}")),
            graph(g),
        )
    };
    let [mut base_inferred, mut view_added, mut deleted] = [0_usize; 3];
    for seed in 0..48_u64 {
        let mut rng = Seeded(seed);
        let subjects = 10 + 10 * (seed % 9);
        // Graphs 1 to 5; the reader reads 1, 2 and the default graph.
        let mut data: Vec<Quad> = Vec::new();
        for s in 0..subjects {
            for _ in 0..1 + rng.below(3) {
                data.push(member(s, rng.below(3), 1 + rng.below(5)));
            }
            if rng.below(3) == 0 {
                // Asserted out of reach: the view adds it where the reader's graphs derive it.
                data.push(member(s, 3 + rng.below(2), 3 + rng.below(3)));
            }
        }
        let schema: String = (0..4)
            .map(|c| {
                format!(
                    "<urn:t:C{c}> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:t:C{}> . ",
                    c + 1
                )
            })
            .collect();
        let quads: String = data
            .iter()
            .map(|q| {
                let triple = format!("{} {} {}", q.subject, q.predicate, q.object);
                match &q.graph_name {
                    GraphName::DefaultGraph => format!("{triple} . "),
                    named => format!("GRAPH {named} {{ {triple} }} "),
                }
            })
            .collect();
        let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
        let reasoner =
            nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::default());
        crate::MutationPipeline::new(Arc::new(store.clone()), Arc::new(reasoner))
            .execute_update(&crate::SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ {schema} {quads} }}"
            )))
            .unwrap();
        store
            .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
            .unwrap();
        store.use_reasoning_rules(Some(nrese_reasoner::rulesets::Ruleset::Owl2Rl.into()));
        let access = Arc::new(GraphAccess {
            graphs: vec!["urn:t:g1".to_owned(), "urn:t:g2".to_owned()],
            default_graph: true,
            inferred: true,
            inferred_by_support: seed % 2 == 0,
            ..GraphAccess::default()
        });
        let reading = Reading::of(
            store.supports(),
            store.support_compilation(),
            Some(&access),
            &store.envelope(None),
            None,
        )
        .unwrap();
        let mut tx = store.engine().speculative();
        let view = reading.committed(tx.base()).unwrap();
        for round in 0..3 {
            for _ in 0..1 + rng.below(2 + 4 * (seed % 5)) {
                if rng.below(4) == 0 && !data.is_empty() {
                    let gone = data.swap_remove(rng.below(data.len() as u64) as usize);
                    deleted += usize::from(tx.remove(gone.as_ref()));
                    continue;
                }
                let quad = member(rng.below(subjects), rng.below(5), rng.below(6));
                if let Some(fact) = (|| {
                    let id = |term| tx.lookup(term);
                    Some(EncodedQuad::new(
                        id(quad.subject.as_ref().into())?,
                        id(quad.predicate.as_ref().into())?,
                        id(quad.object.as_ref())?,
                        TermId::DEFAULT_GRAPH,
                    ))
                })() {
                    let in_view = view.contains_in(ReadModel::Inferred, &fact);
                    let of_base = tx.base().contains_in(ReadModel::Inferred, &fact);
                    let readable = access.allows_graph(&quad.graph_name);
                    base_inferred +=
                        usize::from(in_view && of_base && quad.graph_name.is_default_graph());
                    view_added += usize::from(in_view && !of_base && readable);
                }
                tx.insert(quad.as_ref());
                data.push(quad);
            }
            assert_eq!(
                statements(&reading.pending(&tx).unwrap()),
                statements(&gathered(&reading, &tx)),
                "seed {seed}, round {round}"
            );
        }
    }
    assert!(
        base_inferred > 0 && view_added > 0 && deleted > 0,
        "the kinds of change covered: base-inferred asserted {base_inferred}, view-added \
         asserted in reach {view_added}, deletions {deleted}"
    );
}
