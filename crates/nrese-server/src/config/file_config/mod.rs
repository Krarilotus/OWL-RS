use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::source::KeyValueSource;

#[cfg(test)]
mod tests;

/// The settings of the configuration file at `path` ([`super::settings`]).
pub(super) fn load_file_source(path: &Path) -> Result<KeyValueSource> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let document: toml::Table = toml::from_str(&raw)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;
    super::settings::from_file(&document)
        .with_context(|| format!("invalid config file {}", path.display()))
}
