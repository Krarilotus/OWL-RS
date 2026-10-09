use super::*;
use crate::{QueryOptions, QueryResults, evaluate_query, plan_query};
use nrese_engine::{EncodedQuad, Engine, EngineConfig, InferredSubset};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.org/";
const SCHEMA: &str = "@prefix : <http://example.org/> .
    @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
    @prefix owl: <http://www.w3.org/2002/07/owl#> .
    :Employee rdfs:subClassOf _:restriction .
    _:restriction a owl:Restriction ; owl:onProperty :worksFor ;
        owl:someValuesFrom :Organisation .
    :ann a :Employee .";

fn engine(data: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let text = format!("{SCHEMA} {data}");
    let mut tx = engine.transaction();
    for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
        tx.insert(q.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn quad(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("{EX}{s}")),
        NamedNode::new_unchecked(p),
        NamedNode::new_unchecked(format!("{EX}{o}")),
        GraphName::DefaultGraph,
    )
}

fn options() -> QueryOptions {
    QueryOptions {
        ql: Some(Arc::new(QlRewriting::new(Closure { lists: true }))),
        ..QueryOptions::default()
    }
}

fn replace_role(tx: &mut nrese_engine::Transaction<'_>) {
    let predicate = tx
        .lookup(NamedNodeRef::new_unchecked(&format!("{OWL}onProperty")).into())
        .unwrap();
    let original = tx
        .quads_for_pattern(&QuadPattern {
            predicate: Some(predicate),
            ..QuadPattern::all()
        })
        .next()
        .unwrap();
    let mut replacement = tx.decode_quad(original).unwrap();
    replacement.object = NamedNode::new_unchecked(format!("{EX}knows")).into();
    assert!(tx.remove_encoded(original));
    tx.insert(replacement.as_ref());
}

fn check(snapshot: &Snapshot, options: &QueryOptions, expected: &[&str]) -> QlReport {
    let query = SparqlParser::new()
        .parse_query("SELECT ?x WHERE { ?x <http://example.org/worksFor> ?y }")
        .unwrap();
    let report = plan_query(snapshot, &query, options).unwrap().ql.unwrap();
    let QueryResults::Solutions(rows) = evaluate_query(snapshot, &query, options).unwrap() else {
        panic!("solutions expected");
    };
    let mut got: Vec<_> = rows
        .map(|row| row.unwrap().get("x").unwrap().to_string())
        .collect();
    got.sort();
    assert_eq!(got, expected);
    report
}

#[test]
fn equal_count_pending_schema_replacements_and_rollback_use_their_own_tbox() {
    let engine = engine("");
    let options = options();
    let base = engine.snapshot();
    check(&base, &options, &["<http://example.org/ann>"]);
    let mut tx = engine.transaction();
    replace_role(&mut tx);
    let pending = tx.pending_snapshot();
    assert_eq!(
        (base.revision(), base.len()),
        (pending.revision(), pending.len())
    );
    check(&pending, &options, &[]);
    drop(tx);
    check(&engine.snapshot(), &options, &["<http://example.org/ann>"]);
}

#[test]
fn equal_count_pending_witness_replacements_and_rollback_recheck_the_data() {
    let engine = engine(":ann :worksFor :acme .");
    let options = options();
    let base = engine.snapshot();
    let report = check(&base, &options, &["<http://example.org/ann>"]);
    assert_eq!((report.patterns, report.realised, report.checks), (0, 1, 1));
    let mut tx = engine.transaction();
    tx.remove(quad("ann", &format!("{EX}worksFor"), "acme").as_ref());
    tx.insert(quad("ann", &format!("{EX}knows"), "acme").as_ref());
    let pending = tx.pending_snapshot();
    assert_eq!(
        (base.revision(), base.len()),
        (pending.revision(), pending.len())
    );
    let report = check(&pending, &options, &["<http://example.org/ann>"]);
    assert_eq!((report.patterns, report.realised, report.checks), (1, 0, 1));
    drop(tx);
    let report = check(&engine.snapshot(), &options, &["<http://example.org/ann>"]);
    assert_eq!((report.patterns, report.realised, report.checks), (0, 1, 1));
    let report = check(&engine.snapshot(), &options, &["<http://example.org/ann>"]);
    assert_eq!((report.realised, report.checks), (1, 0));
}

#[test]
fn equal_count_inferred_masks_do_not_share_realised_witnesses() {
    let engine = engine(":other :knows :acme .");
    let snapshot = engine.snapshot();
    let id = |s: &str| {
        snapshot
            .lookup(NamedNodeRef::new_unchecked(&format!("{EX}{s}")).into())
            .unwrap()
    };
    let edge = |p| EncodedQuad {
        subject: id("ann"),
        predicate: id(p),
        object: id("acme"),
        graph: TermId::DEFAULT_GRAPH,
    };
    let works = edge("worksFor");
    let knows = edge("knows");
    let all = snapshot.with_inferred_added(&[works, knows]);
    let visible = all.with_inferred_subset(InferredSubset::Only(vec![works]));
    let hidden = all.with_inferred_subset(InferredSubset::Only(vec![knows]));
    assert_eq!(
        (visible.revision(), visible.len()),
        (hidden.revision(), hidden.len())
    );
    let options = options();
    assert_eq!(
        check(&visible, &options, &["<http://example.org/ann>"]).realised,
        1
    );
    assert_eq!(
        check(&hidden, &options, &["<http://example.org/ann>"]).realised,
        0
    );
    assert_eq!(
        check(&visible, &options, &["<http://example.org/ann>"]).realised,
        1
    );
}

#[test]
fn unchanged_schema_reuses_the_compiled_tbox_across_views_and_commits() {
    let engine = engine("");
    let ql = QlRewriting::new(Closure { lists: true });
    let original = ql.tbox(&engine.snapshot(), None);
    let mut tx = engine.transaction();
    tx.insert(quad("ann", &format!("{EX}worksFor"), "acme").as_ref());
    assert!(Arc::ptr_eq(
        &original,
        &ql.tbox(&tx.pending_snapshot(), None)
    ));
    tx.commit().unwrap();
    assert!(Arc::ptr_eq(&original, &ql.tbox(&engine.snapshot(), None)));
}

#[test]
fn supported_graph_readers_keep_separate_schema_entries() {
    let engine = engine("");
    let ql = QlRewriting::new(Closure { lists: true });
    let visible = GraphAccess {
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    };
    let hidden = GraphAccess {
        default_graph: false,
        ..visible.clone()
    };
    let snapshot = engine.snapshot();
    let original = ql.tbox(&snapshot, Some(&visible));
    assert!(!original.is_empty());
    assert!(ql.tbox(&snapshot, Some(&hidden)).is_empty());
    assert!(Arc::ptr_eq(&original, &ql.tbox(&snapshot, Some(&visible))));
    let mut tx = engine.transaction();
    replace_role(&mut tx);
    let pending = tx.pending_snapshot();
    assert!(!Arc::ptr_eq(&original, &ql.tbox(&pending, Some(&visible))));
    assert!(ql.tbox(&pending, Some(&hidden)).is_empty());
}
