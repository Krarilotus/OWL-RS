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
fn equal_count_pending_witness_replacements_and_rollback_keep_the_published_probes() {
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
    // The pending read kept nothing (contract S3), so the published snapshot's own probe
    // still answers: no check is asked again.
    let report = check(&engine.snapshot(), &options, &["<http://example.org/ann>"]);
    assert_eq!((report.patterns, report.realised, report.checks), (0, 1, 0));
    let report = check(&engine.snapshot(), &options, &["<http://example.org/ann>"]);
    assert_eq!((report.realised, report.checks), (1, 0));
}

/// Each entry the rewriting keeps for other operations (contract S3): its key, the asserted
/// state its memberships were read at, and its TBox.
fn kept_entries(ql: &QlRewriting) -> Vec<(SnapshotIdentity, AssertedIdentity, *const Tbox)> {
    let cached = ql.cached.lock().unwrap();
    cached
        .iter()
        .map(|c| (c.key, c.members.at, Arc::as_ptr(&c.tbox)))
        .collect()
}

/// The probes the rewriting keeps for other operations (contract S3): the view they were
/// asked of, and each probe with its truth value.
fn kept_probes(ql: &QlRewriting) -> (Option<SnapshotIdentity>, Vec<(String, bool)>) {
    let realised = ql.realised.lock().unwrap();
    let mut probes: Vec<(String, bool)> = realised.1.iter().map(|(k, &v)| (k.clone(), v)).collect();
    probes.sort();
    (realised.0, probes)
}

/// Rewriting options with `ql`, sharing its state.
fn sharing(ql: &Arc<QlRewriting>) -> QueryOptions {
    QueryOptions {
        ql: Some(Arc::clone(ql)),
        ..QueryOptions::default()
    }
}

/// The schema's restriction, as the store holds it.
fn restriction(snapshot: &Snapshot) -> TermId {
    let on = iri(snapshot, OWL, "onProperty").unwrap();
    snapshot
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
        .subject
}

/// The statement that `s` is in `class`, by ids the store has.
fn membership(snapshot: &Snapshot, s: &str, class: TermId) -> EncodedQuad {
    EncodedQuad {
        subject: snapshot
            .lookup(NamedNodeRef::new_unchecked(&format!("{EX}{s}")).into())
            .unwrap(),
        predicate: iri(snapshot, RDF, "type").unwrap(),
        object: class,
        graph: TermId::DEFAULT_GRAPH,
    }
}

/// The snapshots no commit published that `changes` (each added or removed, by ids the
/// store has) make over the latest: a writer's pending one, its canonical view, a
/// speculative one and a private one (contract S3, review r0 §5 items 1 and 2). None has a
/// `SharedState`.
fn unpublished(engine: &Engine, changes: &[(EncodedQuad, bool)]) -> Vec<(&'static str, Snapshot)> {
    let apply = |tx: &mut nrese_engine::Transaction<'_>| {
        for &(quad, added) in changes {
            match added {
                true => tx.insert_encoded(quad),
                false => tx.remove_encoded(quad),
            };
        }
    };
    let mut out = Vec::new();
    let mut tx = engine.transaction();
    apply(&mut tx);
    let pending = tx.pending_snapshot();
    drop(tx);
    out.push((
        "the canonical view of a pending",
        pending.with_canonical_equality(),
    ));
    out.push(("a pending", pending));
    let mut tx = engine.speculative();
    apply(&mut tx);
    out.push(("a speculative", tx.pending_snapshot()));
    drop(tx);
    let budget = Arc::new(nrese_exec::Budget::unlimited());
    let mut tx = engine.speculative_over(&engine.snapshot(), budget).unwrap();
    for &(quad, added) in changes {
        match added {
            true => tx.insert_encoded(quad),
            false => tx.remove_encoded(quad),
        };
    }
    out.push(("a private", tx.pending_snapshot().unwrap()));
    for (name, snapshot) in &out {
        assert!(
            snapshot.shared_state().is_none(),
            "{name} snapshot is published"
        );
    }
    out
}

/// Contract S3, the TBox family (dictionary 3.3, G1): a snapshot no commit published whose
/// restriction gains a member reads its own schema, answering as a fresh rewriting does,
/// and leaves the kept entry as it found it. The next published read is the kept TBox,
/// with no membership scanned.
#[test]
fn unpublished_schema_reads_leave_the_kept_tbox() {
    let engine = engine("");
    let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
    let options = sharing(&ql);
    let published = engine.snapshot();
    let expected = ["<http://example.org/ann>"];
    check(&published, &options, &expected);
    let before = kept_entries(&ql);
    let gains = membership(&published, "ann", restriction(&published));
    // A version keeps what any rewriting reads of it (dictionary 3.0c), so the fresh
    // rewriting reads equal snapshots of versions of their own.
    let alike = unpublished(&engine, &[(gains, true)]);
    for ((name, snapshot), (_, same)) in unpublished(&engine, &[(gains, true)]).iter().zip(&alike) {
        let fresh = sharing(&Arc::new(QlRewriting::new(Closure { lists: true })));
        let own = check(same, &fresh, &expected);
        assert_eq!(check(snapshot, &options, &expected), own, "{name} snapshot");
        assert_eq!(kept_entries(&ql), before, "{name} snapshot");
    }
    MEMBER_SCANS.set(0);
    check(&published, &options, &expected);
    assert_eq!((kept_entries(&ql), MEMBER_SCANS.get()), (before, 0));
}

/// Contract S3, the memberships family (G2): snapshots no commit published that leave the
/// same expressions with a member (one more, one swapped, or no change) reuse the kept
/// TBox and leave its key and memberships as they were. The next published read scans no
/// membership.
#[test]
fn unpublished_membership_reads_leave_the_kept_memberships() {
    let engine = engine(":m0 a _:restriction .");
    let ql = QlRewriting::new(Closure { lists: true });
    let published = engine.snapshot();
    let original = ql.tbox(&published, None);
    let before = kept_entries(&ql);
    let class = restriction(&published);
    let (added, kept) = (
        membership(&published, "ann", class),
        membership(&published, "m0", class),
    );
    for changes in [
        &[(added, true)][..],
        &[(added, true), (kept, false)][..],
        &[][..],
    ] {
        for (name, snapshot) in unpublished(&engine, changes) {
            let tbox = ql.tbox(&snapshot, None);
            assert!(
                Arc::ptr_eq(&original, &tbox),
                "{name} snapshot, {changes:?}"
            );
            assert_eq!(kept_entries(&ql), before, "{name} snapshot, {changes:?}");
        }
    }
    MEMBER_SCANS.set(0);
    assert!(Arc::ptr_eq(&original, &ql.tbox(&published, None)));
    assert_eq!((kept_entries(&ql), MEMBER_SCANS.get()), (before, 0));
}

/// Contract S3, the probes family (G3): a probe asked of a snapshot no commit published is
/// answered from that snapshot's data and kept by its own version alone (dictionary 3.0c;
/// review r1 F1), so asked again of the same snapshot it is answered without a check; the
/// published view's probes stay and answer it again. A probe the budget leaves unasked
/// keeps no truth value (review r0 §5 item 4).
#[test]
fn unpublished_probes_are_kept_by_their_own_version_alone() {
    let engine = engine(":ann :worksFor :acme .");
    let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
    let options = sharing(&ql);
    let published = engine.snapshot();
    let expected = ["<http://example.org/ann>"];
    let report = check(&published, &options, &expected);
    assert_eq!((report.realised, report.checks), (1, 1));
    let before = kept_probes(&ql);
    assert!(!before.1.is_empty());
    let id = |s: &str| {
        published
            .lookup(NamedNodeRef::new_unchecked(&format!("{EX}{s}")).into())
            .unwrap()
    };
    let witness = EncodedQuad {
        subject: id("ann"),
        predicate: id("worksFor"),
        object: id("acme"),
        graph: TermId::DEFAULT_GRAPH,
    };
    let unasked = || {
        Arc::new(
            QlRewriting::new(Closure { lists: true }).with_limits(Limits {
                checks: 0,
                ..Limits::default()
            }),
        )
    };
    for (name, snapshot) in unpublished(&engine, &[(witness, false)]) {
        // Left unasked first, the version keeps nothing, so the next asks it.
        let report = check(&snapshot, &sharing(&unasked()), &expected);
        assert_eq!(report.checks, 0, "{name} snapshot, unasked");
        for (ask, checks) in [(0, 1), (1, 0)] {
            let report = check(&snapshot, &options, &expected);
            assert_eq!(
                (report.realised, report.checks),
                (0, checks),
                "{name} snapshot, ask {ask}"
            );
        }
        assert_eq!(kept_probes(&ql), before, "{name} snapshot");
    }
    let report = check(&published, &options, &expected);
    assert_eq!((report.realised, report.checks), (1, 0));
    let unasked = unasked();
    check(&published, &sharing(&unasked), &expected);
    assert!(kept_probes(&unasked).1.is_empty());
}

/// Review r1 F1 (dictionary 3.0c): a pending snapshot read again, as a store session's is,
/// reads its schema once. Its first read compiles, or reuses a published TBox its schema
/// matches; later reads of it scan no membership and give the same TBox, and the store's
/// shared entries stay as they were. A new pending snapshot is a new version, read afresh.
#[test]
fn a_retained_pending_snapshot_reads_its_schema_once() {
    for (data, gains, reuses) in [
        // The memberless restriction gains a member: its own TBox.
        ("", "ann", false),
        // The restriction has one, and gets another: the published TBox.
        (":m0 a _:restriction .", "ann", true),
    ] {
        let engine = engine(data);
        let ql = QlRewriting::new(Closure { lists: true });
        let published = engine.snapshot();
        let original = ql.tbox(&published, None);
        let before = kept_entries(&ql);
        let member = membership(&published, gains, restriction(&published));
        let mut tx = engine.transaction();
        tx.insert_encoded(member);
        let pending = tx.pending_snapshot();
        let first = ql.tbox(&pending, None);
        assert_eq!(Arc::ptr_eq(&original, &first), reuses, "{data:?}");
        for read in 0..3 {
            MEMBER_SCANS.set(0);
            let again = ql.tbox(&pending, None);
            assert!(Arc::ptr_eq(&first, &again), "{data:?}, read {read}");
            assert_eq!(MEMBER_SCANS.get(), 0, "{data:?}, read {read}");
        }
        assert_eq!(kept_entries(&ql), before, "{data:?}");
        MEMBER_SCANS.set(0);
        ql.tbox(&tx.pending_snapshot(), None);
        assert!(MEMBER_SCANS.get() > 0, "{data:?}: a new pending snapshot");
    }
}

/// Dictionary 3.0c's required guard: views of one version with other equality modes (the
/// canonical view of a pending snapshot) share its slot but never an entry. Each reads its
/// own schema once, and answers as a fresh rewriting does.
#[test]
fn views_of_one_version_in_other_equality_modes_keep_their_own_entries() {
    let engine = engine("");
    let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
    let options = sharing(&ql);
    let published = engine.snapshot();
    let member = membership(&published, "ann", restriction(&published));
    let mut tx = engine.transaction();
    tx.insert_encoded(member);
    let pending = tx.pending_snapshot();
    let canonical = pending.with_canonical_equality();
    assert_ne!(canonical.identity(), pending.identity());
    // The fresh rewriting reads equal views of a version of their own.
    let alike = tx.pending_snapshot();
    let alike_canonical = alike.with_canonical_equality();
    let expected = ["<http://example.org/ann>"];
    for (view, same) in [(&pending, &alike), (&canonical, &alike_canonical)] {
        let fresh = sharing(&Arc::new(QlRewriting::new(Closure { lists: true })));
        let own = check(same, &fresh, &expected);
        MEMBER_SCANS.set(0);
        assert_eq!(check(view, &options, &expected), own);
        assert!(MEMBER_SCANS.get() > 0, "the first read of each view");
    }
    for view in [&pending, &canonical, &pending] {
        MEMBER_SCANS.set(0);
        check(view, &options, &expected);
        assert_eq!(MEMBER_SCANS.get(), 0, "a view read again");
    }
}

/// What the TBox compiles to depends on the closure, so two rewritings with other closures
/// reading one pending snapshot never get each other's TBox from its version.
#[test]
fn rewritings_with_other_closures_keep_their_own_tboxes_of_a_version() {
    let engine = engine("");
    let published = engine.snapshot();
    let member = membership(&published, "ann", restriction(&published));
    let mut tx = engine.transaction();
    tx.insert_encoded(member);
    let pending = tx.pending_snapshot();
    let rl = QlRewriting::new(Closure { lists: true });
    let ql = QlRewriting::new(Closure { lists: false });
    let on_rl = rl.tbox(&pending, None);
    let on_ql = ql.tbox(&pending, None);
    assert!(!Arc::ptr_eq(&on_rl, &on_ql));
    assert!(Arc::ptr_eq(&on_rl, &rl.tbox(&pending, None)));
    assert!(Arc::ptr_eq(&on_ql, &ql.tbox(&pending, None)));
}

/// A mask of a published snapshot is published (dictionary 3.0): changing only its
/// inferred mask, the schema read reuses the kept TBox, and its answers and probes are what
/// a fresh rewriting gives it (review r0 §5 item 4).
#[test]
fn a_published_mask_reuses_the_tbox_and_answers_as_a_fresh_read() {
    let engine = engine(":ann :worksFor :acme .");
    let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
    let options = sharing(&ql);
    let published = engine.snapshot();
    let expected = ["<http://example.org/ann>"];
    check(&published, &options, &expected);
    let original = ql.tbox(&published, None);
    let mask = published.with_inferred_subset(InferredSubset::Only(Vec::new()));
    assert!(mask.shared_state().is_some());
    assert_ne!(mask.identity(), published.identity());
    let fresh = sharing(&Arc::new(QlRewriting::new(Closure { lists: true })));
    assert_eq!(
        check(&mask, &options, &expected),
        check(&mask, &fresh, &expected)
    );
    assert!(Arc::ptr_eq(&original, &ql.tbox(&mask, None)));
}

/// The probe gate (review r0 §1, "QL realised probes"): a reader of some graphs asks no
/// probe of the data, so the store's probes stay as they were.
#[test]
fn graph_restricted_readers_keep_no_probes() {
    let engine = engine(":ann :worksFor :acme .");
    let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
    let options = QueryOptions {
        access: Some(Arc::new(GraphAccess {
            default_graph: true,
            inferred: true,
            inferred_by_support: true,
            ..GraphAccess::default()
        })),
        ..sharing(&ql)
    };
    let report = check(&engine.snapshot(), &options, &["<http://example.org/ann>"]);
    assert_eq!(report.checks, 0);
    assert_eq!(kept_probes(&ql), (None, Vec::new()));
}

/// A view that adds inferred statements to a published snapshot, as the DL bounds derive
/// (master, 8 Nov 23:20), and its base: in either order of reading, each gets what a fresh
/// rewriting gives it.
/// - Their TBoxes may be one: what the TBox reads (the core, the memberships, the equality
///   probe) is asserted only (`read_core`, `member_of`, `has_equality`).
/// - Their probes ask the materialised data, which differs (here the witness is only in the
///   view), so they're kept apart by the view's identity (`realised`'s lookup).
#[test]
// The DL bounds' own constructor (dictionary 3.0b): only they and their tests may call it.
#[allow(clippy::disallowed_methods)]
fn an_added_view_and_its_base_keep_their_own_probes() {
    let engine = engine(":other :knows :acme .");
    let base = engine.snapshot();
    let id = |s: &str| {
        base.lookup(NamedNodeRef::new_unchecked(&format!("{EX}{s}")).into())
            .unwrap()
    };
    let witness = nrese_engine::EncodedTriple {
        subject: id("ann"),
        predicate: id("worksFor"),
        object: id("acme"),
    };
    // The view keeps its base's publication, so it may keep what it reads (3.0b).
    let added = base
        .with_inferred_derived_within(&[witness], &Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    assert!(added.shared_state().is_some());
    assert_ne!(added.identity(), base.identity());
    assert_eq!(added.asserted_identity(), base.asserted_identity());
    let expected = ["<http://example.org/ann>"];
    let fresh = |snapshot: &Snapshot| {
        let report = check(
            snapshot,
            &sharing(&Arc::new(QlRewriting::new(Closure { lists: true }))),
            &expected,
        );
        (report.patterns, report.realised, report.completeness)
    };
    let (on_base, on_added) = (fresh(&base), fresh(&added));
    // The case is one only where the two answer their probes differently.
    assert_ne!(on_base.1, on_added.1);
    for order in [[&base, &added, &base], [&added, &base, &added]] {
        let ql = Arc::new(QlRewriting::new(Closure { lists: true }));
        let options = sharing(&ql);
        for snapshot in order {
            let report = check(snapshot, &options, &expected);
            let own = match snapshot.identity() == base.identity() {
                true => &on_base,
                false => &on_added,
            };
            assert_eq!(
                &(report.patterns, report.realised, report.completeness),
                own
            );
        }
        assert!(Arc::ptr_eq(&ql.tbox(&base, None), &ql.tbox(&added, None)));
    }
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
/// Each read counts three things: memberships scanned, membership changes walked,
/// membership statements written into a schema. A noted commit walks only its changes,
/// scans only where it removed the kept member, and writes a schema only where an
/// expression gains or loses its last member (P2-C1 probes, 8 Nov: 188 us a snapshot for
/// the rebuild, 1 us for each touched rescan). Each read is compared with a fresh one by
/// what the TBox reads: the core, and which expressions have a member.
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
    // What the TBox reads of a schema: its core, and which expressions have a member
    // (the kept statement's identity is read by nothing: design note, "Membership cache").
    let presence = |schema: &[nrese_owl::Statement], snapshot: &Snapshot| {
        let rdf_type = iri(snapshot, RDF, "type").map(TermId::raw);
        let mut core = Vec::new();
        let mut members = Vec::new();
        for s in schema {
            let blank = TermId::from_raw(s.triple[2]).kind() == nrese_engine::TermKind::BlankNode;
            match Some(s.triple[1]) == rdf_type && blank {
                true => members.push(s.triple[2]),
                false => core.push(*s),
            }
        }
        members.sort_unstable();
        members.dedup();
        (core, members)
    };
    let fresh = |snapshot: &Snapshot| presence(&schema_statements(snapshot, &|_| true), snapshot);
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
        let read = presence(&cached[0].schema(&snapshot), &snapshot);
        drop(cached);
        if n != expected || read != fresh(&snapshot) {
            wrong.push(format!(
                "{name}: {n:?} (expected {expected:?}; scanned, walked, written), fresh: {}",
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
    check("a member added", [0, 1, 0]);
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
    check("the held member deleted", [1, 1, 0]);
    commit(
        &|tx| {
            for m in &members[1..] {
                tx.remove_encoded(*m);
            }
            tx.remove(member("new").as_ref());
        },
        true,
    );
    check("every member deleted", [1, 3, restrictions - 1]);
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

/// An engine with `n` restrictions, `_:e0` to `_:e{n-1}`, each with members `:m{e}_0` to
/// `:m{e}_2`.
fn restrictions(n: usize) -> Engine {
    let mut text = String::from(
        "@prefix : <http://example.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .\n",
    );
    for e in 0..n {
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
    engine
}

/// The `rdf:type` statement making `:{name}` a member of `_:e0` ([`restrictions`]): never
/// the member a schema holds, which is one of the loaded ones.
fn member_of_e0(engine: &Engine, name: &str) -> EncodedQuad {
    let snapshot = engine.snapshot();
    let rdf_type = iri(&snapshot, RDF, "type").unwrap();
    let m0 = snapshot
        .lookup(NamedNodeRef::new_unchecked(&format!("{EX}m0_0")).into())
        .unwrap();
    let e0 = snapshot
        .quads_for_pattern_in(
            ReadModel::Asserted,
            &QuadPattern {
                subject: Some(m0),
                predicate: Some(rdf_type),
                object: None,
                graph: GraphSelector::Any,
            },
        )
        .next()
        .unwrap()
        .object;
    let tx = engine.transaction();
    let subject = tx.intern(NamedNodeRef::new_unchecked(&format!("{EX}{name}")).into());
    drop(tx);
    EncodedQuad::new(subject, rdf_type, e0, TermId::DEFAULT_GRAPH)
}

/// A commit that never published leaves the next commit from the same state noted (DUR's
/// condition 5 for `asserted_identity_after`, 8 Nov): a note to a state no snapshot has
/// (a transaction's pending one), then a commit through [`QlRewriting::commit`] adding a
/// member to `_:e0`. Expected: the read after it walks that change and scans nothing,
/// `[0, 1, 0]` (scanned, walked, written), as after any noted commit.
#[test]
fn a_note_of_a_commit_that_never_published_leaves_the_next_noted() {
    let engine = restrictions(20);
    let ql = QlRewriting::new(Closure { lists: false });
    ql.tbox(&engine.snapshot(), None);
    let member = member_of_e0(&engine, "new");
    let tx = engine.transaction();
    let (from, never) = (
        tx.base().asserted_identity(),
        tx.pending_snapshot().asserted_identity(),
    );
    drop(tx);
    ql.note_commit(from, never, Some(Vec::new()));
    let mut tx = engine.transaction();
    assert!(tx.insert_encoded(member));
    ql.commit(tx).unwrap();
    for cell in [&MEMBER_SCANS, &EXPRESSIONS_WALKED, &MEMBERS_WRITTEN] {
        cell.set(0);
    }
    ql.tbox(&engine.snapshot(), None);
    let counted = [
        MEMBER_SCANS.get(),
        EXPRESSIONS_WALKED.get(),
        MEMBERS_WRITTEN.get(),
    ];
    assert_eq!(counted, [0, 1, 0], "(scanned, walked, written)");
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

/// Structural guard (I24c-2): every term of the mapping's vocabulary is either read as
/// schema or named in `NOT_READ` with its reason, never both, and `NOT_READ` names only
/// terms the vocabulary has. A term the mapping gains is read until someone classifies it
/// otherwise; a renamed one fails here.
#[test]
fn the_reader_reads_the_mapping_vocabulary() {
    let fields: Vec<&str> = nrese_owl::Vocabulary::iris()
        .into_iter()
        .map(|(f, _)| f)
        .collect();
    let stale: Vec<&&str> = NOT_READ.iter().filter(|n| !fields.contains(*n)).collect();
    assert!(stale.is_empty(), "not in the vocabulary: {stale:?}");
    let (properties, classes) = (&READ_PROPERTIES[..], &READ_CLASSES[..]);
    for (field, iri) in nrese_owl::Vocabulary::iris() {
        let read = properties
            .iter()
            .chain(classes)
            .filter(|r| **r == iri)
            .count();
        let skipped = NOT_READ.contains(&field);
        assert_eq!(
            (read, skipped),
            (usize::from(!skipped), skipped),
            "{field} ({iri}): read {read} times, listed as not read: {skipped}"
        );
    }
    let facet = format!("{}minInclusive", nrese_owl::vocab::XSD);
    assert!(properties.contains(&facet.as_str()), "the facets are read");
}
