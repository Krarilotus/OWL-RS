use std::collections::{BTreeMap, BTreeSet};

use crate::dataset_index::{IndexedDataset, parse_rdf_list};

const MAX_SUPPORTED_CHAIN_LENGTH: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BinaryPropertyChain {
    pub(crate) super_property_id: u32,
    pub(crate) first_property_id: u32,
    pub(crate) second_property_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PropertyChainPlan {
    by_first_property: BTreeMap<u32, Vec<BinaryPropertyChain>>,
    by_second_property: BTreeMap<u32, Vec<BinaryPropertyChain>>,
    diagnostics: Vec<String>,
}

impl PropertyChainPlan {
    pub(crate) fn build(index: &IndexedDataset) -> Self {
        let mut binary_chains = BTreeSet::new();
        let mut diagnostics = BTreeSet::new();

        for (&super_property_id, head_ids) in index.property_chain_axiom_heads() {
            for &head_id in head_ids {
                match parse_chain(index, head_id) {
                    Ok(chain) if chain.len() == MAX_SUPPORTED_CHAIN_LENGTH => {
                        binary_chains.insert(BinaryPropertyChain {
                            super_property_id,
                            first_property_id: chain[0],
                            second_property_id: chain[1],
                        });
                    }
                    Ok(chain) => {
                        diagnostics.insert(format!(
                            "owl:propertyChainAxiom beyond binary chains is not implemented in rules-mvp; ignored chain of length {} for <{}>",
                            chain.len(),
                            resolve_or_unknown(index, super_property_id)
                        ));
                    }
                    Err(reason) => {
                        diagnostics.insert(format!(
                            "owl:propertyChainAxiom over malformed named-node RDF lists is not implemented in rules-mvp; ignored chain for <{}> ({reason})",
                            resolve_or_unknown(index, super_property_id)
                        ));
                    }
                }
            }
        }

        let mut by_first_property: BTreeMap<u32, Vec<BinaryPropertyChain>> = BTreeMap::new();
        let mut by_second_property: BTreeMap<u32, Vec<BinaryPropertyChain>> = BTreeMap::new();
        for chain in binary_chains {
            by_first_property
                .entry(chain.first_property_id)
                .or_default()
                .push(chain);
            by_second_property
                .entry(chain.second_property_id)
                .or_default()
                .push(chain);
        }

        Self {
            by_first_property,
            by_second_property,
            diagnostics: diagnostics.into_iter().collect(),
        }
    }

    pub(crate) fn chains_starting_with(&self, property_id: u32) -> Option<&[BinaryPropertyChain]> {
        self.by_first_property.get(&property_id).map(Vec::as_slice)
    }

    pub(crate) fn chains_ending_with(&self, property_id: u32) -> Option<&[BinaryPropertyChain]> {
        self.by_second_property.get(&property_id).map(Vec::as_slice)
    }

    pub(crate) fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    #[cfg(test)]
    pub(crate) fn chain_count(&self) -> usize {
        self.by_first_property.values().map(Vec::len).sum()
    }
}

fn parse_chain(index: &IndexedDataset, head_id: u32) -> Result<Vec<u32>, &'static str> {
    parse_rdf_list(
        index.list_first_by_node(),
        index.list_rest_by_node(),
        index.rdf_nil_id(),
        head_id,
    )
}

fn resolve_or_unknown(index: &IndexedDataset, term_id: u32) -> &str {
    index.symbols().resolve(term_id).unwrap_or("<unknown>")
}

#[cfg(test)]
#[path = "tests/property_chain_tests.rs"]
mod tests;
