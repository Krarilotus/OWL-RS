use nrese_reasoner::{ReasonerConfig, ReasoningMode};

use crate::{
    generator::{case, insert},
    judges, sweep,
};

#[test]
fn nil_equal_cell_keeps_its_existing_clash() {
    // Tables 3, 4 and 6 entail the oneOf clash. Compact still stops at the
    // merged head as nil and misses the clash because the prefix is empty.
    // The per-cell form must report that clash.
    let fixture = case(
        "nil-equal-cell",
        ["ex:a", "rdf:type", "ex:C"],
        vec![insert(
            "
            ex:C owl:oneOf ex:l
            ex:C rdfs:subClassOf owl:Nothing
            ex:l rdf:first ex:a
            ex:l rdf:rest rdf:nil
            ex:l owl:sameAs rdf:nil
            ",
        )],
    );
    assert!(sweep::steps(&fixture)[0].model.inconsistent);
    for (mode, clash) in [
        ("replication", true),
        ("representatives", true),
        ("compact", false),
    ] {
        let store = judges::open(mode, None);
        let raw = judges::pipeline(&store, ReasoningMode::Disabled);
        assert!(judges::apply(&raw, &fixture.writes[0]));
        let rl = ReasoningMode::Owl2Rl;
        let _read = judges::pipeline(&store, rl);
        let program = ReasonerConfig::for_mode(rl).materialised_program().unwrap();
        let report = store.rematerialise(program).unwrap();
        assert_eq!(report.violations > 0, clash, "{mode}: oneOf clash");
        assert!(
            report.diagnostics.iter().any(|d| d.kind == "cyclic-list"),
            "{mode}: {:?}",
            report.diagnostics
        );
        let (_, completeness) = judges::ask(&store, &fixture.watch);
        assert_eq!(completeness, "sound-only", "{mode}");
    }
}

/// The prefix produces its old AllDifferent clash and leaves the read incomplete.
/// The initial write still commits even though the tables find a clash by visiting
/// the nil cell twice. The per-cell form must refuse that initial write.
#[test]
fn pairwise_list_keeps_its_clash_before_the_nil_cycle() {
    // Spec: Tables 3 and 4 give an AllDifferent clash on the prefix ending
    // at the first visit to nil. The nil cycle leaves the reading incomplete.
    let fixture = case(
        "pairwise-nil-cycle",
        ["ex:a", "ex:p", "ex:b"],
        vec![
            insert(
                "
                ex:all rdf:type owl:AllDifferent
                ex:all owl:distinctMembers ex:h
                ex:h rdf:first ex:a
                ex:h rdf:rest ex:t
                ex:t rdf:first ex:b
                ex:t rdf:rest rdf:nil
                rdf:nil rdf:first ex:c
                rdf:nil rdf:rest rdf:nil
                ex:a ex:p ex:b
                ",
            ),
            insert("ex:a owl:sameAs ex:b"),
        ],
    );
    assert!(sweep::steps(&fixture)[0].model.inconsistent);
    for &mode in judges::MODES {
        let store = judges::open(mode, None);
        let pipeline = judges::pipeline(&store, ReasoningMode::Owl2Rl);
        assert!(judges::apply(&pipeline, &fixture.writes[0]));
        let (present, completeness) = judges::ask(&store, &fixture.watch);
        assert!(present, "{mode}: the named data query still has its answer");
        let report = pipeline.last_reasoning_run().unwrap();
        assert!(
            !judges::apply(&pipeline, &fixture.writes[1]),
            "{mode}: the existing clash is kept"
        );
        println!("pairwise-nil-cycle {mode}: {completeness}");
        assert_eq!(completeness, "sound-only", "{mode}");
        assert!(
            report.diagnostics.iter().any(|d| d.kind == "cyclic-list"),
            "{mode}: {:?}",
            report.diagnostics
        );
    }
}
