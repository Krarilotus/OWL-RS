use nrese_owl::{Diagnostic, read};

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
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// OWL 2 Structural Specification §4 and §7.5 require the pair and the
/// literal's value to belong to the permitted facet space. An impossible pair
/// can be rejected even when the literal exceeds the value owner's capacity.
#[test]
fn facets_outside_their_facet_space_are_named() {
    let large = "\"170141183460469231731687303715884105728\"^^xsd:integer";
    let rejected = Some(("facet is not applicable", true));
    let cases = [
        ("xsd:string", "minInclusive", "5", rejected),
        ("owl:real", "minInclusive", "\"low\"", rejected),
        ("xsd:string", "minInclusive", large, rejected),
        (
            "xsd:integer",
            "minInclusive",
            large,
            Some(("facet membership undecided", false)),
        ),
        ("xsd:string", "maxLength", "3", None),
        ("xsd:integer", "minInclusive", "5", None),
    ];
    let mut wrong = Vec::new();
    for (datatype, facet, value, expected) in cases {
        let mut terms = Table::default();
        let input = load(
            &mut terms,
            &format!(
                "ex:d a owl:DatatypeProperty ; rdfs:range [
                 a rdfs:Datatype ; owl:onDatatype {datatype} ;
                 owl:withRestrictions ( [ xsd:{facet} {value} ] ) ] ."
            ),
        );
        let ontology = read(&input, &terms);
        let actual = ontology.diagnostics.iter().find_map(|d| {
            let text = d.describe(&|t| terms.name(t));
            ["facet is not applicable", "facet membership undecided"]
                .into_iter()
                .find(|what| text.contains(what))
                .map(|what| (what, Diagnostic::is_fatal(d)))
        });
        if actual != expected {
            wrong.push(format!(
                "{datatype} {facet} {value}: expected {expected:?}, got {actual:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
