//! Package 3.1's guards: the planner's statistics admit only from published snapshots (the
//! lifecycle contract's S3 and A15; `notes/2026-11-08-dict3-1-statistics-design.md` §4, the design
//! check `review-dict3-token-r0` §5 items 1, 2, 7, 8 and 9). An unpublished snapshot (a writer's
//! pending one, `Engine::speculative()`'s, a private one, any view of them) reads what published
//! snapshots kept and computes the rest for itself: no kept set, claim, rebuild thread, load, save,
//! distinct entry, node count or heavy group of the engine's statistics comes from it. And a closed
//! store's statistics neither write nor read the directory a later open owns.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

use super::*;
use crate::engine::{InferredMask, InferredSubset};
use crate::{EncodedQuad, EncodedTriple, Engine, EngineConfig};

const MODEL: ReadModel = ReadModel::Materialised;

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    }
}

/// Subjects `s{i}` with predicates `p0` up to `p{i % 3}`, each to the literal `"{i}"`: subject `i`
/// has `1 + i % 3` quads in the default graph, so 300 subjects have 600.
fn add(engine: &Engine, subjects: std::ops::Range<u32>) {
    let mut tx = engine.transaction();
    for i in subjects {
        for p in 0..1 + i % 3 {
            let quad = Quad::new(
                iri(&format!("s{i}")),
                iri(&format!("p{p}")),
                Literal::new_simple_literal(format!("{i}")),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
    }
    tx.commit().unwrap();
}

/// The `p0` quad of subject `i`, encoded by `snapshot`.
fn p0_of(snapshot: &Snapshot, i: u32) -> EncodedQuad {
    let subject = iri(&format!("s{i}"));
    let p0 = iri("p0");
    let object = Literal::new_simple_literal(format!("{i}"));
    EncodedQuad::new(
        snapshot.lookup(subject.as_ref().into()).expect("interned"),
        snapshot.lookup(p0.as_ref().into()).expect("interned"),
        snapshot.lookup(object.as_ref().into()).expect("interned"),
        TermId::DEFAULT_GRAPH,
    )
}

fn statistics(engine: &Engine) -> Arc<Statistics> {
    Arc::clone(&engine.inner.shared.statistics)
}

/// Where a durable engine at `dir` saves `MODEL`'s sets.
fn sets_file(dir: &Path) -> PathBuf {
    dir.join("derived")
        .join(format!("{}.index", Statistics::sets_name(MODEL)))
}

/// Nothing of the sets kept, claimed, scanned or being rebuilt.
fn no_sets_admitted(statistics: &Statistics, what: &str) {
    assert!(
        statistics.sets.lock().is_empty(),
        "{what}: no sets kept or loaded"
    );
    assert!(statistics.building.is_idle(), "{what}: no build claimed");
    assert_eq!(statistics.scans(), 0, "{what}: no scan");
}

/// Waits (30 s at most) until `done` holds of `statistics`.
fn until(statistics: &Statistics, what: &str, done: impl Fn(&Statistics) -> bool) {
    let started = Instant::now();
    while !done(statistics) {
        assert!(started.elapsed().as_secs() < 30, "{what}: never happened");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The size `MODEL`'s sets are kept at, if any are.
fn kept_at(statistics: &Statistics) -> Option<u64> {
    statistics.sets.lock().get(&MODEL).map(|sets| sets.at)
}

/// Holds the next build of `statistics` at its first scan: `entered` hears when it got there, and
/// it goes on once `release` sends (or is dropped).
fn hold_next_build(statistics: &Statistics) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    *statistics.hook.lock() = Some(Box::new(move |_: &Build<CharacteristicSets>| {
        let _ = entered_tx.send(());
        let _ = release_rx.recv();
    }));
    (entered, release)
}

/// The unpublished snapshots of `engine` the review's §5 item 1 names, each made only of existing
/// ids: a writer's `pending_snapshot` and `pending_over` after an insert and a delete, with no
/// change, and after an INSERT–DELETE of one statement; `Engine::speculative()`'s; a private
/// transaction's, from both builders.
fn unpublished(engine: &Engine) -> Vec<(&'static str, Snapshot)> {
    let published = engine.snapshot();
    let (a, b) = (p0_of(&published, 0), p0_of(&published, 1));
    let moved = EncodedQuad::new(a.subject, a.predicate, b.object, a.graph);
    let view = published.with_inferred_subset(InferredSubset::Without(Vec::new()));
    let mut out = Vec::new();

    let mut changed = engine.transaction();
    assert!(changed.insert_encoded(moved), "existing ids only");
    assert!(changed.remove_encoded(b));
    out.push(("writer, changed", changed.pending_snapshot()));
    out.push((
        "writer, changed, over a view",
        changed.pending_over(&view, &[]),
    ));
    drop(changed);

    let unchanged = engine.transaction();
    out.push(("writer, no change", unchanged.pending_snapshot()));
    out.push((
        "writer, no change, over",
        unchanged.pending_over(&view, &[]),
    ));
    drop(unchanged);

    let mut undone = engine.transaction();
    assert!(undone.insert_encoded(moved));
    assert!(undone.remove_encoded(moved));
    out.push(("writer, INSERT then DELETE", undone.pending_snapshot()));
    drop(undone);

    let mut speculative = engine.speculative();
    assert!(speculative.remove_encoded(b));
    out.push(("speculative", speculative.pending_snapshot()));
    drop(speculative);

    let mut private = engine
        .speculative_over(&published, Arc::new(Budget::unlimited()))
        .unwrap();
    assert!(private.remove_encoded(b));
    out.push(("private", private.pending_snapshot().unwrap()));
    out.push(("private, over", private.pending_over(&view, &[]).unwrap()));
    for (name, snapshot) in &out {
        assert!(
            snapshot.shared_state().is_none(),
            "{name}: the fixture is unpublished"
        );
    }
    out
}

/// T1 (§5 items 1, 9): an unpublished snapshot never loads the saved sets into the kept map.
///
/// - Spec: S3 and A15; design note §2: without a token, "no claim, no wait, no load, no scan";
///   with none kept, the planner plans without sets.
/// - Input: a durable store loaded with 300 subjects, its sets prepared and saved, reopened (the
///   file there, nothing kept); every unpublished snapshot of [`unpublished`] asks for the sets.
/// - Expected: each gets none, the engine's statistics keep, claim and scan nothing, the file is
///   unchanged. A published snapshot afterwards gets the saved sets (300 subjects) with no scan.
#[test]
fn an_unpublished_snapshot_loads_no_saved_sets() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        add(&engine, 0..300);
        engine.prepare_statistics();
    }
    let saved = std::fs::read(sets_file(dir.path())).expect("the sets were saved");
    let engine = Engine::open(dir.path(), config()).unwrap();
    let statistics = statistics(&engine);
    for (name, snapshot) in unpublished(&engine) {
        assert!(
            snapshot.characteristic_sets_in(MODEL).is_none(),
            "{name}: none kept, so none"
        );
        no_sets_admitted(&statistics, name);
    }
    assert_eq!(std::fs::read(sets_file(dir.path())).unwrap(), saved);
    let sets = engine
        .snapshot()
        .characteristic_sets_in(MODEL)
        .expect("a published snapshot reads the saved sets");
    assert_eq!(sets.subjects(), 300);
    assert_eq!(statistics.scans(), 0, "read, not scanned");
}

/// T2 (§5 item 1): an unpublished snapshot never claims or builds the sets.
///
/// - Spec: as T1.
/// - Input: an in-memory engine with 300 subjects and no sets kept; every unpublished snapshot of
///   [`unpublished`] asks for them.
/// - Expected: none for each, nothing claimed or scanned; a published snapshot then builds them
///   once (300 subjects).
#[test]
fn an_unpublished_snapshot_builds_no_sets() {
    let engine = Engine::new(config()).unwrap();
    add(&engine, 0..300);
    let statistics = statistics(&engine);
    for (name, snapshot) in unpublished(&engine) {
        assert!(
            snapshot.characteristic_sets_in(MODEL).is_none(),
            "{name}: none kept, so none"
        );
        no_sets_admitted(&statistics, name);
    }
    let sets = engine
        .snapshot()
        .characteristic_sets_in(MODEL)
        .expect("built");
    assert_eq!(sets.subjects(), 300);
    assert_eq!(statistics.scans(), 1);
}

/// T3 (§5 items 1, 7): an unpublished snapshot reads drifted kept sets and starts no rebuild.
///
/// - Spec: design note §2: without a token, kept sets are read "whether they fit the size or
///   drifted, with no rebuild start".
/// - Input: sets kept at 600 quads (300 subjects); a writer's pending state without 200 of them
///   (a drift by a third); a build claim held, so a rebuild thread, if one starts, waits.
/// - Expected: the pending snapshot gets the kept sets themselves; no rebuild is asked for and
///   nothing is scanned again. Committed, the same drift on a published snapshot does ask for one.
#[test]
fn an_unpublished_snapshot_starts_no_rebuild() {
    let engine = Engine::new(config()).unwrap();
    add(&engine, 0..300);
    let statistics = statistics(&engine);
    let published = engine.snapshot();
    let kept = published.characteristic_sets_in(MODEL).expect("built");
    assert_eq!(kept_at(&statistics), Some(600));
    let held = statistics.claim_anyway(MODEL);
    let mut tx = engine.transaction();
    for i in 0..200 {
        assert!(tx.remove_encoded(p0_of(&published, i)));
    }
    let pending = tx.pending_snapshot();
    let read = pending
        .characteristic_sets_in(MODEL)
        .expect("the kept sets");
    assert!(Arc::ptr_eq(&read, &kept));
    {
        let kept_sets = statistics.sets.lock();
        let sets = kept_sets.get(&MODEL).unwrap();
        assert!(!sets.rebuilding, "no rebuild asked for by a pending read");
        assert_eq!(sets.at, 600);
    }
    assert_eq!(statistics.scans(), 1);
    drop(pending);
    tx.commit().unwrap();
    let drifted = engine
        .snapshot()
        .characteristic_sets_in(MODEL)
        .expect("kept");
    assert!(Arc::ptr_eq(&drifted, &kept));
    assert!(
        statistics.sets.lock().get(&MODEL).unwrap().rebuilding,
        "a published drift asks for one"
    );
    drop(held);
    until(&statistics, "the published rebuild", |s| {
        kept_at(s) == Some(400)
    });
}

/// T4 (§5 item 1): an unpublished snapshot's distinct count is its own, and a published read
/// afterwards gets the published one.
///
/// - Spec: S3; design note §2: without a token, distinct is computed "with no insert and no
///   clear"; a kept entry within its quarter answers later published reads.
/// - Input: 300 subjects in 600 quads; a writer's pending state with 100 quads of 100 new
///   subjects `t{k} p0 "0"` (700 quads: within the quarter that keeps an entry, and fewer
///   distinct subjects than quads published). The distinct subjects of every quad, first
///   pending, then published. Insertions, because a pending deletion's values still count in
///   `distinct_in`'s runs (an estimate's slack that predates the slice).
/// - Expected: 400 pending, 300 published (counted by construction); no entry from the pending
///   read.
#[test]
fn an_unpublished_distinct_count_answers_no_published_read() {
    let engine = Engine::new(config()).unwrap();
    add(&engine, 0..300);
    let statistics = statistics(&engine);
    let published = engine.snapshot();
    let pattern = QuadPattern::all();
    let mut tx = engine.transaction();
    for k in 0..100 {
        let quad = Quad::new(
            iri(&format!("t{k}")),
            iri("p0"),
            Literal::new_simple_literal("0"),
            GraphName::DefaultGraph,
        );
        assert!(tx.insert(quad.as_ref()));
    }
    let pending = tx.pending_snapshot();
    assert_eq!(
        pending.distinct_estimate_in(MODEL, &pattern, Permutation::Spog),
        Some(400)
    );
    assert_eq!(
        published.distinct_estimate_in(MODEL, &pattern, Permutation::Spog),
        Some(300),
        "the published read gets its own count"
    );
    assert_eq!(
        statistics.distinct.lock().len(),
        1,
        "the published entry alone"
    );
}

/// T5 (§5 item 1): an unpublished snapshot's node counts and heavy groups are computed for it and
/// kept for no one.
///
/// - Spec: S3; design note §2: without a token, node counts and heavy groups are computed and
///   returned with no push.
/// - Input: the pending state of T4 (30 subjects, each with one quad, gone). Its node count over
///   every graph, twice; its subjects with more than two quads, twice; then the published ones.
/// - Expected: pending 540 nodes (270 subjects, 270 literals) and 100 heavy subjects (`i % 3 ==
///   2`), each walk its own, nothing kept; published 600 and 100, kept.
#[test]
fn unpublished_node_counts_and_heavy_groups_are_kept_for_no_one() {
    let engine = Engine::new(config()).unwrap();
    add(&engine, 0..300);
    let statistics = statistics(&engine);
    let published = engine.snapshot();
    let mut tx = engine.transaction();
    for i in (0..90).step_by(3) {
        assert!(tx.remove_encoded(p0_of(&published, i)));
    }
    let pending = tx.pending_snapshot();
    let heavy = |snapshot: &Snapshot| {
        snapshot
            .heavy_groups_in(MODEL, &QuadPattern::all(), Permutation::Spog, 2)
            .expect("walkable")
            .len()
    };
    let unlimited = Budget::unlimited();
    for _ in 0..2 {
        assert_eq!(
            pending.node_count_in(MODEL, GraphSelector::Any, &unlimited),
            Ok(Some(540))
        );
        assert_eq!(heavy(&pending), 100);
    }
    assert!(statistics.nodes.lock().is_empty(), "no node count kept");
    assert!(statistics.heavy.lock().is_empty(), "no heavy groups kept");
    assert_eq!(statistics.heavy_walks(), 2, "each pending read walks");
    assert_eq!(
        published.node_count_in(MODEL, GraphSelector::Any, &unlimited),
        Ok(Some(600))
    );
    assert_eq!(heavy(&published), 100);
    assert_eq!(statistics.nodes.lock().len(), 1);
    assert_eq!(statistics.heavy.lock().len(), 1);
    assert_eq!(statistics.heavy_walks(), 3);
    assert_eq!(heavy(&published), 100);
    assert_eq!(statistics.heavy_walks(), 3, "the published walk is kept");
}

/// T6 (§5 item 2): no view launders an unpublished snapshot into an admission.
///
/// - Spec: S3; 3.0's matrix (no view of an ineligible input has a token); design note §2.
/// - Input: 300 subjects and two inferred statements of `ex:c`; a writer's pending state, and each
///   view of it: stored, equality, canonical, a reader's budget, the two subsets, kept-only, a
///   mask, an addition, and a chain. Each asks for the sets; the pending state and the views that
///   share its statistics also for a distinct count, a node count and heavy groups.
/// - Expected: no sets for any (none are kept); the engine's statistics keep, claim, scan and
///   count nothing.
#[test]
fn no_view_of_an_unpublished_snapshot_admits() {
    let engine = Engine::new(config()).unwrap();
    add(&engine, 0..300);
    let published = engine.snapshot();
    let mut tx = engine.transaction();
    let c = tx.intern(iri("c").as_ref().into());
    let p0 = p0_of(&published, 0).predicate;
    for object in ["c1", "c2"] {
        let object = tx.intern(Literal::new_simple_literal(object).as_ref().into());
        assert!(tx.insert_inferred(EncodedTriple::new(c, p0, object)));
    }
    tx.commit().unwrap();
    let statistics = statistics(&engine);
    let published = engine.snapshot();
    let mut tx = engine.transaction();
    assert!(tx.remove_encoded(p0_of(&published, 1)));
    let pending = tx.pending_snapshot();
    let all: Vec<EncodedQuad> = pending
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .collect();
    assert_eq!(all.len(), 2);
    let budget = || Arc::new(Budget::unlimited());
    let mask = InferredMask::hiding(&pending, &all[..1], &budget()).unwrap();
    let shared: Vec<(&str, Snapshot)> = vec![
        ("pending", pending.clone()),
        ("stored", pending.stored()),
        ("equality", pending.with_equality(Some(p0))),
        ("canonical", pending.with_canonical_equality()),
        ("reading within", pending.reading_within(budget()).unwrap()),
    ];
    let own: Vec<(&str, Snapshot)> = vec![
        (
            "subset without",
            pending.with_inferred_subset(InferredSubset::Without(all[..1].to_vec())),
        ),
        (
            "subset only",
            pending.with_inferred_subset(InferredSubset::Only(all[1..].to_vec())),
        ),
        (
            "inferred only",
            pending.with_inferred_only(&all[1..], &budget()).unwrap(),
        ),
        (
            "mask",
            pending
                .with_inferred_mask(&mask, &budget())
                .unwrap()
                .unwrap(),
        ),
        (
            "added",
            pending.with_inferred_added(&[p0_of(&published, 1)]),
        ),
        (
            "a chain",
            pending
                .with_inferred_subset(InferredSubset::Without(all[..1].to_vec()))
                .stored()
                .reading_within(budget())
                .unwrap(),
        ),
    ];
    let pattern = QuadPattern {
        predicate: Some(p0),
        ..QuadPattern::all()
    };
    for (name, view) in shared.iter().chain(&own) {
        assert!(
            view.shared_state().is_none(),
            "{name}: the fixture is unpublished"
        );
        assert!(
            view.characteristic_sets_in(MODEL).is_none(),
            "{name}: none kept, so none"
        );
        no_sets_admitted(&statistics, name);
    }
    for (name, view) in &shared {
        let _ = view.distinct_estimate_in(MODEL, &pattern, Permutation::Posg);
        let _ = view.node_count_in(
            ReadModel::Asserted,
            GraphSelector::Any,
            &Budget::unlimited(),
        );
        let _ = view.heavy_groups_in(
            ReadModel::Asserted,
            &QuadPattern::all(),
            Permutation::Spog,
            2,
        );
        assert!(
            statistics.distinct.lock().is_empty(),
            "{name}: no distinct entry"
        );
        assert!(statistics.nodes.lock().is_empty(), "{name}: no node count");
        assert!(
            statistics.heavy.lock().is_empty(),
            "{name}: no heavy groups"
        );
    }
}

/// T7 (§5 items 2, 8; design check §1's statistics rows): a published restriction admits into
/// its own statistics, which have no directory: it neither loads nor saves the engine's file.
///
/// - Spec: design check §1, "A restricted view's fresh statistics has no derived directory, so
///   it neither saves nor loads the engine's sets files."
/// - Input: a durable store with 300 asserted subjects and one subject `ex:c` with only an
///   inferred statement, its sets prepared and saved (301 subjects), reopened; a published mask
///   that hides `ex:c`'s statement asks for the sets.
/// - Expected: the view has its own: 300 subjects (scanned, not the file's 301). The engine's
///   statistics keep and scan nothing; the file is unchanged; a published snapshot then reads the
///   file's 301.
#[test]
fn a_published_restriction_keeps_its_own_sets_and_no_file() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        add(&engine, 0..300);
        let mut tx = engine.transaction();
        let c = tx.intern(iri("c").as_ref().into());
        let p = tx.intern(iri("p0").as_ref().into());
        let o = tx.intern(Literal::new_simple_literal("c").as_ref().into());
        assert!(tx.insert_inferred(EncodedTriple::new(c, p, o)));
        tx.commit().unwrap();
        engine.prepare_statistics();
    }
    let saved = std::fs::read(sets_file(dir.path())).expect("the sets were saved");
    let engine = Engine::open(dir.path(), config()).unwrap();
    let statistics = statistics(&engine);
    let published = engine.snapshot();
    let inferred: Vec<EncodedQuad> = published
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .collect();
    assert_eq!(inferred.len(), 1);
    let budget = Arc::new(Budget::unlimited());
    let mask = InferredMask::hiding(&published, &inferred, &budget).unwrap();
    let view = published
        .with_inferred_mask(&mask, &budget)
        .unwrap()
        .unwrap();
    assert!(view.shared_state().is_some(), "a published restriction");
    let own = view.characteristic_sets_in(MODEL).expect("its own, built");
    assert_eq!(
        own.subjects(),
        300,
        "scanned from the view, not read from the file"
    );
    no_sets_admitted(&statistics, "the engine's, beside the view");
    assert_eq!(std::fs::read(sets_file(dir.path())).unwrap(), saved);
    let sets = published.characteristic_sets_in(MODEL).expect("the file's");
    assert_eq!(sets.subjects(), 301);
    assert_eq!(statistics.scans(), 0);
}

/// T8 (§5 item 7; F3): a rebuild finishes for the snapshot that started it, whatever was
/// published meanwhile.
///
/// - Spec: design check F3 and §5 item 7: deferred work "completes for the original authorised
///   snapshot/destination or is discarded", never rekeyed as latest.
/// - Input: sets kept at 600 quads; 300 more subjects committed and a published read of that
///   version (1,200 quads, a drift) starts the rebuild, held at its first scan; that snapshot
///   dropped; 300 more committed; the rebuild released.
/// - Expected: the sets kept are the starting snapshot's: at 1,200 quads, 600 subjects.
#[test]
fn a_rebuild_finishes_for_the_snapshot_that_started_it() {
    let engine = Engine::new(config()).unwrap();
    add(&engine, 0..300);
    let statistics = statistics(&engine);
    engine
        .snapshot()
        .characteristic_sets_in(MODEL)
        .expect("built");
    add(&engine, 300..600);
    let (entered, release) = hold_next_build(&statistics);
    let started = engine.snapshot();
    assert!(
        started.characteristic_sets_in(MODEL).is_some(),
        "the old ones meanwhile"
    );
    entered
        .recv_timeout(Duration::from_secs(30))
        .expect("the rebuild began");
    drop(started);
    add(&engine, 600..900);
    drop(release);
    until(&statistics, "the rebuild", |s| kept_at(s) != Some(600));
    assert_eq!(kept_at(&statistics), Some(1_200));
    let sets = statistics.sets.lock().get(&MODEL).unwrap().sets().unwrap();
    assert_eq!(sets.subjects(), 600);
}

/// T9 (§5 item 8): a rebuild that outlives its store saves nothing into the directory a later
/// open owns.
///
/// - Spec: design check §2's last row and §5 item 8: "A delayed old save cannot write into a new
///   destination generation"; design note §2.4.
/// - Input: a durable store with sets saved at 600 quads; 300 more subjects committed and a
///   published read starts a rebuild, held at its first scan; the engine dropped (its `LOCK`
///   released); the store reopened, 100 more subjects committed and its sets prepared (the file
///   now the reopened store's, at 1,399 quads); the old rebuild released and finished (at 1,200).
/// - Expected: the file is still the reopened store's, byte for byte; the old rebuild's sets are
///   kept in its own statistics only.
#[test]
fn a_rebuild_that_outlives_its_store_saves_nothing_into_a_later_open() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    add(&engine, 0..300);
    engine.prepare_statistics();
    add(&engine, 300..600);
    let old = statistics(&engine);
    let (entered, release) = hold_next_build(&old);
    let snapshot = engine.snapshot();
    assert!(snapshot.characteristic_sets_in(MODEL).is_some());
    entered
        .recv_timeout(Duration::from_secs(30))
        .expect("the rebuild began");
    drop(snapshot);
    drop(engine);

    let reopened = Engine::open(dir.path(), config()).unwrap();
    add(&reopened, 600..700);
    reopened.prepare_statistics();
    let theirs = std::fs::read(sets_file(dir.path())).expect("the reopened store's sets");
    drop(release);
    until(&old, "the old rebuild", |s| kept_at(s) == Some(1_200));
    assert_eq!(
        std::fs::read(sets_file(dir.path())).unwrap(),
        theirs,
        "the later open's file, untouched"
    );
}

/// T10 (§5 item 8): a snapshot that outlives its store neither loads from nor saves into the
/// directory a later open owns.
///
/// - Spec: as T9; design note §2.4: no save or load starts after the close.
/// - Input: a durable store with 300 subjects and nothing kept; a published snapshot kept while
///   the engine drops; the store reopened, 100 more subjects committed and its sets prepared (the
///   file the reopened store's, 400 subjects); the kept snapshot asks for the sets.
/// - Expected: it gets its own, built from its 300 subjects; the file is unchanged.
#[test]
fn a_snapshot_that_outlives_its_store_touches_no_later_file() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    add(&engine, 0..300);
    let kept = engine.snapshot();
    drop(engine);

    let reopened = Engine::open(dir.path(), config()).unwrap();
    add(&reopened, 300..400);
    reopened.prepare_statistics();
    let theirs = std::fs::read(sets_file(dir.path())).expect("the reopened store's sets");
    let sets = kept.characteristic_sets_in(MODEL).expect("its own, built");
    assert_eq!(sets.subjects(), 300);
    assert_eq!(
        std::fs::read(sets_file(dir.path())).unwrap(),
        theirs,
        "the later open's file, untouched"
    );
}
