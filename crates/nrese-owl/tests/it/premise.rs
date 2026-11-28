//! The premise of the OWL 2 RL rules' closure (theorem PR1, OWL 2 Profiles §4.3: the graph is
//! exactly the RDF mapping of the ontology read), told in one place
//! (`nrese_owl::profile::premise`).

use nrese_owl::model::EntityKind;
use nrese_owl::profile::{Premise, premise};
use nrese_owl::{Diagnostic, Ontology, Treatment};

/// Spec: PR1 excludes annotation-property axioms; each part of a read ontology its docs name
/// as one the rules read otherwise (an empty intersection, a list's cell outside its list, a
/// name of two kinds an axiom reads, an unused structure the rules read more of) or not at
/// all (a cardinality spelled otherwise, a statement the rules don't read, a qualified data
/// maximum) breaks the premise that way, alone; a diagnostic past which no model speaks
/// (a reserved name) is fatal; a part that breaks nothing (an annotation use, a bare name of
/// two kinds, an undeclared property) leaves it exact, and so does an ontology with none.
#[test]
fn each_part_that_breaks_the_rules_premise_breaks_it_alone() {
    let two_kinds = |by_axiom, treatment| Diagnostic::TwoKinds {
        term: 1,
        kinds: [EntityKind::ObjectProperty, EntityKind::DataProperty],
        by_axiom,
        treatment,
    };
    // What is set alone; whether it is fatal, read otherwise, read less.
    type Case = (&'static str, Box<dyn Fn(&mut Ontology)>, [bool; 3]);
    let cases: Vec<Case> = vec![
        ("nothing", Box::new(|_| {}), [false, false, false]),
        (
            "an annotation-property axiom",
            Box::new(|o| o.annotation_schema.push(1)),
            [false, true, false],
        ),
        (
            "an empty intersection",
            Box::new(|o| o.empty_list = Some(1)),
            [false, true, false],
        ),
        (
            "a list's cell outside its list",
            Box::new(|o| o.list_cell_outside = Some(1)),
            [false, true, false],
        ),
        (
            "a cardinality spelled otherwise",
            Box::new(|o| o.noncanonical_cardinality = Some(1)),
            [false, false, true],
        ),
        (
            "a statement the rules don't read",
            Box::new(|o| o.rules_unread = Some(1)),
            [false, false, true],
        ),
        (
            "a qualified data maximum",
            Box::new(|o| o.qualified_data_max = Some(1)),
            [false, false, true],
        ),
        (
            "an annotation use",
            Box::new(|o| {
                o.annotation_uses.insert(1);
            }),
            [false, false, false],
        ),
        (
            "a name of two kinds an axiom reads",
            Box::new(move |o| o.diagnostics.push(two_kinds(true, Treatment::Fatal))),
            [true, true, false],
        ),
        (
            "a bare name of two kinds",
            Box::new(move |o| o.diagnostics.push(two_kinds(false, Treatment::Counted))),
            [false, false, false],
        ),
        (
            "an unused structure the rules read more of",
            Box::new(|o| {
                o.diagnostics.push(Diagnostic::Unused {
                    node: 1,
                    reads_more: true,
                    treatment: Treatment::OffRoute,
                })
            }),
            [false, true, false],
        ),
        (
            "a reserved name",
            Box::new(|o| o.diagnostics.push(Diagnostic::Reserved { term: 1 })),
            [true, false, false],
        ),
        (
            "an undeclared property",
            Box::new(|o| (o.diagnostics).push(Diagnostic::UndeclaredProperty { property: 1 })),
            [false, false, false],
        ),
    ];
    let mut wrong = Vec::new();
    for (what, set, [fatal, reads_otherwise, reads_less]) in cases {
        let mut o = Ontology::default();
        set(&mut o);
        let expected = Premise {
            fatal,
            reads_otherwise,
            reads_less,
        };
        let told = premise(&o);
        if told != expected || told.exact() != (expected == Premise::default()) {
            wrong.push(format!("{what}: {told:?}, expected {expected:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
