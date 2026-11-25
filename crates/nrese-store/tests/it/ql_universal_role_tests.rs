use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationPipeline, QlRewritingMode, SparqlQueryRequest, StoreConfig, StoreService,
};
use serde_json::Value;

use crate::support::{in_memory_store_config, load_past_the_pipeline};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";
const EQUIVALENT: &str = ":q a owl:ObjectProperty ; owl:equivalentProperty owl:topObjectProperty .";
const SUBPROPERTY: &str = ":q a owl:ObjectProperty ; rdfs:subPropertyOf owl:topObjectProperty .";

#[derive(Debug)]
enum Expected {
    True,
    False,
    FalseWithConsistencyReason,
    TrueOrIncomplete,
    Incomplete,
    /// A `SELECT` with no row, or one not told `complete`.
    NoRowOrIncomplete,
}

// OWL 2 Direct Semantics §2.2–2.3.3: the top object and data properties relate
// every pair in their domains. Their equivalents, inverses (for object roles)
// and superproperties keep that relation. A subproperty may be empty.
#[test]
fn reads_of_universal_roles() {
    use Expected::{
        False, FalseWithConsistencyReason, Incomplete, NoRowOrIncomplete, True, TrueOrIncomplete,
    };

    let cases = [
        ("equivalent", EQUIVALENT, "ASK { :a :q ?y }", True),
        ("named pair", EQUIVALENT, "ASK { :a :q :b }", True),
        // `:a` has a `:q` partner, so its absence is not entailed: `true` would be unsound.
        (
            "negation",
            EQUIVALENT,
            "ASK { FILTER NOT EXISTS { :a :q ?y } }",
            False,
        ),
        (
            "minus",
            EQUIVALENT,
            "SELECT ?x { VALUES ?x { :a } MINUS { ?x :q ?y } }",
            NoRowOrIncomplete,
        ),
        (
            "control: negation over a subproperty",
            SUBPROPERTY,
            "ASK { FILTER NOT EXISTS { :a :q ?y } }",
            True,
        ),
        ("class join", EQUIVALENT, "ASK { :a :q ?y . ?y a :C }", True),
        (
            "control: built-in top",
            SUBPROPERTY,
            "ASK { :a owl:topObjectProperty ?y }",
            TrueOrIncomplete,
        ),
        (
            "superproperty",
            ":q a owl:ObjectProperty . owl:topObjectProperty rdfs:subPropertyOf :q .",
            "ASK { :a :q ?y }",
            True,
        ),
        (
            "inverse",
            ":q a owl:ObjectProperty ; owl:inverseOf owl:topObjectProperty .",
            "ASK { :a :q ?y }",
            True,
        ),
        (
            "data equivalent",
            ":d a owl:DatatypeProperty ; owl:equivalentProperty owl:topDataProperty .",
            "ASK { :a :d ?v }",
            True,
        ),
        ("select", EQUIVALENT, "SELECT ?y { :a :q ?y }", Incomplete),
        (
            "control: object subproperty",
            SUBPROPERTY,
            "ASK { :a :q ?y }",
            FalseWithConsistencyReason,
        ),
        (
            "control: named pair",
            SUBPROPERTY,
            "ASK { :a :q :b }",
            FalseWithConsistencyReason,
        ),
        (
            "control: class join",
            SUBPROPERTY,
            "ASK { :a :q ?y . ?y a :C }",
            FalseWithConsistencyReason,
        ),
        (
            "control: no class witness",
            EQUIVALENT,
            "ASK { :a :q ?y . ?y a :Absent }",
            FalseWithConsistencyReason,
        ),
        (
            "control: data subproperty",
            ":d a owl:DatatypeProperty ; rdfs:subPropertyOf owl:topDataProperty .",
            "ASK { :a :d ?v }",
            FalseWithConsistencyReason,
        ),
    ];

    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Ql);
    let mut failures = Vec::new();
    for (name, schema, query, expected) in cases {
        let data = format!(":C a owl:Class . :a a owl:NamedIndividual . :b a :C . {schema}");
        for rebuild in [false, true] {
            let store = Arc::new(
                StoreService::new(StoreConfig {
                    ql_rewriting: QlRewritingMode::Auto,
                    ..in_memory_store_config()
                })
                .unwrap(),
            );
            let route = if rebuild {
                load_past_the_pipeline(&store, &format!("{PREFIXES}{data}"));
                store
                    .rematerialise(config.materialised_program().unwrap())
                    .unwrap();
                "rebuild"
            } else {
                MutationPipeline::new(
                    Arc::clone(&store),
                    Arc::new(ReasonerService::new(config.clone())),
                )
                .execute_update_str(&format!("{PREFIXES}INSERT DATA {{ {data} }}"))
                .unwrap();
                "commit"
            };
            let result = store
                .execute_query(&SparqlQueryRequest::all(format!("{PREFIXES}{query}")))
                .unwrap();
            let value: Value = serde_json::from_slice(&result.payload).unwrap();
            let answer = value["boolean"].as_bool();
            let complete = result.completeness.as_ref().map(|status| status.complete);
            let correct = match expected {
                True => answer == Some(true),
                False => answer == Some(false),
                FalseWithConsistencyReason => {
                    answer == Some(false)
                        && result.completeness.is_some_and(|status| {
                            status.as_str() == "sound-only"
                                && status.reasons.len() == 1
                                && status.reasons[0].source == "consistency"
                        })
                }
                TrueOrIncomplete => answer == Some(true) || complete == Some(false),
                Incomplete => complete == Some(false),
                NoRowOrIncomplete => {
                    value["results"]["bindings"]
                        .as_array()
                        .is_some_and(|rows| rows.is_empty())
                        || complete == Some(false)
                }
            };
            if !correct {
                failures.push(format!(
                    "{name}, {route}: expected {expected:?}, got {value}, complete={complete:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
