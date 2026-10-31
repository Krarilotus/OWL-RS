//! Guard of the views' memory: the canonical views (canonical equality answers,
//! research-designs §1) and the restricted inferred views (`inferred = "supported"`). A
//! view is reserved before it is made, on the store's view budget or else on its reader's
//! own, and holds its charge until its last holder lets it go. Measured against those
//! budgets (`nrese_exec::measure`), so this file is a test binary of its own.

use std::sync::{Arc, Barrier, Mutex};

use nrese_engine::{
    EncodedQuad, EncodedTriple, Engine, EngineConfig, InferredMask, QuadPattern, ReadModel,
    Snapshot,
};
use nrese_exec::{Budget, measure};
use nrese_rdf::{NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

/// An engine without background maintenance: no compaction or checkpoint runs on a thread of
/// its own while a measurement does (every thread counts).
fn quiet() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    }
}

/// `size` statements about `alias` in a named graph, `alias sameAs rep`, canonical views
/// charged to `views`. The classes are made here, before any measurement: they are the
/// version's, charged nowhere, as before canonical answers.
fn engine(size: usize, views: &Arc<Budget>) -> Engine {
    let engine = Engine::new(quiet()).unwrap();
    engine.set_canonical_view_budget(Arc::clone(views));
    let mut tx = engine.transaction();
    let rep = tx.intern(node("rep").as_ref().into());
    let alias = tx.intern(node("alias").as_ref().into());
    let same_as = tx.intern(node("sameAs").as_ref().into());
    tx.insert_inferred(EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: rep,
    });
    for i in 0..size {
        let object = node(&format!("o{i}"));
        tx.insert(Quad::new(node("alias"), node("p"), object, node("g")).as_ref());
    }
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    engine
        .snapshot()
        .count_in(ReadModel::Materialised, &QuadPattern::all());
    // Readers that bring no budget share one, made once.
    Engine::new(quiet())
        .unwrap()
        .snapshot()
        .with_canonical_equality();
    engine
}

/// A view is charged for everything it holds, its run and its version, from before it is
/// made until it is freed, kept or made for its reader alone (a fixed estimate once
/// reserved less than the run held; the run once released its charge before the version
/// holding it was freed).
#[test]
fn a_view_is_charged_for_all_it_holds_until_it_goes() {
    if !measure::alone() {
        return;
    }
    // The view kept, charged to the view budget, freed with the engine and its readers.
    let views = Arc::new(Budget::unlimited());
    let store = engine(1, &views);
    let snapshot = store.snapshot();
    let ((), excess) = measure::measure_within(&views, || {
        let canonical = snapshot.with_canonical_equality();
        assert!(views.used() > 0);
        drop((canonical, snapshot, store));
    });
    assert_eq!(excess, 0, "a kept view held beyond its charge");
    assert_eq!(views.used(), 0);
    // The view the view budget refuses, charged to its reader's, freed with the reader.
    let store = engine(1, &Arc::new(Budget::new(1)));
    let snapshot = store.snapshot();
    let query = Arc::new(Budget::unlimited());
    let ((), excess) = measure::measure_within(&query, || {
        let canonical = snapshot.with_canonical_equality_within(&query).unwrap();
        assert!(query.used() > 0);
        drop(canonical);
    });
    assert_eq!(excess, 0, "a reader's view held beyond its charge");
    assert_eq!(query.used(), 0);
}

/// A view's run of any size is charged for what building it holds: its keys made beside its
/// entries, in the room reserved for them. Collected in place over the entries (40 bytes
/// each, the keys 32), the allocation was shrunk by a reallocation that held both blocks,
/// uncharged, wherever the entries' bytes aren't a multiple of the keys' (here, an odd
/// number of statements: each renamed one an insert and a delete). Found by the lock
/// sweep's view guard: 96,032 bytes at 12,005 statements.
#[test]
fn a_view_of_any_size_holds_no_more_than_its_charge() {
    if !measure::alone() {
        return;
    }
    for size in [3, 5, 1_001] {
        let views = Arc::new(Budget::unlimited());
        let store = engine(size, &views);
        let snapshot = store.snapshot();
        let ((), excess) = measure::measure_within(&views, || {
            let canonical = snapshot.with_canonical_equality();
            drop((canonical, snapshot, store));
        });
        assert_eq!(
            excess, 0,
            "{size} statements: {excess} bytes beyond the charge"
        );
        assert_eq!(views.used(), 0);
    }
}

/// Readers that miss the cache at once each make a view and keep its charge while they
/// hold it, whichever the cache keeps (replacing the cache's once released what readers
/// still held).
#[test]
fn views_made_at_once_keep_their_charges_while_held() {
    if !measure::alone() {
        return;
    }
    const READERS: usize = 16;
    let views = Arc::new(Budget::unlimited());
    let engine = engine(4096, &views);
    let snapshot = engine.snapshot();
    let (start, done) = (
        Arc::new(Barrier::new(READERS + 1)),
        Arc::new(Barrier::new(READERS + 1)),
    );
    let held: Arc<Mutex<Vec<Option<Snapshot>>>> = Arc::new(Mutex::new(vec![None; READERS]));
    let readers: Vec<_> = (0..READERS)
        .map(|i| {
            let (snapshot, start, done, held) = (
                snapshot.clone(),
                Arc::clone(&start),
                Arc::clone(&done),
                Arc::clone(&held),
            );
            std::thread::spawn(move || {
                start.wait();
                let canonical = snapshot.with_canonical_equality();
                held.lock().unwrap()[i] = Some(canonical);
                done.wait();
            })
        })
        .collect();
    let ((), excess) = measure::measure_within(&views, || {
        start.wait();
        done.wait();
    });
    assert_eq!(excess, 0, "views held beyond their charges");
    for reader in readers {
        reader.join().unwrap();
    }
    drop((held, snapshot, engine));
    assert_eq!(views.used(), 0);
}

/// A view the view budget refuses is made on its reader's budget, and where that refuses
/// it too, nothing is made (it was once made in full, then refused).
#[test]
fn a_view_both_budgets_refuse_is_never_made() {
    if !measure::alone() {
        return;
    }
    let views = Arc::new(Budget::new(1));
    let engine = engine(4096, &views);
    let snapshot = engine.snapshot();
    let query = Arc::new(Budget::new(1));
    let (made, excess) =
        measure::measure_within(&query, || snapshot.with_canonical_equality_within(&query));
    assert!(made.is_err());
    assert_eq!(excess, 0, "made before it was refused");
    assert_eq!((views.used(), query.used()), (0, 0));
}

/// A restricted inferred view (`inferred = "supported"`), a list of what a reader sees or a
/// mask over the stack of what it misses, is charged for all it makes before it is made,
/// and holds it until the last of the masks and views over it goes. The charge follows
/// the view's size.
#[test]
fn inferred_views_are_charged_for_all_they_hold() {
    if !measure::alone() {
        return;
    }
    let mut charged = Vec::new();
    for size in [500, 4_000] {
        let store = Engine::new(quiet()).unwrap();
        let mut tx = store.transaction();
        let s = tx.intern(node("s").as_ref().into());
        let p = tx.intern(node("p").as_ref().into());
        for i in 0..size {
            let o = tx.intern(node(&format!("o{i}")).as_ref().into());
            tx.insert_inferred(EncodedTriple {
                subject: s,
                predicate: p,
                object: o,
            });
        }
        tx.commit().unwrap();
        let snapshot = store.snapshot();
        let mut quads: Vec<EncodedQuad> = snapshot
            .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
            .collect();
        quads.sort_unstable();
        let (seen, missed) = quads.split_at(size / 2);
        let budget = Arc::new(Budget::unlimited());
        let (used, excess) = measure::measure_within(&budget, || {
            let only = snapshot.with_inferred_only(seen, &budget).unwrap();
            let mask = InferredMask::hiding(&snapshot, missed, &budget).unwrap();
            let masked = snapshot
                .with_inferred_mask(&mask, &budget)
                .unwrap()
                .expect("the stack's layout");
            assert_eq!(only.len_in(ReadModel::Inferred), seen.len() as u64);
            assert_eq!(masked.len_in(ReadModel::Inferred), seen.len() as u64);
            let used = budget.used();
            drop((only, mask, masked));
            used
        });
        assert_eq!(
            excess, 0,
            "{size} statements: {excess} bytes beyond the charge"
        );
        assert_eq!(budget.used(), 0, "{size} statements: still charged");
        charged.push(used);
    }
    assert!(charged[1] > 4 * charged[0], "{charged:?}");
}

/// A view or mask refused anywhere in its making holds nothing past its charge, and nothing
/// once refused: every budget from none to what it needs, for a view of kept statements, a
/// mask stacked on an earlier one, and the view of such a mask. Its charge is taken before
/// anything it covers is made, so no refusal releases it while the caller still owns a run
/// (review r1 of item 18 part 1, F5). What the earlier masks hold is charged elsewhere and
/// made before the measurement.
#[test]
fn inferred_views_refused_anywhere_free_before_they_release() {
    if !measure::alone() {
        return;
    }
    let store = Engine::new(quiet()).unwrap();
    let mut tx = store.transaction();
    let s = tx.intern(node("s").as_ref().into());
    let p = tx.intern(node("p").as_ref().into());
    for i in 0..200 {
        let o = tx.intern(node(&format!("o{i}")).as_ref().into());
        tx.insert_inferred(EncodedTriple {
            subject: s,
            predicate: p,
            object: o,
        });
    }
    tx.commit().unwrap();
    let snapshot = store.snapshot();
    let mut quads: Vec<EncodedQuad> = snapshot
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .collect();
    quads.sort_unstable();
    let (seen, missed) = quads.split_at(100);
    let (earlier, later) = missed.split_at(50);
    let unlimited = Arc::new(Budget::unlimited());
    // The masks a later one stacks on and a view reads, charged elsewhere.
    let first = InferredMask::hiding(&snapshot, earlier, &unlimited).unwrap();
    let second = first.changed(&snapshot, later, &[], &unlimited).unwrap();
    let only = |budget: &Arc<Budget>| snapshot.with_inferred_only(seen, budget).map(drop);
    let stacked = |budget: &Arc<Budget>| first.changed(&snapshot, later, &[], budget).map(drop);
    let viewed = |budget: &Arc<Budget>| {
        snapshot
            .with_inferred_mask(&second, budget)
            .map(|view| drop(view.expect("the stack's layout")))
    };
    type Make<'a> = &'a dyn Fn(&Arc<Budget>) -> Result<(), nrese_exec::BudgetExceeded>;
    let makes: [(&str, Make); 3] = [("only", &only), ("stacked", &stacked), ("viewed", &viewed)];
    for (name, make) in makes {
        // The most it charges at once: every budget below it refuses somewhere.
        let needed = {
            let budget = Arc::new(Budget::unlimited());
            make(&budget).unwrap();
            budget.peak()
        };
        let (mut refused, mut made) = (0, 0);
        for limit in (0..=needed + 8).step_by(4) {
            let budget = Arc::new(Budget::new(limit));
            let (result, excess) = measure::measure_within(&budget, || make(&budget));
            refused += usize::from(result.is_err());
            made += usize::from(result.is_ok());
            assert_eq!(
                excess, 0,
                "{name} within {limit} bytes: {excess} past the charge"
            );
            assert_eq!(
                budget.used(),
                0,
                "{name} within {limit} bytes: still charged"
            );
        }
        assert!(
            refused > 0 && made > 0,
            "{name}: {refused} refused, {made} made"
        );
    }
}
