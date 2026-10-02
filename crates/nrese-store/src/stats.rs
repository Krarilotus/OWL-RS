use nrese_engine::{ReadModel, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreStats {
    /// Asserted quads.
    pub quad_count: usize,
    /// Inferred quads (default graph; disjoint from the asserted ones).
    pub inferred_count: usize,
    pub named_graph_count: usize,
    pub is_empty: bool,
}

/// O(1) for the counts plus O(g log n) for listing g named graphs.
pub fn collect_stats(snapshot: &Snapshot) -> StoreStats {
    StoreStats {
        quad_count: snapshot.len_in(ReadModel::Asserted) as usize,
        inferred_count: snapshot.len_in(ReadModel::Inferred) as usize,
        named_graph_count: snapshot.named_graphs().count(),
        is_empty: snapshot.is_empty(),
    }
}
