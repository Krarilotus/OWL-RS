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

/// Whether `pattern` has a triple pattern with a variable predicate.
fn variable_predicate(pattern: &GraphPattern) -> bool {
    pattern.find(&mut |node| {
        matches!(
            node,
            Node::Pattern(GraphPattern::Bgp { patterns })
                if patterns.iter().any(|t| matches!(t.predicate, NamedNodePattern::Variable(_)))
        )
    })
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
    assert!(
        prepared.find(&mut |node| matches!(
            node,
            Node::Expression(Expression::Exists(copy)) if variable_predicate(copy)
        )),
        "a NOT EXISTS of the bag form copies the variable predicate: {prepared:?}"
    );
    assert!(!as_written(parsed, &options), "parsed");
    // The store evaluates the prepared query with the rewriting done, so off.
    let evaluated = crate::QueryOptions {
        ql: None,
        ..options
    };
    assert!(!as_written(prepared, &evaluated), "prepared: {prepared:?}");
}

/// Of `SELECT * { ex:x ex:v ?p . BIND(1 AS ?k) FILTER EXISTS { body } }`: its `EXISTS`'s
/// pattern as parsed, where the mode is decided; the query's pattern; and the `EXISTS`'s
/// pattern as evaluation reads it, after preparation's rewrites.
fn exists_of(engine: &Engine, body: &str) -> (GraphPattern, GraphPattern, GraphPattern) {
    let text = format!(
        "PREFIX ex: <{E}> SELECT * {{ ex:x ex:v ?p . BIND(1 AS ?k) FILTER EXISTS {body} }}"
    );
    let query = nrese_sparql_syntax::SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let (pattern, _) = crate::native::pattern_and_form(&query);
    let inner = exists_in(pattern).expect("the EXISTS").clone();
    let (evaluated, _) = crate::native::preparation::semantic_pattern(
        &engine.snapshot(),
        &query,
        &crate::QueryOptions::default(),
        &mut Vec::new(),
    )
    .unwrap();
    let evaluated = exists_in(&evaluated).expect("the evaluated EXISTS").clone();
    (inner, pattern.clone(), evaluated)
}

/// `pattern` below the filters at its top, as `exists` evaluates it.
fn below_filters(mut pattern: &GraphPattern) -> &GraphPattern {
    while let GraphPattern::Filter { inner, .. } = pattern {
        pattern = inner;
    }
    pattern
}

/// Spec: an `EXISTS` the rule leaves out (`joined`, decided on the parsed pattern) is one
/// evaluation joins, whatever its rows bind: each construct the rule allows, and their
/// nestings, as preparation's rewrites leave it, below any top filters, passes
/// `exists::correlation_safe` for every set of row variables. And an `EXISTS` evaluation
/// puts values in per value, which some row makes unsafe, still counts: a subquery with a
/// `LIMIT`; an `OPTIONAL`, `MINUS`, inner `FILTER` or `BIND` reading the row; a triple term
/// with variables in a union or under `GRAPH`, which preparation takes apart into a `BIND`
/// and a `FILTER` below the top. With OWL 2 QL rewriting on, which reshapes an `EXISTS`
/// after the mode is decided, every `EXISTS` a value may be put in counts. A change to
/// either definition, or a rewrite, that breaks this inclusion fails here.
#[test]
fn an_exists_the_rule_leaves_out_is_one_evaluation_joins() {
    let names = ["s", "p", "o", "g", "z", "k", "x", "tag", "a"];
    let rows: Vec<Vec<Variable>> = (0..1u32 << names.len())
        .map(|set| {
            (0..names.len())
                .filter(|i| set & (1 << i) != 0)
                .map(|i| Variable::new_unchecked(names[i]))
                .collect()
        })
        .collect();
    let engine = engine_of("");
    let options = crate::QueryOptions::default();
    let ql = crate::QueryOptions {
        ql: Some(std::sync::Arc::new(crate::ql::QlRewriting::new(
            crate::ql::Closure { lists: true },
        ))),
        ..crate::QueryOptions::default()
    };
    for body in [
        "{ ?s ?p ?o }",
        "{ ?s ex:r+ ?o }",
        "{ VALUES (?s ?o) { (ex:a ex:b) } }",
        "{ { ?s ?p ?o } UNION { ?o ?p ?s } }",
        "{ GRAPH ?g { ?s ?p ?o } }",
        "{ ?s ?p ?o . ?o ex:r* ?z }",
        "{ GRAPH ?g { { ?s ?p ?o } UNION { VALUES ?o { 1 } } } ?s ex:r ?z }",
        "{ ?s ?p ?o FILTER(?k = 1) }",
        "{ ?s ?p ?o FILTER(?k = 1) FILTER(?o != ?s) }",
        "{ ?s ?p <<( ex:a ex:b ex:c )>> }",
    ] {
        let (inner, query, evaluated) = exists_of(&engine, body);
        assert!(joined(&inner), "left out: {body}");
        for row in &rows {
            assert!(
                crate::native::exists::correlation_safe(below_filters(&evaluated), row),
                "{body} joins with the row {row:?}: {evaluated:?}"
            );
        }
        assert!(!as_written(&query, &options), "planned: {body}");
        assert_eq!(
            as_written(&query, &ql),
            variable_predicate(&inner),
            "with QL on: {body}"
        );
    }
    for body in [
        "{ SELECT ?p ?s WHERE { ?s ?p \"one\" } LIMIT 1 }",
        "{ VALUES ?tag { 1 } OPTIONAL { ?s ?p \"one\" } FILTER(BOUND(?s)) }",
        "{ ?o ex:r ?z MINUS { ?s ?p ?o } }",
        "{ ?o ex:r ?z { ?s ?p ?o FILTER(?k = 1) } }",
        "{ BIND(?k AS ?q) ?s ?p ?q }",
        "{ { ?s ?p <<( ?a ex:b ?a )>> } UNION { ?s ?p ?o } }",
        "{ GRAPH ?g { ?s ?p <<( ?a ex:b ?a )>> } }",
    ] {
        let (inner, query, evaluated) = exists_of(&engine, body);
        assert!(!joined(&inner), "counted: {body}");
        assert!(
            rows.iter()
                .any(|row| !crate::native::exists::correlation_safe(
                    below_filters(&evaluated),
                    row
                )),
            "{body} is put in per value for some row: {evaluated:?}"
        );
        assert!(as_written(&query, &options), "as written: {body}");
        assert!(as_written(&query, &ql), "as written with QL on: {body}");
    }
    // A triple term with variables counts wherever it stands: alone at the top, its
    // rewrite's `FILTER` is a top one and the `EXISTS` is still joined, but the rule
    // decides before the rewrite and doesn't follow it.
    let (inner, query, _) = exists_of(&engine, "{ ?s ?p <<( ?a ex:b ?a )>> }");
    assert!(!joined(&inner), "an open triple term counts");
    assert!(
        as_written(&query, &options),
        "as written beside an open triple term"
    );
}
