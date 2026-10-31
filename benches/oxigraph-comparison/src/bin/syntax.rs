//! Parse time of nrese-sparql-syntax against spargebra: the queries and updates of the W3C
//! SPARQL 1.0, 1.1 and 1.2 suites that both accept (small, varied queries, as an endpoint
//! sees them), and generated large ones (a long basic graph pattern, a large `VALUES`
//! block, a long expression), as applications and federation produce them. Writing
//! (algebra to text, what federation does per `SERVICE` call) is measured too.
//!
//! `cargo run --release --bin syntax [filter]`

use std::fmt::Write;
use std::hint::black_box;
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

/// The comparisons the filter selects, and of them those timed: a run that times none, or
/// fewer than it selects, didn't do what it was asked (Sol, PR #61 F3: a filter matching no
/// comparison timed nothing and succeeded).
#[derive(Default)]
struct Receipt {
    selected: Vec<String>,
    timed: usize,
}

fn report(
    filter: &Option<String>,
    receipt: &mut Receipt,
    name: &str,
    bytes: usize,
    ours: impl FnMut() -> usize,
    theirs: impl FnMut() -> usize,
) {
    if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
        return;
    }
    receipt.selected.push(name.to_owned());
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
    receipt.timed += 1;
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
    let mut receipt = Receipt::default();
    // The corpus: the pinned W3C suites in full (no W3C case is timed without them), and of
    // them what both parse, with each file's kind.
    let files = oxigraph_comparison::w3c::sparql(&["rq", "ru"]).unwrap_or_else(|why| {
        eprintln!("{why}: no W3C case is timed");
        std::process::exit(2)
    });
    let mut queries = Vec::new();
    let mut updates = Vec::new();
    for file in files {
        // Neither parser reads bytes: a file that isn't UTF-8 is no case.
        let Ok(text) = String::from_utf8(file.bytes) else {
            continue;
        };
        if file.path.extension().is_some_and(|e| e == "ru") {
            if ours().parse_update(&text).is_ok() && theirs().parse_update(&text).is_ok() {
                updates.push(text);
            }
        } else if ours().parse_query(&text).is_ok() && theirs().parse_query(&text).is_ok() {
            queries.push(text);
        }
    }
    if queries.is_empty() || updates.is_empty() {
        eprintln!("no W3C query or no update both parsers accept: nothing to time as W3C cases");
        std::process::exit(2);
    }
    let bytes = |texts: &[String]| texts.iter().map(String::len).sum::<usize>();
    println!(
        "Parsing ({} W3C queries, {} updates; time ratio < 1: nrese faster)",
        queries.len(),
        updates.len()
    );
    report(
        &filter,
        &mut receipt,
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
        &mut receipt,
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
            &mut receipt,
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
        &mut receipt,
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
        &mut receipt,
        "write BGP of 6,000 patterns",
        big.len(),
        || a.to_string().len(),
        || b.to_string().len(),
    );
    if receipt.timed == 0 || receipt.timed < receipt.selected.len() {
        eprintln!(
            "{} of {} selected comparisons timed ({:?}; filter {filter:?}): less than asked",
            receipt.timed,
            receipt.selected.len(),
            receipt.selected
        );
        std::process::exit(2);
    }
}
