//! Package 3.4's guards for the static equality representatives
//! (`notes/2026-11-08-dict3-4-equality-design.md` §5; the design check `review-dict3-token-r0`
//! §5 items 1, 2, 3 and 8 in 3.4's share). Only a published snapshot's classes enter the
//! process-wide entry; an unpublished one's are built for its request and kept nowhere else.
//!
//! The oracle for every answer is the same query evaluated as written (`as_written`), which
//! reads no representatives: the representatives only let a join run on one identity per
//! class where its aggregates can't tell, so the answers are equal. The entry is process-wide
//! and tests run in parallel: each guard checks that it holds nothing of the guard's own
//! unpublished snapshots and only published state at all, which no other test can make false
//! once the admission is right.

use std::sync::Arc;

use nrese_engine::{
    EncodedQuad, EncodedTriple, Engine, EngineConfig, InferredMask, InferredSubset, Snapshot,
    TermId,
};
use nrese_exec::{Budget, Envelope};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql_syntax::Query;

use super::equality::{builds, kept_for, kept_is_published, refused};
use crate::{QueryOptions, QueryResults, ReadView};

const EX: &str = "http://example.com/";
const SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

/// Per key, the distinct values its partners have: the detached OPTIONAL joins on `?y`, which
/// feeds no aggregate, so it runs over representatives (`sets`).
const QUERY: &str = "SELECT ?k (COUNT(DISTINCT ?x) AS ?n) WHERE { \
     ?k <http://example.com/p0> ?y . OPTIONAL { ?y <http://example.com/p1> ?x } } GROUP BY ?k";

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn literal(text: &str) -> Term {
    Literal::new_simple_literal(text).into()
}

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    }
}

fn closed() -> QueryOptions {
    QueryOptions {
        equality_closed: true,
        ..QueryOptions::default()
    }
}

/// A count's number (its literal's value), `-` where unbound.
fn count(term: Option<&Term>) -> String {
    match term {
        Some(Term::Literal(literal)) => literal.value().to_owned(),
        Some(other) => other.to_string(),
        None => "-".to_owned(),
    }
}

/// `view`'s answers to [`QUERY`] under `options`, one `key count` line each, sorted.
fn answers<V: ReadView>(view: &V, options: &QueryOptions) -> Vec<String> {
    let query = Query::parse(QUERY, None).unwrap();
    let QueryResults::Solutions(rows) = crate::evaluate_query(view, &query, options).unwrap()
    else {
        panic!("solutions")
    };
    let mut out: Vec<String> = rows
        .map(|row| {
            let row = row.unwrap();
            format!("{} {}", row.get("k").unwrap(), count(row.get("n")))
        })
        .collect();
    out.sort();
    out
}

/// `view`'s answers under `options` and as written, which must agree.
fn agreed<V: ReadView>(view: &V, options: &QueryOptions, what: &str) -> Vec<String> {
    let got = answers(view, options);
    let written = QueryOptions {
        as_written: true,
        ..options.clone()
    };
    assert_eq!(got, answers(view, &written), "{what}: answers as written");
    got
}

/// An unpublished view's query: answered as written, the classes built for it (the guard
/// reaches them), and nothing kept for it or for any unpublished state.
fn unpublished<V: ReadView>(view: &V, evaluated: Option<&Snapshot>, what: &str) {
    let before = builds();
    let got = answers(view, &closed());
    if let Some(snapshot) = evaluated {
        assert!(!kept_for(snapshot), "{what}: nothing kept for its version");
    }
    assert!(kept_is_published(), "{what}: only published state kept");
    assert!(builds() > before, "{what}: the classes were built for it");
    let written = QueryOptions {
        as_written: true,
        ..closed()
    };
    assert_eq!(got, answers(view, &written), "{what}: answers as written");
}

/// The terms of [`ordinary`] and [`colliding`], interned in this order in both, so that both
/// engines give them the same ids.
struct Ids {
    a: TermId,
    link: TermId,
    b: TermId,
    k1: TermId,
    k2: TermId,
    k3: TermId,
    c: TermId,
    p0: TermId,
    p1: TermId,
    p2: TermId,
    one: TermId,
    two: TermId,
    three: TermId,
    nine: TermId,
}

impl Ids {
    fn interned(tx: &nrese_engine::Transaction<'_>, link: &str) -> Self {
        let named = |local: &str| tx.intern(iri(local).as_ref().into());
        let a = named("a");
        let link = tx.intern(NamedNode::new_unchecked(link).as_ref().into());
        let b = named("b");
        let (k1, k2, k3, c) = (named("k1"), named("k2"), named("k3"), named("c"));
        let (p0, p1, p2) = (named("p0"), named("p1"), named("p2"));
        let text = |text: &str| tx.intern(literal(text).as_ref());
        Ids {
            a,
            link,
            b,
            k1,
            k2,
            k3,
            c,
            p0,
            p1,
            p2,
            one: text("1"),
            two: text("2"),
            three: text("3"),
            nine: text("9"),
        }
    }
}

fn asserted(s: TermId, p: TermId, o: TermId) -> EncodedQuad {
    EncodedQuad::new(s, p, o, TermId::DEFAULT_GRAPH)
}

/// Ordinary equality, closed: `a` and `b` one identity (`sameAs` both ways, inferred), `c`
/// alone. Asserted: `k1 p0 a`, `k2 p0 b`, `k3 p0 c`, `a p1 "1"`, `b p1 "2"`, `c p1 "3"`; the
/// rest of the closure inferred, and `c p2 "9"`, an inferred statement no answer reads.
/// Answers: `k1` and `k2` 2 each, `k3` 1.
fn ordinary() -> Engine {
    let engine = Engine::new(config()).unwrap();
    let mut tx = engine.transaction();
    let i = Ids::interned(&tx, SAME_AS);
    for (s, p, o) in [
        (i.k1, i.p0, i.a),
        (i.k2, i.p0, i.b),
        (i.k3, i.p0, i.c),
        (i.a, i.p1, i.one),
        (i.b, i.p1, i.two),
        (i.c, i.p1, i.three),
    ] {
        assert!(tx.insert_encoded(asserted(s, p, o)));
    }
    for (s, p, o) in [
        (i.a, i.link, i.b),
        (i.b, i.link, i.a),
        (i.k1, i.p0, i.b),
        (i.k2, i.p0, i.a),
        (i.a, i.p1, i.two),
        (i.b, i.p1, i.one),
        (i.c, i.p2, i.nine),
    ] {
        assert!(tx.insert_inferred(EncodedTriple::new(s, p, o)));
    }
    tx.commit().unwrap();
    engine
}

fn ids(engine: &Engine) -> Ids {
    Ids::interned(&engine.transaction(), SAME_AS)
}

const ORDINARY: [&str; 3] = [
    "<http://example.com/k1> 2",
    "<http://example.com/k2> 2",
    "<http://example.com/k3> 1",
];

/// A writer's change of existing ids only, keeping the closure: `k3` gains the class of `a`
/// (`k3 p0 a`, and `k3 p0 b` inferred) and `c` loses its value. Answers: every key 2.
fn change(tx: &mut nrese_engine::Transaction<'_>, i: &Ids) {
    assert!(
        tx.insert_encoded(asserted(i.k3, i.p0, i.a)),
        "existing ids only"
    );
    assert!(tx.insert_inferred(EncodedTriple::new(i.k3, i.p0, i.b)));
    assert!(tx.remove_encoded(asserted(i.c, i.p1, i.three)));
}

fn budget() -> Arc<Budget> {
    Arc::new(Budget::unlimited())
}

/// Review §5 item 1, 3.4's share: pending state of both kinds, no private ids, through explicit
/// snapshots (route A) and transactions read as views (route B).
///
/// - Input: closed ordinary data, its published answer warmed; a writer's change of existing
///   ids (an insert and a delete) read through `pending_snapshot`, `pending_over` and the
///   transaction itself; an INSERT then DELETE of the same statement; `Engine::speculative`'s
///   private transaction changed the same way, also read as a view (`PendingRead`); a private
///   transaction's change, from both builders.
/// - Expected: each pending query is answered as written, builds its classes, and leaves
///   nothing in the process-wide entry for its version, nor any unpublished state; the
///   published snapshot is answered as before.
#[test]
fn pending_state_of_both_kinds_keeps_no_classes() {
    let engine = ordinary();
    let published = engine.snapshot();
    assert_eq!(agreed(&published, &closed(), "published"), ORDINARY);
    let i = ids(&engine);

    let mut writer = engine.transaction();
    change(&mut writer, &i);
    let pending = writer.pending_snapshot();
    assert_eq!(
        answers(
            &pending,
            &QueryOptions {
                as_written: true,
                ..closed()
            }
        ),
        [
            "<http://example.com/k1> 2",
            "<http://example.com/k2> 2",
            "<http://example.com/k3> 2",
        ],
        "the fixture's pending answer"
    );
    unpublished(&pending, Some(&pending), "writer, pending_snapshot");
    let over = writer.pending_over(&published, &[]);
    unpublished(&over, Some(&over), "writer, pending_over");
    unpublished(&writer, None, "writer, as a view");
    drop(writer);

    let mut undone = engine.transaction();
    let moved = asserted(i.k3, i.p0, i.a);
    assert!(undone.insert_encoded(moved));
    assert!(undone.remove_encoded(moved));
    let undone_pending = undone.pending_snapshot();
    unpublished(&undone_pending, Some(&undone_pending), "INSERT then DELETE");
    drop(undone);

    // `change` on the private kind, whose removal is the fallible one.
    let charged = budget();
    let mut speculative = engine.speculative(Arc::clone(&charged));
    assert!(
        speculative.try_insert_encoded(moved).unwrap(),
        "existing ids only"
    );
    assert!(speculative.insert_inferred(EncodedTriple::new(i.k3, i.p0, i.b)));
    assert!(
        speculative
            .try_remove_encoded(asserted(i.c, i.p1, i.three))
            .unwrap()
    );
    let speculative_pending = speculative.pending_snapshot().unwrap();
    unpublished(
        &speculative_pending,
        Some(&speculative_pending),
        "speculative, pending_snapshot",
    );
    let as_view = crate::view::PendingRead::new(&speculative, &charged);
    unpublished(&as_view, None, "speculative, as a view");
    drop(speculative);

    let mut private = engine.speculative_over(&published, budget()).unwrap();
    for (s, o) in [("k3", "a"), ("k3", "b")] {
        let quad = Quad::new(iri(s), iri("p0"), iri(o), GraphName::DefaultGraph);
        assert!(
            private.try_insert(quad.as_ref()).unwrap(),
            "existing terms only"
        );
    }
    let private_pending = private.pending_snapshot().unwrap();
    unpublished(&private_pending, Some(&private_pending), "private");
    let private_over = private.pending_over(&published, &[]).unwrap();
    unpublished(&private_over, Some(&private_over), "private, over");
    drop(private);

    assert_eq!(
        agreed(&engine.snapshot(), &closed(), "published, after"),
        ORDINARY
    );
}

/// Review §5 item 2, 3.4's share: no laundering through views.
///
/// - Input: a writer's pending snapshot through every derived view (stored, equality,
///   canonical, a reader's budget, an inferred subset, a mask, a chain of them); the published
///   snapshot with statements added, as the OWL 2 DL bounds make them (`with_inferred_added`,
///   `with_inferred_added_within`, then equality). Each hides or adds only what keeps the data
///   closed.
/// - Expected: none of them keeps classes in the process-wide entry, each is answered as
///   written; a published restriction is answered as written, and so is the published
///   snapshot after it.
#[test]
fn views_of_unpublished_state_keep_no_classes() {
    let engine = ordinary();
    let published = engine.snapshot();
    let i = ids(&engine);
    let unread = asserted(i.c, i.p2, i.nine);
    let same_as = Some(i.link);

    let mut writer = engine.transaction();
    change(&mut writer, &i);
    let pending = writer.pending_snapshot();
    let mask = InferredMask::hiding(&pending, &[unread], &budget()).unwrap();
    let views: Vec<(&str, Snapshot)> = vec![
        ("stored", pending.stored()),
        ("equality", pending.with_equality(same_as)),
        ("canonical", pending.with_canonical_equality()),
        ("reading within", pending.reading_within(budget()).unwrap()),
        (
            "subset without",
            pending.with_inferred_subset(InferredSubset::Without(vec![unread])),
        ),
        (
            "mask",
            pending
                .with_inferred_mask(&mask, &budget())
                .unwrap()
                .expect("a mask over the pending state"),
        ),
        (
            "a chain",
            pending
                .with_inferred_subset(InferredSubset::Without(vec![unread]))
                .stored()
                .with_equality(same_as)
                .reading_within(budget())
                .unwrap(),
        ),
    ];
    for (what, view) in &views {
        unpublished(view, Some(view), &format!("pending, {what}"));
    }
    drop(writer);

    // The DL bounds' construction: statements the published state doesn't have, added.
    let added = [
        EncodedTriple::new(i.k3, i.p0, i.a),
        EncodedTriple::new(i.k3, i.p0, i.b),
    ];
    let added_quads: Vec<EncodedQuad> = added
        .iter()
        .map(|t| asserted(t.subject, t.predicate, t.object))
        .collect();
    let additions: Vec<(&str, Snapshot)> = vec![
        ("added", published.with_inferred_added(&added_quads)),
        (
            "added, equality",
            published
                .with_inferred_added(&added_quads)
                .with_equality(same_as),
        ),
        (
            "added within",
            published
                .with_inferred_added_within(&added, &budget())
                .unwrap(),
        ),
    ];
    for (what, view) in &additions {
        unpublished(view, Some(view), &format!("published, {what}"));
    }

    let restriction = published.with_inferred_subset(InferredSubset::Without(vec![unread]));
    assert_eq!(
        agreed(&restriction, &closed(), "a published restriction"),
        ORDINARY
    );
    assert_eq!(agreed(&published, &closed(), "published, after"), ORDINARY);
}

/// Compact equality: facts over each class's smallest id, `b sameAs a` inferred. Answers
/// expanded: `k1` and `k2` 2 each, `k3` 1.
fn compact() -> Engine {
    let engine = Engine::new(config()).unwrap();
    let mut tx = engine.transaction();
    let i = Ids::interned(&tx, SAME_AS);
    for (s, p, o) in [
        (i.k1, i.p0, i.a),
        (i.k2, i.p0, i.a),
        (i.k3, i.p0, i.c),
        (i.a, i.p1, i.one),
        (i.a, i.p1, i.two),
        (i.c, i.p1, i.three),
    ] {
        assert!(tx.insert_encoded(asserted(s, p, o)));
    }
    assert!(tx.insert_inferred(EncodedTriple::new(i.b, i.link, i.a)));
    tx.commit().unwrap();
    engine.set_equality(Some(i.link));
    engine
}

/// Review §5 item 3, 3.4's share: one version read in each interpretation, each first on a
/// version of its own, and a restriction against its source, warmed in both orders.
///
/// - Input: [`compact`] read with equality (the classes), stored (a scan of `sameAs`) and with
///   canonical answers, each read first on a fresh engine's version, then the others; ordinary
///   closed data and its restriction hiding every inferred statement, in which `a` and `b` are
///   two identities.
/// - Expected: in every order each is answered as written: the restriction `k1` and `k2` 1
///   each (another identity's map would join `b`'s value to `k1`), its source as
///   [`ORDINARY`].
#[test]
fn interpretations_and_restrictions_answer_in_either_order() {
    let plain = closed();
    let canonical = QueryOptions {
        equality_canonical: true,
        ..closed()
    };
    let expanded = [
        "<http://example.com/k1> 2",
        "<http://example.com/k2> 2",
        "<http://example.com/k3> 1",
    ];
    for first in 0..3 {
        let engine = compact();
        let snapshot = engine.snapshot();
        let mut reads: Vec<(&str, Snapshot, &QueryOptions)> = vec![
            ("compact, equality", snapshot.clone(), &plain),
            ("compact, stored", snapshot.stored(), &plain),
            ("compact, canonical", snapshot.clone(), &canonical),
        ];
        reads.rotate_left(first);
        for (what, view, options) in &reads {
            let got = agreed(view, options, what);
            if *what == "compact, equality" {
                assert_eq!(got, expanded, "{what}");
            }
        }
    }

    let engine = ordinary();
    let source = engine.snapshot();
    let hidden: Vec<EncodedQuad> = source
        .quads_for_pattern_in(
            nrese_engine::ReadModel::Inferred,
            &nrese_engine::QuadPattern::all(),
        )
        .collect();
    let restriction = source.with_inferred_subset(InferredSubset::Without(hidden));
    let alone = [
        "<http://example.com/k1> 1",
        "<http://example.com/k2> 1",
        "<http://example.com/k3> 1",
    ];
    for restricted_first in [false, true] {
        for step in 0..2 {
            if (step == 0) == restricted_first {
                assert_eq!(agreed(&restriction, &closed(), "the restriction"), alone);
            } else {
                assert_eq!(agreed(&source, &closed(), "its source"), ORDINARY);
            }
        }
    }
}

/// Two identities at the same ids and revision: `a` and `b` one class in the first engine
/// ([`ordinary`]'s order of terms), two in this one (`link` is no `sameAs`). Asserted:
/// `k1 p0 a`, `k2 p0 b`, `a p1 "1"`, `b p1 "2"`, `a link b`. Answers: each key 1.
fn colliding() -> Engine {
    let engine = Engine::new(config()).unwrap();
    let mut tx = engine.transaction();
    let i = Ids::interned(&tx, &format!("{EX}link"));
    for (s, p, o) in [
        (i.k1, i.p0, i.a),
        (i.k2, i.p0, i.b),
        (i.a, i.p1, i.one),
        (i.b, i.p1, i.two),
        (i.a, i.link, i.b),
    ] {
        assert!(tx.insert_encoded(asserted(s, p, o)));
    }
    tx.commit().unwrap();
    engine
}

/// Review §5 item 8, 3.4's share: classes kept for one engine's version are never another's.
///
/// - Input: the first engine's classes kept (`a` and `b` one identity), the engine then
///   dropped, its snapshot living on in the entry alone; a second engine at the same revision,
///   with `a` and `b` at the same ids, two identities; then both engines live at once.
/// - Expected: the second engine's query builds its own classes and answers each key 1, as
///   written (the first engine's classes would join `b`'s value to `k1`).
#[test]
fn classes_kept_for_one_engine_are_not_anothers() {
    let separate = ["<http://example.com/k1> 1", "<http://example.com/k2> 1"];
    for both_live in [false, true] {
        let first = ordinary();
        let warmed = first.snapshot();
        assert_eq!(agreed(&warmed, &closed(), "the first engine"), ORDINARY);
        let first_ids = (ids(&first).a, ids(&first).b, warmed.revision());
        let kept = both_live.then_some((first, warmed));

        let second = colliding();
        let snapshot = second.snapshot();
        let tx = second.transaction();
        let second_ids = Ids::interned(&tx, &format!("{EX}link"));
        drop(tx);
        assert_eq!(
            first_ids,
            (second_ids.a, second_ids.b, snapshot.revision()),
            "the fixture: the same ids and revision in both engines"
        );
        let before = builds();
        assert_eq!(
            agreed(&snapshot, &closed(), "the second engine"),
            separate,
            "both live: {both_live}"
        );
        assert!(builds() > before, "the second engine built its own classes");
        drop(kept);
    }
}

/// The design note's D1: an unpublished request's classes are charged to its query's budget,
/// and where it has no room for them, the query is answered without them.
///
/// - Input: compact equality, one class of 20,000 identities read canonically (the query reads
///   a row per class, the classes a pair per identity), a writer's pending change; the least
///   budget that answers the query (a search: answering is monotone in the budget), and an
///   ample one.
/// - Expected: under the least budget the classes are refused, being an entry per identity
///   beyond what the query holds, and the query answers as written without them; under the
///   ample one they are built and none is refused. Nothing is kept for the pending version.
#[test]
fn a_refused_request_build_answers_without_classes() {
    let engine = Engine::new(config()).unwrap();
    let mut tx = engine.transaction();
    let i = Ids::interned(&tx, SAME_AS);
    for (s, p, o) in [(i.k1, i.p0, i.a), (i.a, i.p1, i.one)] {
        assert!(tx.insert_encoded(asserted(s, p, o)));
    }
    for n in 0..20_000 {
        let alias = tx.intern(iri(&format!("alias{n}")).as_ref().into());
        assert!(tx.insert_inferred(EncodedTriple::new(alias, i.link, i.a)));
    }
    tx.commit().unwrap();
    engine.set_equality(Some(i.link));
    let mut writer = engine.transaction();
    assert!(writer.insert_encoded(asserted(i.k2, i.p0, i.a)));
    let pending = writer.pending_snapshot();
    let canonical = QueryOptions {
        equality_canonical: true,
        ..closed()
    };
    let expected = answers(
        &pending,
        &QueryOptions {
            as_written: true,
            ..canonical.clone()
        },
    );
    assert_eq!(
        expected,
        ["<http://example.com/k1> 1", "<http://example.com/k2> 1"],
        "the fixture's answer"
    );
    let query = Query::parse(QUERY, None).unwrap();
    // The answer under `limit` bytes, `None` where the budget refuses the query.
    let under = |limit: usize| -> Option<Vec<String>> {
        let mut options = canonical.clone();
        options.resources.memory = Envelope::new(Some(limit), None);
        let Ok(QueryResults::Solutions(rows)) = crate::evaluate_query(&pending, &query, &options)
        else {
            return None;
        };
        let rows = rows.collect::<Result<Vec<_>, _>>().ok()?;
        let mut got: Vec<String> = rows
            .iter()
            .map(|row| format!("{} {}", row.get("k").unwrap(), count(row.get("n"))))
            .collect();
        got.sort();
        Some(got)
    };
    let ample = 1 << 30;
    let (before, built) = (refused(), builds());
    assert_eq!(under(ample).as_deref(), Some(&expected[..]), "ample");
    assert_eq!(refused(), before, "ample: nothing refused");
    assert!(builds() > built, "ample: the classes were built");
    let (mut low, mut high) = (0, ample);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        match under(middle) {
            Some(_) => high = middle,
            None => low = middle,
        }
    }
    let before = refused();
    assert_eq!(
        under(high).as_deref(),
        Some(&expected[..]),
        "under {high} bytes"
    );
    assert!(
        refused() > before,
        "under the least budget ({high} bytes) the classes were refused"
    );
    assert!(!kept_for(&pending), "nothing kept for the pending version");
    assert!(kept_is_published(), "only published state kept");
}
