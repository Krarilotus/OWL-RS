//! `results` before `head`, a legal member order of SPARQL Query Results JSON: the rows are
//! the ones the usual order gives, and reading them makes nothing the size of every row at
//! every variable. From memory the rows stream as in the usual order; from a reader the
//! bindings wait for the head as they were written, each row laid out when handed out.
//!
//! One test in its own binary: the counting allocator sees only it.

use std::io::Cursor;
use std::mem::size_of;

use nrese_exec::heap;
use nrese_rdf::Term;
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput,
    SliceQueryResultsParserOutput,
};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// `rows` bindings over `width` variables, each binding one variable (the row's own).
fn document(width: usize, rows: usize, results_first: bool) -> String {
    let vars: Vec<String> = (0..width).map(|i| format!("\"v{i}\"")).collect();
    let head = format!(r#""head":{{"vars":[{}]}}"#, vars.join(","));
    let bindings: Vec<String> = (0..rows)
        .map(|r| format!(r#"{{"v{}":{{"type":"literal","value":"{r}"}}}}"#, r % width))
        .collect();
    let results = format!(r#""results":{{"bindings":[{}]}}"#, bindings.join(","));
    if results_first {
        format!("{{{results},{head}}}")
    } else {
        format!("{{{head},{results}}}")
    }
}

fn parser() -> QueryResultsParser {
    QueryResultsParser::from_format(QueryResultsFormat::Json)
}

fn slice_rows(bytes: &[u8]) -> Vec<Vec<Option<Term>>> {
    let SliceQueryResultsParserOutput::Solutions(solutions) = parser().for_slice(bytes).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|s| {
            let s = s.unwrap();
            variables.iter().map(|v| s.get(v).cloned()).collect()
        })
        .collect()
}

fn reader_rows(bytes: &[u8]) -> Vec<Vec<Option<Term>>> {
    let ReaderQueryResultsParserOutput::Solutions(solutions) =
        parser().for_reader(Cursor::new(bytes)).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|s| {
            let s = s.unwrap();
            variables.iter().map(|v| s.get(v).cloned()).collect()
        })
        .collect()
}

/// The most the calling thread holds while `read` runs.
fn peak_of(read: impl FnOnce()) -> usize {
    heap::start("results-first");
    heap::begin_step();
    read();
    heap::end_step("results-first");
    let peak = heap::steps().last().expect("the step").peak;
    let _ = heap::finish();
    peak
}

#[test]
fn results_before_head_read_without_every_row_at_every_variable() {
    // A million cells each, in three shapes, and two small ones for the answers alone.
    for (width, rows) in [(1, 1), (3, 7), (1_000, 1_000), (250, 4_000), (4_000, 250)] {
        let first = document(width, rows, true);
        let usual = document(width, rows, false);
        let expected = slice_rows(usual.as_bytes());
        assert_eq!(expected.len(), rows);
        assert_eq!(
            slice_rows(first.as_bytes()),
            expected,
            "slice, {width} × {rows}"
        );
        assert_eq!(
            reader_rows(first.as_bytes()),
            expected,
            "reader, {width} × {rows}"
        );
        let cells = width * rows;
        if cells < 1_000_000 {
            continue;
        }
        // Every row at every variable: what may not be made.
        let dense = cells * size_of::<Option<Term>>();
        // What may: a few times the document, and a few rows.
        let bound = 8 * first.len() + 4 * width * size_of::<Option<Term>>() + (1 << 20);
        assert!(bound * 8 < dense, "the shape doesn't tell them apart");
        let slice = peak_of(|| {
            let SliceQueryResultsParserOutput::Solutions(mut solutions) =
                parser().for_slice(first.as_bytes()).unwrap()
            else {
                panic!("solutions")
            };
            drop(solutions.next().expect("a first row").unwrap());
        });
        assert!(
            slice < bound,
            "slice, {width} × {rows}: {slice} bytes held up to the first row (bound {bound}, every row at every variable {dense})"
        );
        let reader = peak_of(|| {
            let ReaderQueryResultsParserOutput::Solutions(mut solutions) =
                parser().for_reader(Cursor::new(first.as_bytes())).unwrap()
            else {
                panic!("solutions")
            };
            drop(solutions.next().expect("a first row").unwrap());
        });
        assert!(
            reader < bound,
            "reader, {width} × {rows}: {reader} bytes held up to the first row (bound {bound}, every row at every variable {dense})"
        );
    }
}
