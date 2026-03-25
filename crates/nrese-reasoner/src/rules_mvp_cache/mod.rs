mod prepared;
mod schema;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use nrese_core::DatasetSnapshot;

use crate::dataset_index::IndexedDataset;
use crate::output::InferenceDelta;
use crate::rules_mvp_policy::RulesMvpFeaturePolicy;

use self::prepared::{PreparedRulesMvp, build_prepared};
use self::schema::CachedPreparedSchema;

#[derive(Debug, Default)]
pub struct RulesMvpExecutionCache {
    entry: Mutex<Option<CachedRulesMvpExecution>>,
    schema_entry: Mutex<Option<CachedRulesMvpSchema>>,
    hits: AtomicU64,
    misses: AtomicU64,
    schema_hits: AtomicU64,
    schema_misses: AtomicU64,
}

impl RulesMvpExecutionCache {
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
            return CacheExecutionResult {
                inferred: build_prepared(snapshot, None, policy).execute(),
                cache_hit: false,
                schema_cache_hit: false,
            };
        };
        let policy_cache_key = policy.cache_key();

        if let Some(cached) = self
            .entry
            .lock()
            .expect("rules-mvp cache lock poisoned")
            .as_ref()
            .filter(|cached| {
                cached.cache_key == cache_key && cached.policy_cache_key == policy_cache_key
            })
            .cloned()
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return CacheExecutionResult {
                inferred: cached.inferred,
                cache_hit: true,
                schema_cache_hit: true,
            };
        }

        let index = IndexedDataset::from_snapshot(snapshot);
        let schema_lookup = self.lookup_schema_cache(&index, policy);
        let inferred =
            PreparedRulesMvp::build_from_index(index, schema_lookup.entry.as_ref(), policy)
                .execute();

        let cached = CachedRulesMvpExecution {
            cache_key,
            policy_cache_key,
            inferred: inferred.clone(),
        };
        *self.entry.lock().expect("rules-mvp cache lock poisoned") = Some(cached);
        self.misses.fetch_add(1, Ordering::Relaxed);

        CacheExecutionResult {
            inferred,
            cache_hit: false,
            schema_cache_hit: schema_lookup.hit,
        }
    }

    fn lookup_schema_cache(
        &self,
        index: &IndexedDataset,
        policy: &RulesMvpFeaturePolicy,
    ) -> SchemaCacheLookup {
        let schema_cache_key = index.schema_cache_key();
        let policy_cache_key = policy.cache_key();

        if let Some(cached) = self
            .schema_entry
            .lock()
            .expect("rules-mvp schema cache lock poisoned")
            .as_ref()
            .filter(|cached| {
                cached.schema_cache_key == schema_cache_key
                    && cached.policy_cache_key == policy_cache_key
            })
            .cloned()
        {
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
        *self
            .schema_entry
            .lock()
            .expect("rules-mvp schema cache lock poisoned") = Some(cached);
        self.schema_misses.fetch_add(1, Ordering::Relaxed);

        SchemaCacheLookup {
            entry: Some(entry),
            hit: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheExecutionResult {
    pub inferred: InferenceDelta,
    pub cache_hit: bool,
    pub schema_cache_hit: bool,
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
