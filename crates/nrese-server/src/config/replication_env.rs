use anyhow::{Result, bail};

use super::env_names as names;
use super::env_values::{parse_bytes, parse_millis};
use super::source::ConfigSource;
use crate::replication::{ReplicationConfig, ReplicationMode};

/// `replication.*`: a replica needs its primary.
pub(super) fn parse_replication_config(source: &dyn ConfigSource) -> Result<ReplicationConfig> {
    let defaults = ReplicationConfig::default();
    let mode = match source
        .get(names::REPLICATION_MODE)
        .map(|m| m.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("off") => ReplicationMode::Off,
        Some("primary") => ReplicationMode::Primary,
        Some("replica") => ReplicationMode::Replica,
        Some(other) => bail!(
            "unsupported value '{other}' in {} (expected 'off', 'primary' or 'replica')",
            names::REPLICATION_MODE
        ),
    };
    let primary = source
        .get(names::REPLICATION_PRIMARY)
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty());
    if mode == ReplicationMode::Replica && primary.is_none() {
        bail!(
            "replication.mode = \"replica\" needs replication.primary ({})",
            names::REPLICATION_PRIMARY
        );
    }
    Ok(ReplicationConfig {
        mode,
        primary,
        token: source
            .get(names::REPLICATION_TOKEN)
            .filter(|t| !t.trim().is_empty()),
        poll: std::time::Duration::from_millis(parse_millis(
            source,
            names::REPLICATION_POLL_MS,
            defaults.poll.as_millis() as u64,
        )?),
        batch_bytes: parse_bytes(source, names::REPLICATION_BATCH_BYTES, defaults.batch_bytes)?,
    })
}
