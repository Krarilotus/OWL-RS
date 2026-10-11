//! Resources shared by repositories and their operations: the query budget all of
//! them draw on.

use std::sync::Arc;

use nrese_exec::SharedBudget;

#[derive(Debug)]
pub struct Runtime {
    query_memory: Option<Arc<SharedBudget>>,
}

impl Runtime {
    /// Zero query bytes is unlimited.
    pub fn new(query_memory_bytes: usize) -> Self {
        Self {
            query_memory: (query_memory_bytes > 0).then(|| SharedBudget::new(query_memory_bytes)),
        }
    }

    pub(crate) fn query_memory(&self) -> Option<Arc<SharedBudget>> {
        self.query_memory.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_exec::Budget;

    #[test]
    fn catalog_repositories_contend_for_one_query_budget() {
        let store = crate::StoreService::new(crate::StoreConfig {
            total_query_memory_bytes: 128,
            ..crate::StoreConfig::in_memory()
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
        let first = Budget::unlimited().within(runtime.query_memory());
        let next = Budget::unlimited().within(second.runtime().query_memory());
        first.charge(100).unwrap();
        assert!(next.charge(40).unwrap_err().shared);
        drop(first);
        next.charge(40).unwrap();
        drop(next);
        assert_eq!(runtime.query_memory().unwrap().used(), 0);
        assert!(Runtime::new(0).query_memory().is_none());
    }
}
