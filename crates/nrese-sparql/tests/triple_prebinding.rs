//! SHACL pre-binding (§5.6.2) applies before evaluation, including reads by inner BIND.
use std::collections::HashMap;
use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{Literal, NamedNode, Term, Triple, Variable};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

#[test]
fn triple_this_is_visible_to_inner_bind() {
    // Spec: the user's RDF 1.2 triple is a value even when absent from the store;
    // BIND($this AS ?value) copies exactly that term, including nested triple objects.
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let node = |name: &str| NamedNode::new(format!("urn:{name}")).unwrap();
    let inner: Term = Triple::new(node("s"), node("p"), Literal::from("object")).into();
    let nested: Term = Triple::new(node("outer"), node("q"), inner.clone()).into();
    for term in [inner, nested] {
        let options = QueryOptions {
            pre_bound: Some(Arc::new(HashMap::from([(
                Variable::new("this").unwrap(),
                term.clone(),
            )]))),
            ..QueryOptions::default()
        };
        // The same typed value through the other Values callers and scoped copies.
        for (body, present) in [
            ("BIND($this AS ?value)".to_owned(), true),
            (
                format!("VALUES $this {{ {term} }} BIND($this AS ?value)"),
                true,
            ),
            (
                "VALUES $this { <urn:other> } BIND($this AS ?value)".to_owned(),
                false,
            ),
            ("?s ?p $this".to_owned(), false),
            ("GRAPH $this {}".to_owned(), false),
            (
                format!("VALUES ?outer {{ {term} }} LATERAL {{ BIND(?outer AS ?value) }}"),
                true,
            ),
            (
                format!(
                    "VALUES ?value {{ {term} }} FILTER(EXISTS {{
                    BIND(?value AS ?copy) FILTER(sameTerm(?copy, ?value)) }})"
                ),
                true,
            ),
            (
                "{ SELECT $this (COUNT(*) AS ?n) WHERE {} GROUP BY $this }
                 BIND($this AS ?value)"
                    .to_owned(),
                true,
            ),
        ] {
            let query = SparqlParser::new()
                .parse_query(&format!("SELECT $this ?value WHERE {{ {body} }}"))
                .unwrap();
            let QueryResults::Solutions(rows) =
                evaluate_query(&snapshot, &query, &options).unwrap()
            else {
                panic!("SELECT")
            };
            let copied: Vec<_> = rows.map(|row| row.unwrap().get("value").cloned()).collect();
            let expected: Vec<_> = present.then(|| Some(term.clone())).into_iter().collect();
            assert_eq!(copied, expected, "triple substitution: {body}");
        }
    }
}
