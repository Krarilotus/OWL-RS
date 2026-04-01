use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

use crate::symbols::SymbolTable;

pub(super) struct SchemaKeyInput<'a> {
    pub(super) symbols: &'a SymbolTable,
    pub(super) subclass_edges: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) subproperty_edges: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) property_chain_axiom_heads: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) list_first_by_node: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) list_rest_by_node: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) domain_by_property: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) range_by_property: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) disjoint_class_pairs: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) property_disjoint_pairs: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) inverse_of_pairs: &'a HashMap<u32, BTreeSet<u32>>,
    pub(super) functional_properties: &'a BTreeSet<u32>,
    pub(super) inverse_functional_properties: &'a BTreeSet<u32>,
    pub(super) irreflexive_properties: &'a BTreeSet<u32>,
    pub(super) asymmetric_properties: &'a BTreeSet<u32>,
    pub(super) reflexive_properties: &'a BTreeSet<u32>,
    pub(super) symmetric_properties: &'a BTreeSet<u32>,
    pub(super) transitive_properties: &'a BTreeSet<u32>,
}

pub(super) fn compute_schema_cache_key(input: SchemaKeyInput<'_>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();

    hash_edge_map(input.symbols, input.subclass_edges, &mut hasher);
    hash_edge_map(input.symbols, input.subproperty_edges, &mut hasher);
    hash_edge_map(input.symbols, input.property_chain_axiom_heads, &mut hasher);
    hash_edge_map(input.symbols, input.list_first_by_node, &mut hasher);
    hash_edge_map(input.symbols, input.list_rest_by_node, &mut hasher);
    hash_edge_map(input.symbols, input.domain_by_property, &mut hasher);
    hash_edge_map(input.symbols, input.range_by_property, &mut hasher);
    hash_edge_map(input.symbols, input.disjoint_class_pairs, &mut hasher);
    hash_edge_map(input.symbols, input.property_disjoint_pairs, &mut hasher);
    hash_edge_map(input.symbols, input.inverse_of_pairs, &mut hasher);
    hash_set(input.symbols, input.functional_properties, &mut hasher);
    hash_set(
        input.symbols,
        input.inverse_functional_properties,
        &mut hasher,
    );
    hash_set(input.symbols, input.irreflexive_properties, &mut hasher);
    hash_set(input.symbols, input.asymmetric_properties, &mut hasher);
    hash_set(input.symbols, input.reflexive_properties, &mut hasher);
    hash_set(input.symbols, input.symmetric_properties, &mut hasher);
    hash_set(input.symbols, input.transitive_properties, &mut hasher);

    hasher.finish()
}

fn hash_edge_map(
    symbols: &SymbolTable,
    edges: &HashMap<u32, BTreeSet<u32>>,
    hasher: &mut impl Hasher,
) {
    let mut ordered = edges
        .iter()
        .map(|(key, values)| {
            (
                resolve_or_unknown(symbols, *key).to_owned(),
                values
                    .iter()
                    .map(|value| resolve_or_unknown(symbols, *value).to_owned())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.0.cmp(&right.0));

    ordered.len().hash(hasher);
    for (key, mut values) in ordered {
        key.hash(hasher);
        values.sort();
        values.len().hash(hasher);
        for value in values {
            value.hash(hasher);
        }
    }
}

fn hash_set(symbols: &SymbolTable, values: &BTreeSet<u32>, hasher: &mut impl Hasher) {
    values.len().hash(hasher);
    for value in values {
        resolve_or_unknown(symbols, *value).hash(hasher);
    }
}

fn resolve_or_unknown(symbols: &SymbolTable, id: u32) -> &str {
    symbols.resolve(id).unwrap_or("<unknown>")
}
