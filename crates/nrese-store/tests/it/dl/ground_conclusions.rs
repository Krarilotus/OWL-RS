//! Ground conclusions under `owl2-dl`: the rules show one only by finding each of its
//! statements in the closure; whatever else the statements say is the DL check's to decide.
//!
//! Entailment is of the whole conclusion, every axiom its graph maps to (*OWL 2 Direct
//! Semantics* §2.5; *OWL 2 Mapping to RDF Graphs* §3). A statement the store's check reads
//! as something else, or sets aside, is not shown.

use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::dl::entailment::Entailed;

use super::entailment::turtle;
use super::{insert, pipeline};

/// Both checks of `conclusion` by a store under `owl2-dl` with `premise` committed.
fn checks(premise: &str, conclusion: &str) -> (String, Entailed) {
    let dl = pipeline();
    insert(&dl, premise).expect("premise");
    let conclusion = turtle(conclusion);
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .expect("owl2-dl maintains the OWL 2 RL closure");
    let by_store = format!("{:?}", dl.store().entails(rules, &conclusion));
    let by_dl = dl.store().entails_dl(&conclusion).expect("entailment");
    (by_store, by_dl.answer)
}

/// Spec (module docs; *Mapping* §3: a statement about the ontology's node with an object
/// property is an assertion of it): `:ont :p :b` beside the header is part of the
/// conclusion, and the premise doesn't say it.
#[test]
fn a_statement_beside_the_ontology_header_is_part_of_the_conclusion() {
    assert_eq!(
        checks(
            ":p a owl:ObjectProperty .",
            ":p a owl:ObjectProperty . :ont a owl:Ontology ; :p :b ."
        ),
        ("Ok(NotEntailed)".to_owned(), Entailed::No),
        "entails under owl2-dl, entails_dl"
    );
}

/// Spec (module docs): `text:query` is a data property here like any other, and the
/// premise gives `:a` no value of it; a full-text match of `:a`'s label isn't one.
#[test]
fn a_ground_statement_is_looked_up_not_evaluated() {
    assert_eq!(
        checks(
            ":label a owl:DatatypeProperty . <http://jena.apache.org/text#query> a \
             owl:DatatypeProperty . :a :label \"needle\" .",
            ":a <http://jena.apache.org/text#query> \"needle\" ."
        ),
        ("Ok(NotEntailed)".to_owned(), Entailed::No),
        "entails under owl2-dl, entails_dl"
    );
}
