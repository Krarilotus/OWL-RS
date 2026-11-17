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
    let document: toml::Table = toml::from_str(&raw).map_err(|mut error: toml::de::Error| {
        // Keep the position, then remove source text while retaining reason and keys.
        let display = error.to_string();
        let position = display.lines().next().unwrap_or("invalid TOML");
        error.set_input(None);
        anyhow::anyhow!("{filename}: {position}: {error}")
    })?;
    super::settings::from_file(&document).with_context(|| format!("invalid config file {filename}"))
}
