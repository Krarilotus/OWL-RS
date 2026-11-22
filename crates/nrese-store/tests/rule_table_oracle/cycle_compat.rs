use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::StoreService;

use crate::{generator::insert, judges, oracle, triple};

const ONE_OF: &str = "
    ex:C owl:oneOf ex:l
    ex:C rdfs:subClassOf owl:Nothing
    ex:l rdf:first ex:a
    ex:l rdf:rest rdf:nil
    ex:l owl:sameAs rdf:nil
";
const ALL_DIFFERENT: &str = "
    ex:all rdf:type owl:AllDifferent
    ex:all owl:distinctMembers ex:h
    ex:h rdf:first ex:a
    ex:h rdf:rest ex:t
    ex:t rdf:first ex:b
    ex:t rdf:rest rdf:nil
    rdf:nil rdf:first ex:c
    rdf:nil rdf:rest rdf:nil
    ex:a ex:p ex:b
";

fn rebuild_cycle(mode: &str, setup: &str) -> (Arc<StoreService>, usize) {
    let store = judges::open(mode, None);
    let raw = judges::pipeline(&store, ReasoningMode::Disabled);
    assert!(judges::apply(&raw, &insert(setup)));
    let _reader = judges::pipeline(&store, ReasoningMode::Owl2Rl);
    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl);
    let program = config.materialised_program().unwrap();
    let report = store.rematerialise(program).unwrap();
    assert!(
        report.diagnostics.is_empty(),
        "{mode}: accepting walk diagnosed: {:?}",
        report.diagnostics
    );
    (store, report.violations)
}

#[test]
fn nil_equal_cell_keeps_its_existing_clash() {
    // Tables 3, 4 and 6 entail the oneOf clash. QL still sees non-DL syntax.
    assert!(oracle_inconsistent(ONE_OF));
    for &mode in judges::MODES {
        let (store, violations) = rebuild_cycle(mode, ONE_OF);
        assert!(violations > 0, "{mode}: oneOf clash");
        let (_, status) = judges::ask(&store, &triple(["ex:a", "rdf:type", "ex:C"]));
        assert_eq!(status, "sound-only", "{mode}");
    }
}

#[test]
fn pairwise_list_keeps_its_clash_before_the_nil_cycle() {
    // Tables 3 and 4 forbid equating the prefix members and repeating the nil cell.
    assert!(oracle_inconsistent(ALL_DIFFERENT));
    for &mode in judges::MODES {
        let (store, violations) = rebuild_cycle(mode, ALL_DIFFERENT);
        assert!(violations > 0, "{mode}: repeated member clash");
        let pipeline = judges::pipeline(&store, ReasoningMode::Owl2Rl);
        assert!(
            !judges::apply(&pipeline, &insert("ex:a owl:sameAs ex:b")),
            "{mode}: the existing clash is kept"
        );
        let (present, status) = judges::ask(&store, &triple(["ex:a", "ex:p", "ex:b"]));
        assert!(present, "{mode}: missing the asserted row");
        assert_eq!(status, "sound-only", "{mode}");
    }
}

fn oracle_inconsistent(input: &str) -> bool {
    let asserted = insert(input).insert.into_iter().collect();
    oracle::evaluate(&asserted, oracle::Datatypes::Standard).inconsistent
}

#[test]
fn compact_one_of_cell_equated_with_nil_reports_a_clash() {
    // Spec: Tables 3, 4 and 6 entail membership of a subclass of owl:Nothing.
    assert!(oracle_inconsistent(ONE_OF));
    let (_, consistent) = judges::first_and_rebuild("compact", &insert(ONE_OF).insert, true);
    assert!(!consistent, "compact: missing the oneOf clash");
}

#[test]
fn pairwise_nil_cycle_refuses_the_first_write() {
    // Spec: Tables 3 and 4 revisit nil and clash on the repeated AllDifferent member.
    assert!(oracle_inconsistent(ALL_DIFFERENT));
    let input = insert(ALL_DIFFERENT);
    let accepting: Vec<_> = judges::MODES
        .iter()
        .copied()
        .filter(|&mode| judges::first_and_rebuild(mode, &input.insert, false).1)
        .collect();
    assert!(accepting.is_empty(), "accepted in {accepting:?}");
}
