use nrese_owl::{Axiom, Diagnostic, read};

use super::{Table, load};

/// OWL 2 Structural Specification §11.2 requires each datatype to be built in
/// or defined once without cycles, and restricts the top data property to a
/// superproperty position. Diagnostics identify the restriction that broke.
#[test]
fn datatype_rules_outside_owl2_dl_are_named() {
    let cases = [
        (
            "two definitions",
            "ex:dt a rdfs:Datatype ; owl:equivalentClass xsd:integer, xsd:string .",
            "multiple definitions of a datatype",
        ),
        (
            "cycle",
            "ex:d1 a rdfs:Datatype ; owl:equivalentClass ex:d2 .
             ex:d2 a rdfs:Datatype ; owl:equivalentClass
                 [ a rdfs:Datatype ; owl:unionOf ( ex:d1 xsd:string ) ] .",
            "cyclic datatype definition",
        ),
        (
            "undefined range",
            "ex:d a owl:DatatypeProperty ; rdfs:range ex:dt . ex:dt a rdfs:Datatype .",
            "undefined datatype",
        ),
        (
            "definition of a built-in",
            "rdf:type a owl:Class .
             xsd:integer a rdfs:Datatype ; owl:equivalentClass xsd:string .",
            "definition of a built-in datatype",
        ),
        (
            "top data assertion",
            "ex:x owl:topDataProperty \"v\" .",
            "owl:topDataProperty outside the superproperty position",
        ),
        (
            "undefined declaration",
            "ex:dt a rdfs:Datatype .",
            "undefined datatype",
        ),
        ("built-in declaration", "xsd:integer a rdfs:Datatype .", ""),
        (
            "top data superproperty",
            "ex:d a owl:DatatypeProperty ; rdfs:subPropertyOf owl:topDataProperty .",
            "",
        ),
    ];
    let mut wrong = Vec::new();
    for (name, data, expected) in cases {
        let mut terms = Table::default();
        let input = load(&mut terms, data);
        let ontology = read(&input, &terms);
        let fatal: Vec<_> = ontology
            .diagnostics
            .iter()
            .filter(|d| d.is_fatal())
            .map(|d| d.describe(&|t| terms.name(t)))
            .collect();
        let correct = if expected.is_empty() {
            fatal.is_empty()
        } else {
            fatal.iter().any(|d| d.contains(expected))
        };
        if !correct {
            wrong.push(format!("{name}: expected {expected:?}, got {fatal:?}"));
        } else if name == "definition of a built-in" {
            let axiom = ontology
                .diagnostics
                .iter()
                .find_map(|diagnostic| match *diagnostic {
                    Diagnostic::GlobalRestriction { term, axiom, what } if what == expected => {
                        Some((&ontology.axioms[axiom], term))
                    }
                    _ => None,
                });
            assert!(
                matches!(
                    axiom,
                    Some((Axiom::DatatypeDefinition(datatype, _), term)) if *datatype == term
                ),
                "{axiom:?}"
            );
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
