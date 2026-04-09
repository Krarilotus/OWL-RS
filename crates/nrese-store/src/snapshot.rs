use std::hash::{Hash, Hasher};

use nrese_core::{DatasetSnapshot, IriRef, SnapshotCoverageStats, TripleRef, TripleSource};
use oxigraph::model::{GraphNameRef, NamedOrBlankNodeRef, TermRef};
use oxigraph::store::Store;

use crate::error::StoreResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTriple {
    subject: String,
    predicate: String,
    object: String,
}

impl SnapshotTriple {
    fn as_triple_ref(&self) -> TripleRef<'_> {
        let subject = IriRef::new(self.subject.as_str()).expect("validated snapshot subject");
        let predicate = IriRef::new(self.predicate.as_str()).expect("validated snapshot predicate");
        let object = IriRef::new(self.object.as_str()).expect("validated snapshot object");

        TripleRef::new(subject, predicate, object)
    }
}

impl Ord for SnapshotTriple {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.subject, &self.predicate, &self.object).cmp(&(
            &other.subject,
            &other.predicate,
            &other.object,
        ))
    }
}

impl PartialOrd for SnapshotTriple {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDatasetSnapshot {
    revision: u64,
    cache_key: u64,
    asserted_triple_count: u64,
    coverage: SnapshotCoverageStats,
    triples: Vec<SnapshotTriple>,
}

impl StoreDatasetSnapshot {
    pub fn capture(store: &Store, revision: u64) -> StoreResult<Self> {
        let mut triples = Vec::new();
        let mut asserted_triple_count = 0u64;
        let mut coverage = SnapshotCoverageStats::default();

        for quad in store.quads_for_pattern(None, None, None, None) {
            let quad = quad?;
            asserted_triple_count += 1;
            if !matches!(quad.graph_name.as_ref(), GraphNameRef::DefaultGraph) {
                coverage.flattened_named_graph_quads += 1;
            }

            let subject = match quad.subject.as_ref() {
                NamedOrBlankNodeRef::NamedNode(node) => node.as_str().to_owned(),
                _ => {
                    coverage.unsupported_triples += 1;
                    coverage.unsupported_blank_node_subjects += 1;
                    continue;
                }
            };
            let predicate = quad.predicate.as_str().to_owned();
            let object = match quad.object.as_ref() {
                TermRef::NamedNode(node) => node.as_str().to_owned(),
                TermRef::BlankNode(_) => {
                    coverage.unsupported_triples += 1;
                    coverage.unsupported_blank_node_objects += 1;
                    continue;
                }
                _ => {
                    coverage.unsupported_triples += 1;
                    coverage.unsupported_literal_objects += 1;
                    continue;
                }
            };

            triples.push(SnapshotTriple {
                subject,
                predicate,
                object,
            });
            coverage.supported_triples += 1;
        }

        triples.sort();
        let cache_key = compute_snapshot_cache_key(asserted_triple_count, &coverage, &triples);

        Ok(Self {
            revision,
            cache_key,
            asserted_triple_count,
            coverage,
            triples,
        })
    }
}

pub struct SnapshotTripleIter<'a> {
    inner: std::slice::Iter<'a, SnapshotTriple>,
}

impl<'a> Iterator for SnapshotTripleIter<'a> {
    type Item = TripleRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(SnapshotTriple::as_triple_ref)
    }
}

impl<'a> TripleSource<'a> for StoreDatasetSnapshot {
    type Iter = SnapshotTripleIter<'a>;

    fn triples(&'a self) -> Self::Iter {
        SnapshotTripleIter {
            inner: self.triples.iter(),
        }
    }
}

impl<'a> DatasetSnapshot<'a> for StoreDatasetSnapshot {
    fn revision(&self) -> u64 {
        self.revision
    }

    fn cache_key(&self) -> Option<u64> {
        Some(self.cache_key)
    }

    fn asserted_triple_count(&self) -> u64 {
        self.asserted_triple_count
    }

    fn unsupported_triple_count(&self) -> u64 {
        self.coverage.unsupported_triples
    }

    fn coverage_stats(&self) -> SnapshotCoverageStats {
        self.coverage
    }
}

fn compute_snapshot_cache_key(
    asserted_triple_count: u64,
    coverage: &SnapshotCoverageStats,
    triples: &[SnapshotTriple],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    asserted_triple_count.hash(&mut hasher);
    coverage.supported_triples.hash(&mut hasher);
    coverage.unsupported_triples.hash(&mut hasher);
    coverage.unsupported_blank_node_subjects.hash(&mut hasher);
    coverage.unsupported_blank_node_objects.hash(&mut hasher);
    coverage.unsupported_literal_objects.hash(&mut hasher);
    coverage.flattened_named_graph_quads.hash(&mut hasher);
    triples.len().hash(&mut hasher);
    for triple in triples {
        triple.subject.hash(&mut hasher);
        triple.predicate.hash(&mut hasher);
        triple.object.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use oxigraph::sparql::SparqlEvaluator;
    use oxigraph::store::Store;

    use super::StoreDatasetSnapshot;
    use nrese_core::DatasetSnapshot;

    #[test]
    fn snapshot_collects_supported_iri_triples_and_counts_skipped_terms() {
        let store = Store::new().expect("store");
        SparqlEvaluator::new()
            .parse_update(
                "INSERT DATA {
                    <http://example.com/s> <http://example.com/p> <http://example.com/o> .
                    <http://example.com/s> <http://example.com/p2> \"literal\" .
                }",
            )
            .expect("update parse")
            .on_store(&store)
            .execute()
            .expect("update execute");

        let snapshot = StoreDatasetSnapshot::capture(&store, 3).expect("snapshot");
        let triples: Vec<_> = nrese_core::TripleSource::triples(&snapshot).collect();
        let coverage = snapshot.coverage_stats();

        assert_eq!(snapshot.revision(), 3);
        assert_eq!(snapshot.asserted_triple_count(), 2);
        assert_eq!(snapshot.unsupported_triple_count(), 1);
        assert_eq!(coverage.supported_triples, 1);
        assert_eq!(coverage.unsupported_literal_objects, 1);
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0].subject.as_str(), "http://example.com/s");
        assert!(snapshot.cache_key().is_some());
    }

    #[test]
    fn snapshot_cache_key_is_stable_for_equivalent_dataset_state() {
        let first = Store::new().expect("first store");
        let second = Store::new().expect("second store");

        SparqlEvaluator::new()
            .parse_update(
                "INSERT DATA {
                    <http://example.com/a> <http://example.com/p> <http://example.com/b> .
                    <http://example.com/c> <http://example.com/p> <http://example.com/d> .
                }",
            )
            .expect("first update parse")
            .on_store(&first)
            .execute()
            .expect("first update execute");
        SparqlEvaluator::new()
            .parse_update(
                "INSERT DATA {
                    <http://example.com/c> <http://example.com/p> <http://example.com/d> .
                    <http://example.com/a> <http://example.com/p> <http://example.com/b> .
                }",
            )
            .expect("second update parse")
            .on_store(&second)
            .execute()
            .expect("second update execute");

        let first_snapshot = StoreDatasetSnapshot::capture(&first, 1).expect("first snapshot");
        let second_snapshot = StoreDatasetSnapshot::capture(&second, 7).expect("second snapshot");

        assert_eq!(first_snapshot.cache_key(), second_snapshot.cache_key());
    }
}
