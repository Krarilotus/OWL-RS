//! Guard of the canonical views' memory (canonical equality answers, research-designs §1):
//! a view is reserved before it is made, on the store's view budget or else on its reader's
//! own, and holds its charge until its last holder lets it go. Measured against those
//! budgets (`nrese_exec::measure`), so this file is a test binary of its own.

use std::sync::{Arc, Barrier, Mutex};

use nrese_engine::{EncodedTriple, Engine, EngineConfig, QuadPattern, ReadModel, Snapshot};
use nrese_exec::{Budget, measure};
use nrese_rdf::{NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

/// `size` statements about `alias` in a named graph, `alias sameAs rep`, canonical views
/// charged to `views`. The classes are made here, before any measurement: they are the
/// version's, charged nowhere, as before canonical answers.
fn engine(size: usize, views: &Arc<Budget>) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
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
    Engine::new(EngineConfig::default())
        .unwrap()
        .snapshot()
        .with_canonical_equality();
    engine
}

/// A kept view is charged for everything it holds, its run and its version, from before it
/// is made until its version goes (a fixed estimate once reserved less than the run held).
#[test]
fn a_kept_view_is_charged_for_all_it_holds() {
    if !measure::alone() {
        return;
    }
    let views = Arc::new(Budget::unlimited());
    let engine = engine(1, &views);
    let snapshot = engine.snapshot();
    let (canonical, excess) =
        measure::measure_within(&views, || snapshot.with_canonical_equality());
    assert_eq!(excess, 0, "held beyond its charge");
    assert!(views.used() > 0);
    drop((canonical, snapshot, engine));
    assert_eq!(views.used(), 0);
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
