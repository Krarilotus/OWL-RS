use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationPipeline, QlRewritingMode, SparqlQueryRequest, StoreConfig, StoreService,
};

use crate::support::{in_memory_store_config, load_past_the_pipeline};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

const INDIVIDUALS: [&str; 3] = [
    ":a a owl:NamedIndividual .",
    ":p a owl:ObjectProperty . :a :p :c .",
    ":a a :D .",
];

fn check_class_read(axiom: &str, expected: bool) {
    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Ql);
    let mut failures = Vec::new();
    for individual in INDIVIDUALS {
        let data = format!(":B a owl:Class . {axiom} {individual}");
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
                Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                    ReasoningMode::Owl2Ql,
                ))),
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
            let result = store
                .execute_query(&SparqlQueryRequest::all(format!(
                    "{PREFIXES}ASK {{ :a a :B }}"
                )))
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&result.payload).unwrap();
            let answer = json["boolean"].as_bool().expect("ASK Boolean");
            let complete = result.completeness.is_some_and(|status| status.complete);
            if answer != expected {
                failures.push(format!(
                    "{route}, {individual}: ASK expected {expected}, got {answer} (complete={complete})"
                ));
            } else if !complete {
                failures.push(format!("{route}, {individual}: ASK answer was incomplete"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
