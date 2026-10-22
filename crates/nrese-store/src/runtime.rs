//! Resources shared by repositories and their operations: the process's memory, which
//! everything charged draws on, and within it the queries' budget all of them share.

use std::sync::Arc;

use nrese_exec::SharedBudget;

#[derive(Debug)]
pub struct Runtime {
    /// The process's: queries and maintenance (checkpoints, image exports, imports) charge
    /// it, the queries through their own budget within it ([`StoreConfig::process_memory_bytes`](crate::StoreConfig)).
    memory: Option<Arc<SharedBudget>>,
    /// All running queries together, within the process's
    /// ([`StoreConfig::total_query_memory_bytes`](crate::StoreConfig)).
    query_memory: Option<Arc<SharedBudget>>,
}

impl Runtime {
    /// The process's memory and the queries' within it, in bytes; zero is unlimited.
    pub fn new(process_memory_bytes: u64, query_memory_bytes: usize) -> Self {
        let memory = (process_memory_bytes > 0).then(|| {
            SharedBudget::new(usize::try_from(process_memory_bytes).unwrap_or(usize::MAX))
        });
        let query_memory = (query_memory_bytes > 0).then(|| match &memory {
            Some(memory) => SharedBudget::within(query_memory_bytes, Arc::clone(memory)),
            None => SharedBudget::new(query_memory_bytes),
        });
        Self {
            memory,
            query_memory,
        }
    }

    /// The process's memory, for maintenance to reserve within; `None` without a limit.
    pub fn memory(&self) -> Option<Arc<SharedBudget>> {
        self.memory.clone()
    }

    /// The budget queries charge: their own, else the process's.
    pub(crate) fn query_memory(&self) -> Option<Arc<SharedBudget>> {
        self.query_memory.clone().or_else(|| self.memory.clone())
    }

    /// The queries' own budget, if they have one.
    pub(crate) fn query_pool(&self) -> Option<&Arc<SharedBudget>> {
        self.query_memory.as_ref()
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
        let catalog =
            crate::catalog::Catalog::open(store, nrese_reasoner::ReasonerConfig::default())
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
        assert!(Runtime::new(0, 0).query_memory().is_none());
    }

    /// The queries' budget lies within the process's, which maintenance reserves within
    /// directly: what one holds, the other can't take. Without a queries' budget of their
    /// own, queries charge the process's.
    #[test]
    fn queries_and_maintenance_draw_on_the_processs_memory() {
        let runtime = Runtime::new(100, 80);
        let memory = runtime.memory().expect("a process limit");
        let query = Budget::unlimited().within(runtime.query_memory());
        let checkpoint = Budget::new(60).within(runtime.memory());
        checkpoint.charge(50).unwrap();
        let err = query.charge(60).unwrap_err();
        assert!(
            err.shared && err.limit == 100,
            "the process's limit: {err:?}"
        );
        query.charge(50).unwrap();
        assert_eq!(memory.used(), 100);
        assert!(checkpoint.charge(1).unwrap_err().shared);
        drop((query, checkpoint));
        assert_eq!(memory.used(), 0);
        // No queries' budget: they charge the process's; no process limit: the queries'.
        let runtime = Runtime::new(100, 0);
        assert!(Arc::ptr_eq(
            &runtime.query_memory().unwrap(),
            &runtime.memory().unwrap()
        ));
        assert!(runtime.query_pool().is_none());
        let runtime = Runtime::new(0, 80);
        assert!(runtime.memory().is_none());
        assert_eq!(runtime.query_memory().unwrap().limit(), 80);
    }
}
