//! Parse time of nrese-sparql-syntax against spargebra: the queries and updates of the W3C
//! SPARQL 1.0, 1.1 and 1.2 suites that both accept (small, varied queries, as an endpoint
//! sees them), and generated large ones (a long basic graph pattern, a large `VALUES`
//! block, a long expression), as applications and federation produce them. Writing
//! (algebra to text, what federation does per `SERVICE` call) is measured too.
//!
//! `cargo run --release --bin syntax [filter]`

use std::fmt::Write;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

const ROUNDS: usize = 9;

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

fn report(
    filter: &Option<String>,
    name: &str,
    bytes: usize,
    ours: impl FnMut() -> usize,
    theirs: impl FnMut() -> usize,
) {
    if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
        return;
    }
    let (a, b) = (seconds(ours), seconds(theirs));
    let mb = bytes as f64 / 1e6;
    println!(
        "{name:<34} nrese {:>8.1} MB/s {:>9.3} ms   spargebra {:>8.1} MB/s {:>9.3} ms   time ratio {:>5.2}",
        mb / a,
        a * 1e3,
        mb / b,
        b * 1e3,
        a / b
    );
}

/// The `.rq` and `.ru` files of the W3C suites.
fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/rdf-tests/sparql");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rq" || e == "ru") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

const BASE: &str = "http://example.org/base/";

fn ours() -> nrese_sparql_syntax::SparqlParser {
    nrese_sparql_syntax::SparqlParser::new()
        .with_base_iri(BASE)
        .unwrap()
}

fn theirs() -> spargebra::SparqlParser {
    spargebra::SparqlParser::new().with_base_iri(BASE).unwrap()
}

fn large_bgp() -> String {
    let mut q = String::from("PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n");
    for i in 0..2_000 {
        let _ = writeln!(
            q,
            "  ?s{} ex:p{} ?o{} ; ex:label \"name {i}\"@en ; ex:value {i} .",
            i % 50,
            i % 17,
            i
        );
    }
    q.push('}');
    q
}

fn large_values() -> String {
    let mut q = String::from(
        "PREFIX ex: <http://example.org/>\nSELECT ?s ?name WHERE {\n  VALUES (?s ?name) {\n",
    );
    for i in 0..20_000 {
        let _ = writeln!(q, "    (<http://example.org/item/{i}> \"item {i}\")");
    }
    q.push_str("  }\n  ?s ex:name ?name .\n}");
    q
}

fn long_expression() -> String {
    let mut q = String::from("SELECT * WHERE { ?s ?p ?o FILTER(");
    for i in 0..3_000 {
        if i > 0 {
            q.push_str(" || ");
        }
        let _ = write!(
            q,
            "(?o = {i} && STRSTARTS(STR(?s), \"http://example.org/{i}\"))"
        );
    }
    q.push_str(") }");
    q
}

fn main() {
    let filter = std::env::args().nth(1);
    // The corpus: what both parse, with each file's kind.
    let mut queries = Vec::new();
    let mut updates = Vec::new();
    for path in corpus() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if path.extension().is_some_and(|e| e == "ru") {
            if ours().parse_update(&text).is_ok() && theirs().parse_update(&text).is_ok() {
                updates.push(text);
            }
        } else if ours().parse_query(&text).is_ok() && theirs().parse_query(&text).is_ok() {
            queries.push(text);
        }
    }
    let bytes = |texts: &[String]| texts.iter().map(String::len).sum::<usize>();
    println!(
        "Parsing ({} W3C queries, {} updates; time ratio < 1: nrese faster)",
        queries.len(),
        updates.len()
    );
    report(
        &filter,
        "W3C queries",
        bytes(&queries),
        || {
            let parser = ours();
            queries
                .iter()
                .map(|q| parser.parse_query(q).is_ok() as usize)
                .sum()
        },
        || {
            queries
                .iter()
                .map(|q| theirs().parse_query(q).is_ok() as usize)
                .sum()
        },
    );
    report(
        &filter,
        "W3C updates",
        bytes(&updates),
        || {
            let parser = ours();
            updates
                .iter()
                .map(|u| parser.parse_update(u).is_ok() as usize)
                .sum()
        },
        || {
            updates
                .iter()
                .map(|u| theirs().parse_update(u).is_ok() as usize)
                .sum()
        },
    );
    for (name, text) in [
        ("BGP of 6,000 triple patterns", large_bgp()),
        ("VALUES of 20,000 rows", large_values()),
        ("FILTER of 3,000 alternatives", long_expression()),
    ] {
        assert!(ours().parse_query(&text).is_ok() && theirs().parse_query(&text).is_ok());
        report(
            &filter,
            name,
            text.len(),
            || ours().parse_query(&text).is_ok() as usize,
            || theirs().parse_query(&text).is_ok() as usize,
        );
    }

    println!("Writing (algebra to SPARQL text)");
    let ours_parsed: Vec<_> = queries
        .iter()
        .map(|q| ours().parse_query(q).unwrap())
        .collect();
    let theirs_parsed: Vec<_> = queries
        .iter()
        .map(|q| theirs().parse_query(q).unwrap())
        .collect();
    report(
        &filter,
        "write W3C queries",
        bytes(&queries),
        || ours_parsed.iter().map(|q| q.to_string().len()).sum(),
        || theirs_parsed.iter().map(|q| q.to_string().len()).sum(),
    );
    let big = large_bgp();
    let (a, b) = (
        ours().parse_query(&big).unwrap(),
        theirs().parse_query(&big).unwrap(),
    );
    report(
        &filter,
        "write BGP of 6,000 patterns",
        big.len(),
        || a.to_string().len(),
        || b.to_string().len(),
    );
}
