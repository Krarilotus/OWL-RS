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

/// What the schema reads of individuals in a class expression is that the expression has
/// one: more members of a restriction that has one keep the compiled TBox (P2-C1 timing,
/// 7 Nov: every membership read made each commit's first query 10 ms at 10,000 members).
#[test]
fn more_members_of_a_class_expression_keep_the_compiled_tbox() {
    let engine = engine(":m0 a _:restriction .");
    let ql = QlRewriting::new(Closure { lists: true });
    let original = ql.tbox(&engine.snapshot(), None);
    let snapshot = engine.snapshot();
    // The restriction's own blank node, as the store has it.
    let on = iri(&snapshot, OWL, "onProperty").unwrap();
    let restriction = snapshot
        .quads_for_pattern_in(
            nrese_engine::ReadModel::Asserted,
            &QuadPattern {
                subject: None,
                predicate: Some(on),
                object: None,
                graph: GraphSelector::Any,
            },
        )
        .next()
        .map(|q| snapshot.decode(q.subject).unwrap())
        .unwrap();
    let Term::BlankNode(restriction) = restriction else {
        panic!("{restriction:?}");
    };
    let mut tx = engine.transaction();
    for n in 1..50 {
        tx.insert(
            Quad::new(
                NamedNode::new_unchecked(format!("{EX}m{n}")),
                NamedNode::new_unchecked(format!("{RDF}type")),
                restriction.clone(),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    assert!(Arc::ptr_eq(&original, &ql.tbox(&engine.snapshot(), None)));
}

/// A commit's note keeps the other expressions' memberships for the next schema read
/// (P2-C1, the read loss of 8 Nov: each new snapshot scanned every expression's members).
/// With 20 restrictions, 3 members each:
/// - a noted commit adding a member to one rescans that one; an ordinary edge, none;
/// - deleting the member the schema holds while others remain rescans it, and the schema
///   read equals a fresh one (still one member); deleting the last ones too: none left;
/// - a commit without a note, and a transaction's pending state, rescan every one.
///
/// Each read counts three things: memberships scanned, expressions walked, membership
/// statements written into a schema. A noted commit walks only the expressions it
/// touched, and writes a schema only where a membership changed (P2-C1 probe, 8 Nov: the
/// rebuild of every expression's took 188 us a snapshot). Each schema read is compared
/// with a fresh one: reuse never changes what is read.
#[test]
fn a_noted_commit_rescans_only_the_memberships_it_touched() {
    let restrictions = 20;
    let mut text = String::from(
        "@prefix : <http://example.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .\n",
    );
    for e in 0..restrictions {
        text.push_str(&format!(
            "_:e{e} a owl:Restriction ; owl:onProperty :S{e} ; owl:someValuesFrom :A .\n"
        ));
        for k in 0..3 {
            text.push_str(&format!(":m{e}_{k} a _:e{e} .\n"));
        }
    }
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
        tx.insert(q.unwrap().as_ref());
    }
    tx.commit().unwrap();
    let ql = QlRewriting::new(Closure { lists: false });
    let scans = |snapshot: &Snapshot| {
        for cell in [&MEMBER_SCANS, &EXPRESSIONS_WALKED, &MEMBERS_WRITTEN] {
            cell.set(0);
        }
        ql.tbox(snapshot, None);
        [
            MEMBER_SCANS.get(),
            EXPRESSIONS_WALKED.get(),
            MEMBERS_WRITTEN.get(),
        ]
    };
    let fresh = |snapshot: &Snapshot| schema_statements(snapshot, &|_| true);
    let mut wrong = Vec::new();
    let all = scans(&engine.snapshot());
    if all != [restrictions; 3] {
        wrong.push(format!("first read: {all:?} (scanned, walked, written)"));
    }
    // One restriction's node and its members' statements, as the store has them.
    let snapshot = engine.snapshot();
    let rdf_type = iri(&snapshot, RDF, "type").unwrap();
    let on = iri(&snapshot, OWL, "onProperty").unwrap();
    let node = snapshot
        .quads_for_pattern_in(
            ReadModel::Asserted,
            &QuadPattern {
                subject: None,
                predicate: Some(on),
                object: None,
                graph: GraphSelector::Any,
            },
        )
        .next()
        .unwrap()
        .subject;
    let members: Vec<_> = snapshot
        .quads_for_pattern_in(
            ReadModel::Asserted,
            &QuadPattern {
                subject: None,
                predicate: Some(rdf_type),
                object: Some(node),
                graph: GraphSelector::Any,
            },
        )
        .collect();
    drop(snapshot);
    let decode = |id| engine.snapshot().decode(id).unwrap();
    let Term::BlankNode(blank) = decode(node) else {
        panic!("not a blank node");
    };
    let commit = |change: &dyn Fn(&mut nrese_engine::Transaction<'_>), noted: bool| {
        let mut tx = engine.transaction();
        change(&mut tx);
        let (from, touched) = (tx.base().asserted_identity(), touched_expressions(&tx));
        let summary = tx.commit().unwrap();
        if noted {
            ql.note_commit(from, summary.asserted, touched);
        }
    };
    let mut check = |name: &str, expected: [usize; 3]| {
        let snapshot = engine.snapshot();
        let n = scans(&snapshot);
        let cached = ql.cached.lock().unwrap();
        let read = cached[0].schema(&snapshot);
        drop(cached);
        if n != expected || read != fresh(&snapshot) {
            wrong.push(format!(
                "{name}: {n:?} (expected {expected:?}; scanned, walked, written), equal to a                  fresh read: {}",
                read == fresh(&snapshot)
            ));
        }
    };
    let member = |n: &str| {
        Quad::new(
            NamedNode::new_unchecked(format!("{EX}{n}")),
            NamedNode::new_unchecked(format!("{RDF}type")),
            blank.clone(),
            GraphName::DefaultGraph,
        )
    };
    commit(
        &|tx| {
            tx.insert(member("new").as_ref());
        },
        true,
    );
    check("a member added", [1, 1, 0]);
    commit(
        &|tx| {
            tx.insert(quad("ann", &format!("{EX}knows"), "bob").as_ref());
        },
        true,
    );
    check("an edge", [0, 0, 0]);
    // The member the schema holds: the first of the restriction's in store order.
    commit(
        &|tx| {
            tx.remove_encoded(members[0]);
        },
        true,
    );
    check("the held member deleted", [1, 1, restrictions]);
    commit(
        &|tx| {
            for m in &members[1..] {
                tx.remove_encoded(*m);
            }
            tx.remove(member("new").as_ref());
        },
        true,
    );
    check("every member deleted", [1, 1, restrictions - 1]);
    commit(
        &|tx| {
            tx.insert(member("again").as_ref());
        },
        false,
    );
    check("a commit without a note", [restrictions; 3]);
    let tx = engine.transaction();
    let pending = scans(&tx.pending_snapshot());
    if pending[..2] != [restrictions; 2] {
        wrong.push(format!("pending: {pending:?}"));
    }
    drop(tx);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn introducing_owl_thing_without_changing_the_schema_matches_fresh_preparation() {
    let engine = engine("");
    let cached = options();
    check(&engine.snapshot(), &cached, &["<http://example.org/ann>"]);
    assert!(iri(&engine.snapshot(), OWL, "Thing").is_none());
    let mut tx = engine.transaction();
    tx.insert(
        Quad::new(
            NamedNode::new_unchecked(format!("{EX}note")),
            NamedNode::new_unchecked(format!("{EX}mentions")),
            NamedNode::new_unchecked(format!("{OWL}Thing")),
            GraphName::DefaultGraph,
        )
        .as_ref(),
    );
    let query = SparqlParser::new()
        .parse_query(&format!(
            "SELECT ?x WHERE {{ ?x <{EX}worksFor> ?y . ?y a <{OWL}Thing> }}"
        ))
        .unwrap();
    let answers = |snapshot: &Snapshot, options: &QueryOptions| {
        let QueryResults::Solutions(rows) = evaluate_query(snapshot, &query, options).unwrap()
        else {
            panic!("solutions expected");
        };
        let mut rows: Vec<_> = rows
            .map(|row| row.unwrap().get("x").unwrap().to_string())
            .collect();
        rows.sort();
        rows
    };
    let pending = tx.pending_snapshot();
    assert_eq!(
        schema_statements(&engine.snapshot(), &|_| true),
        schema_statements(&pending, &|_| true)
    );
    let expected = answers(&pending, &options());
    assert_eq!(expected, ["<http://example.org/ann>"]);
    assert_eq!(answers(&pending, &cached), expected);
    tx.commit().unwrap();
    assert_eq!(answers(&engine.snapshot(), &cached), expected);
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
