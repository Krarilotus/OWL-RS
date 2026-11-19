use crate::{
    generator::{self, Case, case, delete, insert},
    sweep,
};

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
#[ignore = "asserting an entailed shortcut changes the first load's chain answer"]
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
#[ignore = "a back edge hides an accepting list walk until the chain is rebuilt correctly"]
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
#[ignore = "a chain over rdfs:subClassOf misses a new subclass edge"]
fn schema_operand_insert_derives_chain_row() {
    // Spec: Table 5 prp-spo2 uses rdfs:subClassOf just like any other list operand.
    check(
        generator::schema_operand(true),
        &[true, true],
        &[false, true],
    );
}

#[test]
#[ignore = "a chain over rdfs:subClassOf keeps the result of a deleted subclass edge"]
fn schema_operand_delete_retracts_chain_row() {
    // Spec: Table 5 prp-spo2 cannot derive the chain row once its only first edge is gone.
    check(
        generator::schema_operand(false),
        &[true, true],
        &[true, false],
    );
}

#[test]
#[ignore = "the diagonal is missed in an owl:sameAs chain operand"]
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
#[ignore = "a 251-cell property chain loses its endpoint row"]
fn long_property_chain_reaches_its_endpoint() {
    // Spec: Table 3 and Table 5 prp-spo2 derive the row for a 251-member walk.
    check(generator::long_chain(251), &[true, true], &[false, true]);
}

#[test]
#[ignore = "declaring owl:sameAs irreflexive is accepted despite its diagonal"]
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
#[ignore = "a later write loses equality derived by a functional property between literal forms"]
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
