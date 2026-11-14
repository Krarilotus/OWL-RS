//! Guard of the distinct-value walk's memory (D16-576): a planner's distinct count holds no
//! list of the values it counts, over one run or several. Measured by the allocation oracle
//! (`nrese_exec::measure`), so this file is a test binary of its own.

use nrese_engine::quad::{GraphSelector, Permutation, QuadPattern};
use nrese_engine::{CompactionPolicy, Engine, EngineConfig, ReadModel, TermId};
use nrese_exec::measure;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// What the walk may hold whatever the number of values: a head, a cursor and one block's
/// column (128 values) per run, the list of runs, and the read's own small state.
const FIXED: usize = 16 * 1024;

/// - Spec: the walk behind a planner's distinct count holds memory independent of the number
///   of distinct values: no list of them (today's gathered 8 bytes a value), over one run and
///   over several whose values interleave.
/// - Input: 40,000 subjects of one predicate, first in one commit (one run), then in a
///   second store over four commits (subject `i` in commit `i % 4`).
/// - Expected: the counts are 40,000; the most bytes held during each walk stay under
///   `FIXED` (16 KiB, against 320 KB for a list).
#[test]
fn a_distinct_walk_holds_no_list_of_its_values() {
    if !measure::alone() {
        return;
    }
    for commits in [1u32, 4] {
        // No window ever merges: each commit a run of its own.
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            compaction: CompactionPolicy {
                fanout: 0,
                inline_entry_limit: 0,
            },
            ..EngineConfig::default()
        })
        .unwrap();
        let predicate = NamedNode::new_unchecked("http://example.com/p");
        for part in 0..commits {
            let mut tx = engine.transaction();
            for i in (0..40_000u32).filter(|i| i % commits == part) {
                let quad = Quad::new(
                    NamedNode::new_unchecked(format!("http://example.com/s{i}")),
                    predicate.clone(),
                    Literal::new_simple_literal(format!("{i}")),
                    GraphName::DefaultGraph,
                );
                tx.insert(quad.as_ref());
            }
            tx.commit().unwrap();
        }
        let snapshot = engine.snapshot();
        let pattern = QuadPattern {
            predicate: snapshot.lookup(predicate.as_ref().into()),
            graph: GraphSelector::Exact(TermId::DEFAULT_GRAPH),
            ..QuadPattern::all()
        };
        let (count, held) = measure::measure(|| {
            snapshot.distinct_in(ReadModel::Asserted, &pattern, Permutation::Gpso, &|_| true)
        });
        assert_eq!(count, Ok(Some(40_000)), "{commits} commits");
        assert!(
            held < FIXED,
            "{commits} commits: the walk held {held} bytes for 40,000 values"
        );
    }
}
