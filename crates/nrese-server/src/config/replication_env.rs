use anyhow::{Result, bail};

use super::env_names as names;
use super::env_values::{parse_bytes, parse_limit, parse_millis};
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
        round_timeout: duration_limit(
            source,
            names::REPLICATION_ROUND_TIMEOUT_MS,
            defaults.round_timeout,
        )?,
        image_timeout: duration_limit(
            source,
            names::REPLICATION_IMAGE_TIMEOUT_MS,
            defaults.image_timeout,
        )?,
    })
}

/// A time limit: a duration, or `0` or `unlimited` for none; unset, `default`.
fn duration_limit(
    source: &dyn ConfigSource,
    name: &str,
    default: Option<std::time::Duration>,
) -> Result<Option<std::time::Duration>> {
    Ok(parse_limit(
        source,
        name,
        default.map(|d| d.as_millis() as u64),
        super::units::parse_duration_ms,
    )?
    .map(std::time::Duration::from_millis))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::source::KeyValueSource;
    use super::*;

    /// `replication.round_timeout` (5 minutes unless set) and `replication.image_timeout`
    /// (an hour unless set): a duration, or `0` or `unlimited` for no limit.
    #[test]
    fn a_log_round_and_an_image_take_their_configured_time() {
        let limits = |name: &str, value: Option<&str>| {
            let mut source = KeyValueSource::default();
            if let Some(value) = value {
                source.insert(name, value);
            }
            parse_replication_config(&source)
                .map(|config| (config.round_timeout, config.image_timeout))
        };
        let (round, image) = (
            names::REPLICATION_ROUND_TIMEOUT_MS,
            names::REPLICATION_IMAGE_TIMEOUT_MS,
        );
        let (five_minutes, an_hour) = (Duration::from_secs(300), Duration::from_secs(3600));
        assert_eq!(
            limits(round, None).unwrap(),
            (Some(five_minutes), Some(an_hour))
        );
        assert_eq!(
            limits(round, Some("20min")).unwrap(),
            (Some(Duration::from_secs(1200)), Some(an_hour))
        );
        assert_eq!(
            limits(image, Some("3h")).unwrap(),
            (Some(five_minutes), Some(Duration::from_secs(3 * 3600)))
        );
        for none in ["0", "unlimited"] {
            assert_eq!(limits(round, Some(none)).unwrap().0, None, "{none}");
            assert_eq!(limits(image, Some(none)).unwrap().1, None, "{none}");
        }
        assert!(limits(round, Some("soon")).is_err());
    }
}
