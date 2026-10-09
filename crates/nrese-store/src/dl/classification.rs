//! Classification and realisation of the store's asserted ontology by the DL engines
//! (docs/design/owl2-dl.md §7; `nrese_dl::classify`): the context core where its Horn
//! stage takes the ontology, else the hypertableau driver (known and possible subsumers,
//! parallel tests). Each result says whether it is complete; what it contains is entailed.
//!
//! Results are kept per revision: a second request on an unchanged store, and the lower
//! bound's use of them, cost nothing.

use std::sync::{Arc, Mutex};

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
fn options(store: &StoreService) -> classify::Options {
    store.config().dl.classification_options()
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
    let ontology = source::read_snapshot(snapshot);
    let taxonomy = Arc::new(classify::classify(&ontology, &options(store)));
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
    let ontology = source::read_snapshot(snapshot);
    let realisation = Arc::new(classify::realise(&ontology, &options(store)));
    *cache.realisation.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((revision, Arc::clone(&realisation)));
    realisation
}
