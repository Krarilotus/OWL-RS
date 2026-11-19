use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql_syntax::{
    algebra::{Expression, GraphPattern},
    visit::Node,
};

const E: &str = "http://e/";

fn engine_of(turtle: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(turtle.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The pattern of the first `EXISTS` in `pattern`.
fn exists_in(pattern: &GraphPattern) -> Option<&GraphPattern> {
    pattern.find_local(|node| match node {
        Node::Expression(Expression::Exists(inner)) => Some(&**inner),
        _ => None,
    })
}

/// Spec: a query without a search is evaluated in the mode its parsed pattern gives,
/// whatever OWL 2 QL rewriting adds before the evaluation reads it (the store's route).
/// Sixteen arms of a union each spend one of the questions the rewriting may ask of the
/// data, so the last arm's witness is folded unasked, and the bag form copies that arm, its
/// variable predicate included, into a `NOT EXISTS`. That copy is joined, never put in per
/// value, so the prepared query is planned as the parsed one is.
#[test]
fn a_query_without_a_search_is_planned_after_ql_preparation() {
    let mut turtle = format!(
        "@prefix : <{E}> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom owl:Thing ] .
         :t a :C ; :r :w . :x :t :y .\n"
    );
    let mut arms = Vec::new();
    for k in 0..16 {
        turtle.push_str(&format!(
            ":D{k} rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :q{k} ; owl:someValuesFrom owl:Thing ] .
             :d{k} a :D{k} .\n"
        ));
        arms.push(format!("{{ :d{k} :q{k} ?z . ?s :absent ?o }}"));
    }
    arms.push("{ :t :r ?anon . ?p :r ?anon . ?s ?p ?o }".to_owned());
    let text = format!(
        "PREFIX : <{E}> SELECT ?s ?o WHERE {{ {} }}",
        arms.join(" UNION ")
    );
    let query = nrese_sparql_syntax::SparqlParser::new()
        .parse_query(&text)
        .unwrap();
    let engine = engine_of(&turtle);
    let options = crate::QueryOptions {
        ql: Some(std::sync::Arc::new(crate::ql::QlRewriting::new(
            crate::ql::Closure { lists: true },
        ))),
        ..crate::QueryOptions::default()
    };
    let (prepared, _) =
        crate::native::prepare_ql_query(&engine.snapshot(), &query, &options).unwrap();
    let (parsed, _) = crate::native::pattern_and_form(&query);
    let (prepared, _) = crate::native::pattern_and_form(&prepared);
    assert!(exists_in(parsed).is_none(), "the query has no EXISTS");
    let variable_predicate = |pattern: &GraphPattern| {
        pattern.find(&mut |node| {
            matches!(
                node,
                Node::Pattern(GraphPattern::Bgp { patterns })
                    if patterns.iter().any(|t| matches!(t.predicate, NamedNodePattern::Variable(_)))
            )
        })
    };
    assert!(
        prepared.find(&mut |node| matches!(
            node,
            Node::Expression(Expression::Exists(copy)) if variable_predicate(copy)
        )),
        "a NOT EXISTS of the bag form copies the variable predicate: {prepared:?}"
    );
    assert!(!as_written(parsed, &options), "parsed");
    assert!(!as_written(prepared, &options), "prepared: {prepared:?}");
}
