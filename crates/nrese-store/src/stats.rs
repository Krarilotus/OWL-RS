use oxigraph::store::Store;

use crate::error::StoreResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreStats {
    pub quad_count: usize,
    pub named_graph_count: usize,
    pub is_empty: bool,
}

pub fn collect_stats(store: &Store) -> StoreResult<StoreStats> {
    let quad_count = store.len()?;
    let is_empty = store.is_empty()?;
    let mut named_graph_count = 0usize;

    for graph in store.named_graphs() {
        graph?;
        named_graph_count += 1;
    }

    Ok(StoreStats {
        quad_count,
        named_graph_count,
        is_empty,
    })
}
