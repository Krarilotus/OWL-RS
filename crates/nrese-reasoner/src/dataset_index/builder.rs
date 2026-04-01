use std::collections::{BTreeSet, HashMap, HashSet};

use nrese_core::DatasetSnapshot;

use crate::symbols::SymbolTable;

use super::IndexedDataset;
use super::group_axioms::{GroupAxiomExpansion, expand_group_axioms};
use super::ids::IndexedVocabulary;
use super::keys::{SchemaKeyInput, compute_schema_cache_key};
use super::stats::{DatasetIndexStats, DatasetIndexStatsInput};

pub(super) fn build_from_snapshot<'a, S>(snapshot: &'a S) -> IndexedDataset
where
    S: DatasetSnapshot<'a>,
{
    let mut accumulator = DatasetIndexAccumulator::new();

    for triple in snapshot.triples() {
        accumulator.ingest_triple(
            triple.subject.as_str(),
            triple.predicate.as_str(),
            triple.object.as_str(),
        );
    }

    accumulator.finish(snapshot.unsupported_triple_count())
}

#[derive(Debug)]
struct DatasetIndexAccumulator {
    symbols: SymbolTable,
    vocabulary: IndexedVocabulary,
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
    supported_asserted_triples: u64,
}

impl DatasetIndexAccumulator {
    fn new() -> Self {
        let mut symbols = SymbolTable::default();
        let vocabulary = IndexedVocabulary::new(&mut symbols);

        Self {
            symbols,
            vocabulary,
            asserted_triples: HashSet::new(),
            subclass_edges: HashMap::new(),
            subproperty_edges: HashMap::new(),
            property_chain_axiom_heads: HashMap::new(),
            members_heads_by_subject: HashMap::new(),
            distinct_members_heads_by_subject: HashMap::new(),
            list_first_by_node: HashMap::new(),
            list_rest_by_node: HashMap::new(),
            type_assertions: HashMap::new(),
            domain_by_property: HashMap::new(),
            range_by_property: HashMap::new(),
            property_assertions: Vec::new(),
            same_as_pairs: Vec::new(),
            different_from_pairs: BTreeSet::new(),
            observed_named_resources: BTreeSet::new(),
            disjoint_class_pairs: HashMap::new(),
            property_disjoint_pairs: HashMap::new(),
            inverse_of_pairs: HashMap::new(),
            functional_properties: BTreeSet::new(),
            inverse_functional_properties: BTreeSet::new(),
            irreflexive_properties: BTreeSet::new(),
            asymmetric_properties: BTreeSet::new(),
            reflexive_properties: BTreeSet::new(),
            symmetric_properties: BTreeSet::new(),
            transitive_properties: BTreeSet::new(),
            group_axiom_diagnostics: Vec::new(),
            supported_asserted_triples: 0,
        }
    }

    fn ingest_triple(&mut self, subject: &str, predicate: &str, object: &str) {
        self.supported_asserted_triples += 1;

        let subject_id = self.symbols.get_or_intern(subject);
        let predicate_id = self.symbols.get_or_intern(predicate);
        let object_id = self.symbols.get_or_intern(object);

        self.asserted_triples
            .insert((subject_id, predicate_id, object_id));

        if predicate_id == self.vocabulary.rdfs_subclass_of_id {
            self.subclass_edges
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.owl_equivalent_class_id {
            self.subclass_edges
                .entry(subject_id)
                .or_default()
                .insert(object_id);
            self.subclass_edges
                .entry(object_id)
                .or_default()
                .insert(subject_id);
        } else if predicate_id == self.vocabulary.rdfs_subproperty_of_id {
            self.subproperty_edges
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.owl_property_chain_axiom_id {
            self.property_chain_axiom_heads
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.owl_members_id {
            self.members_heads_by_subject
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.owl_distinct_members_id {
            self.distinct_members_heads_by_subject
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.rdf_first_id {
            self.list_first_by_node
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.rdf_rest_id {
            self.list_rest_by_node
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.owl_equivalent_property_id {
            self.subproperty_edges
                .entry(subject_id)
                .or_default()
                .insert(object_id);
            self.subproperty_edges
                .entry(object_id)
                .or_default()
                .insert(subject_id);
        } else if predicate_id == self.vocabulary.rdf_type_id {
            self.ingest_type_assertion(subject_id, object_id);
        } else if predicate_id == self.vocabulary.rdfs_domain_id {
            self.domain_by_property
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.rdfs_range_id {
            self.range_by_property
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        } else if predicate_id == self.vocabulary.owl_disjoint_with_id {
            insert_symmetric_edge(&mut self.disjoint_class_pairs, subject_id, object_id);
        } else if predicate_id == self.vocabulary.owl_property_disjoint_with_id {
            insert_symmetric_edge(&mut self.property_disjoint_pairs, subject_id, object_id);
        } else if predicate_id == self.vocabulary.owl_inverse_of_id {
            insert_symmetric_edge(&mut self.inverse_of_pairs, subject_id, object_id);
        } else if predicate_id == self.vocabulary.owl_same_as_id {
            self.observed_named_resources.insert(subject_id);
            self.observed_named_resources.insert(object_id);
            self.same_as_pairs.push((subject_id, object_id));
        } else if predicate_id == self.vocabulary.owl_different_from_id {
            self.observed_named_resources.insert(subject_id);
            self.observed_named_resources.insert(object_id);
            self.different_from_pairs
                .insert(ordered_pair(subject_id, object_id));
        } else {
            self.observed_named_resources.insert(subject_id);
            self.observed_named_resources.insert(object_id);
            self.property_assertions
                .push((subject_id, predicate_id, object_id));
        }
    }

    fn ingest_type_assertion(&mut self, subject_id: u32, object_id: u32) {
        if object_id == self.vocabulary.owl_functional_property_id {
            self.functional_properties.insert(subject_id);
        } else if object_id == self.vocabulary.owl_inverse_functional_property_id {
            self.inverse_functional_properties.insert(subject_id);
        } else if object_id == self.vocabulary.owl_irreflexive_property_id {
            self.irreflexive_properties.insert(subject_id);
        } else if object_id == self.vocabulary.owl_asymmetric_property_id {
            self.asymmetric_properties.insert(subject_id);
        } else if object_id == self.vocabulary.owl_reflexive_property_id {
            self.reflexive_properties.insert(subject_id);
        } else if object_id == self.vocabulary.owl_symmetric_property_id {
            self.symmetric_properties.insert(subject_id);
        } else if object_id == self.vocabulary.owl_transitive_property_id {
            self.transitive_properties.insert(subject_id);
        } else {
            self.observed_named_resources.insert(subject_id);
            self.type_assertions
                .entry(subject_id)
                .or_default()
                .insert(object_id);
        }
    }

    fn finish(self, unsupported_asserted_triples: u64) -> IndexedDataset {
        let mut this = self;
        expand_group_axioms(GroupAxiomExpansion {
            symbols: &this.symbols,
            vocabulary: &this.vocabulary,
            type_assertions: &this.type_assertions,
            list_first_by_node: &this.list_first_by_node,
            list_rest_by_node: &this.list_rest_by_node,
            members_heads_by_subject: &this.members_heads_by_subject,
            distinct_members_heads_by_subject: &this.distinct_members_heads_by_subject,
            different_from_pairs: &mut this.different_from_pairs,
            disjoint_class_pairs: &mut this.disjoint_class_pairs,
            property_disjoint_pairs: &mut this.property_disjoint_pairs,
            diagnostics: &mut this.group_axiom_diagnostics,
        });

        let stats = DatasetIndexStats::from_input(DatasetIndexStatsInput {
            supported_asserted_triples: this.supported_asserted_triples,
            unsupported_asserted_triples,
            subclass_edges: &this.subclass_edges,
            subproperty_edges: &this.subproperty_edges,
            type_assertions: &this.type_assertions,
            property_assertions: &this.property_assertions,
            same_as_pairs: &this.same_as_pairs,
            domain_by_property: &this.domain_by_property,
            range_by_property: &this.range_by_property,
        });
        let schema_cache_key = compute_schema_cache_key(SchemaKeyInput {
            symbols: &this.symbols,
            subclass_edges: &this.subclass_edges,
            subproperty_edges: &this.subproperty_edges,
            property_chain_axiom_heads: &this.property_chain_axiom_heads,
            list_first_by_node: &this.list_first_by_node,
            list_rest_by_node: &this.list_rest_by_node,
            domain_by_property: &this.domain_by_property,
            range_by_property: &this.range_by_property,
            disjoint_class_pairs: &this.disjoint_class_pairs,
            property_disjoint_pairs: &this.property_disjoint_pairs,
            inverse_of_pairs: &this.inverse_of_pairs,
            functional_properties: &this.functional_properties,
            inverse_functional_properties: &this.inverse_functional_properties,
            irreflexive_properties: &this.irreflexive_properties,
            asymmetric_properties: &this.asymmetric_properties,
            reflexive_properties: &this.reflexive_properties,
            symmetric_properties: &this.symmetric_properties,
            transitive_properties: &this.transitive_properties,
        });

        IndexedDataset {
            symbols: this.symbols,
            asserted_triples: this.asserted_triples,
            subclass_edges: this.subclass_edges,
            subproperty_edges: this.subproperty_edges,
            property_chain_axiom_heads: this.property_chain_axiom_heads,
            members_heads_by_subject: this.members_heads_by_subject,
            distinct_members_heads_by_subject: this.distinct_members_heads_by_subject,
            list_first_by_node: this.list_first_by_node,
            list_rest_by_node: this.list_rest_by_node,
            type_assertions: this.type_assertions,
            domain_by_property: this.domain_by_property,
            range_by_property: this.range_by_property,
            property_assertions: this.property_assertions,
            same_as_pairs: this.same_as_pairs,
            different_from_pairs: this.different_from_pairs,
            observed_named_resources: this.observed_named_resources,
            disjoint_class_pairs: this.disjoint_class_pairs,
            property_disjoint_pairs: this.property_disjoint_pairs,
            inverse_of_pairs: this.inverse_of_pairs,
            functional_properties: this.functional_properties,
            inverse_functional_properties: this.inverse_functional_properties,
            irreflexive_properties: this.irreflexive_properties,
            asymmetric_properties: this.asymmetric_properties,
            reflexive_properties: this.reflexive_properties,
            symmetric_properties: this.symmetric_properties,
            transitive_properties: this.transitive_properties,
            group_axiom_diagnostics: this.group_axiom_diagnostics,
            schema_cache_key,
            vocabulary: this.vocabulary,
            stats,
        }
    }
}

fn ordered_pair(left: u32, right: u32) -> (u32, u32) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn insert_symmetric_edge(edges: &mut HashMap<u32, BTreeSet<u32>>, left_id: u32, right_id: u32) {
    edges.entry(left_id).or_default().insert(right_id);
    edges.entry(right_id).or_default().insert(left_id);
}
