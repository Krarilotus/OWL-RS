use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::ql::{PrintForm, Printed};
use nrese_store::{
    MutationPipeline, QlRewritingMode, SparqlQueryRequest, StoreConfig, StoreService,
};
use serde_json::{Value, json};

use crate::support::{in_memory_store_config, load_past_the_pipeline};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

fn answer(store: &StoreService, query: &str) -> (Value, bool) {
    let result = store
        .execute_query(&SparqlQueryRequest::all(query))
        .unwrap();
    let json: Value = serde_json::from_slice(&result.payload).unwrap();
    let value = json
        .get("boolean")
        .unwrap_or(&json["results"]["bindings"])
        .clone();
    (
        value,
        result.completeness.is_some_and(|status| status.complete),
    )
}

fn check_ql_reads(premises: &[&str], query: &str, expected: Value) {
    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Ql);
    let query = format!("{PREFIXES}{query}");
    let mut failures = Vec::new();
    for premise in premises {
        let data = format!(":B a owl:Class . {premise}");
        for rebuild in [false, true] {
            let store = Arc::new(
                StoreService::new(StoreConfig {
                    ql_rewriting: QlRewritingMode::Auto,
                    ..in_memory_store_config()
                })
                .unwrap(),
            );
            let pipeline = MutationPipeline::new(
                Arc::clone(&store),
                Arc::new(ReasonerService::new(config.clone())),
            );
            let route = if rebuild {
                load_past_the_pipeline(&store, &format!("{PREFIXES}{data}"));
                store
                    .rematerialise(config.materialised_program().unwrap())
                    .unwrap();
                "rebuild"
            } else {
                pipeline
                    .execute_update_str(&format!("{PREFIXES}INSERT DATA {{ {data} }}"))
                    .unwrap();
                "commit"
            };
            let (actual, complete) = answer(&store, &query);
            if actual != expected {
                failures.push(format!(
                    "{route}, {premise}: expected {expected}, got {actual} (complete={complete})"
                ));
            } else if !complete {
                failures.push(format!("{route}, {premise}: answer was incomplete"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn check_class_read(axiom: &str, expected: bool) {
    let premise = format!(":a a owl:NamedIndividual . {axiom}");
    check_ql_reads(&[&premise], "ASK { :a a :B }", json!(expected));
}

// Spec: OWL 2 Direct Semantics §2.2 and §2.3.2. The range and domain of
// owl:topObjectProperty include every individual of the nonempty object domain.
#[test]
fn top_object_property_range_types_every_individual() {
    check_class_read("owl:topObjectProperty rdfs:range :B .", true);
}

#[test]
fn top_object_property_domain_types_every_individual() {
    check_class_read("owl:topObjectProperty rdfs:domain :B .", true);
}

// Spec: OWL 2 Direct Semantics §2.2 and §2.3.3. The nonempty data domain
// gives every individual an outgoing owl:topDataProperty edge.
#[test]
fn top_data_property_domain_types_every_individual() {
    check_class_read("owl:topDataProperty rdfs:domain :B .", true);
}

// Without the axiom there is a model in which :B is empty.
#[test]
fn fresh_class_is_not_entailed_for_any_individual() {
    check_class_read("", false);
}

// Spec: OWL 2 Direct Semantics §2.2–2.3.3. Equivalents, inverses and superproperties
// of a universal property are universal, so their object endpoints all belong to :B.
#[test]
fn role_axioms_make_the_domain_or_range_universal() {
    check_ql_reads(
        &[
            ":a a owl:NamedIndividual . :q a owl:ObjectProperty ; \
             owl:equivalentProperty owl:topObjectProperty ; rdfs:range :B .",
            ":a a owl:NamedIndividual . :r a owl:ObjectProperty ; rdfs:subPropertyOf :q . \
             owl:topObjectProperty rdfs:subPropertyOf :r . \
             :q a owl:ObjectProperty ; rdfs:range :B .",
            ":a a owl:NamedIndividual . :q a owl:ObjectProperty ; \
             owl:inverseOf owl:topObjectProperty ; rdfs:domain :B .",
            ":a a owl:NamedIndividual . owl:topDataProperty rdfs:subPropertyOf :q . \
             :q a owl:DatatypeProperty ; rdfs:domain :B .",
        ],
        "ASK { :a a :B }",
        json!(true),
    );
}

// Spec: the range includes the object-only individual bound by the property assertion.
#[test]
fn top_range_types_the_object_bound_by_a_join() {
    check_ql_reads(
        &["owl:topObjectProperty rdfs:range :B . :p a owl:ObjectProperty . :a :p :c ."],
        "SELECT ?y { ?x :p ?y . ?y a :B }",
        json!([{"y": {"type": "uri", "value": "http://example.com/c"}}]),
    );
}

// Spec: OWL 2 Direct Semantics §2.2 and §2.3.1–2.3.2. The required r-successor
// of :a belongs to :B by the top range even if the data names no successor.
#[test]
fn top_range_types_anonymous_successors() {
    check_ql_reads(
        &[
            ":r a owl:ObjectProperty . :A a owl:Class ; rdfs:subClassOf [ \
           a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom owl:Thing ] . \
           :a a :A . owl:topObjectProperty rdfs:range :B .",
        ],
        "ASK { :a :r ?y . ?y a :B }",
        json!(true),
    );
}

// Spec: StoreService::print_query preserves the answers from the owl2-rl closure.
// prp-spo1 and prp-dom type :x from the stated subproperty edge.
#[test]
fn printed_domain_query_reads_stated_subproperty_edges() {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    load_past_the_pipeline(
        &store,
        &format!(
            "{PREFIXES} :B a owl:Class . :p a owl:ObjectProperty ; \
             rdfs:subPropertyOf owl:topObjectProperty . \
             owl:topObjectProperty rdfs:domain :B . :x :p :y ."
        ),
    );
    let query = format!("{PREFIXES}SELECT ?s {{ ?s a :B }}");
    let rows = [PrintForm::Paths, PrintForm::Values].map(|form| {
        let Printed::Query { text, .. } = store.print_query(&query, form).unwrap() else {
            panic!("expected a printed {} query", form.name());
        };
        answer(&store, &text).0
    });
    let expected = json!([{"s": {"type": "uri", "value": "http://example.com/x"}}]);
    assert_eq!(rows, [expected.clone(), expected]);
}
