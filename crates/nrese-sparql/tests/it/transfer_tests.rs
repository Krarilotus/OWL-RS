//! `ADD`, `MOVE` and `COPY` (SPARQL 1.1 Update §3.2.3–3.2.5) transfer "all data from an input
//! graph" of the Graph Store (§1.1: its default graph and its named graphs, as stored) into
//! the destination graph. What a `WHERE` clause reads with doesn't change that input: not the
//! union default graph, not the inferred statements (the default graph's inferred stack is
//! derived, not stored data: `CLEAR` leaves it), not a protocol dataset (SYN-54).
//!
//! Each row checks the asserted statements of every graph afterwards. Rows marked `guard`
//! held before the fix; the others failed at their answer on engine-v2 58af963dc.

use std::sync::Arc;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, QuadPattern, ReadModel};
use nrese_rdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad};
use nrese_sparql::{
    GraphAccess, QueryDatasetSpecification, QueryOptions, QueryResults, UpdateError, UpdateOptions,
    apply_update, evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

fn ex(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{name}"))
}

/// `ex:d` in the default graph, `ex:a` in `<a>`, `ex:b` in `<b>`; with `inferred`, `ex:i`
/// in the default graph's inferred stack, where the reasoner writes it.
fn store(inferred: bool) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (s, graph) in [
        ("d", GraphName::DefaultGraph),
        ("a", ex("a").into()),
        ("b", ex("b").into()),
    ] {
        let quad = Quad::new(ex(s), ex("p"), Literal::new_simple_literal(s), graph);
        tx.insert(quad.as_ref());
    }
    if inferred {
        let triple = EncodedTriple {
            subject: tx.intern(ex("i").as_ref().into()),
            predicate: tx.intern(ex("p").as_ref().into()),
            object: tx.intern(Literal::new_simple_literal("i").as_ref().into()),
        };
        tx.insert_inferred(triple);
    }
    tx.commit().unwrap();
    engine
}

/// The asserted statements, sorted, each `graph:subject` (`DEFAULT` or local names).
fn asserted(engine: &Engine) -> String {
    let snapshot = engine.snapshot();
    let local = |iri: &str| iri.strip_prefix(EX).unwrap_or(iri).to_owned();
    let mut all: Vec<String> = snapshot
        .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
        .map(|quad| {
            let quad = snapshot.decode_quad(quad).unwrap();
            let graph = match &quad.graph_name {
                GraphName::NamedNode(n) => local(n.as_str()),
                _ => "DEFAULT".to_owned(),
            };
            let subject = match &quad.subject {
                NamedOrBlankNode::NamedNode(n) => local(n.as_str()),
                NamedOrBlankNode::BlankNode(_) => "_".to_owned(),
            };
            format!("{graph}:{subject}")
        })
        .collect();
    all.sort();
    all.join(" ")
}

/// What the update's `WHERE` clauses read with, by name.
fn options(reads: &str) -> UpdateOptions {
    let only_a = |default_graph| {
        Some(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}a")],
            default_graph,
            ..GraphAccess::default()
        }))
    };
    let mut options = UpdateOptions::default();
    match reads {
        "plain" | "inferred" => {}
        "union" => options.union_default_graph = true,
        // The protocol's using-graph-uri=<b>.
        "using-b" => {
            let mut using = QueryDatasetSpecification::new();
            using.set_default_graph(vec![ex("b").into()]);
            options.using = Some(using);
        }
        // A reader of the default graph and <a>, in union mode.
        "not-b" => {
            options.union_default_graph = true;
            options.access = only_a(true);
        }
        "writes-a" => options.writable = only_a(false),
        _ => unreachable!("{reads}"),
    }
    options
}

/// The store's asserted statements after the update, or `forbidden`, or the update's error.
fn run(reads: &str, update: &str) -> String {
    let engine = store(reads == "inferred");
    let update = update.replace("<ex:", &format!("<{EX}"));
    let update = SparqlParser::new().parse_update(&update).unwrap();
    let mut tx = engine.transaction();
    match apply_update(&mut tx, &update, &options(reads)) {
        Ok(()) => {}
        Err(UpdateError::Forbidden(_)) => return "forbidden".to_owned(),
        Err(e) => return e.to_string(),
    }
    tx.commit().unwrap();
    asserted(&engine)
}

/// `[guard] reads | update | the asserted statements afterwards`.
const ROWS: &str = "
# The union default graph is what WHERE clauses read, not the Graph Store's default graph;
# MOVE drops the destination first, and that doesn't change its input either.
union | COPY DEFAULT TO <ex:dest> | DEFAULT:d a:a b:b dest:d
union | ADD DEFAULT TO <ex:a> | DEFAULT:d a:a a:d b:b
union | MOVE DEFAULT TO <ex:dest> | a:a b:b dest:d
union | MOVE SILENT DEFAULT TO <ex:b> | a:a b:d
guard union | COPY <ex:a> TO DEFAULT | DEFAULT:a a:a b:b
guard plain | COPY DEFAULT TO <ex:dest> | DEFAULT:d a:a b:b dest:d
# The inferred statements are derived from the stored data, not part of it.
inferred | COPY DEFAULT TO <ex:dest> | DEFAULT:d a:a b:b dest:d
inferred | ADD DEFAULT TO <ex:a> | DEFAULT:d a:a a:d b:b
inferred | MOVE DEFAULT TO <ex:dest> | a:a b:b dest:d
# The protocol's dataset is the dataset of DELETE/INSERT operations (its named graphs stay).
guard using-b | COPY <ex:a> TO <ex:dest> | DEFAULT:d a:a b:b dest:a
using-b | COPY DEFAULT TO <ex:dest> | DEFAULT:d a:a b:b dest:d
# Graph access holds: a graph the reader can't read is absent, one it can't write refused.
not-b | COPY DEFAULT TO <ex:dest> | DEFAULT:d a:a b:b dest:d
guard not-b | COPY <ex:b> TO <ex:dest> | DEFAULT:d a:a b:b
guard writes-a | COPY <ex:a> TO <ex:dest> | forbidden
guard writes-a | ADD <ex:none> TO <ex:dest> | DEFAULT:d a:a b:b
# A graph holding nothing doesn't exist (ADR-0002): transferring it moves nothing.
guard plain | COPY <ex:none> TO <ex:a> | DEFAULT:d b:b
guard plain | COPY SILENT <ex:none> TO <ex:a> | DEFAULT:d b:b
guard plain | MOVE <ex:none> TO <ex:a> | DEFAULT:d b:b
guard plain | MOVE <ex:a> TO <ex:a> | DEFAULT:d a:a b:b
# The input is the state after the request's earlier operations.
guard plain | INSERT DATA { GRAPH <ex:a> { <ex:x> <ex:p> 1 } } ; COPY <ex:a> TO <ex:dest> | DEFAULT:d a:a a:x b:b dest:a dest:x
# Another store's name for the default graph names it here too.
guard plain | COPY <urn:x-arq:DefaultGraph> TO <ex:dest> | DEFAULT:d a:a b:b dest:d
";

#[test]
fn transfers_read_the_stored_source_graph_whatever_the_read_options() {
    let mut failures = Vec::new();
    for row in ROWS
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let [reads, update, expected] = row.split(" | ").collect::<Vec<_>>()[..] else {
            panic!("row {row}");
        };
        let got = run(reads.trim_start_matches("guard "), update);
        let ok = got == expected;
        println!(
            "{} {reads} | {update}: {got}",
            if ok { "ok" } else { "FAILED" }
        );
        if !ok {
            failures.push(format!("{row}\n  got {got}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Guard: COPY's `WHERE` binds `?s` to the stored blank node, and its template has none of its
/// own: the copy holds the same blank node (store-wide blank node identity, brief §6).
#[test]
fn guard_a_copy_keeps_its_blank_nodes() {
    let engine = store(false);
    let update =
        format!("INSERT DATA {{ GRAPH <{EX}a> {{ _:x <{EX}p> 1 }} }} ; COPY <{EX}a> TO <{EX}dest>");
    let update = SparqlParser::new().parse_update(&update).unwrap();
    let mut tx = engine.transaction();
    apply_update(&mut tx, &update, &UpdateOptions::default()).unwrap();
    tx.commit().unwrap();
    let ask = format!(
        "ASK {{ GRAPH <{EX}a> {{ ?x <{EX}p> 1 }} GRAPH <{EX}dest> {{ ?x <{EX}p> 1 }} FILTER isBlank(?x) }}"
    );
    let ask = SparqlParser::new().parse_query(&ask).unwrap();
    let snapshot = engine.snapshot();
    let same = evaluate_query(&snapshot, &ask, &QueryOptions::default()).unwrap();
    assert!(matches!(same, QueryResults::Boolean(true)));
}
