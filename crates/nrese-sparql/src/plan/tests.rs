use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::SparqlParser;

use super::Plan;

mod lowering_oracle;

#[test]
fn ordered_join_scope_keeps_the_lowerings_combined_scan_run() {
    use nrese_rdf::Variable;
    use nrese_sparql_syntax::algebra::PropertyPathExpression;
    use nrese_sparql_syntax::term::TriplePattern;
    let scan = |name: &str| {
        Plan::Scan(TriplePattern {
            subject: Variable::new_unchecked(name).into(),
            predicate: nrese_rdf::NamedNode::new_unchecked("urn:p").into(),
            object: Variable::new_unchecked(name).into(),
        })
    };
    let plan = Plan::Join(vec![
        Plan::Join(vec![
            Plan::Path {
                subject: Variable::new_unchecked("path").into(),
                path: PropertyPathExpression::NamedNode(nrese_rdf::NamedNode::new_unchecked(
                    "urn:q",
                )),
                object: Variable::new_unchecked("path").into(),
            },
            scan("first"),
        ]),
        scan("second"),
        Plan::OrderBy {
            input: Box::new(scan("ordered")),
            keys: Vec::new(),
        },
    ]);
    assert_eq!(
        plan.variables(),
        ["first", "second", "path", "ordered"].map(Variable::new_unchecked)
    );
    assert_eq!(plan.clone().into_pattern(), lowering_oracle::lower(&plan));
}

#[test]
fn plan_properties_and_owned_lowering_match_the_algebra_without_inspection_lowering() {
    use nrese_sparql_syntax::algebra::GraphPattern;
    use nrese_sparql_syntax::term::TermPattern;
    use nrese_sparql_syntax::visit::Node;

    for text in [
        "SELECT * WHERE { ?a ?p << ?s ?q ?o >> . _:a <urn:p> ?b }",
        "SELECT * WHERE { ?a <urn:p> ?b OPTIONAL { ?b <urn:q> ?c FILTER(?c > 1) } ?c <urn:r>+ ?d }",
        "SELECT * WHERE { { ?a <urn:p> ?b } UNION { ?c <urn:q> ?d } MINUS { ?hidden <urn:p> ?other } }",
        "SELECT ?a (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?a ?p ?o } VALUES ?a { <urn:x> UNDEF } BIND(1 AS ?k) } GROUP BY ?a ORDER BY DESC(?n) LIMIT 5 OFFSET 2",
        "SELECT DISTINCT * WHERE { SERVICE SILENT ?endpoint { ?a ?b ?c } }",
        "SELECT * WHERE { FILTER EXISTS { _:a <urn:p> ?hidden } BIND(EXISTS { _:b <urn:q> ?x } AS ?yes) }",
        "SELECT * WHERE { ?a <urn:p> ?b OPTIONAL { ?c <urn:q> ?d FILTER EXISTS { _:c <urn:r> ?x } } }",
        "SELECT (SUM(IF(EXISTS { _:g <urn:p> ?x }, 1, 0)) AS ?n) WHERE { ?s ?p ?o } ORDER BY EXISTS { _:o <urn:q> ?y }",
        "SELECT * WHERE { ?a <urn:p> ?b { SELECT ?x WHERE { ?x <urn:k> ?y } ORDER BY ?y LIMIT 2 } ?x <urn:a> ?a }",
        "SELECT * WHERE { { ?x <urn:q>+ ?y . ?s <urn:p> ?o } ?u <urn:p> ?v { SELECT ?a WHERE { ?a <urn:k> ?b } ORDER BY ?b LIMIT 2 } }",
        "SELECT REDUCED * WHERE { ?a <urn:p> ?b LATERAL { SELECT ?c WHERE { ?b <urn:q> ?c } LIMIT 1 } }",
        "SELECT ?b (COUNT(*) AS ?n) WHERE { ?a <urn:p1> ?b . ?a <urn:p0> <urn:e0> . ?b <urn:p2>/<urn:p2> ?d } GROUP BY ?b",
        "SELECT * WHERE {}",
    ] {
        let Query::Select { pattern, .. } = SparqlParser::new().parse_query(text).unwrap() else {
            unreachable!("selects")
        };
        let plan = Plan::of(&pattern);
        for plan in [plan.clone(), plan.flatten_joins()] {
            let algebra = lowering_oracle::lower(&plan);
            let mut expected = Vec::new();
            algebra.on_in_scope_variable(|v| {
                if !expected.contains(v) {
                    expected.push(v.clone());
                }
            });
            let mut blanks = Vec::new();
            let mut add = |term: &TermPattern| {
                if let TermPattern::BlankNode(b) = term
                    && !blanks.contains(b)
                {
                    blanks.push(b.clone());
                }
            };
            algebra.find(&mut |node| {
                match node {
                    Node::Pattern(GraphPattern::Bgp { patterns }) => {
                        for triple in patterns {
                            add(&triple.subject);
                            add(&triple.object);
                        }
                    }
                    Node::Pattern(GraphPattern::Path {
                        subject, object, ..
                    }) => {
                        add(subject);
                        add(object);
                    }
                    _ => {}
                }
                false
            });
            super::LOWERED_NODES.with(|count| count.set(0));
            assert_eq!(plan.variables(), expected, "scope: {text}");
            let mut found = Vec::new();
            plan.blank_nodes(&mut found);
            found.sort();
            blanks.sort();
            assert_eq!(found, blanks, "blank joins: {text}");
            super::LOWERED_NODES
                .with(|count| assert_eq!(count.get(), 0, "property inspection lowered {text}"));
            assert_eq!(plan.into_pattern(), algebra, "lowering: {text}");
        }
    }
}

#[test]
fn owned_lowering_moves_values_buffers() {
    use nrese_rdf::{NamedNode, Variable};
    use nrese_sparql_syntax::algebra::GraphPattern;
    let variables = vec![Variable::new_unchecked("x")];
    let rows = vec![vec![Some(NamedNode::new_unchecked("urn:value").into())]; 100];
    let buffers = (variables.as_ptr(), rows.as_ptr(), rows[0].as_ptr());
    let GraphPattern::Values {
        variables,
        bindings,
    } = (Plan::Values { variables, rows }).into_pattern()
    else {
        unreachable!("values")
    };
    assert_eq!(
        buffers,
        (variables.as_ptr(), bindings.as_ptr(), bindings[0].as_ptr())
    );
}

#[test]
fn lowering_a_plan_gives_its_algebra_back() {
    for text in [
        "SELECT * WHERE { ?a <urn:p> ?b . ?b <urn:q> ?c }",
        "SELECT * WHERE { { ?a <urn:p> ?b } { ?b <urn:q> ?c } }",
        "SELECT * WHERE { ?a <urn:p> ?b OPTIONAL { ?b <urn:q> ?c FILTER(?c > 1) } ?c <urn:r>+ ?d }",
        "SELECT * WHERE { { ?a <urn:p> ?b } UNION { ?a <urn:q> ?b } UNION { ?a <urn:r> ?b } MINUS { ?a <urn:s> 1 } }",
        "SELECT ?a (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?a ?p ?o } VALUES ?a { <urn:x> } BIND(1 AS ?k) } GROUP BY ?a ORDER BY DESC(?n) LIMIT 5 OFFSET 2",
        "SELECT DISTINCT * WHERE { SERVICE SILENT <http://e.example/sparql> { ?a ?b ?c } }",
        "SELECT * WHERE { }",
        "SELECT REDUCED * WHERE { ?a <urn:p> ?b LATERAL { SELECT ?c WHERE { ?b <urn:q> ?c } LIMIT 1 } }",
    ] {
        let Query::Select { pattern, .. } = SparqlParser::new().parse_query(text).unwrap() else {
            unreachable!("selects")
        };
        assert_eq!(Plan::of(&pattern).lower(), pattern, "{text}");
    }
}

#[test]
fn eager_aggregation_keeps_what_a_blank_node_joins() {
    let pattern = |text: &str| match SparqlParser::new().parse_query(text).unwrap() {
        Query::Select { pattern, .. } => pattern,
        _ => unreachable!("selects"),
    };
    let plain = pattern(
        "SELECT ?b (COUNT(*) AS ?n) WHERE { ?a <urn:p1> ?b . ?a <urn:p0> <urn:e0> . ?b <urn:p3> ?d } GROUP BY ?b",
    );
    assert_ne!(
        super::eager_aggregation(&plain),
        plain,
        "?a is shared: aggregated first"
    );
    // The path's two steps are joined through a blank node, one on each side.
    let path = pattern(
        "SELECT ?b (COUNT(*) AS ?n) WHERE { ?a <urn:p1> ?b . ?a <urn:p0> <urn:e0> . ?b <urn:p2>/<urn:p2> ?d } GROUP BY ?b",
    );
    assert_eq!(super::eager_aggregation(&path), path);
}

#[test]
fn groups_joined_to_each_other_become_one_basic_graph_pattern() {
    let pattern = |text: &str| match SparqlParser::new().parse_query(text).unwrap() {
        Query::Select { pattern, .. } => pattern,
        _ => unreachable!("selects"),
    };
    let rewritten = super::rewrite(&pattern(
        "SELECT * WHERE { { ?a <urn:p> ?b } { ?b <urn:q> ?c } ?c <urn:r>+ ?d }",
    ));
    assert_eq!(
        rewritten,
        pattern("SELECT * WHERE { ?a <urn:p> ?b . ?b <urn:q> ?c . ?c <urn:r>+ ?d }"),
        "one pattern, then the path"
    );
    // An OPTIONAL keeps its left side; what is joined to it moves into one pattern
    // before it (a join commutes).
    let rewritten = super::rewrite(&pattern(
        "SELECT * WHERE { ?a <urn:p> ?b OPTIONAL { ?b <urn:q> ?c } ?c <urn:r> ?d }",
    ));
    assert_eq!(
        rewritten,
        pattern("SELECT * WHERE { ?c <urn:r> ?d { ?a <urn:p> ?b OPTIONAL { ?b <urn:q> ?c } } }")
    );
    // A subquery with an ORDER BY keeps its place (the executor keeps its order through
    // the join); only adjacent triple patterns merge.
    let ordered = "SELECT * WHERE { { SELECT ?x WHERE { ?x <urn:k> ?y } ORDER BY ?y LIMIT 2 } ?x <urn:a> ?a }";
    assert_eq!(super::rewrite(&pattern(ordered)), pattern(ordered));
}
