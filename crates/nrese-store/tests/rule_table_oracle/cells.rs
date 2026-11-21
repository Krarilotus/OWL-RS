use crate::{
    Facts,
    generator::{self, Case, case, delete, insert},
    judges, oracle, sweep,
};

const ANNOTATION_AND_CLASS_DECLARATIONS: &str = "
    rdfs:label rdf:type owl:AnnotationProperty
    rdfs:comment rdf:type owl:AnnotationProperty
    rdfs:seeAlso rdf:type owl:AnnotationProperty
    rdfs:isDefinedBy rdf:type owl:AnnotationProperty
    owl:deprecated rdf:type owl:AnnotationProperty
    owl:versionInfo rdf:type owl:AnnotationProperty
    owl:priorVersion rdf:type owl:AnnotationProperty
    owl:backwardCompatibleWith rdf:type owl:AnnotationProperty
    owl:incompatibleWith rdf:type owl:AnnotationProperty
    owl:Thing rdf:type owl:Class
    owl:Nothing rdf:type owl:Class
";

#[test]
fn first_write_publishes_annotation_and_class_seeds() {
    // Spec: OWL 2 Profiles §4.3, prp-ap, cls-thing, cls-nothing1 and scm-cls
    // require these rows after a first write, including after a disk reopen.
    let mut required = insert(ANNOTATION_AND_CLASS_DECLARATIONS).insert;
    let schema = insert(
        "
        owl:Thing rdfs:subClassOf owl:Thing
        owl:Thing owl:equivalentClass owl:Thing
        owl:Nothing rdfs:subClassOf owl:Thing
        owl:Nothing rdfs:subClassOf owl:Nothing
        owl:Nothing owl:equivalentClass owl:Nothing
        ",
    );
    required.extend(schema.insert);
    let write = insert("ex:a ex:p ex:b");
    let mut missing = Vec::new();
    for &mode in judges::MODES {
        let temp = tempfile::tempdir().expect("seed test disk store directory");
        let rl = nrese_reasoner::ReasoningMode::Owl2Rl;
        for (stage, path) in [
            ("memory", None),
            ("disk", Some(temp.path())),
            ("reopened", Some(temp.path())),
        ] {
            let store = judges::open(mode, path);
            let pipeline = judges::pipeline(&store, rl);
            if stage != "reopened" {
                assert!(
                    judges::apply(&pipeline, &write),
                    "{mode} {stage} first write"
                );
            }
            let (rows, _) = judges::rows(&store);
            for row in &required {
                if !rows.contains(row) {
                    missing.push((mode, stage, row));
                }
            }
        }
    }
    assert!(missing.is_empty(), "OWL 2 RL seed rows absent: {missing:?}");
}

#[test]
fn deleting_assertions_keeps_the_seed_declarations() {
    use nrese_reasoner::ReasoningMode::{Owl2Rl, RdfsFull};

    // Spec: reasoning-semantics.md, program axioms remain in the closure after
    // their asserted copies are deleted. OWL 2 Profiles §4.3 cax-sco can use
    // cls-thing in that same write. RDF 1.1 Semantics §8.1.1 supplies the rdf:nil row.
    let mut mixed_answers: Vec<_> = ANNOTATION_AND_CLASS_DECLARATIONS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .chain(["xsd:integer rdf:type rdfs:Datatype"])
        .collect();
    let mixed_assertions = mixed_answers.join(" . ");
    let mixed_update = format!(
        "DELETE DATA {{ {mixed_assertions} }} ; INSERT DATA {{ owl:Class rdfs:subClassOf ex:Metaclass }}"
    );
    mixed_answers.push("owl:Thing rdf:type ex:Metaclass");
    let class = "owl:Thing rdf:type owl:Class";
    let removed_graph = "GRAPH ex:removed { owl:Thing rdf:type owl:Class }";
    let cases = [
        (
            "mixed update",
            Owl2Rl,
            mixed_assertions.as_str(),
            mixed_update.as_str(),
            [12, 1],
            mixed_answers.as_slice(),
        ),
        (
            "delete only",
            Owl2Rl,
            class,
            "DELETE DATA { owl:Thing rdf:type owl:Class }",
            [1, 0],
            &[class],
        ),
        (
            "last named graph copy",
            Owl2Rl,
            removed_graph,
            "DELETE DATA { GRAPH ex:removed { owl:Thing rdf:type owl:Class } }",
            [1, 0],
            &[class],
        ),
        (
            "one of two graph copies",
            Owl2Rl,
            "owl:Thing rdf:type owl:Class .
             GRAPH ex:removed { owl:Thing rdf:type owl:Class }",
            "DELETE DATA { GRAPH ex:removed { owl:Thing rdf:type owl:Class } }",
            [2, 1],
            &[class],
        ),
        (
            // With an inverse expression, deleting an ordinary data triple rebuilds
            // the inferred stack in every equality mode.
            "rebuild",
            Owl2Rl,
            "owl:Thing rdf:type owl:Class . _:inverse owl:inverseOf ex:p . ex:a ex:p ex:b",
            "DELETE DATA { owl:Thing rdf:type owl:Class . ex:a ex:p ex:b }",
            [3, 1],
            &[class],
        ),
        (
            "RDFS axiom",
            RdfsFull,
            "rdf:nil rdf:type rdf:List",
            "DELETE DATA { rdf:nil rdf:type rdf:List }",
            [1, 0],
            &["rdf:nil rdf:type rdf:List"],
        ),
    ];
    let mut missing = Vec::new();
    for (name, reasoning, asserted, update, [before, after], expected) in cases {
        let setup = format!("INSERT DATA {{ {asserted} }}");
        let expected = generator::rows(&expected.join("\n"));
        for &mode in judges::MODES {
            let temp = tempfile::tempdir().expect("seed deletion store directory");
            let open = || judges::open(mode, Some(temp.path()));
            let mut check = |store: &nrese_store::StoreService, stage| {
                assert_eq!(
                    store.stats().expect("asserted count").quad_count,
                    after,
                    "{name} {mode} {stage}"
                );
                let (rows, _) = judges::rows(store);
                for row in &expected {
                    if !rows.contains(row) {
                        missing.push((name, mode, stage, row.clone()));
                    }
                }
            };
            {
                let store = open();
                let pipeline = judges::pipeline(&store, reasoning);
                assert!(
                    judges::apply_update(&pipeline, &setup),
                    "{name} {mode} insert"
                );
                assert_eq!(
                    store.stats().expect("asserted count").quad_count,
                    before,
                    "{name} {mode} before deletion"
                );
                assert!(
                    judges::apply_update(&pipeline, update),
                    "{name} {mode} delete"
                );
                check(&store, "warm");
            }
            let store = open();
            let _pipeline = judges::pipeline(&store, reasoning);
            check(&store, "reopened");
        }
    }
    assert!(
        missing.is_empty(),
        "seed rows or consequences absent: {missing:?}"
    );
}

#[test]
#[ignore = "undocumented: an empty store omits the datatype declarations until its first write attempt"]
fn empty_store_publishes_datatype_axioms() {
    // Spec: Table 8 dt-type1 and reasoning-semantics.md:57 require these on an empty graph.
    let required: Facts = oracle::evaluate(&Facts::new(), oracle::Datatypes::Standard)
        .facts
        .into_iter()
        .filter(|[_, p, o]| p == "rdf:type" && o == "rdfs:Datatype")
        .collect();
    assert_eq!(required.len(), 32, "OWL 2 RL datatype count");
    let mut matched = true;
    for &mode in judges::MODES {
        let temp = tempfile::tempdir().expect("empty disk store directory");
        for (kind, path) in [("memory", None), ("disk", Some(temp.path()))] {
            let store = judges::open(mode, path);
            let _pipeline = judges::pipeline(&store, nrese_reasoner::ReasoningMode::Owl2Rl);
            let (rows, _) = judges::rows(&store);
            let missing: Vec<_> = required.difference(&rows).collect();
            if !missing.is_empty() {
                println!("CELL RED empty-datatypes {mode} {kind} missing={missing:?}");
                matched = false;
            }
        }
    }
    assert!(
        matched,
        "the empty store must publish every dt-type1 declaration"
    );
}

#[test]
#[ignore = "an accepting cycle starting at rdf:nil needs the per-cell form"]
fn nil_head_starts_an_explicit_walk() {
    // Spec: Table 3 matches the head again, so prp-spo2 gives both p,q and p,q,p,q.
    check_nil_walk("nil-head");
}

#[test]
#[ignore = "an accepting cycle through rdf:nil needs the per-cell form"]
fn nil_cell_continues_an_explicit_walk() {
    // Spec: Table 3 matches the interior cell again, so prp-spo2 gives both p,q and p,q,q.
    check_nil_walk("nil-cell");
}

fn check_nil_walk(shape: &'static str) {
    let mut fixture = generator::nil_walk(shape);
    let steps = sweep::steps(&fixture);
    let model = &steps[0].model;
    let continued = crate::triple(["ex:a", "ex:out", "ex:w"]);
    assert!(!model.inconsistent, "the oracle accepts {shape}");
    for row in [&fixture.watch, &continued] {
        assert!(model.facts.contains(row), "the oracle derives {row:?}");
    }
    println!("ORACLE NIL {shape} derives both ex:a ex:out ex:z and ex:a ex:out ex:w");
    let first_matches = sweep::check_cell(&fixture, &steps);
    fixture.watch = continued;
    let later_matches = sweep::check_cell(&fixture, &steps);
    assert!(
        first_matches && later_matches,
        "{shape} differs from the oracle"
    );
}

#[test]
fn unnamed_union_membership_is_visible_when_consumed() {
    // Spec: reasoning-semantics.md:27 hides memberships of unused unnamed unions.
    let membership = crate::triple(["ex:a", "rdf:type", "_:union"]);
    for consumed in [false, true] {
        let fixture = generator::unnamed_union(consumed);
        for &mode in judges::MODES {
            let (store, accepted) =
                judges::first_and_rebuild(mode, &fixture.writes[0].insert, false);
            assert!(accepted);
            let rows = judges::rows(&store).0;
            assert_eq!(rows.contains(&membership), consumed, "{mode}");
            assert_eq!(rows.contains(&fixture.watch), consumed, "{mode}");
        }
    }
}

fn check(cell: Case, accepted: &[bool], present: &[bool]) {
    assert!(
        matches_oracle(&cell, accepted, present),
        "{} differs from the oracle",
        cell.shape
    );
}

fn matches_oracle(cell: &Case, accepted: &[bool], present: &[bool]) -> bool {
    let steps = sweep::steps(cell);
    assert_eq!(steps.len(), accepted.len());
    assert_eq!(steps.len(), present.len());
    for ((step, accepted), present) in steps.iter().zip(accepted).zip(present) {
        assert_eq!(
            !step.model.inconsistent, *accepted,
            "the oracle's write verdict"
        );
        assert_eq!(
            step.model.facts.contains(&cell.watch),
            *present,
            "the oracle's witness"
        );
    }
    sweep::check_cell(cell, &steps)
}

#[test]
fn nil_alias_terminates_linear_and_cyclic_lists() {
    // Tables 3–5 derive the prefix's chain row at the first visit to nil's class.
    for (end, extra) in [
        ("ex:x", ""),
        (
            "rdf:nil",
            "rdf:nil rdf:first ex:r\nrdf:nil rdf:rest rdf:nil",
        ),
    ] {
        let fixture = case(
            "nil-alias",
            ["ex:a", "ex:out", "ex:c"],
            vec![
                insert("ex:x ex:marker ex:y"),
                insert(&format!(
                    "
                    rdf:nil owl:sameAs ex:x
                    ex:out owl:propertyChainAxiom ex:h
                    ex:h rdf:first ex:p
                    ex:h rdf:rest ex:t
                    ex:t rdf:first ex:q
                    ex:t rdf:rest {end}
                    ex:a ex:p ex:b
                    ex:b ex:q ex:c
                    {extra}
                    "
                )),
            ],
        );
        check_vocabulary_alias(fixture, "http://example.org/x", nrese_rdf::vocab::rdf::NIL);
    }
}

#[test]
fn rest_alias_links_list_cells() {
    // Spec: Table 4 eq-rep-p lets an alias of rdf:rest supply the edge
    // in the Table 3 list, so Table 5 prp-spo2 derives the chain row.
    check_rest_alias(rest_alias_fixture("ex:r"));
}

#[test]
fn mixed_rest_spellings_link_the_same_list() {
    // Spec: Table 4 eq-rep-p lets the two spellings supply different steps
    // of the same Table 3 list. Table 5 prp-spo2 derives its chain row.
    check_rest_alias(rest_alias_fixture("rdf:rest"));
}

#[test]
fn alias_only_rest_edits_maintain_chain_answers() {
    // Spec: Tables 3–5 add the new walk's answer when an alias of rdf:rest
    // connects it and retract it when that edge goes, even after reopening.
    let mut fixture = rest_alias_fixture("rdf:rest");
    fixture.writes[1].insert.extend(
        insert(
            "
            ex:u rdf:first ex:s
            ex:u ex:r rdf:nil
            ex:b ex:s ex:d
            ",
        )
        .insert,
    );
    fixture.writes.push(insert("ex:h ex:r ex:u"));
    fixture.writes.push(delete("ex:h ex:r ex:u"));
    let added = crate::triple(["ex:a", "ex:out", "ex:d"]);
    let steps = sweep::steps(&fixture);
    let rl = nrese_reasoner::ReasoningMode::Owl2Rl;
    for &mode in judges::MODES {
        for cold in [false, true] {
            let temp = tempfile::tempdir().expect("private store directory");
            let open = || judges::open(mode, Some(temp.path()));
            let mut store = open();
            let raw = judges::pipeline(&store, nrese_reasoner::ReasoningMode::Disabled);
            assert!(judges::apply(&raw, &fixture.writes[0]));
            drop(raw);
            let mut pipeline = judges::pipeline(&store, rl);
            for (index, step) in steps.iter().enumerate().skip(1) {
                if cold && index > 1 {
                    drop(pipeline);
                    drop(store);
                    store = open();
                    pipeline = judges::pipeline(&store, rl);
                }
                assert!(judges::apply(&pipeline, &fixture.writes[index]));
                let rows = judges::rows(&store).0;
                for row in [&fixture.watch, &added] {
                    assert_eq!(
                        rows.contains(row),
                        step.model.facts.contains(row),
                        "{mode} cold={cold} write={index} row={row:?}"
                    );
                }
            }
        }
    }
}

fn check_rest_alias(fixture: Case) {
    check_vocabulary_alias(fixture, "http://example.org/r", nrese_rdf::vocab::rdf::REST);
}

fn rest_alias_fixture(head_rest: &str) -> Case {
    case(
        "rest-alias",
        ["ex:a", "ex:out", "ex:c"],
        vec![
            insert("ex:r ex:marker ex:y"),
            insert(&format!(
                "
            rdf:rest owl:sameAs ex:r
            ex:out owl:propertyChainAxiom ex:h
            ex:h rdf:first ex:p
            ex:h {head_rest} ex:t
            ex:t rdf:first ex:q
            ex:t ex:r rdf:nil
            ex:a ex:p ex:b
            ex:b ex:q ex:c
        ",
            )),
        ],
    )
}

fn check_vocabulary_alias(fixture: Case, alias: &str, vocabulary: nrese_rdf::NamedNodeRef<'_>) {
    let steps = sweep::steps(&fixture);
    let expected = &steps[1].model;
    assert!(!expected.inconsistent);
    assert!(expected.facts.contains(&fixture.watch));

    use nrese_reasoner::{ReasonerConfig, ReasoningMode};
    let rl = ReasoningMode::Owl2Rl;
    let mut missing = Vec::new();
    for &mode in judges::MODES {
        for rebuild in [false, true] {
            let store = judges::open(mode, None);
            let raw = judges::pipeline(&store, ReasoningMode::Disabled);
            // An RL pipeline interns its rule vocabulary before the first write.
            assert!(judges::apply(&raw, &fixture.writes[0]));
            let reasoned = judges::pipeline(&store, rl);
            if rebuild {
                assert!(judges::apply(&raw, &fixture.writes[1]));
                let program = ReasonerConfig::for_mode(rl).materialised_program().unwrap();
                assert_eq!(store.rematerialise(program).unwrap().violations, 0);
            } else {
                assert!(judges::apply(&reasoned, &fixture.writes[1]));
            }
            let snapshot = store.read_snapshot(None).expect("read term dictionary");
            let alias = nrese_rdf::NamedNodeRef::new(alias).unwrap();
            let id =
                |term: nrese_rdf::NamedNodeRef<'_>| snapshot.lookup(term.into()).unwrap().raw();
            let (alias, term) = (id(alias), id(vocabulary));
            assert!(alias < term, "{mode} rebuild={rebuild}: {alias} >= {term}");
            if !judges::rows(&store).0.contains(&fixture.watch) {
                missing.push((mode, rebuild));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the chain row differs from the oracle in {missing:?} (mode, rebuild)"
    );
}

fn chain_with(extra: &str) -> generator::Write {
    insert(&format!(
        "
        ex:out owl:propertyChainAxiom ex:h
        ex:h rdf:first ex:p
        ex:h rdf:rest ex:t
        ex:t rdf:first ex:q
        ex:t rdf:rest rdf:nil
        ex:a ex:p ex:b
        ex:b ex:q ex:c
        {extra}
        "
    ))
}

#[test]
fn oversized_walks_keep_their_short_alternatives() {
    // Tables 5, 6 and 9 derive the short alternatives' chain, membership and
    // subclass rows. Diagnose only the oversized walk in each case.
    use nrese_reasoner::{ReasonerConfig, ReasoningMode};

    let rl = ReasoningMode::Owl2Rl;
    for (length, axiom, data, answers) in [
        (
            257,
            "owl:intersectionOf",
            "ex:a rdf:type ex:p",
            "ex:a rdf:type ex:out\nex:out rdfs:subClassOf ex:p",
        ),
        (
            251,
            "owl:propertyChainAxiom",
            "ex:a ex:p ex:b",
            "ex:a ex:out ex:b",
        ),
    ] {
        let mut input = generator::long_chain(length).writes.remove(0);
        input.insert[0][1] = axiom.into();
        input
            .insert
            .extend(insert(&format!("ex:c0 rdf:rest rdf:nil\n{data}")).insert);
        let answers = generator::rows(answers);
        let model = oracle::evaluate(
            &input.insert.iter().cloned().collect(),
            oracle::Datatypes::Standard,
        );
        assert!(answers.iter().all(|answer| model.facts.contains(answer)));

        for &mode in judges::MODES {
            let store = judges::open(mode, None);
            let raw = judges::pipeline(&store, ReasoningMode::Disabled);
            assert!(judges::apply(&raw, &insert("rdf:nil ex:marker ex:seed")));
            assert!(judges::apply(&raw, &input));
            let snapshot = store.read_snapshot(None).unwrap();
            let id = |name| {
                let iri = nrese_rdf::NamedNodeRef::new(name).unwrap();
                snapshot.lookup(iri.into()).unwrap().raw()
            };
            assert!(id(nrese_rdf::vocab::rdf::NIL.as_str()) < id("http://example.org/c1"));
            let _reader = judges::pipeline(&store, rl);
            let program = ReasonerConfig::for_mode(rl).materialised_program().unwrap();
            let report = store.rematerialise(program).unwrap();
            for answer in &answers {
                assert!(judges::ask(&store, answer).0, "{mode}: missing {answer:?}");
            }
            assert!(
                report.diagnostics.iter().any(|d| d.kind == "list-too-long"),
                "{mode}: {axiom} must report its oversized walk"
            );
        }
    }
}

#[test]
fn accepting_walk_survives_incomplete_branches() {
    // Spec: Table 3 requires a first and a rest at each position. Table 5
    // prp-spo2 still derives the row from the branch with both.
    check(
        case(
            "incomplete-branches",
            ["ex:a", "ex:out", "ex:c"],
            vec![chain_with(
                "
                ex:h rdf:rest ex:noFirst
                ex:noFirst rdf:rest rdf:nil
                ex:h rdf:rest ex:noRest
                ex:noRest rdf:first ex:r
                ",
            )],
        ),
        &[true],
        &[true],
    );
}

#[test]
fn accepting_walk_survives_a_side_loop() {
    // Spec: Table 3 matches walks ending at rdf:nil. A branch with no exit
    // contributes no walk and leaves the Table 5 prp-spo2 result intact.
    check(
        case(
            "side-loop",
            ["ex:a", "ex:out", "ex:c"],
            vec![chain_with(
                "
                ex:h rdf:rest ex:loop
                ex:loop rdf:first ex:r
                ex:loop rdf:rest ex:loop
                ",
            )],
        ),
        &[true],
        &[true],
    );
}

#[test]
fn accepting_walk_is_maintained_when_a_branch_changes() {
    // Spec: Tables 3 and 5 keep the first branch's conclusion through all
    // writes. Completing the other branch adds its conclusion until its
    // last edge is deleted, including after the store has been reopened.
    let mut fixture = case(
        "edit-incomplete-branch",
        ["ex:a", "ex:out", "ex:c"],
        vec![
            chain_with(
                "
                ex:h rdf:rest ex:branch
                ex:branch rdf:first ex:r
                ex:b ex:r ex:d
                ",
            ),
            delete("ex:h rdf:rest ex:branch"),
            insert("ex:h rdf:rest ex:branch"),
            insert("ex:branch rdf:rest rdf:nil"),
            delete("ex:branch rdf:rest rdf:nil"),
        ],
    );
    let accepted = &[true; 5];
    let kept = matches_oracle(&fixture, accepted, &[true; 5]);
    fixture.watch = crate::triple(["ex:a", "ex:out", "ex:d"]);
    let changed = matches_oracle(&fixture, accepted, &[false, false, false, true, false]);
    assert!(kept && changed, "branch edits differ from the oracle");
}

#[test]
fn accepting_walk_requires_a_first_at_every_cell() {
    // Spec: Table 3 requires rdf:first even on the last cell. With no
    // alternative walk, Table 5 derives no chain row and the axiom is diagnosed.
    let fixture = case(
        "missing-first-only-walk",
        ["ex:a", "ex:out", "ex:c"],
        vec![insert(
            "
            ex:out owl:propertyChainAxiom ex:h
            ex:h rdf:first ex:p
            ex:h rdf:rest ex:t
            ex:t rdf:rest rdf:nil
            ex:a ex:p ex:b
            ex:b ex:q ex:c
            ",
        )],
    );
    assert!(matches_oracle(&fixture, &[true], &[false]));
    for &mode in judges::MODES {
        let store = judges::open(mode, None);
        let pipeline = judges::pipeline(&store, nrese_reasoner::ReasoningMode::Owl2Rl);
        assert!(judges::apply(&pipeline, &fixture.writes[0]));
        let report = pipeline.last_reasoning_run().expect("reasoning ran");
        assert_eq!(
            report.diagnostics_total, 1,
            "{mode}: {:?}",
            report.diagnostics
        );
        let diagnostic = &report.diagnostics[0];
        assert_eq!(diagnostic.kind, "malformed-list", "{mode}");
        assert_eq!(diagnostic.node.as_deref(), Some("http://example.org/t"));
    }
}

#[test]
#[ignore = "documented limit reasoning-semantics.md:63-65: asserting an entailed shortcut changes the first load's chain answer"]
fn redundant_list_premise_leaves_chain_unchanged() {
    // Spec: Table 9 scm-spo and Table 5 prp-spo1 and prp-spo2 give the same
    // chain row with or without the entailed subproperty.
    check(
        generator::redundant(64),
        &[true, true, true],
        &[true, true, true],
    );
}

#[test]
#[ignore = "documented limit reasoning-semantics.md:68: a back edge hides an accepting list walk until the chain is rebuilt correctly"]
fn cycle_keeps_its_exiting_walk() {
    // Spec: Table 3 and Table 5 prp-spo2 give a chain consequence while the
    // exiting walk and both data edges exist, including with a back edge.
    check(
        case(
            "cycle",
            ["ex:a", "ex:out", "ex:c"],
            vec![
                insert(
                    "
            ex:out owl:propertyChainAxiom ex:h
            ex:h rdf:first ex:p
            ex:h rdf:rest ex:t
            ex:t rdf:first ex:q
            ex:t rdf:rest rdf:nil
            ex:a ex:p ex:b
            ex:b ex:q ex:c
        ",
                ),
                insert("ex:t rdf:rest ex:h"),
                delete("ex:b ex:q ex:c"),
            ],
        ),
        &[true, true, true],
        &[true, true, false],
    );
}

#[test]
#[ignore = "undocumented: a chain over rdfs:subClassOf misses a new subclass edge"]
fn schema_operand_insert_derives_chain_row() {
    // Spec: Table 5 prp-spo2 uses rdfs:subClassOf just like any other list operand.
    check(
        generator::schema_operand(true),
        &[true, true],
        &[false, true],
    );
}

#[test]
#[ignore = "undocumented: a chain over rdfs:subClassOf keeps the result of a deleted subclass edge"]
fn schema_operand_delete_retracts_chain_row() {
    // Spec: Table 5 prp-spo2 cannot derive the chain row once its only first edge is gone.
    check(
        generator::schema_operand(false),
        &[true, true],
        &[true, false],
    );
}

#[test]
#[ignore = "undocumented: the diagonal is missed in an owl:sameAs chain operand"]
fn same_as_operand_makes_a_disjoint_chain_inconsistent() {
    // Spec: Table 4 eq-ref and Table 5 prp-spo2 copy ex:p through the chain;
    // Table 5 prp-pdw rejects the copied row because the properties are disjoint.
    check(
        case(
            "equality-member",
            ["ex:a", "ex:out", "ex:b"],
            vec![
                insert(
                    "
            ex:out owl:propertyChainAxiom ex:h
            ex:out owl:propertyDisjointWith ex:p
            ex:h rdf:first owl:sameAs
            ex:h rdf:rest ex:t
            ex:t rdf:first ex:p
            ex:t rdf:rest rdf:nil
        ",
                ),
                insert("ex:a ex:p ex:b"),
            ],
        ),
        &[true, false],
        &[false, true],
    );
}

#[test]
#[ignore = "documented limit reasoning-semantics.md:63-65: a 251-cell property chain loses its endpoint row"]
fn long_property_chain_reaches_its_endpoint() {
    // Spec: Table 3 and Table 5 prp-spo2 derive the row for a 251-member walk.
    check(generator::long_chain(251), &[true, true], &[false, true]);
}

#[test]
#[ignore = "undocumented: declaring owl:sameAs irreflexive is accepted despite its diagonal"]
fn irreflexive_same_as_is_inconsistent() {
    // Spec: Table 4 eq-ref gives the diagonal, which Table 5 prp-irp forbids.
    check(
        case(
            "equality-schema",
            ["ex:a", "owl:sameAs", "ex:a"],
            vec![
                insert("ex:a ex:p ex:b"),
                insert("owl:sameAs rdf:type owl:IrreflexiveProperty"),
            ],
        ),
        &[true, false],
        &[true, true],
    );
}

#[test]
#[ignore = "documented limit reasoning-semantics.md:74: a later write loses equality derived by a functional property between literal forms"]
fn functional_literal_equality_reaches_later_writes() {
    // Spec: Table 5 prp-fp equates the two forms; Table 4 eq-rep-o copies
    // either one into the later row. Table 8 dt-diff sees one data value.
    let mut matched = true;
    for (written, read) in [
        (generator::INTEGER, generator::DECIMAL),
        (generator::DECIMAL, generator::INTEGER),
    ] {
        matched &= matches_oracle(
            &case(
                "literal-functional",
                ["ex:t", "ex:q", read],
                vec![
                    insert(&format!(
                        "ex:p rdf:type owl:FunctionalProperty\nex:s ex:p {written}\nex:s ex:p {read}"
                    )),
                    insert(&format!("ex:t ex:q {written}")),
                ],
            ),
            &[true, true],
            &[false, true],
        );
    }
    assert!(matched, "functional literal forms differ from the oracle");
}
