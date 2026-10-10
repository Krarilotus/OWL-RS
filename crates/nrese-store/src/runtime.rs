//! Resources shared by repositories and their operations. Strategy selection stays
//! with each semantic engine; this owner selects execution policy and query capacity.

use std::sync::Arc;

use nrese_exec::{SharedBudget, workers::Workers};

use crate::{StoreConfig, StoreError, StoreResult};

#[derive(Debug)]
pub struct Runtime {
    threads: usize,
    query_memory_bytes: usize,
    workers: Workers,
    query_memory: Option<Arc<SharedBudget>>,
}

impl Runtime {
    /// Zero threads preserves caller execution and existing parallel kernels, with no
    /// catalog-wide CPU cap. Positive counts select a shared physical pool. Zero query
    /// bytes is unlimited. A failed configured pool retries with one physical worker;
    /// failure of that fallback is an error, never an implicit policy change.
    pub fn new(threads: usize, query_memory_bytes: usize) -> StoreResult<Self> {
        let workers = if threads == 0 {
            Workers::current()
        } else {
            Self::build_workers(threads, Workers::pooled)?
        };
        Ok(Self {
            threads,
            query_memory_bytes,
            workers,
            query_memory: (query_memory_bytes > 0).then(|| SharedBudget::new(query_memory_bytes)),
        })
    }

    fn build_workers(
        threads: usize,
        mut build: impl FnMut(usize) -> Result<Workers, rayon::ThreadPoolBuildError>,
    ) -> StoreResult<Workers> {
        match build(threads) {
            Ok(workers) => Ok(workers),
            Err(error) if threads != 1 => {
                tracing::warn!(%error, "execution pool unavailable; retrying one physical worker");
                build(1).map_err(StoreError::ExecutionUnavailable)
            }
            Err(error) => Err(StoreError::ExecutionUnavailable(error)),
        }
    }

    /// Clones the execution handle, never its threads.
    pub fn workers(&self) -> Workers {
        self.workers.clone()
    }

    pub(crate) fn query_memory(&self) -> Option<Arc<SharedBudget>> {
        self.query_memory.clone()
    }

    pub(crate) fn validate(&self, config: &StoreConfig) -> StoreResult<()> {
        if self.threads != config.execution_threads
            || self.query_memory_bytes != config.total_query_memory_bytes
        {
            return Err(StoreError::Configuration(
                "repositories sharing a runtime must share execution.threads and budgets.total_query_memory".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_exec::Budget;

    fn fail_pool() -> rayon::ThreadPoolBuildError {
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .spawn_handler(|_| Err(std::io::Error::other("injected thread failure")))
            .build()
            .unwrap_err()
    }

    #[test]
    fn default_execution_stays_on_the_caller_without_serialising_parallel_kernels() {
        let runtime = Runtime::new(0, 0).unwrap();
        let caller = std::thread::current().id();
        assert_eq!(
            runtime.workers().install(|| std::thread::current().id()),
            caller
        );
        Workers::pooled(3).unwrap().install(|| {
            assert_eq!(runtime.workers().width(), 3);
            assert_eq!(runtime.workers().limited(2).width(), 2);
        });
    }

    #[test]
    fn failed_pool_creation_keeps_physical_admission_or_returns_an_error() {
        let mut attempts = Vec::new();
        let workers = Runtime::build_workers(4, |width| {
            attempts.push(width);
            if width == 1 {
                Workers::pooled(1)
            } else {
                Err(fail_pool())
            }
        })
        .unwrap();
        assert_eq!(attempts, [4, 1]);
        assert_eq!(workers.width(), 1);
        let caller = std::thread::current().id();
        assert_ne!(workers.install(|| std::thread::current().id()), caller);
        assert!(matches!(
            Runtime::build_workers(4, |_| Err(fail_pool())),
            Err(StoreError::ExecutionUnavailable(_))
        ));
    }

    #[test]
    fn catalog_repositories_share_workers_and_contend_for_one_query_budget() {
        let store = crate::StoreService::new(StoreConfig {
            execution_threads: 1,
            total_query_memory_bytes: 128,
            ..StoreConfig::in_memory()
        })
        .unwrap();
        let runtime = Arc::clone(store.runtime());
        let catalog = crate::catalog::Catalog::open(
            store,
            nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::default()),
        )
        .unwrap();
        catalog
            .create("second", crate::catalog::RepositorySettings::default())
            .unwrap();
        let second = catalog
            .get("second")
            .unwrap()
            .pipeline
            .read()
            .store()
            .clone();
        assert!(Arc::ptr_eq(&runtime, second.runtime()));
        assert_eq!(runtime.workers(), second.runtime().workers());
        let first = Budget::unlimited().within(runtime.query_memory());
        let next = Budget::unlimited().within(second.runtime().query_memory());
        first.charge(100).unwrap();
        assert!(next.charge(40).unwrap_err().shared);
        drop(first);
        next.charge(40).unwrap();
        drop(next);
        assert_eq!(runtime.query_memory().unwrap().used(), 0);
        assert!(Runtime::new(1, 0).unwrap().query_memory().is_none());
    }
}
