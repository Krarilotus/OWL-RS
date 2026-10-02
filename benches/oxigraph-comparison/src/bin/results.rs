//! Throughput of nrese-sparql-results against sparesults: writing every format and reading
//! every readable one, from memory and from a reader, on the same generated solutions.
//!
//! The data: `ROWS` solutions over five variables from a fixed seed (IRIs, blank nodes,
//! strings with escapes and non-ASCII, language tags, typed literals, unbound values).
//! nrese writes each document once; both read the same bytes.
//!
//! `cargo run --release --bin results [filter]`

use std::hint::black_box;
use std::io::Read;
use std::time::Instant;

use nrese_rdf::{BlankNode, Literal, NamedNode, Term, TermRef, Variable};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, QueryResultsSerializer, ReaderQueryResultsParserOutput,
    SliceQueryResultsParserOutput,
};
use oxigraph_comparison::rng;
use rand::Rng;

const ROWS: usize = 200_000;
const ROUNDS: usize = 7;
const VARIABLES: [&str; 5] = ["s", "p", "o", "label", "count"];

/// The generated solutions.
fn solutions() -> Vec<Vec<Option<Term>>> {
    let mut r = rng(7);
    let words = [
        "alpha",
        "beta",
        "Übergröße",
        "naïve",
        "日本語",
        "quote \" here",
        "tab\there",
        "a < b & c > d",
        "line\nbreak",
    ];
    (0..ROWS)
        .map(|i| {
            let s: Term = if r.random_bool(0.1) {
                BlankNode::new_unchecked(format!("b{i}")).into()
            } else {
                NamedNode::new_unchecked(format!("http://example.org/resource/item{i}")).into()
            };
            let p = NamedNode::new_unchecked(format!(
                "http://example.org/vocabulary#property{}",
                r.random_range(0..24)
            ));
            let word = words[r.random_range(0..words.len())];
            let o: Term = match r.random_range(0..4) {
                0 => Literal::new_simple_literal(format!("{word} {i}")).into(),
                1 => Literal::new_language_tagged_literal_unchecked(word, "en").into(),
                2 => Literal::new_typed_literal(
                    format!("{}", r.random_range(0..1_000_000)),
                    nrese_rdf::vocab::xsd::INTEGER,
                )
                .into(),
                _ => NamedNode::new_unchecked(format!(
                    "http://example.org/resource/item{}",
                    r.random_range(0..ROWS)
                ))
                .into(),
            };
            let label = r
                .random_bool(0.6)
                .then(|| Literal::new_language_tagged_literal_unchecked(word, "de").into());
            let count = r.random_bool(0.5).then(|| {
                Literal::new_typed_literal(
                    format!("{}", r.random_range(0..100)),
                    nrese_rdf::vocab::xsd::INTEGER,
                )
                .into()
            });
            vec![Some(s), Some(p.into()), Some(o), label, count]
        })
        .collect()
}

fn variables() -> Vec<Variable> {
    VARIABLES
        .iter()
        .map(|v| Variable::new_unchecked(*v))
        .collect()
}

fn ox_variables() -> Vec<oxrdf::Variable> {
    VARIABLES
        .iter()
        .map(|v| oxrdf::Variable::new_unchecked(*v))
        .collect()
}

fn ours_format(format: QueryResultsFormat) -> sparesults::QueryResultsFormat {
    match format {
        QueryResultsFormat::Json => sparesults::QueryResultsFormat::Json,
        QueryResultsFormat::Xml => sparesults::QueryResultsFormat::Xml,
        QueryResultsFormat::Csv => sparesults::QueryResultsFormat::Csv,
        QueryResultsFormat::Tsv => sparesults::QueryResultsFormat::Tsv,
    }
}

/// Writes the solutions by row, as the engine does.
fn write_rows(format: QueryResultsFormat, rows: &[Vec<Option<Term>>]) -> Vec<u8> {
    let mut serializer = QueryResultsSerializer::from_format(format)
        .serialize_solutions_to_writer(Vec::new(), variables())
        .unwrap();
    let mut row: Vec<Option<TermRef<'_>>> = Vec::with_capacity(VARIABLES.len());
    for values in rows {
        row.clear();
        row.extend(values.iter().map(|t| t.as_ref().map(Term::as_ref)));
        serializer.serialize_row(&row).unwrap();
    }
    serializer.finish().unwrap()
}

/// Writes the solutions as named values, the API both have.
fn write_named(format: QueryResultsFormat, rows: &[Vec<Option<Term>>]) -> Vec<u8> {
    let mut serializer = QueryResultsSerializer::from_format(format)
        .serialize_solutions_to_writer(Vec::new(), variables())
        .unwrap();
    for values in rows {
        serializer
            .serialize(
                VARIABLES
                    .iter()
                    .zip(values)
                    .filter_map(|(v, t)| Some((*v, t.as_ref()?.as_ref()))),
            )
            .unwrap();
    }
    serializer.finish().unwrap()
}

fn theirs_write(format: QueryResultsFormat, rows: &[Vec<Option<oxrdf::Term>>]) -> Vec<u8> {
    let variables = ox_variables();
    let mut serializer = sparesults::QueryResultsSerializer::from_format(ours_format(format))
        .serialize_solutions_to_writer(Vec::new(), variables.clone())
        .unwrap();
    for values in rows {
        serializer
            .serialize(
                variables
                    .iter()
                    .zip(values)
                    .filter_map(|(v, t)| Some((v.as_ref(), t.as_ref()?.as_ref()))),
            )
            .unwrap();
    }
    serializer.finish().unwrap()
}

fn seconds(mut work: impl FnMut() -> usize) -> f64 {
    let mut times: Vec<f64> = (0..ROUNDS)
        .map(|_| {
            let start = Instant::now();
            black_box(work());
            start.elapsed().as_secs_f64()
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[ROUNDS / 2]
}

struct Report {
    filter: Option<String>,
}

impl Report {
    /// One case: ours and theirs over `bytes` bytes.
    fn case(
        &self,
        name: &str,
        bytes: usize,
        ours: impl FnMut() -> usize,
        theirs: impl FnMut() -> usize,
    ) {
        if self
            .filter
            .as_ref()
            .is_some_and(|f| !name.contains(f.as_str()))
        {
            return;
        }
        let (a, b) = (seconds(ours), seconds(theirs));
        let mb = bytes as f64 / 1e6;
        println!(
            "{name:<32} nrese {:>7.1} MB/s {:>6.2} Mrows/s   sparesults {:>7.1} MB/s {:>6.2} Mrows/s   time ratio {:>5.2}",
            mb / a,
            ROWS as f64 / a / 1e6,
            mb / b,
            ROWS as f64 / b / 1e6,
            a / b
        );
    }
}

/// A reader that hands out `chunk` bytes per call, as a socket or file would.
struct Chunks<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl Read for Chunks<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.chunk.min(out.len()).min(self.data.len());
        out[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

const CHUNK: usize = 16 * 1024;

fn ours_slice(format: QueryResultsFormat, bytes: &[u8]) -> usize {
    let SliceQueryResultsParserOutput::Solutions(parser) = QueryResultsParser::from_format(format)
        .for_slice(bytes)
        .unwrap()
    else {
        panic!("expected solutions")
    };
    parser.map(|s| black_box(s.unwrap()).values().len()).sum()
}

fn ours_reader(format: QueryResultsFormat, bytes: &[u8]) -> usize {
    let ReaderQueryResultsParserOutput::Solutions(parser) = QueryResultsParser::from_format(format)
        .for_reader(Chunks {
            data: bytes,
            chunk: CHUNK,
        })
        .unwrap()
    else {
        panic!("expected solutions")
    };
    parser.map(|s| black_box(s.unwrap()).values().len()).sum()
}

fn theirs_slice(format: QueryResultsFormat, bytes: &[u8]) -> usize {
    let sparesults::SliceQueryResultsParserOutput::Solutions(parser) =
        sparesults::QueryResultsParser::from_format(ours_format(format))
            .for_slice(bytes)
            .unwrap()
    else {
        panic!("expected solutions")
    };
    parser.map(|s| black_box(s.unwrap()).values().len()).sum()
}

fn theirs_reader(format: QueryResultsFormat, bytes: &[u8]) -> usize {
    let sparesults::ReaderQueryResultsParserOutput::Solutions(parser) =
        sparesults::QueryResultsParser::from_format(ours_format(format))
            .for_reader(Chunks {
                data: bytes,
                chunk: CHUNK,
            })
            .unwrap()
    else {
        panic!("expected solutions")
    };
    parser.map(|s| black_box(s.unwrap()).values().len()).sum()
}

fn main() {
    let report = Report {
        filter: std::env::args().nth(1),
    };
    // Both sides read their solutions from the same TSV, so their terms lie in memory alike.
    let rows: Vec<Vec<Option<Term>>> = {
        let bytes = write_rows(QueryResultsFormat::Tsv, &solutions());
        let SliceQueryResultsParserOutput::Solutions(parser) =
            QueryResultsParser::from_format(QueryResultsFormat::Tsv)
                .for_slice(&bytes)
                .unwrap()
        else {
            panic!("expected solutions")
        };
        parser.map(|s| s.unwrap().values().to_vec()).collect()
    };
    // The same solutions as Oxigraph's types, read with sparesults.
    let ox_rows: Vec<Vec<Option<oxrdf::Term>>> = {
        let bytes = write_rows(QueryResultsFormat::Tsv, &rows);
        let sparesults::SliceQueryResultsParserOutput::Solutions(parser) =
            sparesults::QueryResultsParser::from_format(sparesults::QueryResultsFormat::Tsv)
                .for_slice(&bytes)
                .unwrap()
        else {
            panic!("expected solutions")
        };
        parser.map(|s| s.unwrap().values().to_vec()).collect()
    };
    assert_eq!(ox_rows.len(), ROWS);
    let formats = [
        QueryResultsFormat::Json,
        QueryResultsFormat::Xml,
        QueryResultsFormat::Tsv,
        QueryResultsFormat::Csv,
    ];

    println!("Writing ({ROWS} solutions; time ratio < 1: nrese faster)");
    for format in formats {
        let ours = write_rows(format, &rows);
        let theirs = theirs_write(format, &ox_rows);
        assert!(ours == theirs, "{format}: the written bytes differ");
        let len = ours.len();
        report.case(
            &format!("write {format} rows"),
            len,
            || write_rows(format, &rows).len(),
            || theirs_write(format, &ox_rows).len(),
        );
        report.case(
            &format!("write {format} named"),
            len,
            || write_named(format, &rows).len(),
            || theirs_write(format, &ox_rows).len(),
        );
    }

    println!("Reading");
    for format in &formats[..3] {
        let format = *format;
        let bytes = write_rows(format, &rows);
        assert_eq!(ours_slice(format, &bytes), theirs_slice(format, &bytes));
        report.case(
            &format!("read {format} slice"),
            bytes.len(),
            || ours_slice(format, &bytes),
            || theirs_slice(format, &bytes),
        );
        report.case(
            &format!("read {format} reader"),
            bytes.len(),
            || ours_reader(format, &bytes),
            || theirs_reader(format, &bytes),
        );
    }
}
