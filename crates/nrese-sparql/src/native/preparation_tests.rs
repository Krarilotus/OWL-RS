use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use std::sync::Arc;

#[test]
fn preparation_uses_the_executors_equality_view_at_the_probe_row_limit() {
    use nrese_engine::{EncodedTriple, QuadPattern};
    use nrese_rdf::NamedNodeRef;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data = "@prefix : <http://e/> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
        :ann a :A ; :p :company . :alias owl:sameAs :ann .";
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(data.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let same_as = snapshot
        .lookup(NamedNodeRef::new_unchecked("http://www.w3.org/2002/07/owl#sameAs").into())
        .unwrap();
    let alias = snapshot
        .quads_for_pattern(&QuadPattern {
            predicate: Some(same_as),
            ..QuadPattern::all()
        })
        .next()
        .unwrap();
    let mut tx = engine.transaction();
    tx.remove_encoded(alias);
    tx.insert_inferred(EncodedTriple {
        subject: alias.subject,
        predicate: same_as,
        object: alias.object,
    });
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let snapshot = engine.snapshot();
    let query = Query::parse("SELECT ?x WHERE { ?x <http://e/p> ?y } ORDER BY ?x", None).unwrap();
    for canonical in [false, true] {
        for early in [false, true] {
            let options = || QueryOptions {
                equality_closed: true,
                equality_canonical: canonical,
                equality_early_expansion: early,
                ql: Some(Arc::new(
                    crate::ql::QlRewriting::new(crate::ql::Closure { lists: true }).with_limits(
                        crate::ql::Limits {
                            check_rows: 2,
                            ..Default::default()
                        },
                    ),
                )),
                ..QueryOptions::default()
            };
            let expected = crate::plan_query(&snapshot, &query, &options())
                .unwrap()
                .ql
                .unwrap();
            assert_eq!(
                expected.checks,
                usize::from(canonical),
                "fixture straddles the row limit"
            );
            let options = options();
            for _ in 0..2 {
                let (prepared, report) = prepare_ql_query(&snapshot, &query, &options).unwrap();
                let report = report.unwrap();
                assert_eq!(
                    (report.patterns, report.realised, &report.completeness),
                    (expected.patterns, expected.realised, &expected.completeness)
                );
                let collect = |query: &Query, options: &QueryOptions| {
                    let crate::QueryResults::Solutions(rows) =
                        crate::evaluate_query(&snapshot, query, options).unwrap()
                    else {
                        panic!("solutions")
                    };
                    rows.map(|row| row.unwrap().get("x").unwrap().to_string())
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    collect(
                        &prepared,
                        &QueryOptions {
                            ql: None,
                            ..options.clone()
                        }
                    ),
                    collect(&query, &options)
                );
            }
        }
    }
}

#[test]
fn prepared_algebra_runs_ql_once_even_with_warm_probe_caches() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data = "@prefix : <http://e/> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
        :ann a :A .";
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(data.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        ql: Some(Arc::new(crate::ql::QlRewriting::new(crate::ql::Closure {
            lists: true,
        }))),
        ..QueryOptions::default()
    };
    let query = Query::parse("ASK { <http://e/ann> <http://e/p> ?y }", None).unwrap();
    for warm in [false, true] {
        QL_STAGES.set(0);
        let (prepared, report) = prepare_ql_query(&snapshot, &query, &options).unwrap();
        assert!(matches!(prepared, Cow::Owned(_)));
        assert!(report.unwrap().patterns > 0);
        let execution = QueryOptions {
            ql: None,
            ..options.clone()
        };
        assert!(matches!(
            crate::evaluate_query(&snapshot, &prepared, &execution).unwrap(),
            crate::QueryResults::Boolean(true)
        ));
        assert_eq!(
            QL_STAGES.get(),
            1,
            "warm={warm}: count stages, not cached probes"
        );
    }
    let query = Query::parse("ASK { ?s <http://e/unrelated> ?o }", None).unwrap();
    assert!(matches!(
        prepare_ql_query(&snapshot, &query, &options).unwrap().0,
        Cow::Borrowed(_)
    ));
    for options in [
        QueryOptions::default(),
        QueryOptions {
            read_model: ReadModel::Asserted,
            ..options
        },
    ] {
        let (prepared, report) = prepare_ql_query(&snapshot, &query, &options).unwrap();
        assert!(matches!(prepared, Cow::Borrowed(_)));
        assert!(report.is_none());
    }
}
