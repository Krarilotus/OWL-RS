mod prepared;
mod schema;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use nrese_core::DatasetSnapshot;

use crate::dataset_index::IndexedDataset;
use crate::output::{InferenceDelta, ReasoningCacheStats};
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;

use self::prepared::{PreparedRulesMvp, build_prepared};
use self::schema::CachedPreparedSchema;

const EXECUTION_CACHE_CAPACITY: usize = 8;
const SCHEMA_CACHE_CAPACITY: usize = 8;

#[derive(Debug, Default)]
pub struct RulesMvpExecutionCache {
    entries: Mutex<Vec<CachedRulesMvpExecution>>,
    schema_entries: Mutex<Vec<CachedRulesMvpSchema>>,
    hits: AtomicU64,
    misses: AtomicU64,
    schema_hits: AtomicU64,
    schema_misses: AtomicU64,
}

impl RulesMvpExecutionCache {
    pub fn snapshot(&self) -> ReasoningCacheStats {
        ReasoningCacheStats {
            execution_cache_hit: false,
            schema_cache_hit: false,
            execution_cache_entries: self
                .entries
                .lock()
                .expect("rules-mvp cache lock poisoned")
                .len(),
            schema_cache_entries: self
                .schema_entries
                .lock()
                .expect("rules-mvp schema cache lock poisoned")
                .len(),
            execution_cache_capacity: EXECUTION_CACHE_CAPACITY,
            schema_cache_capacity: SCHEMA_CACHE_CAPACITY,
            execution_cache_hits_total: self.hits.load(Ordering::Relaxed),
            execution_cache_misses_total: self.misses.load(Ordering::Relaxed),
            schema_cache_hits_total: self.schema_hits.load(Ordering::Relaxed),
            schema_cache_misses_total: self.schema_misses.load(Ordering::Relaxed),
        }
    }

    pub fn execute<'a, S>(
        &self,
        snapshot: &'a S,
        policy: &RulesMvpFeaturePolicy,
    ) -> CacheExecutionResult
    where
        S: DatasetSnapshot<'a>,
    {
        let Some(cache_key) = snapshot.cache_key() else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            let mut inferred = build_prepared(snapshot, None, policy).execute();
            inferred.cache = self.execution_telemetry(false, false);
            return CacheExecutionResult { inferred };
        };
        let policy_cache_key = policy.cache_key();

        if let Some(cached) = self.lookup_execution_cache(cache_key, policy_cache_key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            let mut inferred = cached.inferred;
            inferred.cache = self.execution_telemetry(true, true);
            return CacheExecutionResult { inferred };
        }

        let index = IndexedDataset::from_snapshot(snapshot);
        let coverage = snapshot.coverage_stats();
        let schema_lookup = self.lookup_schema_cache(&index, policy);
        let inferred = PreparedRulesMvp::build_from_index(
            index,
            coverage,
            schema_lookup.entry.as_ref(),
            policy,
        )
        .execute();

        let mut inferred = inferred;
        let cached = CachedRulesMvpExecution {
            cache_key,
            policy_cache_key,
            inferred: inferred.clone(),
        };
        self.store_execution_cache(cached);
        self.misses.fetch_add(1, Ordering::Relaxed);
        inferred.cache = self.execution_telemetry(false, schema_lookup.hit);

        CacheExecutionResult { inferred }
    }

    fn execution_telemetry(
        &self,
        execution_cache_hit: bool,
        schema_cache_hit: bool,
    ) -> ReasoningCacheStats {
        let mut telemetry = self.snapshot();
        telemetry.execution_cache_hit = execution_cache_hit;
        telemetry.schema_cache_hit = schema_cache_hit;
        telemetry
    }

    fn lookup_schema_cache(
        &self,
        index: &IndexedDataset,
        policy: &RulesMvpFeaturePolicy,
    ) -> SchemaCacheLookup {
        let schema_cache_key = index.schema_cache_key();
        let policy_cache_key = policy.cache_key();

        if let Some(cached) = self.lookup_schema_entry(schema_cache_key, policy_cache_key) {
            self.schema_hits.fetch_add(1, Ordering::Relaxed);
            return SchemaCacheLookup {
                entry: Some(cached.entry),
                hit: true,
            };
        }

        let entry = CachedPreparedSchema::build(index, policy);
        let cached = CachedRulesMvpSchema {
            schema_cache_key,
            policy_cache_key,
            entry: entry.clone(),
        };
        self.store_schema_entry(cached);
        self.schema_misses.fetch_add(1, Ordering::Relaxed);

        SchemaCacheLookup {
            entry: Some(entry),
            hit: false,
        }
    }

    fn lookup_execution_cache(
        &self,
        cache_key: u64,
        policy_cache_key: u64,
    ) -> Option<CachedRulesMvpExecution> {
        let mut entries = self.entries.lock().expect("rules-mvp cache lock poisoned");
        let index = entries.iter().position(|cached| {
            cached.cache_key == cache_key && cached.policy_cache_key == policy_cache_key
        })?;
        let cached = entries.remove(index);
        let result = cached.clone();
        entries.insert(0, cached);
        Some(result)
    }

    fn store_execution_cache(&self, cached: CachedRulesMvpExecution) {
        let mut entries = self.entries.lock().expect("rules-mvp cache lock poisoned");
        entries.retain(|entry| {
            !(entry.cache_key == cached.cache_key
                && entry.policy_cache_key == cached.policy_cache_key)
        });
        entries.insert(0, cached);
        if entries.len() > EXECUTION_CACHE_CAPACITY {
            entries.truncate(EXECUTION_CACHE_CAPACITY);
        }
    }

    fn lookup_schema_entry(
        &self,
        schema_cache_key: u64,
        policy_cache_key: u64,
    ) -> Option<CachedRulesMvpSchema> {
        let mut entries = self
            .schema_entries
            .lock()
            .expect("rules-mvp schema cache lock poisoned");
        let index = entries.iter().position(|cached| {
            cached.schema_cache_key == schema_cache_key
                && cached.policy_cache_key == policy_cache_key
        })?;
        let cached = entries.remove(index);
        let result = cached.clone();
        entries.insert(0, cached);
        Some(result)
    }

    fn store_schema_entry(&self, cached: CachedRulesMvpSchema) {
        let mut entries = self
            .schema_entries
            .lock()
            .expect("rules-mvp schema cache lock poisoned");
        entries.retain(|entry| {
            !(entry.schema_cache_key == cached.schema_cache_key
                && entry.policy_cache_key == cached.policy_cache_key)
        });
        entries.insert(0, cached);
        if entries.len() > SCHEMA_CACHE_CAPACITY {
            entries.truncate(SCHEMA_CACHE_CAPACITY);
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheExecutionResult {
    pub inferred: InferenceDelta,
}

#[derive(Debug, Clone)]
struct CachedRulesMvpExecution {
    cache_key: u64,
    policy_cache_key: u64,
    inferred: InferenceDelta,
}

#[derive(Debug, Clone)]
struct CachedRulesMvpSchema {
    schema_cache_key: u64,
    policy_cache_key: u64,
    entry: CachedPreparedSchema,
}

#[derive(Debug, Clone)]
struct SchemaCacheLookup {
    entry: Option<CachedPreparedSchema>,
    hit: bool,
}
