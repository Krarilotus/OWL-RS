use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::source::KeyValueSource;

#[cfg(test)]
mod tests;

/// The settings of the configuration file at `path` ([`super::settings`]).
pub(super) fn load_file_source(path: &Path) -> Result<KeyValueSource> {
    let filename = path.display();
    let raw = fs::read_to_string(path).with_context(|| format!("failed to read {filename}"))?;
    let document: toml::Table = toml::from_str(&raw).map_err(|error: toml::de::Error| {
        // Only structured positions leave the parser; its messages may contain values.
        let before = raw.get(..error.span().unwrap_or(0..0).start).unwrap_or("");
        let line = before.split('\n').count();
        let column = before.chars().rev().take_while(|&c| c != '\n').count() + 1;
        anyhow::anyhow!("{filename}: invalid TOML at line {line}, column {column}")
    })?;
    super::settings::from_file(&document).with_context(|| format!("invalid config file {filename}"))
}
