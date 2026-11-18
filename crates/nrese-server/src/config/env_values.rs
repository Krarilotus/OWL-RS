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

/// A size in bytes: a plain number or one with a unit (`4GiB`; see [`super::units`]).
pub(super) fn parse_bytes(source: &dyn ConfigSource, name: &str, default: usize) -> Result<usize> {
    match source.get(name) {
        Some(value) => super::units::parse_size(&value)
            .map(|bytes| bytes as usize)
            .with_context(|| format!("failed to parse {name}")),
        None => Ok(default),
    }
}

/// A duration in milliseconds: a plain number or one with a unit (`30s`).
pub(super) fn parse_millis(source: &dyn ConfigSource, name: &str, default: u64) -> Result<u64> {
    match source.get(name) {
        Some(value) => super::units::parse_duration_ms(&value)
            .with_context(|| format!("failed to parse {name}")),
        None => Ok(default),
    }
}

/// A budget: a value as `parse` reads it, or `0` or `unlimited` for none (`None`); unset,
/// `default`.
pub(super) fn parse_limit<T: PartialEq + Default>(
    source: &dyn ConfigSource,
    name: &str,
    default: Option<T>,
    parse: impl Fn(&str) -> Result<T>,
) -> Result<Option<T>> {
    match source.get(name) {
        None => Ok(default),
        Some(value) if value.trim().eq_ignore_ascii_case("unlimited") => Ok(None),
        Some(value) => {
            let value = parse(value.trim()).with_context(|| format!("failed to parse {name}"))?;
            Ok((value != T::default()).then_some(value))
        }
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
