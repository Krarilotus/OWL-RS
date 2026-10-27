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
