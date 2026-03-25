use std::collections::BTreeMap;
use std::env;

pub(super) trait ConfigSource {
    fn get(&self, key: &str) -> Option<String>;
}

#[derive(Debug, Clone, Default)]
pub(super) struct KeyValueSource {
    values: BTreeMap<String, String>,
}

impl KeyValueSource {
    pub(super) fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }
}

impl ConfigSource for KeyValueSource {
    fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ProcessEnv;

impl ConfigSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

#[derive(Debug, Clone)]
pub(super) struct LayeredSource<L, H> {
    low: L,
    high: H,
}

impl<L, H> LayeredSource<L, H> {
    pub(super) fn new(low: L, high: H) -> Self {
        Self { low, high }
    }
}

impl<L, H> ConfigSource for LayeredSource<L, H>
where
    L: ConfigSource,
    H: ConfigSource,
{
    fn get(&self, key: &str) -> Option<String> {
        self.high.get(key).or_else(|| self.low.get(key))
    }
}
