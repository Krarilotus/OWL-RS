//! Guard of the full-text corpora's memory under churn (docs/design/performance.md §0):
//! commits over a bounded pool of literals, each followed by a search, derive each version's
//! corpus from the last; the corpus and the lineage they keep must not grow past the first
//! rounds. Counted with a counting allocator, so this file is a test binary of its own.

use nrese_engine::{Engine, EngineConfig, TextQuery};
use nrese_exec::heap;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// Literals and subjects the commits draw from.
const POOL: u64 = 2_000;

fn label(i: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/s{}", i % 300)),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Literal::new_simple_literal(format!("word{} word{} item{i}", i % 17, i % 5)),
        match i % 3 {
            0 => GraphName::DefaultGraph,
            g => NamedNode::new_unchecked(format!("http://example.com/g{g}")).into(),
        },
    )
}

#[test]
fn corpora_under_churn_hold_the_heap_level() {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for i in 0..POOL {
        tx.insert(label(i).as_ref());
    }
    tx.commit().unwrap();
    let query = TextQuery {
        text: "word3 word4".to_owned(),
        all_words: false,
        prefix: false,
        stem: None,
    };
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % POOL
    };
    let mut present = vec![true; POOL as usize];
    let mut round = |engine: &Engine| {
        let mut tx = engine.transaction();
        for _ in 0..5 {
            let i = next();
            match present[i as usize] {
                true => tx.remove(label(i).as_ref()),
                false => tx.insert(label(i).as_ref()),
            };
            present[i as usize] = !present[i as usize];
        }
        tx.commit().unwrap();
        assert!(!engine.snapshot().text_search(&query).is_empty());
    };
    for _ in 0..300 {
        round(&engine);
    }
    let level = heap::live();
    for _ in 0..900 {
        round(&engine);
    }
    let after = heap::live();
    let grew = after.saturating_sub(level);
    let stats = engine.stats().text_corpora;
    eprintln!(
        "{level} bytes live after 300 rounds, {after} after 900 more: {grew} grown; {stats:?}"
    );
    // The first round's search walks; every later one derives.
    assert_eq!((stats.walked, stats.derived), (1, 1_199), "{stats:?}");
    // Kept per commit, 36 bytes a round would pass this.
    assert!(
        grew < 32 << 10,
        "the heap grew by {grew} bytes over 900 rounds"
    );
}
