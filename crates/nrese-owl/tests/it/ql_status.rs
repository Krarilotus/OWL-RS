//! What the reader does with an axiom, and the QL rewriting's claim on it: every diagnostic
//! under which the reader leaves out or alters an axiom makes the compiled TBox's hazard
//! global, its reason the reader's own words, so no read over it is told `complete`; a kind
//! under which every axiom is read as the source states it raises none. One cell per kind,
//! each a small ontology that raises it.

use nrese_owl::ql::{Closure, Tbox};
use nrese_owl::{Diagnostic, Ontology, read, read_functional};

use super::{Table, load};

/// A source: Turtle (with the shared prefixes) or a functional-syntax document's axioms.
enum Source {
    Turtle(&'static str),
    Functional(&'static str),
}

/// A cell: the kind it raises (its name and a test for it), its source, and whether the
/// rule makes it a hazard.
struct Cell {
    kind: &'static str,
    is: fn(&Diagnostic) -> bool,
    source: Source,
    hazard: bool,
}

fn cells() -> Vec<Cell> {
    use Source::{Functional, Turtle};
    vec![
        Cell {
            kind: "Malformed",
            is: |d| matches!(d, Diagnostic::Malformed { .. }),
            source: Turtle(
                "ex:p a owl:ObjectProperty .
                 ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ] .",
            ),
            hazard: true,
        },
        Cell {
            kind: "SharedBlankNode",
            is: |d| matches!(d, Diagnostic::SharedBlankNode { .. }),
            source: Turtle(
                "ex:r a owl:ObjectProperty .
                 _:s a owl:Restriction ; owl:onProperty ex:r ; owl:someValuesFrom ex:C .
                 ex:E rdfs:subClassOf _:s . ex:F rdfs:subClassOf _:s .",
            ),
            hazard: false,
        },
        Cell {
            kind: "BrokenList",
            is: |d| matches!(d, Diagnostic::BrokenList { .. }),
            source: Turtle(
                "ex:B rdfs:subClassOf [ a owl:Class ; owl:unionOf _:l ] . _:l rdf:first ex:C .",
            ),
            hazard: true,
        },
        Cell {
            kind: "UndeclaredProperty",
            is: |d| matches!(d, Diagnostic::UndeclaredProperty { .. }),
            source: Turtle("ex:x ex:undeclared ex:y ."),
            hazard: false,
        },
        Cell {
            kind: "AmbiguousProperty",
            is: |d| matches!(d, Diagnostic::AmbiguousProperty { .. }),
            source: Turtle(
                "ex:p a owl:ObjectProperty , owl:DatatypeProperty . ex:p rdfs:domain ex:C .",
            ),
            hazard: true,
        },
        Cell {
            kind: "Unsupported",
            is: |d| matches!(d, Diagnostic::Unsupported { .. }),
            source: Turtle(
                "ex:d a owl:DatatypeProperty .
                 ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperties ( ex:d ) ;
                     owl:someValuesFrom rdfs:Literal ] .",
            ),
            hazard: true,
        },
        Cell {
            kind: "NotOwl",
            is: |d| matches!(d, Diagnostic::NotOwl { .. }),
            source: Turtle("ex:p a owl:ObjectProperty . ex:H owl:onProperty ex:p ."),
            hazard: true,
        },
        Cell {
            kind: "Unused",
            is: |d| matches!(d, Diagnostic::Unused { .. }),
            source: Turtle(
                "ex:p a owl:ObjectProperty .
                 [ a owl:Restriction ; owl:onProperty ex:p ; owl:someValuesFrom ex:C ] .",
            ),
            hazard: false,
        },
        Cell {
            kind: "NonSimpleProperty",
            is: |d| matches!(d, Diagnostic::NonSimpleProperty { .. }),
            source: Turtle(
                "ex:t a owl:ObjectProperty , owl:TransitiveProperty , owl:IrreflexiveProperty .",
            ),
            hazard: false,
        },
        Cell {
            kind: "AnonymousIndividual",
            is: |d| matches!(d, Diagnostic::AnonymousIndividual { .. }),
            source: Turtle(
                "ex:p a owl:ObjectProperty .
                 ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ;
                     owl:hasValue _:b ] .",
            ),
            hazard: false,
        },
        Cell {
            kind: "Syntax",
            is: |d| matches!(d, Diagnostic::Syntax { .. }),
            source: Functional("SubClassOf(:A :B) SubClassOf(:C"),
            hazard: true,
        },
        Cell {
            kind: "RepeatedOperand",
            is: |d| matches!(d, Diagnostic::RepeatedOperand),
            source: Turtle("ex:A a owl:Class ; owl:disjointWith ex:A ."),
            hazard: false,
        },
        Cell {
            kind: "Reserved",
            is: |d| matches!(d, Diagnostic::Reserved { .. }),
            source: Turtle("ex:A rdfs:subClassOf [ owl:intersectionOf ( rdf:List ex:C ) ] ."),
            hazard: true,
        },
        Cell {
            kind: "Rule",
            is: |d| matches!(d, Diagnostic::Rule { .. }),
            source: Functional(
                "DLSafeRule(Body(ClassAtom(:A Variable(:x))) Head(ClassAtom(:B Variable(:x))))",
            ),
            hazard: true,
        },
    ]
}

fn ontology(table: &mut Table, source: &Source) -> Ontology {
    match source {
        Source::Turtle(turtle) => {
            let statements = load(table, turtle);
            read(&statements, &*table)
        }
        Source::Functional(axioms) => {
            let text = format!(
                "Prefix(:=<http://example.com/>)\nOntology(<http://example.com/o>\n{axioms}\n)"
            );
            read_functional(&text, table).0
        }
    }
}

/// Spec (the rule, `nrese_owl::Diagnostic` and the QL TBox, I24c-2): every diagnostic kind
/// the reader has, raised by a small ontology under `owl2-ql`'s closure. Where the reader
/// leaves an axiom out or alters it (a structure it can't read, OWL vocabulary outside the
/// mapping, text not read, a reserved name used as an entity, a property declared both an
/// object and a data property, a rule beside the ontology), the TBox's hazard is global and
/// says the reader's own words for one of them. Where it reads every axiom as the source
/// states it (a shared blank node, an undeclared property typed by its use, a structure no
/// axiom uses, a non-simple property, an anonymous individual in `hasValue`, a disjointness
/// repeating an operand), it raises no global hazard.
#[test]
fn every_axiom_the_reader_leaves_out_or_alters_lowers_the_rewritings_claim() {
    let mut wrong = Vec::new();
    for cell in cells() {
        let mut table = Table::default();
        let o = ontology(&mut table, &cell.source);
        let raised: Vec<&Diagnostic> = o.diagnostics.iter().filter(|&d| (cell.is)(d)).collect();
        if raised.is_empty() {
            wrong.push(format!("{}: not raised: {:?}", cell.kind, o.diagnostics));
            continue;
        }
        let thing = nrese_owl::Terms::iri(&table, "http://www.w3.org/2002/07/owl#Thing");
        let tbox = Tbox::compile(&o, Closure { lists: false }, thing);
        let name = |t| table.name(t);
        let global = tbox.global_hazard().map(|h| h.describe(&name));
        match (cell.hazard, &global) {
            (true, Some(why)) if raised.iter().any(|d| why.contains(&d.describe(&name))) => {}
            (false, None) => {}
            _ => wrong.push(format!(
                "{}: expected a hazard: {}, the global one: {global:?}, the reader's: {:?}",
                cell.kind, cell.hazard, o.diagnostics
            )),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
