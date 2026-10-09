//! Ownership guards: live results retain capacity, and shared CPU execution never
//! moves a caller's writer. Large output crosses the existing encoding windows.

use std::{cell::RefCell, io::Write, rc::Rc};

use nrese_engine::{Engine, EngineConfig};
use nrese_exec::{SharedBudget, workers::Workers};
use nrese_sparql::{QueryOptions, ResultsFormat, evaluate_query, write_results};
use nrese_sparql_syntax::SparqlParser;

fn values(rows: usize) -> nrese_sparql_syntax::Query {
    let data = (0..rows)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    SparqlParser::new()
        .parse_query(&format!("SELECT ?x WHERE {{ VALUES ?x {{ {data} }} }}"))
        .unwrap()
}

#[test]
fn an_unconsumed_result_keeps_its_shared_capacity_until_drop() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let shared = SharedBudget::new(1 << 20);
    let options = QueryOptions {
        shared_memory: Some(shared.clone()),
        workers: Some(Workers::pooled(1).unwrap()),
        ..QueryOptions::default()
    };
    let result = evaluate_query(&snapshot, &values(128), &options).unwrap();
    assert!(
        shared.used() > 0,
        "a live ID table must still count against the shared budget"
    );
    drop(result);
    assert_eq!(shared.used(), 0);
}

#[test]
fn update_where_uses_shared_capacity_and_reads_earlier_operations() {
    use nrese_sparql::{QueryResults, UpdateOptions, apply_update};
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let update = SparqlParser::new()
        .parse_update(
            "INSERT DATA { <urn:a> <urn:p> 1 }; \
         INSERT { <urn:a> <urn:q> ?x } WHERE { <urn:a> <urn:p> ?x }",
        )
        .unwrap();
    let workers = Workers::pooled(1).unwrap();
    for bytes in [1, 1 << 20] {
        let shared = SharedBudget::new(bytes);
        let options = UpdateOptions {
            workers: Some(workers.clone()),
            shared_memory: Some(shared.clone()),
            ..UpdateOptions::default()
        };
        let mut tx = engine.transaction();
        let result = apply_update(&mut tx, &update, &options);
        if bytes == 1 {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("memory"), "{error}");
            drop(tx);
        } else {
            result.unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(shared.used(), 0);
        let query = SparqlParser::new()
            .parse_query("ASK { <urn:a> <urn:p> 1; <urn:q> 1 }")
            .unwrap();
        assert!(matches!(
            evaluate_query(&engine.snapshot(), &query, &QueryOptions::default()).unwrap(),
            QueryResults::Boolean(found) if found == (bytes > 1)
        ));
    }
}

struct CallerWriter {
    bytes: Rc<RefCell<Vec<u8>>>,
    caller: std::thread::ThreadId,
    fail: bool,
}

impl Write for CallerWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert_eq!(std::thread::current().id(), self.caller);
        if self.fail {
            return Err(std::io::Error::other("consumer stopped"));
        }
        self.bytes.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn shared_workers_preserve_direct_output_and_caller_owned_writers() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let snapshot = engine.snapshot();
    let wide = Workers::pooled(4).unwrap();
    for rows in [3, 20_000] {
        let query = values(rows);
        for format in [ResultsFormat::Json, ResultsFormat::Tsv, ResultsFormat::Csv] {
            let mut baseline = Vec::new();
            write_results(
                &snapshot,
                &query,
                &QueryOptions::default(),
                format,
                None,
                &mut baseline,
            )
            .unwrap()
            .unwrap();
            for workers in [Workers::pooled(1).unwrap(), wide.limited(2)] {
                let shared = SharedBudget::new(16 << 20);
                let options = QueryOptions {
                    workers: Some(workers),
                    shared_memory: Some(shared.clone()),
                    ..QueryOptions::default()
                };
                let bytes = Rc::new(RefCell::new(Vec::new()));
                let mut writer = CallerWriter {
                    bytes: bytes.clone(),
                    caller: std::thread::current().id(),
                    fail: false,
                };
                write_results(&snapshot, &query, &options, format, None, &mut writer)
                    .unwrap()
                    .unwrap();
                assert_eq!(*bytes.borrow(), baseline);
                assert_eq!(shared.used(), 0);
                writer.fail = true;
                assert!(
                    write_results(&snapshot, &query, &options, format, None, &mut writer)
                        .unwrap()
                        .is_err()
                );
                assert_eq!(
                    shared.used(),
                    0,
                    "writer failure must release execution capacity"
                );
            }
        }
    }
}
