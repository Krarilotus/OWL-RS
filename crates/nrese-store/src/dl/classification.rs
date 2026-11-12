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
    taxonomy: Slot<Taxonomy>,
    realisation: Slot<Realisation>,
}

type Slot<T> = Mutex<Option<(u64, Arc<T>)>>;

/// How the engines run under the store's settings. A classification is the store's work,
/// shared per revision, not a request's: its memory is an envelope of its own within the
/// catalog's budget, which every test and the context core draw from.
pub(super) fn options(store: &StoreService) -> classify::Options {
    let config = &store.config().dl;
    let workers = nrese_exec::workers::Workers::current().limited(config.threads);
    let mut options = config.classification_options(workers, Some(store.envelope(None)));
    // A limit that can't be found runs no engine ([`run`]); here it leaves none room.
    options.max_memory = Some(store.process_memory_bytes().unwrap_or(1));
    options
}

/// The taxonomy of `snapshot`'s asserted ontology (from the cache if it is current,
/// or from a current realisation).
pub(crate) fn taxonomy(store: &StoreService, snapshot: &Snapshot) -> Arc<Taxonomy> {
    let cache = &store.dl().classification;
    let revision = snapshot.revision();
    cached(
        &cache.taxonomy,
        revision,
        Taxonomy::complete,
        || match latest(&cache.realisation, revision) {
            Some(realisation) => realisation.taxonomy.clone(),
            None => run(store, snapshot, classify::classify, |t, why| {
                t.classification.consistency = nrese_dl::verdict::Verdict::Unknown(why.clone());
                t.incomplete.push(why)
            }),
        },
    )
}

/// The realisation of `snapshot`'s asserted ontology (from the cache if it is current).
pub(crate) fn realisation(store: &StoreService, snapshot: &Snapshot) -> Arc<Realisation> {
    let complete = |r: &Realisation| r.incomplete.is_empty() && r.taxonomy.complete();
    cached(
        &store.dl().classification.realisation,
        snapshot.revision(),
        complete,
        || {
            run(store, snapshot, classify::realise, |r, why| {
                r.taxonomy.classification.consistency =
                    nrese_dl::verdict::Verdict::Unknown(why.clone());
                r.taxonomy.incomplete.push(why.clone());
                r.incomplete.push(why)
            })
        },
    )
}

/// The cached result at `revision`, else `compute`'s, kept if `complete`. Publishing never
/// lets an older in-flight request evict a newer revision; same-revision races keep the
/// first complete result. No solver runs locked.
fn cached<T>(
    slot: &Slot<T>,
    revision: u64,
    complete: impl Fn(&T) -> bool,
    compute: impl FnOnce() -> T,
) -> Arc<T> {
    if let Some(result) = latest(slot, revision) {
        return result;
    }
    let result = Arc::new(compute());
    if complete(&result) {
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_none_or(|(at, _)| *at < revision) {
            *slot = Some((revision, Arc::clone(&result)));
        }
    }
    result
}

fn latest<T>(slot: &Slot<T>, revision: u64) -> Option<Arc<T>> {
    match &*slot.lock().unwrap_or_else(|p| p.into_inner()) {
        Some((at, result)) if *at == revision => Some(Arc::clone(result)),
        _ => None,
    }
}

/// `engine` over `snapshot`'s asserted ontology, within one timeout that includes
/// reading the ontology.
/// Its reading of the ontology draws on the run's envelope too (SYN-61); where that can't
/// take it, the engine's result over no axioms with its consistency unknown, `incomplete`
/// saying why (never kept: a result kept is complete). Over no axioms the engine would say
/// consistent, which nothing read decided.
fn run<T>(
    store: &StoreService,
    snapshot: &Snapshot,
    engine: fn(&nrese_owl::Ontology, &classify::Options) -> T,
    incomplete: fn(&mut T, String),
) -> T {
    let started = Instant::now();
    // The process's limit that can't be found (its memory unread) holds no engine to it:
    // none runs, and the answer over no axioms says why (never kept: fail closed).
    if let Err(unread) = store.process_memory_bytes() {
        let mut none = engine(
            &nrese_owl::Ontology::default(),
            &classify::Options::default(),
        );
        incomplete(
            &mut none,
            format!("the process's memory limit couldn't be found ({unread})"),
        );
        return none;
    }
    let mut options = options(store);
    let memory = options.tableau.memory.clone().unwrap_or_default();
    let read = source::read_snapshot(snapshot, memory);
    options.timeout = options
        .timeout
        .map(|limit| limit.saturating_sub(started.elapsed()));
    match read {
        Ok(ontology) => engine(&ontology, &options),
        Err(_) => {
            let mut none = engine(&nrese_owl::Ontology::default(), &options);
            incomplete(&mut none, source::refused("the revision"));
            none
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DlConfig, StoreConfig};
    use std::time::Duration;

    fn store(timeout: Duration) -> StoreService {
        let store = StoreService::new(StoreConfig {
            process_memory_bytes: crate::ProcessMemory::Bytes(0),
            dl: DlConfig {
                timeout: Some(timeout),
                threads: 1,
                ..DlConfig::default()
            },
            ..StoreConfig::in_memory()
        })
        .unwrap();
        store
            .update_str_ungated(
                "PREFIX : <urn:cache:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA { :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] .
                 :B rdfs:subClassOf :D . :C rdfs:subClassOf :D . :a a :A . }",
            )
            .unwrap();
        store
    }

    /// D2, A-902: where the process's limit can't be found (its memory unread), no engine
    /// runs, even on a runtime of no limit supplied by the caller: the taxonomy and the
    /// realisation say why and leave consistency unknown, and neither is kept.
    #[test]
    fn a_classification_whose_limit_cant_be_found_runs_no_engine() {
        fn unread() -> nrese_exec::memory::Reading {
            Err(nrese_exec::memory::Unread::Failed)
        }
        let store = StoreService::with_runtime(
            StoreConfig::in_memory(),
            Arc::new(crate::Runtime::new(0, 0)),
        )
        .unwrap()
        .with_capacity(unread);
        store
            .update_str_ungated(
                "PREFIX : <urn:cache:> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                 INSERT DATA { :A rdfs:subClassOf :B . :a a :A . }",
            )
            .unwrap();
        let snapshot = store.engine().snapshot();
        let taxonomy = taxonomy(&store, &snapshot);
        let realisation = realisation(&store, &snapshot);
        for (consistency, incomplete) in [
            (&taxonomy.classification.consistency, &taxonomy.incomplete),
            (
                &realisation.taxonomy.classification.consistency,
                &realisation.incomplete,
            ),
        ] {
            assert!(
                matches!(consistency, nrese_dl::verdict::Verdict::Unknown(_)),
                "{consistency:?}"
            );
            assert!(
                incomplete.iter().any(|why| why.contains("memory limit")),
                "{incomplete:?}"
            );
        }
        let cache = &store.dl().classification;
        assert!(latest(&cache.taxonomy, snapshot.revision()).is_none());
        assert!(latest(&cache.realisation, snapshot.revision()).is_none());
    }

    /// A classification whose ontology the run's memory can't read reports its consistency
    /// unknown, never consistent (SYN-61): over the empty ontology the engines are left with,
    /// consistent would be their answer, which nothing read decided.
    #[test]
    fn a_classification_whose_ontology_does_not_fit_is_never_consistent() {
        let store = StoreService::new(StoreConfig {
            total_query_memory_bytes: 1 << 30,
            ..StoreConfig::in_memory()
        })
        .unwrap();
        let mut data = String::from(
            "PREFIX : <urn:cache:> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA { :A rdfs:subClassOf :B .",
        );
        for i in 0..2_000 {
            data.push_str(&format!(" :a{i} a :A ."));
        }
        data.push('}');
        store.update_str_ungated(&data).unwrap();
        let snapshot = store.engine().snapshot();
        // All but 4 KiB of the queries' pool held: the 2,000 statements don't fit.
        let pool = store.runtime().query_memory().expect("the queries' pool");
        let mut hold = nrese_exec::Reservation::new(Arc::new(store.envelope(None).part()));
        hold.charge(pool.limit() - pool.used() - 4096).unwrap();
        let verdicts = [
            taxonomy(&store, &snapshot)
                .classification
                .consistency
                .clone(),
            realisation(&store, &snapshot)
                .taxonomy
                .classification
                .consistency
                .clone(),
        ];
        for verdict in verdicts {
            assert!(
                matches!(verdict, nrese_dl::verdict::Verdict::Unknown(_)),
                "{verdict:?}"
            );
        }
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
            .update_str_ungated("INSERT DATA { <urn:cache:b> a <urn:cache:A> }")
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
