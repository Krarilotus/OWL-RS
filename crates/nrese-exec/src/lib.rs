//! NRESE execution core (decision D11, `docs/design/execution-core.md`).
//!
//! The id-level machinery of the native SPARQL executor (`nrese-sparql`), the engine and
//! the reasoner's batch and delta executors (`nrese-reasoner`). What is shared is kernels,
//! not executors: the reasoner's rule joins are their own (investigation of 6 October
//! 2026, G1); it uses the closures ([`graph`]) and the memory watch, and the sort kernel
//! and the engine's probe cursors (`nrese_engine::ProbeCursor`) are there for it.
//! - [`table`]: [`IdTable`], column-major `u64` tables with sort metadata
//! - [`join`]: merge, hash, left (OPTIONAL) and anti (MINUS, NOT EXISTS) joins on key columns
//! - [`group`]: grouping with aggregates over sorted or unsorted input
//! - [`graph`]: adjacency, strongly connected components, reachability and closures
//!   (property paths, hierarchies)
//! - [`classes`]: equivalence classes by union (`owl:sameAs` classes)
//! - [`budget`]: per-query memory accounting
//! - [`memory`]: the process's memory, and a watch for limits on long operations
//! - [`heap`]: heap profiles by phase, for memory guards and profiles
//! - [`sort`]: sorting and deduplicating id keys by radix over their significant bits
//!
//! Everything here works on ids: it never decodes a term. Operations that need values
//! (numeric comparison, ordering, aggregation) take them through the caller's resolver,
//! so this crate knows nothing about RDF, SPARQL or the dictionary.
//!
//! Two ids are reserved for executors and never stored: [`UNDEF`], an unbound value, and
//! the [`COMPUTED_TAG`] range, values computed during a query (for example a sum) that
//! have no stored id.

pub mod budget;
pub mod classes;
pub mod graph;
pub mod group;
pub mod heap;
pub mod join;
pub mod memory;
pub mod search;
pub mod sort;
pub mod table;
pub mod workers;

pub use budget::{Budget, BudgetExceeded, SharedBudget};
pub use join::RowLimit;
pub use table::IdTable;

/// A hash map keyed by term ids, with the fast hasher the operators use.
pub type IdMap<V> = hashbrown::HashMap<u64, V, foldhash::fast::FixedState>;

/// An unbound value (SPARQL `UNDEF`, from OPTIONAL or VALUES). Tag 15, which the engine never
/// assigns; it sorts after every stored id.
pub const UNDEF: u64 = u64::MAX;

/// The kind tag (top 4 bits) of ids for values computed during a query; the payload indexes
/// the query's own table of computed terms. The engine never assigns tag 14.
pub const COMPUTED_TAG: u64 = 14;

/// The id of the `index`-th computed term of a query.
pub const fn computed_id(index: u64) -> u64 {
    (COMPUTED_TAG << 60) | (index & ((1 << 60) - 1))
}

/// The index of a computed id in its query's table, if `id` is one.
pub const fn computed_index(id: u64) -> Option<u64> {
    if id >> 60 == COMPUTED_TAG {
        Some(id & ((1 << 60) - 1))
    } else {
        None
    }
}
