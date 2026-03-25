use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClosureIndex {
    ancestors_by_node: HashMap<u32, BTreeSet<u32>>,
    nodes: BTreeSet<u32>,
    node_count: usize,
}

impl ClosureIndex {
    pub fn from_edges(edges: &HashMap<u32, BTreeSet<u32>>) -> Self {
        let mut memo = HashMap::new();
        let mut all_nodes = BTreeSet::new();

        for (&node_id, parents) in edges {
            all_nodes.insert(node_id);
            all_nodes.extend(parents.iter().copied());
        }

        for node_id in &all_nodes {
            let mut visiting = BTreeSet::new();
            let _ = collect_ancestors(*node_id, edges, &mut memo, &mut visiting);
        }

        Self {
            ancestors_by_node: memo,
            nodes: all_nodes.clone(),
            node_count: all_nodes.len(),
        }
    }

    pub fn ancestors_of(&self, node_id: u32) -> Option<&BTreeSet<u32>> {
        self.ancestors_by_node.get(&node_id)
    }

    pub fn node_count(&self) -> usize {
        self.node_count
    }

    pub fn nodes(&self) -> &BTreeSet<u32> {
        &self.nodes
    }
}

fn collect_ancestors(
    node_id: u32,
    edges: &HashMap<u32, BTreeSet<u32>>,
    memo: &mut HashMap<u32, BTreeSet<u32>>,
    visiting: &mut BTreeSet<u32>,
) -> BTreeSet<u32> {
    if let Some(existing) = memo.get(&node_id) {
        return existing.clone();
    }

    if !visiting.insert(node_id) {
        return BTreeSet::new();
    }

    let mut ancestors = BTreeSet::new();
    if let Some(parents) = edges.get(&node_id) {
        for parent in parents {
            ancestors.insert(*parent);
            ancestors.extend(collect_ancestors(*parent, edges, memo, visiting));
        }
    }

    visiting.remove(&node_id);
    memo.insert(node_id, ancestors.clone());
    ancestors
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use super::ClosureIndex;

    #[test]
    fn closure_index_memoizes_transitive_ancestors() {
        let mut edges = HashMap::new();
        edges.insert(1, BTreeSet::from([2]));
        edges.insert(2, BTreeSet::from([3]));

        let closure = ClosureIndex::from_edges(&edges);
        let ancestors = closure.ancestors_of(1).expect("ancestors");

        assert_eq!(closure.node_count(), 3);
        assert!(ancestors.contains(&2));
        assert!(ancestors.contains(&3));
    }

    #[test]
    fn closure_index_handles_cycles_without_recursing_forever() {
        let mut edges = HashMap::new();
        edges.insert(1, BTreeSet::from([2]));
        edges.insert(2, BTreeSet::from([1]));

        let closure = ClosureIndex::from_edges(&edges);

        let ancestors_one = closure.ancestors_of(1).expect("ancestors for 1");
        let ancestors_two = closure.ancestors_of(2).expect("ancestors for 2");

        assert!(ancestors_one.contains(&2));
        assert!(ancestors_two.contains(&1));
    }
}
