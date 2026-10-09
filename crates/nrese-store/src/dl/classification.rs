//! Classification and realisation of the store's asserted ontology by the DL engines
//! (docs/design/owl2-dl.md §7; `nrese_dl::classify`): the context core where its Horn
//! stage takes the ontology, else the hypertableau driver (known and possible subsumers,
//! parallel tests). Each result says whether it is complete; what it contains is entailed.
//!
//! Results are kept per revision: a second request on an unchanged store, and the lower
//! bound's use of them, cost nothing.
//! The context core receives the store's explicit process-memory policy, including zero
//! to disable its watch; opening another store cannot change that policy.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use nrese_dl::classify::{self, Realisation, Taxonomy};
use nrese_engine::Snapshot;

use super::source;
use crate::StoreService;

/// The latest results, by revision.
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
    *cache.taxonomy.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((revision, Arc::clone(&taxonomy)));
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
    *cache.realisation.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((revision, Arc::clone(&realisation)));
    realisation
}
