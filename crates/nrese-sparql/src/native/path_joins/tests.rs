use super::*;
use nrese_sparql_syntax::{Query, SparqlParser};

#[test]
fn logical_path_joins_match_execution_without_lowering() {
    for (body, eligible) in [
        ("?s <urn:p> ?o . ?s <urn:q>+ ?t", true),
        ("?s <urn:q>+ ?t . ?s <urn:p> ?o . ?o <urn:r>* ?u", true),
        ("{ ?s <urn:p> ?o FILTER(?o > 1) } ?s <urn:q>+ ?t", true),
        ("{ ?s <urn:q>+ ?t FILTER(?t != ?s) } ?s <urn:p> ?o", true),
        ("{ {} ?s <urn:q>+ ?t FILTER(?t != ?s) } ?s <urn:p> ?o", true),
        ("{ ?s <urn:p> ?o FILTER(BOUND(?t)) } ?s <urn:q>+ ?t", false),
        (
            "{ ?s <urn:p> ?o FILTER(RAND() > 0.5) } ?s <urn:q>+ ?t",
            false,
        ),
        (
            "{ ?s <urn:p> ?o FILTER EXISTS { ?s <urn:r> ?z } } ?s <urn:q>+ ?t",
            false,
        ),
        (
            "{ ?s <urn:q>+ ?t FILTER EXISTS { ?s <urn:r> ?z } } ?s <urn:p> ?o",
            false,
        ),
        (
            "{ ?s <urn:p> ?o OPTIONAL { ?s <urn:r> ?x } } ?s <urn:q>+ ?t",
            false,
        ),
        (
            "{ SELECT ?s WHERE { ?s <urn:p> ?o } ORDER BY ?o } ?s <urn:q>+ ?t",
            false,
        ),
        (
            "?s <urn:q>+ ?t SERVICE <urn:remote> { ?s <urn:p> ?o }",
            false,
        ),
        (
            "{ ?s <urn:p> ?o } { ?s <urn:r> ?x } ?s <urn:q>+ ?t . ?s <urn:a> ?b",
            true,
        ),
    ] {
        let query = format!("SELECT * WHERE {{ {body} }}");
        let Query::Select {
            pattern: GraphPattern::Project { inner, .. },
            ..
        } = SparqlParser::new().parse_query(&query).unwrap()
        else {
            unreachable!("select")
        };
        let plan = LogicalPlan::of(&inner);
        for plan in [plan.clone(), plan.flatten_joins()] {
            let algebra = plan.lower();
            let expected = PathJoin::of(&algebra);
            crate::plan::LOWERED_NODES.with(|count| count.set(0));
            let actual = PathJoin::of_plan(&plan);
            crate::plan::LOWERED_NODES.with(|count| assert_eq!(count.get(), 0, "{body}"));
            assert_eq!(actual.is_some(), eligible, "eligibility: {body}");
            assert_eq!(actual.is_some(), expected.is_some(), "{body}");
            if let (Some(actual), Some(expected)) = (actual, expected) {
                assert_eq!(actual.triples, expected.triples, "scan order: {body}");
                assert_eq!(actual.conjuncts, expected.conjuncts, "filters: {body}");
                let describe = |join: PathJoin<'_>| {
                    join.paths
                        .iter()
                        .map(|p| {
                            (
                                p.subject.clone(),
                                p.path.clone(),
                                p.object.clone(),
                                p.filter.cloned(),
                            )
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(describe(actual), describe(expected), "paths: {body}");
            }
        }
    }
}
