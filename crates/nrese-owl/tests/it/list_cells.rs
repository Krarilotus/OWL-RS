//! A list's cell is named only by its list (*OWL 2 Mapping to RDF Graphs* §3: the blank nodes
//! of a sequence occur only in that sequence). A statement outside the list that names a cell
//! may make the OWL 2 RL rules equate it with another node, and give it two members or two
//! rests: the rules then read sequences the graph doesn't write.

use nrese_owl::profile::rules_read_differently;
use nrese_owl::{Diagnostic, Term, Terms, read};

use super::{Table, load};

const WHAT: &str = "a list's cell used outside its list";

const PROPERTIES: &str = "ex:p a owl:ObjectProperty . ex:r a owl:ObjectProperty . \
     ex:q1 a owl:ObjectProperty . ex:q2 a owl:ObjectProperty .";

/// The chain `ex:p ← ex:q1 ∘ ex:q2` written cell by cell, `_:l1` and `_:l2`.
const CHAIN: &str = "ex:p owl:propertyChainAxiom _:l1 . _:l1 rdf:first ex:q1 ; rdf:rest _:l2 . \
     _:l2 rdf:first ex:q2 ; rdf:rest rdf:nil .";

/// `turtle` read: the cells reported used outside their lists, each by its `rdf:first` member
/// as [`Table::name`] writes it, sorted; whether the rules read the ontology differently; and
/// every diagnostic.
fn reported(turtle: &str) -> (Vec<String>, bool, Vec<Diagnostic>) {
    let mut table = Table::default();
    let statements = load(&mut table, turtle);
    let ontology = read(&statements, &table);
    let first = table.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#first");
    let member = |cell: Term| {
        (statements.iter())
            .find(|s| s.triple[0] == cell && Some(s.triple[1]) == first)
            .map_or("no member".to_owned(), |s| table.name(s.triple[2]))
    };
    let mut cells: Vec<String> = (ontology.diagnostics.iter())
        .filter_map(|d| match d {
            Diagnostic::Malformed { node, what } if *what == WHAT => Some(member(*node)),
            _ => None,
        })
        .collect();
    cells.sort();
    let differently = rules_read_differently(&ontology);
    (cells, differently, ontology.diagnostics)
}

/// Spec (*Mapping to RDF* §3; OWL 2 Profiles §4.3, `eq-rep-s` and `eq-rep-o`): each case
/// names a cell outside its list, and each such cell is reported once, by its member, and the
/// rules read the ontology differently (`profile::rules_read_differently`):
/// - the chain's second cell the same as its first, a cycle the rules read as every word of
///   `ex:q1` and `ex:q2`: both cells;
/// - a second list whose cells are each the same as the chain's (the store's `variants`
///   shape): the chain's two cells;
/// - one list the chain of two axioms: its head, the object of two statements;
/// - an annotation on a cell: that cell.
#[test]
fn a_cell_a_statement_outside_its_list_names_is_reported() {
    let second = "_:m1 rdf:first ex:r ; rdf:rest _:m2 . _:m2 rdf:first ex:p ; rdf:rest rdf:nil . \
                  _:m1 owl:sameAs _:l1 . _:m2 owl:sameAs _:l2 .";
    let cases = [
        ("_:l2 owl:sameAs _:l1 .", &["ex:q1", "ex:q2"][..]),
        (second, &["ex:q1", "ex:q2"]),
        ("ex:r owl:propertyChainAxiom _:l1 .", &["ex:q1"]),
        ("_:l1 rdfs:comment \"x\" .", &["ex:q1"]),
    ];
    let mut wrong = Vec::new();
    for (outside, expected) in cases {
        let turtle = format!("{PROPERTIES} {CHAIN} {outside}");
        let (cells, differently, diagnostics) = reported(&turtle);
        eprintln!("CELL {outside:?}: reported {cells:?}; read differently {differently}");
        if cells != expected || !differently {
            wrong.push(format!(
                "{outside}: {cells:?} {differently}; {diagnostics:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (*Mapping to RDF* §2.3.1, Table 17: an annotated axiom's reification is a blank node
/// typed `owl:Axiom`, its one source, property and target the axiom's main triple): only such
/// a reification of a list's axiom may name the list's head as its `owl:annotatedTarget`. Each
/// case names a cell otherwise, and the cell is reported, by its member:
/// - the annotated chain's reification typed `owl:Annotation`: the head;
/// - the same untyped: the head;
/// - a reification of `_:l1 rdf:rest _:l2`, a predecessor's `rdf:rest`, not an axiom: `_:l2`,
///   its target, and `_:l1`, its source.
#[test]
fn an_annotated_target_names_a_cell_only_as_its_axioms_reification() {
    let chain = "owl:annotatedSource ex:p ; owl:annotatedProperty owl:propertyChainAxiom ; \
                 owl:annotatedTarget _:l1 ; rdfs:comment \"why\" .";
    let cases = [
        (format!("[] a owl:Annotation ; {chain}"), &["ex:q1"][..]),
        (format!("[] {chain}"), &["ex:q1"]),
        (
            "[] a owl:Axiom ; owl:annotatedSource _:l1 ; owl:annotatedProperty rdf:rest ; \
             owl:annotatedTarget _:l2 ; rdfs:comment \"why\" ."
                .to_owned(),
            &["ex:q1", "ex:q2"],
        ),
    ];
    let mut wrong = Vec::new();
    for (outside, expected) in &cases {
        let (cells, differently, diagnostics) =
            reported(&format!("{PROPERTIES} {CHAIN} {outside}"));
        eprintln!("CELL TARGET {outside:?}: reported {cells:?}; read differently {differently}");
        if cells != *expected || !differently {
            wrong.push(format!(
                "{outside}: {cells:?} {differently}; {diagnostics:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The valid neighbours get no report at all, and the rules read them as OWL 2 DL does: two
/// chains with a list each; `owl:sameAs` between named individuals beside a chain; an
/// annotated axiom whose main triple's object is a union (not a cell); an annotated chain, key
/// and disjoint union, whose reifications name the list's head as their `owl:annotatedTarget`
/// (*Mapping to RDF* §2.3.2: the reification repeats the main triple); a list whose cells are typed `rdf:List`
/// (as RDF/XML's `rdf:List` elements write it, W3C test WebOnt-I5.5-005); a list of 300
/// members.
#[test]
fn the_valid_neighbours_of_a_cell_outside_its_list_get_no_report() {
    let members: Vec<String> = (1..=300).map(|i| format!("ex:A{i}")).collect();
    let long = format!(
        "ex:C owl:equivalentClass [ owl:unionOf ( {} ) ] .",
        members.join(" ")
    );
    let cases = [
        format!("{PROPERTIES} {CHAIN} ex:r owl:propertyChainAxiom ( ex:q1 ex:q2 ) ."),
        format!("{PROPERTIES} {CHAIN} ex:a owl:sameAs ex:b ."),
        "ex:A rdfs:subClassOf _:u . _:u owl:unionOf ( ex:B ex:C ) . [] a owl:Axiom ; \
         owl:annotatedSource ex:A ; owl:annotatedProperty rdfs:subClassOf ; \
         owl:annotatedTarget _:u ; rdfs:comment \"why\" ."
            .to_owned(),
        format!(
            "{PROPERTIES} {CHAIN} [] a owl:Axiom ; owl:annotatedSource ex:p ; \
             owl:annotatedProperty owl:propertyChainAxiom ; owl:annotatedTarget _:l1 ; \
             rdfs:comment \"why\" ."
        ),
        format!(
            "{PROPERTIES} ex:K owl:hasKey _:k1 . _:k1 rdf:first ex:q1 ; rdf:rest rdf:nil . \
             [] a owl:Axiom ; owl:annotatedSource ex:K ; owl:annotatedProperty owl:hasKey ; \
             owl:annotatedTarget _:k1 ; rdfs:comment \"why\" ."
        ),
        "ex:U owl:disjointUnionOf _:d1 . _:d1 rdf:first ex:A ; rdf:rest _:d2 . \
         _:d2 rdf:first ex:B ; rdf:rest rdf:nil . [] a owl:Axiom ; owl:annotatedSource ex:U ; \
         owl:annotatedProperty owl:disjointUnionOf ; owl:annotatedTarget _:d1 ; \
         rdfs:comment \"why\" ."
            .to_owned(),
        "ex:C owl:equivalentClass [ owl:unionOf _:l1 ] . _:l1 a rdf:List ; rdf:first ex:A ; \
         rdf:rest _:l2 . _:l2 a rdf:List ; rdf:first ex:B ; rdf:rest rdf:nil ."
            .to_owned(),
        long,
    ];
    let mut wrong = Vec::new();
    for turtle in &cases {
        let (cells, differently, diagnostics) = reported(turtle);
        let shown: String = turtle.chars().take(160).collect();
        eprintln!("CELL NEIGHBOUR {shown:?}: {diagnostics:?}; read differently {differently}");
        if !diagnostics.is_empty() || differently {
            wrong.push(format!("{shown}: {cells:?} {differently}; {diagnostics:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
