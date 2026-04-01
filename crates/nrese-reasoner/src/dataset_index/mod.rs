use std::collections::{BTreeSet, HashMap, HashSet};

use nrese_core::DatasetSnapshot;

use crate::symbols::SymbolTable;

mod builder;
mod group_axioms;
mod ids;
mod keys;
mod rdf_list;
mod stats;
#[cfg(test)]
mod tests;

use builder::build_from_snapshot;
use ids::IndexedVocabulary;
pub(crate) use rdf_list::parse_list as parse_rdf_list;
use stats::DatasetIndexStats;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedDataset {
    symbols: SymbolTable,
    asserted_triples: HashSet<(u32, u32, u32)>,
    subclass_edges: HashMap<u32, BTreeSet<u32>>,
    subproperty_edges: HashMap<u32, BTreeSet<u32>>,
    property_chain_axiom_heads: HashMap<u32, BTreeSet<u32>>,
    members_heads_by_subject: HashMap<u32, BTreeSet<u32>>,
    distinct_members_heads_by_subject: HashMap<u32, BTreeSet<u32>>,
    list_first_by_node: HashMap<u32, BTreeSet<u32>>,
    list_rest_by_node: HashMap<u32, BTreeSet<u32>>,
    type_assertions: HashMap<u32, BTreeSet<u32>>,
    domain_by_property: HashMap<u32, BTreeSet<u32>>,
    range_by_property: HashMap<u32, BTreeSet<u32>>,
    property_assertions: Vec<(u32, u32, u32)>,
    same_as_pairs: Vec<(u32, u32)>,
    different_from_pairs: BTreeSet<(u32, u32)>,
    observed_named_resources: BTreeSet<u32>,
    disjoint_class_pairs: HashMap<u32, BTreeSet<u32>>,
    property_disjoint_pairs: HashMap<u32, BTreeSet<u32>>,
    inverse_of_pairs: HashMap<u32, BTreeSet<u32>>,
    functional_properties: BTreeSet<u32>,
    inverse_functional_properties: BTreeSet<u32>,
    irreflexive_properties: BTreeSet<u32>,
    asymmetric_properties: BTreeSet<u32>,
    reflexive_properties: BTreeSet<u32>,
    symmetric_properties: BTreeSet<u32>,
    transitive_properties: BTreeSet<u32>,
    group_axiom_diagnostics: Vec<String>,
    schema_cache_key: u64,
    vocabulary: IndexedVocabulary,
    stats: DatasetIndexStats,
}

impl IndexedDataset {
    pub fn from_snapshot<'a, S>(snapshot: &'a S) -> Self
    where
        S: DatasetSnapshot<'a>,
    {
        build_from_snapshot(snapshot)
    }

    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    pub fn asserted_triples(&self) -> &HashSet<(u32, u32, u32)> {
        &self.asserted_triples
    }

    pub fn subclass_edges(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.subclass_edges
    }

    pub fn subproperty_edges(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.subproperty_edges
    }

    pub fn property_chain_axiom_heads(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.property_chain_axiom_heads
    }

    pub fn list_first_by_node(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.list_first_by_node
    }

    pub fn list_rest_by_node(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.list_rest_by_node
    }

    pub fn type_assertions(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.type_assertions
    }

    pub fn domain_by_property(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.domain_by_property
    }

    pub fn range_by_property(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.range_by_property
    }

    pub fn property_assertions(&self) -> &[(u32, u32, u32)] {
        &self.property_assertions
    }

    pub fn same_as_pairs(&self) -> &[(u32, u32)] {
        &self.same_as_pairs
    }

    pub fn different_from_pairs(&self) -> &BTreeSet<(u32, u32)> {
        &self.different_from_pairs
    }

    pub fn observed_named_resources(&self) -> &BTreeSet<u32> {
        &self.observed_named_resources
    }

    pub fn disjoint_class_pairs(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.disjoint_class_pairs
    }

    pub fn property_disjoint_pairs(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.property_disjoint_pairs
    }

    pub fn inverse_of_pairs(&self) -> &HashMap<u32, BTreeSet<u32>> {
        &self.inverse_of_pairs
    }

    pub fn functional_properties(&self) -> &BTreeSet<u32> {
        &self.functional_properties
    }

    pub fn inverse_functional_properties(&self) -> &BTreeSet<u32> {
        &self.inverse_functional_properties
    }

    pub fn irreflexive_properties(&self) -> &BTreeSet<u32> {
        &self.irreflexive_properties
    }

    pub fn asymmetric_properties(&self) -> &BTreeSet<u32> {
        &self.asymmetric_properties
    }

    pub fn reflexive_properties(&self) -> &BTreeSet<u32> {
        &self.reflexive_properties
    }

    pub fn symmetric_properties(&self) -> &BTreeSet<u32> {
        &self.symmetric_properties
    }

    pub fn transitive_properties(&self) -> &BTreeSet<u32> {
        &self.transitive_properties
    }

    pub fn group_axiom_diagnostics(&self) -> &[String] {
        &self.group_axiom_diagnostics
    }

    pub fn schema_cache_key(&self) -> u64 {
        self.schema_cache_key
    }

    pub fn rdf_type_id(&self) -> u32 {
        self.vocabulary.rdf_type_id
    }

    pub fn rdf_nil_id(&self) -> u32 {
        self.vocabulary.rdf_nil_id
    }

    pub fn rdfs_subclass_of_id(&self) -> u32 {
        self.vocabulary.rdfs_subclass_of_id
    }

    pub fn rdfs_subproperty_of_id(&self) -> u32 {
        self.vocabulary.rdfs_subproperty_of_id
    }

    pub fn owl_same_as_id(&self) -> u32 {
        self.vocabulary.owl_same_as_id
    }

    pub fn owl_nothing_id(&self) -> u32 {
        self.vocabulary.owl_nothing_id
    }

    pub fn supported_asserted_triples(&self) -> u64 {
        self.stats.supported_asserted_triples
    }

    pub fn unsupported_asserted_triples(&self) -> u64 {
        self.stats.unsupported_asserted_triples
    }

    pub fn subclass_edge_count(&self) -> usize {
        self.stats.subclass_edge_count
    }

    pub fn subproperty_edge_count(&self) -> usize {
        self.stats.subproperty_edge_count
    }

    pub fn type_assertion_count(&self) -> usize {
        self.stats.type_assertion_count
    }

    pub fn property_assertion_count(&self) -> usize {
        self.stats.property_assertion_count
    }

    pub fn same_as_assertion_count(&self) -> usize {
        self.stats.same_as_assertion_count
    }

    pub fn domain_assertion_count(&self) -> usize {
        self.stats.domain_assertion_count
    }

    pub fn range_assertion_count(&self) -> usize {
        self.stats.range_assertion_count
    }
}
