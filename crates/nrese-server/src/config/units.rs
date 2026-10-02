//! Sizes and durations as people write them: `4GiB`, `512 MiB`, `30s`, `2min`, `50%`.
//!
//! Every budget setting takes a plain number (bytes or milliseconds, as before) or a
//! number with a unit.
//!
//! | Kind | Units |
//! |---|---|
//! | Size | `B`; binary `KiB`, `MiB`, `GiB`, `TiB` and their short forms `K`, `M`, `G`, `T`; decimal `KB`, `MB`, `GB`, `TB` |
//! | Duration | `ms`, `s`, `min` (or `m`), `h` |
//! | Share of the machine's memory | `%` |

use anyhow::{Result, anyhow, bail};

/// The number and the unit of `text`: `"1.5 GiB"` is `(1.5, "gib")`.
fn split(text: &str) -> Result<(f64, String)> {
    let text = text.trim();
    let digits = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_'))
        .unwrap_or(text.len());
    let number: f64 = text[..digits]
        .replace('_', "")
        .parse()
        .map_err(|_| anyhow!("'{text}' doesn't start with a number"))?;
    Ok((number, text[digits..].trim().to_ascii_lowercase()))
}

/// Bytes: `1048576`, `1MiB`, `1.5 GiB`, `2G`, `500MB`.
pub(super) fn parse_size(text: &str) -> Result<u64> {
    let (number, unit) = split(text)?;
    let factor: u64 = match unit.as_str() {
        "" | "b" => 1,
        "k" | "kib" => 1 << 10,
        "m" | "mib" => 1 << 20,
        "g" | "gib" => 1 << 30,
        "t" | "tib" => 1 << 40,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "tb" => 1_000_000_000_000,
        other => {
            bail!("unknown size unit '{other}' in '{text}' (KiB, MiB, GiB, TiB, KB, MB, GB, TB)")
        }
    };
    Ok((number * factor as f64).round() as u64)
}

/// Milliseconds: `30000`, `30s`, `500ms`, `2min`, `1h`.
pub(super) fn parse_duration_ms(text: &str) -> Result<u64> {
    let (number, unit) = split(text)?;
    let factor: u64 = match unit.as_str() {
        "" | "ms" => 1,
        "s" | "sec" => 1_000,
        "m" | "min" => 60_000,
        "h" => 3_600_000,
        other => bail!("unknown time unit '{other}' in '{text}' (ms, s, min, h)"),
    };
    Ok((number * factor as f64).round() as u64)
}

/// Bytes, or a share of the machine's memory (`50%`). `Ok(None)` if a share is asked for
/// and the machine's memory isn't known: the caller then sets no limit.
pub(super) fn parse_memory(text: &str) -> Result<Option<u64>> {
    let trimmed = text.trim();
    let Some(percent) = trimmed.strip_suffix('%') else {
        return parse_size(trimmed).map(Some);
    };
    let share: f64 = percent
        .trim()
        .parse()
        .map_err(|_| anyhow!("'{text}' is not a percentage"))?;
    if !(0.0..=100.0).contains(&share) {
        bail!("'{text}' is not between 0% and 100%");
    }
    Ok(machine_memory_bytes().map(|total| (total as f64 * share / 100.0) as u64))
}

/// The memory this process may use: the container's limit if there is one, else the
/// machine's. Known on Linux (where the server is deployed); `None` elsewhere.
pub fn machine_memory_bytes() -> Option<u64> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let machine = read("/proc/meminfo").and_then(|text| {
        let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    })?;
    // cgroup v2, then v1; "max" and absurd values mean "no limit".
    let container = [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ]
    .iter()
    .filter_map(|path| read(path)?.trim().parse::<u64>().ok())
    .find(|&limit| limit > 0 && limit < machine);
    Some(container.unwrap_or(machine))
}

/// A size for people: `4 GiB`, `512 MiB`, `1536 B`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];
    for (unit, factor) in UNITS {
        if bytes >= factor {
            let value = bytes as f64 / factor as f64;
            return if value.fract() == 0.0 {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_take_plain_bytes_binary_and_decimal_units() {
        for (text, bytes) in [
            ("4294967296", 4u64 << 30),
            ("4GiB", 4 << 30),
            ("4 gib", 4 << 30),
            ("4G", 4 << 30),
            ("1.5GiB", 3 << 29),
            ("512MiB", 512 << 20),
            ("64 KiB", 64 << 10),
            ("2GB", 2_000_000_000),
            ("1_000", 1000),
            ("0", 0),
        ] {
            assert_eq!(parse_size(text).unwrap(), bytes, "{text}");
        }
        for text in ["", "GiB", "4 parsecs", "-1", "4GiBB"] {
            assert!(parse_size(text).is_err(), "{text}");
        }
    }

    #[test]
    fn durations_take_plain_milliseconds_and_units() {
        for (text, ms) in [
            ("30000", 30_000u64),
            ("30s", 30_000),
            ("0.5 s", 500),
            ("500ms", 500),
            ("2min", 120_000),
            ("2m", 120_000),
            ("1h", 3_600_000),
        ] {
            assert_eq!(parse_duration_ms(text).unwrap(), ms, "{text}");
        }
        assert!(parse_duration_ms("30 fortnights").is_err());
    }

    #[test]
    fn memory_is_bytes_or_a_share_of_the_machine() {
        assert_eq!(parse_memory("8GiB").unwrap(), Some(8 << 30));
        assert!(parse_memory("150%").is_err());
        assert!(parse_memory("half").is_err());
        // Known on Linux, unknown elsewhere: either way not an error.
        let half = parse_memory("50%").unwrap();
        assert_eq!(half, machine_memory_bytes().map(|total| total / 2));
    }

    #[test]
    fn sizes_print_in_the_largest_unit_that_fits() {
        assert_eq!(format_size(4 << 30), "4 GiB");
        assert_eq!(format_size(3 << 29), "1.5 GiB");
        assert_eq!(format_size(128 << 20), "128 MiB");
        assert_eq!(format_size(1000), "1000 B");
    }
}
