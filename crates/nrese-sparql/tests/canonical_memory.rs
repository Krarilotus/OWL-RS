//! Guard of canonical answers' `SERVICE` bind join (`native/federation.rs`): a bound value
//! goes to the endpoint as every identity of its class, so a row of two values in classes
//! of n identities stands for n² rows. Past `BIND_LIMIT` they are counted, never made (the
//! block goes without `VALUES`); within it they are reserved in the query's envelope
//! before they are made. Measured with a counting allocator, so this file is a test
//! binary of its own.

use std::sync::{Arc, Mutex};

use nrese_engine::{EncodedTriple, Engine, EngineConfig};
use nrese_exec::{Budget, Envelope, heap};
use nrese_rdf::{GraphName, NamedNode, Quad, Variable};
use nrese_sparql::{
    CancellationToken, QueryOptions, QueryResults, Resources, ServiceClient, ServiceResults,
    Services, evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

const EX: &str = "http://example.com/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// An endpoint with nothing, recording the `VALUES` rows it is sent.
struct Endpoint(Mutex<usize>);

impl ServiceClient for Endpoint {
    fn select(
        &self,
        _endpoint: &str,
        query: &str,
        budget: &Arc<Budget>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, nrese_sparql::QueryEvaluationError> {
        if let Some(token) = cancellation {
            token.check()?;
        }
        *self.0.lock().unwrap() += query.matches(" ) (").count() + query.matches("VALUES").count();
        if let Some(token) = cancellation {
            token.check()?;
        }
        Ok(ServiceResults::new(
            vec![Variable::new_unchecked("x"), Variable::new_unchecked("y")],
            budget,
        ))
    }
}

/// `Rep ex:p Rep`, `Rep` in a class of `size` identities.
fn engine(size: usize) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(ex("Rep").as_ref().into());
    let same_as = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let same_as = tx.intern(same_as.as_ref().into());
    tx.insert(Quad::new(ex("Rep"), ex("p"), ex("Rep"), GraphName::DefaultGraph).as_ref());
    for i in 1..size {
        let alias = tx.intern(ex(&format!("alias{i}")).as_ref().into());
        tx.insert_inferred(EncodedTriple {
            subject: alias,
            predicate: same_as,
            object: rep,
        });
    }
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    engine
}

/// The heap the query's thread held at most, the envelope's peak and the rows sent.
fn run(size: usize) -> (usize, usize, usize) {
    let engine = engine(size);
    let snapshot = engine.snapshot();
    let _ = snapshot.with_canonical_equality().equality_classes();
    let endpoint = Arc::new(Endpoint(Mutex::new(0)));
    let envelope = Envelope::new(Some(64 << 20), None);
    let options = QueryOptions {
        equality_canonical: true,
        as_written: true,
        services: Some(Services(endpoint.clone())),
        resources: Resources {
            memory: envelope.clone(),
            ..Resources::default()
        },
        ..QueryOptions::default()
    };
    let query = SparqlParser::new()
        .parse_query(&format!(
            "PREFIX ex: <{EX}> SELECT ?x ?y {{ ?x ex:p ?y SERVICE <urn:remote> {{ ?x ex:r ?y }} }}"
        ))
        .unwrap();
    heap::start("service");
    heap::begin_step();
    let QueryResults::Solutions(rows) = evaluate_query(&snapshot, &query, &options).unwrap() else {
        panic!("solutions")
    };
    assert_eq!(rows.count(), 0);
    heap::end_step("service");
    let peak = heap::steps().last().unwrap().peak;
    let _ = heap::finish();
    let sent = *endpoint.0.lock().unwrap();
    (peak, envelope.peak(), sent)
}

#[test]
fn class_products_past_the_bind_limit_are_never_made_and_the_rest_are_reserved() {
    // 512² = 262,144 combinations: past the limit, the block goes without VALUES.
    let (heap_peak, _, sent) = run(512);
    assert_eq!(sent, 0, "past BIND_LIMIT nothing goes as VALUES");
    assert!(
        heap_peak < 2 << 20,
        "the product was made: {heap_peak} bytes"
    );
    // 100² = 10,000 combinations: sent, and reserved before they were made.
    let (_, reserved, sent) = run(100);
    assert!(sent > 0, "within BIND_LIMIT the values go as VALUES");
    assert!(
        reserved >= 10_000 * 2 * std::mem::size_of::<nrese_sparql_syntax::term::GroundTerm>(),
        "the rows weren't reserved: the envelope's peak is {reserved} bytes"
    );
}
