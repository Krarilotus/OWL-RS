use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::ql::{Closure, QlRewriting};
use std::sync::Arc;

fn fixture() -> (Engine, PreparedQuery, StoreSettings) {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data = "@prefix : <http://example.org/> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        :Employee rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :worksFor ;
            owl:someValuesFrom :Organisation ] .
        :ann a :Employee ; :worksFor :acme .";
    let mut tx = engine.transaction();
    for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(data.as_bytes()) {
        tx.insert(q.unwrap().as_ref());
    }
    tx.commit().unwrap();
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(
        "SELECT ?x WHERE { ?x <http://example.org/worksFor> ?y }",
    ))
    .unwrap();
    let store = crate::StoreService::new(crate::StoreConfig {
        execution_threads: 1,
        ..crate::StoreConfig::in_memory()
    })
    .unwrap();
    let settings = store.query_settings().clone();
    *settings.ql.write().unwrap() = Some(Arc::new(QlRewriting::new(Closure { lists: true })));
    (engine, prepared, settings)
}

#[test]
fn preparation_cancellation_is_an_error_and_the_original_report_is_retained() {
    let (engine, prepared, settings) = fixture();
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    token.cancel();
    let mut bytes = Vec::new();
    assert!(matches!(
        run_query(
            &snapshot,
            &prepared,
            &settings,
            &token,
            &mut bytes,
            |_| panic!("cancelled before reporting")
        ),
        Err(StoreError::SparqlEvaluation(
            QueryEvaluationError::Cancelled
        ))
    ));
    assert!(bytes.is_empty());
    let token = CancellationToken::new();
    let successful = run_query(
        &snapshot,
        &prepared,
        &settings,
        &token,
        &mut bytes,
        |report| {
            assert_eq!(report.unwrap().checks, 1);
            String::new()
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        (successful.patterns, successful.realised, successful.checks),
        (0, 1, 1)
    );
    let cached = run_query(
        &snapshot,
        &prepared,
        &settings,
        &token,
        std::io::sink(),
        |_| String::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!((cached.realised, cached.checks), (1, 0));
}

#[test]
fn reporting_keeps_its_snapshot_and_options_and_precedes_cached_bytes() {
    let (engine, mut prepared, mut settings) = fixture();
    settings.result_cache = Some(Arc::new(nrese_sparql::ResultCache::new(1 << 20)));
    let snapshot = engine.snapshot();
    let token = CancellationToken::new();
    let mut first = Vec::new();
    run_query(&snapshot, &prepared, &settings, &token, &mut first, |_| {
        // Mutating the engine during reporting must not change the answers' view.
        let mut tx = engine.transaction();
        tx.insert(
            nrese_rdf::Quad::new(
                nrese_rdf::NamedNode::new_unchecked("http://example.org/bob"),
                nrese_rdf::NamedNode::new_unchecked("http://example.org/worksFor"),
                nrese_rdf::NamedNode::new_unchecked("http://example.org/acme"),
                nrese_rdf::GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        tx.commit().unwrap();
        "same-context".to_owned()
    })
    .unwrap();
    let text = std::str::from_utf8(&first).unwrap();
    assert!(text.contains("ann") && !text.contains("bob"));
    let options = query_options(&prepared, &settings, &token);
    assert!(
        matches!(
            nrese_sparql::cached_output(
                &snapshot,
                &prepared.query,
                &options,
                prepared.media_type(),
                "same-context"
            ),
            CachedOutput::Hit(_)
        ),
        "original key retained"
    );
    let mut bytes = Vec::new();
    let error = run_query(&snapshot, &prepared, &settings, &token, &mut bytes, |_| {
        token.cancel();
        "same-context".to_owned()
    })
    .unwrap_err();
    assert!(matches!(
        error,
        StoreError::SparqlEvaluation(QueryEvaluationError::Cancelled)
    ));
    assert!(
        bytes.is_empty(),
        "reporting cancellation precedes cached bytes"
    );
    // Memory limits do not change cache identity. Evaluation with zero bytes fails,
    // but copying a cached answer needs no query operators or planning.
    prepared.memory_limit = Some(0);
    let mut second = Vec::new();
    run_query(
        &snapshot,
        &prepared,
        &settings,
        &CancellationToken::new(),
        &mut second,
        |_| "same-context".to_owned(),
    )
    .unwrap();
    assert_eq!(first, second);
}

#[test]
fn collected_response_keeps_the_first_preparation_report() {
    let store = crate::StoreService::new(crate::StoreConfig {
        ql_rewriting: crate::QlRewritingMode::On,
        ..crate::StoreConfig::in_memory()
    })
    .unwrap();
    store
        .execute_update_str(
            "PREFIX : <http://example.org/>
        PREFIX owl: <http://www.w3.org/2002/07/owl#>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        INSERT DATA { :Employee rdfs:subClassOf [ a owl:Restriction ;
            owl:onProperty :worksFor ; owl:someValuesFrom :Organisation ] .
            :ann a :Employee ; :worksFor :acme . }",
        )
        .unwrap();
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
        .unwrap();
    store.use_reasoning_rules(Some(nrese_reasoner::rulesets::Ruleset::Owl2Rl.into()));
    let request =
        SparqlQueryRequest::all("SELECT ?x WHERE { ?x <http://example.org/worksFor> ?y }");
    let first = store.execute_query(&request).unwrap();
    let first_ql = first.ql.unwrap();
    assert_eq!((first_ql.checks, first_ql.realised), (1, 1));
    assert_eq!(first.completeness, Some(first_ql.completeness));
    let second = store.execute_query(&request).unwrap();
    assert_eq!(
        second.ql.unwrap().checks,
        0,
        "only the later operation uses the warm probe"
    );
    assert_eq!(first.payload, second.payload);
}

#[test]
fn prepared_serializers_agree_with_standalone_execution_for_every_form_and_format() {
    let (engine, _, settings) = fixture();
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(b"<http://example.org/bob> a <http://example.org/Employee> .".as_slice())
    {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    for text in [
        "SELECT ?x WHERE { ?x <http://example.org/worksFor> ?y } ORDER BY ?x",
        "ASK { <http://example.org/bob> <http://example.org/worksFor> ?y }",
        "CONSTRUCT { ?x <http://example.org/employed> true } WHERE { ?x <http://example.org/worksFor> ?y }",
        "DESCRIBE ?x WHERE { ?x <http://example.org/worksFor> ?y }",
    ] {
        for format in [
            SolutionsResultFormat::Json,
            SolutionsResultFormat::Xml,
            SolutionsResultFormat::Csv,
            SolutionsResultFormat::Tsv,
        ] {
            let mut prepared = PreparedQuery::parse(&SparqlQueryRequest::all(text)).unwrap();
            prepared.solutions_format = format;
            let token = CancellationToken::new();
            let mut expected = Vec::new();
            serialize(
                &snapshot,
                &prepared,
                &prepared.query,
                &query_options(&prepared, &settings, &token),
                &token,
                &mut expected,
            )
            .unwrap();
            let mut actual = Vec::new();
            let report = run_query(&snapshot, &prepared, &settings, &token, &mut actual, |_| {
                String::new()
            })
            .unwrap()
            .unwrap();
            assert!(report.patterns > 0, "{text}");
            assert_eq!(actual, expected, "{text} {format:?}");
        }
    }
}
