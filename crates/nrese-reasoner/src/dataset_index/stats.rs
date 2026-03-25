use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DatasetIndexStats {
    pub(super) supported_asserted_triples: u64,
    pub(super) unsupported_asserted_triples: u64,
    pub(super) subclass_edge_count: usize,
    pub(super) subproperty_edge_count: usize,
    pub(super) type_assertion_count: usize,
    pub(super) property_assertion_count: usize,
    pub(super) same_as_assertion_count: usize,
    pub(super) domain_assertion_count: usize,
    pub(super) range_assertion_count: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DatasetIndexStatsInput<'a> {
    pub(super) supported_asserted_triples: u64,
    pub(super) unsupported_asserted_triples: u64,
    pub(super) subclass_edges: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) subproperty_edges: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) type_assertions: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) property_assertions: &'a [(u32, u32, u32)],
    pub(super) same_as_pairs: &'a [(u32, u32)],
    pub(super) domain_by_property: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) range_by_property: &'a HashMap<u32, BTreeSet<u32>>,
}

impl DatasetIndexStats {
    pub(super) fn from_input(input: DatasetIndexStatsInput<'_>) -> Self {
        Self {
            supported_asserted_triples: input.supported_asserted_triples,
            unsupported_asserted_triples: input.unsupported_asserted_triples,
            subclass_edge_count: input.subclass_edges.values().map(BTreeSet::len).sum(),
            subproperty_edge_count: input.subproperty_edges.values().map(BTreeSet::len).sum(),
            type_assertion_count: input.type_assertions.values().map(BTreeSet::len).sum(),
            property_assertion_count: input.property_assertions.len(),
            same_as_assertion_count: input.same_as_pairs.len(),
            domain_assertion_count: input.domain_by_property.values().map(BTreeSet::len).sum(),
            range_assertion_count: input.range_by_property.values().map(BTreeSet::len).sum(),
        }
    }
}
