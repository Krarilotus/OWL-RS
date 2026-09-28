//! E5 gate (engine side): bulk loads equal the same load through transactions, from any
//! number of threads, and are durable before they become visible.

use std::collections::HashSet;

use nrese_engine::{
    BulkMode, EncodedQuad, EncodedTriple, Engine, EngineConfig, QuadPattern, ReadModel, Snapshot,
    TermId, Transaction,
};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad};

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    }
}

/// Varied term shapes: IRIs, blank nodes, inline and dictionary literals, named graphs.
fn quad(n: u64) -> Quad {
    let object: oxrdf::Term = match n % 4 {
        0 => NamedNode::new_unchecked(format!("http://example.com/o{}", n % 50)).into(),
        1 => Literal::new_typed_literal(format!("{}", n % 70), xsd::INTEGER).into(),
        2 => Literal::new_language_tagged_literal_unchecked(format!("label {n}"), "en").into(),
        _ => BlankNode::new_unchecked(format!("b{}", n % 30)).into(),
    };
    let graph = match n % 3 {
        0 => GraphName::DefaultGraph,
        _ => NamedNode::new_unchecked(format!("http://example.com/g{}", n % 2)).into(),
    };
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/s{}", n % 400)),
        NamedNode::new_unchecked(format!("http://example.com/p{}", n % 7)),
        object,
        graph,
    )
}

fn quads(range: std::ops::Range<u64>) -> Vec<Quad> {
    range.map(quad).collect()
}

fn contents(snapshot: &Snapshot, model: ReadModel) -> HashSet<Quad> {
    snapshot
        .quads_for_pattern_in(model, &QuadPattern::all())
        .map(|q| snapshot.decode_quad(q).expect("decodable"))
        .collect()
}

/// Encodes a default-graph quad, interning its terms.
fn encode(tx: &Transaction<'_>, quad: &Quad) -> EncodedQuad {
    assert!(quad.graph_name.is_default_graph());
    EncodedQuad::new(
        tx.intern(quad.subject.as_ref().into()),
        tx.intern(quad.predicate.as_ref().into()),
        tx.intern(quad.object.as_ref()),
        TermId::DEFAULT_GRAPH,
    )
}

fn load_by_transaction(engine: &Engine, quads: &[Quad]) {
    let mut tx = engine.transaction();
    for quad in quads {
        tx.insert(quad.as_ref());
    }
    tx.commit().expect("commit");
}

#[test]
fn bulk_append_equals_a_transactional_load() {
    let data = quads(0..5_000); // with duplicates: n % 400 subjects etc.
    let reference = Engine::new(config()).unwrap();
    load_by_transaction(&reference, &data);

    let engine = Engine::new(config()).unwrap();
    let load = engine.bulk_load(BulkMode::Append);
    for chunk in data.chunks(700) {
        load.add(chunk);
    }
    let summary = load.finish().expect("finish");
    let snapshot = engine.snapshot();
    assert_eq!(summary.revision, 1);
    assert_eq!(summary.inserted, reference.snapshot().len());
    assert_eq!(snapshot.len(), reference.snapshot().len());
    assert_eq!(
        contents(&snapshot, ReadModel::Materialised),
        contents(&reference.snapshot(), ReadModel::Materialised)
    );
}

#[test]
fn parallel_batches_give_the_same_dataset() {
    let data = quads(0..20_000);
    let engine = Engine::new(config()).unwrap();
    let load = engine.bulk_load(BulkMode::Append);
    std::thread::scope(|scope| {
        for chunk in data.chunks(2_500) {
            let load = &load;
            scope.spawn(move || {
                for batch in chunk.chunks(300) {
                    load.add(batch);
                }
            });
        }
    });
    load.finish().expect("finish");
    let expected: HashSet<Quad> = data.into_iter().collect();
    assert_eq!(contents(&engine.snapshot(), ReadModel::Asserted), expected);
    // Every interned term decodes to itself: ids assigned under concurrency are consistent.
    let snapshot = engine.snapshot();
    for quad in &expected {
        assert!(snapshot.lookup_quad(quad.as_ref()).is_some(), "{quad}");
    }
}

#[test]
fn append_skips_existing_quads_and_makes_inferred_ones_explicit() {
    let engine = Engine::new(config()).unwrap();
    load_by_transaction(&engine, &quads(0..100));
    // An inferred statement that the bulk load will assert.
    let explicit = Quad::new(
        NamedNode::new_unchecked("http://example.com/x"),
        NamedNode::new_unchecked("http://example.com/p"),
        NamedNode::new_unchecked("http://example.com/y"),
        GraphName::DefaultGraph,
    );
    let mut tx = engine.transaction();
    let encoded = encode(&tx, &explicit);
    assert!(tx.insert_inferred(EncodedTriple::from(encoded)));
    tx.commit().expect("commit");

    let load = engine.bulk_load(BulkMode::Append);
    load.add(&quads(50..150));
    load.add(std::slice::from_ref(&explicit));
    let summary = load.finish().expect("finish");
    assert_eq!(
        summary.inserted, 51,
        "50 new quads plus the explicit one: {summary:?}"
    );
    assert_eq!(summary.inferred_deleted, 1, "{summary:?}");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.len_in(ReadModel::Inferred), 0);
    let mut expected: HashSet<Quad> = quads(0..150).into_iter().collect();
    expected.insert(explicit);
    assert_eq!(contents(&snapshot, ReadModel::Asserted), expected);
}

#[test]
fn replace_swaps_the_dataset_and_clears_inferences() {
    let engine = Engine::new(config()).unwrap();
    load_by_transaction(&engine, &quads(0..300));
    let mut tx = engine.transaction();
    let encoded = encode(&tx, &quad(10_002)); // n % 3 == 0: default graph
    tx.insert_inferred(EncodedTriple::from(encoded));
    tx.commit().expect("commit");

    let load = engine.bulk_load(BulkMode::Replace);
    load.add(&quads(1_000..1_200));
    let summary = load.finish().expect("finish");
    assert_eq!((summary.inserted, summary.inferred_deleted), (200, 1));
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.revision(), 3);
    assert_eq!(
        contents(&snapshot, ReadModel::Materialised),
        quads(1_000..1_200).into_iter().collect()
    );

    // Commits continue on top of the bulk-loaded base.
    load_by_transaction(&engine, &quads(5..6));
    assert_eq!(engine.snapshot().revision(), 4);
}

#[test]
fn an_empty_or_redundant_load_creates_no_revision() {
    let engine = Engine::new(config()).unwrap();
    load_by_transaction(&engine, &quads(0..10));
    let load = engine.bulk_load(BulkMode::Append);
    load.add(&quads(0..10));
    assert_eq!(load.finish().expect("finish").revision, 1);
    assert_eq!(engine.snapshot().revision(), 1);
}

#[test]
fn durable_bulk_loads_are_checkpointed_and_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let expected: HashSet<Quad> = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        load_by_transaction(&engine, &quads(0..50));
        let load = engine.bulk_load(BulkMode::Append);
        load.add(&quads(40..3_000));
        assert_eq!(load.finish().expect("finish").revision, 2);
        load_by_transaction(&engine, &quads(3_000..3_010)); // a WAL record after the load
        contents(&engine.snapshot(), ReadModel::Materialised)
    };
    let checkpoints: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.ends_with(".nck").then_some(name)
        })
        .collect();
    assert_eq!(checkpoints, ["checkpoint-00000000000000000002.nck"]);

    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.snapshot().revision(), 3);
    assert_eq!(
        contents(&engine.snapshot(), ReadModel::Materialised),
        expected
    );

    // Replace, then reopen once more.
    let load = engine.bulk_load(BulkMode::Replace);
    load.add(&quads(7_000..7_100));
    load.finish().expect("finish");
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.snapshot().revision(), 4);
    assert_eq!(
        contents(&engine.snapshot(), ReadModel::Materialised),
        quads(7_000..7_100).into_iter().collect()
    );
}

/// A default-graph triple over the `quad` shapes.
fn triple(n: u64) -> Quad {
    Quad {
        graph_name: GraphName::DefaultGraph,
        ..quad(n)
    }
}

fn encode_with(
    rematerialisation: &nrese_engine::Rematerialisation<'_>,
    quad: &Quad,
) -> EncodedTriple {
    EncodedTriple::new(
        rematerialisation.intern(quad.subject.as_ref().into()),
        rematerialisation.intern(quad.predicate.as_ref().into()),
        rematerialisation.intern(quad.object.as_ref()),
    )
}

#[test]
fn rematerialisation_replaces_the_inferred_stack_durably() {
    let dir = tempfile::tempdir().unwrap();
    let asserted: Vec<Quad> = (0..40).map(triple).collect();
    let (inferred, expected) = {
        let engine = Engine::open(dir.path(), config()).unwrap();
        load_by_transaction(&engine, &asserted);
        // A first set, then a replacement overlapping it; both include asserted statements,
        // which stay asserted only.
        let first = engine.rematerialisation();
        let set: Vec<EncodedTriple> = (20..100).map(|n| encode_with(&first, &triple(n))).collect();
        let summary = first.finish(set).expect("finish");
        assert_eq!((summary.revision, summary.inferred_inserted), (2, 60));
        let second = engine.rematerialisation();
        assert_eq!(second.base().len_in(ReadModel::Inferred), 60);
        let set: Vec<EncodedTriple> = (80..150)
            .map(|n| encode_with(&second, &triple(n)))
            .collect();
        let summary = second.finish(set).expect("finish");
        assert_eq!(
            (
                summary.revision,
                summary.inferred_inserted,
                summary.inferred_deleted
            ),
            (3, 50, 40)
        );
        // An identical set changes nothing.
        let third = engine.rematerialisation();
        let set: Vec<EncodedTriple> = (80..150).map(|n| encode_with(&third, &triple(n))).collect();
        assert_eq!(third.finish(set).expect("finish").revision, 3);
        let snapshot = engine.snapshot();
        (
            contents(&snapshot, ReadModel::Inferred),
            contents(&snapshot, ReadModel::Materialised),
        )
    };
    let wanted: HashSet<Quad> = (80..150)
        .map(triple)
        .filter(|q| !asserted.contains(q))
        .collect();
    assert_eq!(inferred, wanted);
    let engine = Engine::open(dir.path(), config()).unwrap();
    assert_eq!(engine.snapshot().revision(), 3);
    assert_eq!(
        contents(&engine.snapshot(), ReadModel::Materialised),
        expected
    );
    assert_eq!(contents(&engine.snapshot(), ReadModel::Inferred), inferred);
}
