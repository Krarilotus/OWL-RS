use anyhow::{Context, Result};

use super::source::ConfigSource;

pub(super) fn parse_bool(source: &dyn ConfigSource, name: &str, default: bool) -> Result<bool> {
    match source.get(name).as_deref() {
        None => Ok(default),
        Some("1" | "true" | "TRUE" | "True" | "yes" | "on") => Ok(true),
        Some("0" | "false" | "FALSE" | "False" | "no" | "off") => Ok(false),
        Some(value) => anyhow::bail!("invalid boolean for {name}: {value}"),
    }
}

pub(super) fn parse_u64(source: &dyn ConfigSource, name: &str, default: u64) -> Result<u64> {
    match source.get(name) {
        Some(value) => value
            .parse()
            .with_context(|| format!("failed to parse {name}")),
        None => Ok(default),
    }
}

pub(super) fn parse_usize(source: &dyn ConfigSource, name: &str, default: usize) -> Result<usize> {
    match source.get(name) {
        Some(value) => value
            .parse()
            .with_context(|| format!("failed to parse {name}")),
        None => Ok(default),
    }
}
