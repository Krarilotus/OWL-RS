//! RDF 1.2 and SPARQL 1.2 through the store: reified triples and directional literals
//! loaded in bulk and by `INSERT DATA`, matched by triple-term patterns, written back in
//! the results, and kept by an on-disk store across a reopen (the dictionary keys a triple
//! term by its components' ids, which the log replays in order).

use std::fs;
use std::path::Path;

use nrese_rdf::{BaseDirection, Literal, NamedNode, Term, Triple};
use nrese_sparql_results::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};
use nrese_store::{BulkLoadRequest, GraphTarget, StoreConfig, StoreService};
use tempfile::tempdir;

const DATA: &str = r#"
PREFIX : <http://example/>
:alice :knows :bob {| :since 2001 ; :source _:doc |} .
_:doc :title "Briefe"@de--ltr .
<< :s :p :o ~ :r >> :q :z .
<< << :s :p2 :o >> :p3 :z >> :q :nested .
"#;

/// The solutions of a query, as rows of terms in the variables' order.
fn select(service: &StoreService, query: &str) -> Vec<Vec<Option<Term>>> {
    let result = service.execute_query_str(query).expect("query");
    let SliceQueryResultsParserOutput::Solutions(solutions) =
        QueryResultsParser::from_format(QueryResultsFormat::Json)
            .for_slice(&result.payload)
            .expect("results")
    else {
        panic!("not solutions")
    };
    let mut rows: Vec<Vec<Option<Term>>> = solutions
        .map(|s| s.expect("solution").values().to_vec())
        .collect();
    rows.sort();
    rows
}

fn iri(local: &str) -> Term {
    NamedNode::new_unchecked(format!("http://example/{local}")).into()
}

fn load(service: &StoreService, dir: &Path) {
    let path = dir.join("data.ttl");
    fs::write(&path, DATA).unwrap();
    service
        .bulk_load(&BulkLoadRequest {
            files: vec![path],
            replace: false,
            graph: GraphTarget::DefaultGraph,
        })
        .expect("load");
}

/// What every check below asks of a store holding `DATA`.
fn check(service: &StoreService) {
    // An annotation: the reifier of `:alice :knows :bob`, found through its triple term.
    assert_eq!(
        select(
            service,
            "PREFIX : <http://example/> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
             SELECT ?since WHERE { ?r rdf:reifies <<( :alice :knows ?who )>> ; :since ?since }"
        ),
        vec![vec![Some(
            Literal::new_typed_literal("2001", nrese_rdf::vocab::xsd::INTEGER).into()
        )]]
    );
    // The reified-triple syntax in a query, nested, with a variable inside.
    assert_eq!(
        select(
            service,
            "PREFIX : <http://example/> SELECT ?o ?x WHERE { << << :s :p2 ?o >> :p3 :z >> :q ?x }"
        ),
        vec![vec![Some(iri("o")), Some(iri("nested"))]]
    );
    // A triple term as a result value, and the functions that take it apart.
    assert_eq!(
        select(
            service,
            "PREFIX : <http://example/> SELECT ?t (SUBJECT(?t) AS ?s) WHERE { :r <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> ?t }"
        ),
        vec![vec![
            Some(
                Triple::new(
                    NamedNode::new_unchecked("http://example/s"),
                    NamedNode::new_unchecked("http://example/p"),
                    iri("o")
                )
                .into()
            ),
            Some(iri("s")),
        ]]
    );
    // A literal with a base direction keeps it, and LANGDIR reads it.
    assert_eq!(
        select(
            service,
            "PREFIX : <http://example/> SELECT ?title (LANGDIR(?title) AS ?dir) WHERE { ?doc :title ?title }"
        ),
        vec![vec![
            Some(
                Literal::new_directional_language_tagged_literal_unchecked(
                    "Briefe",
                    "de",
                    BaseDirection::Ltr
                )
                .into()
            ),
            Some(Literal::new_simple_literal("ltr").into()),
        ]]
    );
}

#[test]
fn rdf_1_2_terms_load_match_and_come_back() {
    let dir = tempdir().unwrap();
    let service = StoreService::new(StoreConfig::in_memory()).expect("store");
    load(&service, dir.path());
    check(&service);
}

#[test]
fn rdf_1_2_terms_survive_a_reopen() {
    let dir = tempdir().unwrap();
    let data = dir.path().join("store");
    {
        let service = StoreService::new(StoreConfig::on_disk(&data)).expect("store");
        load(&service, dir.path());
        check(&service);
        // And by update, a triple term with a blank node inside.
        service
            .execute_update_str(
                "PREFIX : <http://example/> INSERT DATA { :e :says <<( _:x :p \"x\"@en--rtl )>> }",
            )
            .expect("update");
    }
    let reopened = StoreService::new(StoreConfig::on_disk(&data)).expect("reopen");
    check(&reopened);
    let rows = select(
        &reopened,
        "PREFIX : <http://example/> SELECT (LANGDIR(OBJECT(?t)) AS ?d) WHERE { :e :says ?t }",
    );
    assert_eq!(
        rows,
        vec![vec![Some(Literal::new_simple_literal("rtl").into())]]
    );
}
