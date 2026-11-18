//! A class assertion of an anonymous class expression under the OWL 2 QL rewriting: OWL 2 QL
//! allows only a named class there (OWL 2 Profiles §3.2.4), so a read whose answer it can
//! change is told complete only where the closure's rules apply it.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, QlRewritingMode, Requester,
    SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

use crate::support::in_memory_store_config;

const PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

/// The answer to `ASK { question }` after one commit of `data`, and whether the read is
/// told complete; `None` where the commit is refused.
fn ask(
    mode: ReasoningMode,
    ql: QlRewritingMode,
    equality: &str,
    data: &str,
    question: &str,
) -> Option<(bool, bool)> {
    let store = StoreService::new(StoreConfig {
        equality_by_representatives: equality != "replicate",
        equality_compact: equality == "compact",
        ql_rewriting: ql,
        ..in_memory_store_config()
    })
    .expect("store");
    let pipeline = MutationPipeline::new(
        Arc::new(store.clone()),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    let update = format!("{PREFIXES}INSERT DATA {{ {data} }}");
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .ok()?;
    let request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ {question} }}"));
    let result = store.execute_query(&request).expect("query");
    let text: String = String::from_utf8_lossy(&result.payload)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let complete = result.completeness.is_some_and(|c| c.complete);
    Some((text.contains("\"boolean\":true"), complete))
}

/// Each equality setting, under `mode` with the rewriting at `ql`: `check` of the outcome.
fn each(
    mode: ReasoningMode,
    ql: QlRewritingMode,
    data: &str,
    question: &str,
    check: impl Fn(Option<(bool, bool)>) -> bool,
) {
    for equality in ["representatives", "replicate", "compact"] {
        let outcome = ask(mode, ql, equality, data, question);
        assert!(check(outcome), "{mode:?}, {ql:?}, {equality}: {outcome:?}");
    }
}

const INTERSECTION: &str = ":x a [ a owl:Class ; owl:intersectionOf ( :A :B ) ] .";

/// Spec: OWL 2 Direct Semantics §2.3.1 and Table 4: `ClassAssertion(ObjectIntersectionOf(:A
/// :B) :x)` entails `ClassAssertion(:A :x)`. The `owl2-ql` closure reads no lists, so it
/// doesn't derive it; a read that misses it must not be told complete.
#[test]
fn an_asserted_intersection_is_not_told_complete_without_its_members() {
    for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
        each(ReasoningMode::Owl2Ql, ql, INTERSECTION, ":x a :A", |o| {
            matches!(o, Some((true, _) | (false, false)))
        });
    }
}

/// Spec: OWL 2 RL's cls-int2, cls-avf and cls-hv1 (OWL 2 Profiles §4.3, table 6) give
/// `:x a :A` from an asserted intersection, `:y a :D` from an asserted `∀:p.:D` with
/// `:x :p :y`, and `:x :p :v` from an asserted `∃:p.{:v}`; under `owl2-rl` with the
/// rewriting on, each read is right and stays complete.
#[test]
fn asserted_expressions_the_rules_apply_stay_complete() {
    let restriction = ":x a [ a owl:Restriction ; owl:onProperty :p ;";
    for (data, question) in [
        (INTERSECTION.to_owned(), ":x a :A"),
        (
            format!("{restriction} owl:allValuesFrom :D ] . :x :p :y ."),
            ":y a :D",
        ),
        (format!("{restriction} owl:hasValue :v ] ."), ":x :p :v"),
    ] {
        each(
            ReasoningMode::Owl2Rl,
            QlRewritingMode::On,
            &data,
            question,
            |o| o == Some((true, true)),
        );
    }
}

/// Spec: OWL 2 Profiles §3.2.4 allows an intersection on the right of a subclass axiom, so
/// `:C ⊑ :A ⊓ :B` with `:x a :C` is OWL 2 QL, and `:x a :A` follows: right, and complete.
#[test]
fn an_inclusion_into_an_intersection_stays_complete() {
    let data = ":C <http://www.w3.org/2000/01/rdf-schema#subClassOf> \
                [ a owl:Class ; owl:intersectionOf ( :A :B ) ] . :x a :C .";
    for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
        each(ReasoningMode::Owl2Ql, ql, data, ":x a :A", |o| {
            o == Some((true, true))
        });
    }
}

/// The cost, stated on purpose: a store that asserts an individual into an anonymous
/// expression is outside OWL 2 QL as a whole, so under `owl2-ql` a read on terms the
/// assertion doesn't name (`:y a :E` from `:y a :F`, `:F ⊑ :E`) isn't told complete
/// either, though its answer is right.
#[test]
fn such_an_assertion_takes_complete_from_every_read() {
    let data = format!(
        "{INTERSECTION} :y a :F . :F <http://www.w3.org/2000/01/rdf-schema#subClassOf> :E ."
    );
    each(
        ReasoningMode::Owl2Ql,
        QlRewritingMode::Auto,
        &data,
        ":y a :E",
        |o| o == Some((true, false)),
    );
}

/// Spec: `ClassAssertion(ObjectMaxCardinality(0 :p) :x)` with `:x :p :y` is inconsistent
/// (OWL 2 Direct Semantics, Table 5), so every assertion is entailed, `:x a :D` too. Under
/// `owl2-ql` the closure finds no clash; a read that answers `false` must not be told
/// complete.
#[test]
fn an_asserted_maximum_of_none_is_not_told_complete() {
    let data = ":x a [ a owl:Restriction ; owl:onProperty :p ; \
                owl:maxCardinality \"0\"^^xsd:nonNegativeInteger ] . \
                :x :p :y . :D a owl:Class .";
    for ql in [QlRewritingMode::Auto, QlRewritingMode::On] {
        each(ReasoningMode::Owl2Ql, ql, data, ":x a :D", |o| {
            matches!(o, None | Some((true, _) | (false, false)))
        });
    }
}

/// The control: a named class in the assertion is OWL 2 QL, and the rewriting gives
/// `:x a :E` from `:A rdfs:subClassOf :E`, complete.
#[test]
fn an_asserted_named_class_stays_complete() {
    each(
        ReasoningMode::Owl2Ql,
        QlRewritingMode::Auto,
        ":x a :A . :A <http://www.w3.org/2000/01/rdf-schema#subClassOf> :E .",
        ":x a :E",
        |o| o == Some((true, true)),
    );
}
