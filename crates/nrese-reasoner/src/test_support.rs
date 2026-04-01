use std::hash::{Hash, Hasher};

use nrese_core::{DatasetSnapshot, IriRef, TripleRef, TripleSource};

use crate::dataset_index::IndexedDataset;
use crate::effective_types::EffectiveTypeSet;
use crate::identity::EqualityIndex;
use crate::property_chain::PropertyChainPlan;
use crate::property_closure::PropertyClosure;
use crate::property_taxonomy::PropertyTaxonomyIndex;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;
use crate::taxonomy::TaxonomyIndex;

#[derive(Debug, Clone)]
pub struct OwnedSnapshot {
    revision: u64,
    cache_key: u64,
    triples: Vec<(String, String, String)>,
    unsupported: u64,
}

impl OwnedSnapshot {
    pub fn empty_with_revision(revision: u64) -> Self {
        Self {
            revision,
            cache_key: compute_snapshot_cache_key(&[], 0),
            triples: Vec::new(),
            unsupported: 0,
        }
    }

    pub fn new(triples: Vec<(&str, &str, &str)>) -> Self {
        Self::with_revision_and_unsupported(1, triples, 0)
    }

    pub fn with_revision_and_unsupported(
        revision: u64,
        triples: Vec<(&str, &str, &str)>,
        unsupported: u64,
    ) -> Self {
        let triples = triples
            .into_iter()
            .map(|(s, p, o)| (s.to_owned(), p.to_owned(), o.to_owned()))
            .collect::<Vec<_>>();

        Self {
            revision,
            cache_key: compute_snapshot_cache_key(&triples, unsupported),
            triples,
            unsupported,
        }
    }
}

pub fn default_rules_mvp_policy() -> RulesMvpFeaturePolicy {
    RulesMvpFeaturePolicy::industry_default()
}

pub fn build_property_closure(index: &IndexedDataset) -> PropertyClosure {
    let property_taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let equality = EqualityIndex::build(index);
    let property_chain_plan = PropertyChainPlan::build(index);
    PropertyClosure::build(
        index,
        &property_taxonomy,
        &equality,
        &property_chain_plan,
        &default_rules_mvp_policy(),
    )
}

pub fn build_effective_types(index: &IndexedDataset) -> EffectiveTypeSet {
    let class_taxonomy = TaxonomyIndex::from_edges(index.subclass_edges());
    let property_closure = build_property_closure(index);
    let equality = EqualityIndex::build(index);
    EffectiveTypeSet::build(index, &class_taxonomy, &property_closure, &equality, true)
}

pub struct OwnedSnapshotIter<'a> {
    inner: std::slice::Iter<'a, (String, String, String)>,
}

impl<'a> Iterator for OwnedSnapshotIter<'a> {
    type Item = TripleRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(s, p, o)| {
            TripleRef::new(
                IriRef::new(s.as_str()).expect("subject"),
                IriRef::new(p.as_str()).expect("predicate"),
                IriRef::new(o.as_str()).expect("object"),
            )
        })
    }
}

impl<'a> TripleSource<'a> for OwnedSnapshot {
    type Iter = OwnedSnapshotIter<'a>;

    fn triples(&'a self) -> Self::Iter {
        OwnedSnapshotIter {
            inner: self.triples.iter(),
        }
    }
}

impl<'a> DatasetSnapshot<'a> for OwnedSnapshot {
    fn revision(&self) -> u64 {
        self.revision
    }

    fn cache_key(&self) -> Option<u64> {
        Some(self.cache_key)
    }

    fn unsupported_triple_count(&self) -> u64 {
        self.unsupported
    }
}

fn compute_snapshot_cache_key(triples: &[(String, String, String)], unsupported: u64) -> u64 {
    let mut ordered = triples.to_vec();
    ordered.sort();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    unsupported.hash(&mut hasher);
    ordered.len().hash(&mut hasher);
    for (subject, predicate, object) in ordered {
        subject.hash(&mut hasher);
        predicate.hash(&mut hasher);
        object.hash(&mut hasher);
    }
    hasher.finish()
}
