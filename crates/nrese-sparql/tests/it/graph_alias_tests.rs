//! Other stores' names for the default graph (`urn:x-arq:DefaultGraph`, `urn:x-rdflib:default`,
//! [`nrese_sparql::compat`]) name it in every update operation, as they do in `GRAPH` patterns,
//! data, templates and datasets: `CLEAR`, `DROP` and `CREATE` of such a name act on the default
//! graph, under the default graph's access rules (`CREATE` as ADR-0002 has it: a graph exists
//! iff it holds a quad), and so do the `DROP` steps of `COPY` and `MOVE`.
//!
//! Each row checks the asserted statements of every graph afterwards. Rows marked `guard` held
//! before the fix; the others failed at their answer on engine-v2 58af963dc.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig, QuadPattern, ReadModel};
use nrese_rdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad};
use nrese_sparql::{GraphAccess, UpdateError, UpdateOptions, apply_update};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

fn ex(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{name}"))
}

/// The asserted statements, sorted, each `graph:subject` (`DEFAULT` or local names), after
/// `update` on a store of `ex:d` in the default graph and `ex:a` in `<a>`; or the update's
/// error: `forbidden`, `exists`.
fn run(reads: &str, update: &str) -> String {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (s, graph) in [("d", GraphName::DefaultGraph), ("a", ex("a").into())] {
        let quad = Quad::new(ex(s), ex("p"), Literal::new_simple_literal(s), graph);
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    let access = |graphs: &[&str], default_graph| {
        Some(Arc::new(GraphAccess {
            graphs: graphs.iter().map(|g| (*g).to_owned()).collect(),
            default_graph,
            ..GraphAccess::default()
        }))
    };
    let a = format!("{EX}a");
    let mut options = UpdateOptions::default();
    match reads {
        "plain" => {}
        // A reader of <a> alone.
        "reads-a" => options.access = access(&[a.as_str()], false),
        // A reader of everything who may write <a> alone.
        "writes-a" => options.writable = access(&[a.as_str()], false),
        // A writable list that spells the alias as a graph IRI, without the default graph.
        "writes-alias-iri" => options.writable = access(&["urn:x-arq:DefaultGraph"], false),
        // A reader of everything who may write the default graph alone.
        "writes-default" => options.writable = access(&[], true),
        _ => unreachable!("{reads}"),
    }
    let update = update.replace("<ex:", &format!("<{EX}"));
    let update = SparqlParser::new().parse_update(&update).unwrap();
    let mut tx = engine.transaction();
    match apply_update(&mut tx, &update, &options) {
        Ok(()) => {}
        Err(UpdateError::Forbidden(_)) => return "forbidden".to_owned(),
        Err(UpdateError::GraphAlreadyExists(_)) => return "exists".to_owned(),
        Err(e) => return e.to_string(),
    }
    tx.commit().unwrap();
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

/// `[guard] reads | update | the asserted statements afterwards, or the error`.
const ROWS: &str = "
# CLEAR and DROP of another store's name for the default graph empty the default graph.
plain | CLEAR GRAPH <urn:x-arq:DefaultGraph> | a:a
plain | DROP GRAPH <urn:x-rdflib:default> | a:a
plain | DROP SILENT GRAPH <urn:x-arq:DefaultGraph> | a:a
guard plain | CLEAR DEFAULT | a:a
# So do the DROP steps of COPY and MOVE.
plain | COPY <ex:a> TO <urn:x-arq:DefaultGraph> | DEFAULT:a a:a
plain | MOVE <urn:x-arq:DefaultGraph> TO <ex:a> | a:d
plain | MOVE <ex:a> TO <urn:x-rdflib:default> | DEFAULT:a
# CREATE: the default graph exists iff it holds a quad (ADR-0002).
plain | CREATE GRAPH <urn:x-arq:DefaultGraph> | exists
guard plain | CREATE SILENT GRAPH <urn:x-arq:DefaultGraph> | DEFAULT:d a:a
plain | DROP GRAPH <urn:x-arq:DefaultGraph> ; CREATE GRAPH <urn:x-arq:DefaultGraph> | a:a
# The default graph's access rules: absent for a reader who can't read it, refused for one who
# can't write it, whatever IRI a list spells.
guard reads-a | CLEAR GRAPH <urn:x-arq:DefaultGraph> | DEFAULT:d a:a
guard reads-a | CREATE GRAPH <urn:x-arq:DefaultGraph> | DEFAULT:d a:a
writes-a | CLEAR GRAPH <urn:x-arq:DefaultGraph> | forbidden
writes-alias-iri | CLEAR GRAPH <urn:x-arq:DefaultGraph> | forbidden
# The name already meant the default graph in data, templates, GRAPH patterns and datasets.
guard plain | INSERT DATA { GRAPH <urn:x-arq:DefaultGraph> { <ex:x> <ex:p> 1 } } | DEFAULT:d DEFAULT:x a:a
guard plain | DELETE WHERE { GRAPH <urn:x-rdflib:default> { ?s ?p ?o } } | a:a
guard plain | INSERT { GRAPH <ex:dest> { ?s ?p ?o } } USING <urn:x-arq:DefaultGraph> WHERE { ?s ?p ?o } | DEFAULT:d a:a dest:d
# Review r1, S2-F1 (rows failed on slice 2's 10e51e7e): a transfer between two names of the
# default graph is a transfer of a graph to itself, which changes nothing.
plain | COPY DEFAULT TO <urn:x-arq:DefaultGraph> | DEFAULT:d a:a
plain | COPY <urn:x-arq:DefaultGraph> TO DEFAULT | DEFAULT:d a:a
plain | COPY <urn:x-arq:DefaultGraph> TO <urn:x-rdflib:default> | DEFAULT:d a:a
plain | MOVE DEFAULT TO <urn:x-rdflib:default> | DEFAULT:d a:a
plain | MOVE <urn:x-rdflib:default> TO DEFAULT | DEFAULT:d a:a
plain | MOVE <urn:x-rdflib:default> TO <urn:x-arq:DefaultGraph> | DEFAULT:d a:a
guard plain | ADD <urn:x-arq:DefaultGraph> TO DEFAULT | DEFAULT:d a:a
# Review r1, S2-F2: a template's graph variable bound to such a name writes the default graph,
# under the default graph's rights.
plain | INSERT { GRAPH ?g { <ex:x> <ex:p> 1 } } WHERE { VALUES ?g { <urn:x-arq:DefaultGraph> } } | DEFAULT:d DEFAULT:x a:a
plain | DELETE { GRAPH ?g { ?s ?p ?o } } WHERE { VALUES ?g { <urn:x-rdflib:default> } ?s ?p ?o } | a:a
writes-default | INSERT { GRAPH ?g { <ex:x> <ex:p> 1 } } WHERE { VALUES ?g { <urn:x-arq:DefaultGraph> } } | DEFAULT:d DEFAULT:x a:a
writes-alias-iri | INSERT { GRAPH ?g { <ex:x> <ex:p> 1 } } WHERE { VALUES ?g { <urn:x-arq:DefaultGraph> } } | forbidden
";

#[test]
fn another_store_s_default_graph_name_is_the_default_graph_in_every_update() {
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
