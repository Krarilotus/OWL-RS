use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::source::KeyValueSource;

mod mapping;
mod raw;
#[cfg(test)]
mod tests;

use mapping::into_key_value_source;
use raw::RawFileConfig;

pub(super) fn load_file_source(path: &Path) -> Result<KeyValueSource> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config: RawFileConfig = toml::from_str(&raw)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;

    Ok(into_key_value_source(config))
}
