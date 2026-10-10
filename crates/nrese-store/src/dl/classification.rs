//! Classification and realisation of the store's asserted ontology by the DL engines
//! (docs/design/owl2-dl.md §7; `nrese_dl::classify`): the context core where its Horn
//! stage takes the ontology, else the hypertableau driver (known and possible subsumers,
//! parallel tests). Each result says whether it is complete; what it contains is entailed.
//!
//! Complete results are kept per revision. An incomplete explicit request is retried
//! on the next call under the configured budget, including unsupported ontologies.
//! The query lower bound owns its separate schema-lifetime cache (`bounds.rs`).
//! The context core receives the store's explicit process-memory policy, including zero
//! to disable its watch; opening another store cannot change that policy.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use nrese_dl::classify::{self, Realisation, Taxonomy};
use nrese_engine::Snapshot;

use super::source;
use crate::StoreService;

/// The latest complete results, by revision.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    taxonomy: Mutex<Option<(u64, Arc<Taxonomy>)>>,
    realisation: Mutex<Option<(u64, Arc<Realisation>)>>,
}

/// How the engines run under the store's settings.
pub(super) fn options(store: &StoreService) -> classify::Options {
    let config = &store.config().dl;
    let workers = store.runtime().workers().limited(config.threads);
    let mut options = config.classification_options(workers);
    options.max_memory = Some(store.config().process_memory_bytes);
    options
}

/// The taxonomy of `snapshot`'s asserted ontology (from the cache if it is current).
pub(crate) fn taxonomy(store: &StoreService, snapshot: &Snapshot) -> Arc<Taxonomy> {
    let revision = snapshot.revision();
    let cache = &store.dl().classification;
    if let Some((at, taxonomy)) = &*cache.taxonomy.lock().unwrap_or_else(|p| p.into_inner())
        && *at == revision
    {
        return Arc::clone(taxonomy);
    }
    if let Some((at, realisation)) = &*cache.realisation.lock().unwrap_or_else(|p| p.into_inner())
        && *at == revision
    {
        return Arc::new(realisation.taxonomy.clone());
    }
    let started = Instant::now();
    let ontology = source::read_snapshot(snapshot);
    let mut options = options(store);
    let workers = options.workers.as_ref().unwrap().clone();
    let taxonomy = Arc::new(workers.install(|| {
        options.timeout = options
            .timeout
            .map(|limit| limit.saturating_sub(started.elapsed()));
        classify::classify(&ontology, &options)
    }));
    if taxonomy.complete() {
        remember(&cache.taxonomy, revision, &taxonomy);
    }
    taxonomy
}

/// The realisation of `snapshot`'s asserted ontology (from the cache if it is current).
pub(crate) fn realisation(store: &StoreService, snapshot: &Snapshot) -> Arc<Realisation> {
    let revision = snapshot.revision();
    let cache = &store.dl().classification;
    if let Some((at, realisation)) = &*cache.realisation.lock().unwrap_or_else(|p| p.into_inner())
        && *at == revision
    {
        return Arc::clone(realisation);
    }
    let started = Instant::now();
    let ontology = source::read_snapshot(snapshot);
    let mut options = options(store);
    let workers = options.workers.as_ref().unwrap().clone();
    let realisation = Arc::new(workers.install(|| {
        options.timeout = options
            .timeout
            .map(|limit| limit.saturating_sub(started.elapsed()));
        classify::realise(&ontology, &options)
    }));
    if realisation.incomplete.is_empty() && realisation.taxonomy.complete() {
        remember(&cache.realisation, revision, &realisation);
    }
    realisation
}

/// Publish a complete result without letting an older in-flight request evict a newer
/// revision. Same-revision races keep the first complete result. No solver runs locked.
fn remember<T>(cache: &Mutex<Option<(u64, Arc<T>)>>, revision: u64, result: &Arc<T>) {
    let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
    if cache.as_ref().is_none_or(|(at, _)| *at < revision) {
        *cache = Some((revision, Arc::clone(result)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DlConfig, StoreConfig};
    use std::time::Duration;

    fn store(timeout: Duration) -> StoreService {
        let store = StoreService::new(StoreConfig {
            execution_threads: 1,
            process_memory_bytes: 0,
            dl: DlConfig {
                timeout,
                threads: 1,
                ..DlConfig::default()
            },
            ..StoreConfig::in_memory()
        })
        .unwrap();
        store
            .execute_update_str(
                "PREFIX : <urn:cache:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA { :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] .
                 :B rdfs:subClassOf :D . :C rdfs:subClassOf :D . :a a :A . }",
            )
            .unwrap();
        store
    }

    #[test]
    fn incomplete_explicit_results_are_retried() {
        // The public configuration requires a positive timeout. One nanosecond is
        // spent before the first solver entry, so both attempts report incomplete.
        let store = store(Duration::from_nanos(1));
        let snapshot = store.engine().snapshot();
        let first = taxonomy(&store, &snapshot);
        let next = taxonomy(&store, &snapshot);
        assert!(!first.complete());
        assert!(!next.complete());
        assert!(!Arc::ptr_eq(&first, &next), "classification must retry");
        let first = realisation(&store, &snapshot);
        let next = realisation(&store, &snapshot);
        assert!(!first.incomplete.is_empty());
        assert!(!next.incomplete.is_empty());
        assert!(!Arc::ptr_eq(&first, &next), "realisation must retry");
    }

    #[test]
    fn complete_results_are_reused_and_old_requests_do_not_evict_them() {
        let store = store(Duration::from_secs(30));
        let old = store.engine().snapshot();
        store
            .execute_update_str("INSERT DATA { <urn:cache:b> a <urn:cache:A> }")
            .unwrap();
        let current = store.engine().snapshot();
        assert!(old.revision() < current.revision());
        let classes = taxonomy(&store, &current);
        let types = realisation(&store, &current);
        assert!(classes.complete(), "{:?}", classes.incomplete);
        assert!(types.incomplete.is_empty(), "{:?}", types.incomplete);
        // An older request finishing later still receives its own snapshot's answer.
        assert!(taxonomy(&store, &old).complete());
        let old_types = realisation(&store, &old);
        assert!(old_types.incomplete.is_empty());
        assert!(old_types.individuals.len() < types.individuals.len());
        assert!(Arc::ptr_eq(&classes, &taxonomy(&store, &current)));
        assert!(Arc::ptr_eq(&types, &realisation(&store, &current)));
    }
}
