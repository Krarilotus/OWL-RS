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

use anyhow::{Context, Result, anyhow, bail};
use nrese_store::memory::Reading;

/// The number and the unit of `text`: `"1.5 GiB"` is `("1.5", "gib")`.
fn split(text: &str) -> Result<(String, String)> {
    let text = text.trim();
    let digits = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_'))
        .unwrap_or(text.len());
    let number = text[..digits].replace('_', "");
    if number.parse::<f64>().is_err() {
        bail!("'{text}' doesn't start with a number");
    }
    Ok((number, text[digits..].trim().to_ascii_lowercase()))
}

/// `number` (of `text`) times `factor`, exact for a whole number. An error past u64, and
/// for a positive value below one (`smallest`): it would read as 0, which a budget takes
/// as none. Whether the value is positive is read from its digits, not from the float: a
/// decimal too small for f64 parses as 0.0.
fn scale(text: &str, number: &str, factor: u64, smallest: &str) -> Result<u64> {
    let too_large = || anyhow!("'{text}' is too large");
    if !number.contains('.') {
        let whole: u64 = number.parse().map_err(|_| too_large())?;
        return whole.checked_mul(factor).ok_or_else(too_large);
    }
    let positive = number.bytes().any(|b| matches!(b, b'1'..=b'9'));
    let value = number.parse::<f64>()? * factor as f64;
    if value >= u64::MAX as f64 {
        return Err(too_large());
    }
    if positive && value < 1.0 {
        bail!("'{text}' is more than 0 but less than {smallest}");
    }
    Ok(value.round() as u64)
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
    scale(text, &number, factor, "one byte")
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
    scale(text, &number, factor, "one millisecond")
}

/// Bytes, or a share of `machine`, the machine's memory as read ([`machine_memory_bytes`]):
/// `50%`. The inner `Err` if a share is asked for and the machine's memory wasn't read.
pub(super) fn parse_memory(text: &str, machine: Reading) -> Result<Reading> {
    let trimmed = text.trim();
    let Some(percent) = trimmed.strip_suffix('%') else {
        return parse_size(trimmed).map(Ok);
    };
    let share: f64 = percent
        .trim()
        .parse()
        .map_err(|_| anyhow!("'{text}' is not a percentage"))?;
    if !(0.0..=100.0).contains(&share) {
        bail!("'{text}' is not between 0% and 100%");
    }
    Ok(machine.map(|total| (total as f64 * share / 100.0) as u64))
}

/// A memory limit (`name`'s): bytes, or a share of `machine` ([`parse_memory`]). A share
/// of memory that wasn't read is refused: the limit couldn't be known, and none is no
/// limit at all.
pub(super) fn parse_memory_limit(text: &str, machine: Reading, name: &str) -> Result<u64> {
    let bytes = parse_memory(text, machine).with_context(|| format!("failed to parse {name}"))?;
    bytes.map_err(|unread| {
        anyhow!(
            "{name} is '{text}', a share of the memory the process may use, which wasn't \
             read ({unread}); set it in bytes, or 0 for no limit"
        )
    })
}

/// The memory this process may use: the container's limit if there is one, else the
/// machine's (`nrese_store::memory::available_bytes`, on Linux and Windows).
pub fn machine_memory_bytes() -> Reading {
    nrese_store::memory::available_bytes()
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

    /// A positive value below one byte or one millisecond would read as 0, which budgets
    /// take as none, and a value past u64 as its largest: both are errors. Every row is
    /// checked; the failures are reported together.
    #[test]
    fn values_below_one_unit_or_past_the_range_are_errors() {
        let mut wrong = Vec::new();
        for text in [
            "0.4",
            "0.4 B",
            "0.0001 KiB",
            "18446744073709551616",
            "17179869184 GiB",
            "99999999999999999999999",
        ] {
            if let Ok(bytes) = parse_size(text) {
                wrong.push(format!("size {text:?} read as {bytes}"));
            }
        }
        for text in ["0.1ms", "0.4", "0.0001 s", "5124095576030432 h"] {
            if let Ok(ms) = parse_duration_ms(text) {
                wrong.push(format!("duration {text:?} read as {ms} ms"));
            }
        }
        // Guards: zero, one unit, a fraction of a larger unit, the largest size.
        for (text, bytes) in [
            ("0", 0u64),
            ("0.0", 0),
            ("1", 1),
            ("0.5 KiB", 512),
            ("18446744073709551615", u64::MAX),
        ] {
            if parse_size(text).ok() != Some(bytes) {
                wrong.push(format!(
                    "size {text:?}: {:?}, not {bytes}",
                    parse_size(text)
                ));
            }
        }
        for (text, ms) in [("0", 0u64), ("1ms", 1), ("0.5 s", 500), ("0.001 s", 1)] {
            if parse_duration_ms(text).ok() != Some(ms) {
                wrong.push(format!(
                    "duration {text:?}: {:?}, not {ms}",
                    parse_duration_ms(text)
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A positive decimal too small for f64 (it parses as 0.0) is still positive: an error,
    /// not 0, which a budget reads as none (review p2-b7-a r1 R1). Every row is checked.
    #[test]
    fn positive_values_too_small_for_a_float_are_errors() {
        let tiny = format!("0.{}1", "0".repeat(400));
        let mut wrong = Vec::new();
        for text in [tiny.clone(), format!("{tiny} B"), format!("{tiny}KiB")] {
            if let Ok(bytes) = parse_size(&text) {
                wrong.push(format!("size {}…: read as {bytes}", &text[..8]));
            }
        }
        for text in [format!("{tiny}ms"), format!("{tiny} h")] {
            if let Ok(ms) = parse_duration_ms(&text) {
                wrong.push(format!("duration {}…: read as {ms} ms", &text[..8]));
            }
        }
        // Guards: lexical zeros, however long, are 0.
        let zeros = format!("0.{}", "0".repeat(400));
        for text in [zeros.clone(), format!("{zeros}ms"), "000".to_owned()] {
            if parse_size(text.trim_end_matches("ms")).ok() != Some(0)
                || parse_duration_ms(&text).ok() != Some(0)
            {
                wrong.push(format!("zero {}…: not 0", &text[..6]));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn memory_is_bytes_or_a_share_of_the_machine() {
        const G: u64 = 1 << 30;
        assert_eq!(parse_memory("8GiB", Ok(16 * G)).unwrap(), Ok(8 * G));
        assert!(parse_memory("150%", Ok(16 * G)).is_err());
        assert!(parse_memory("half", Ok(16 * G)).is_err());
        assert_eq!(parse_memory("50%", Ok(16 * G)).unwrap(), Ok(8 * G));
        let unread = Err(nrese_store::memory::Unread::Failed);
        assert_eq!(parse_memory("50%", unread).unwrap(), unread);
        assert_eq!(parse_memory("8GiB", unread).unwrap(), Ok(8 * G));
    }

    /// DC-07: a memory limit given as a share of memory that couldn't be read is refused,
    /// naming the setting and the way out (bytes, or 0 for none); bytes need no reading.
    #[test]
    fn a_share_of_unread_memory_is_no_limit_and_is_refused() {
        const NAME: &str = "NRESE_PROCESS_MEMORY_BYTES";
        for unread in [
            nrese_store::memory::Unread::Failed,
            nrese_store::memory::Unread::Unsupported,
        ] {
            let error = parse_memory_limit("75%", Err(unread), NAME).unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains(NAME) && message.contains(" 0"),
                "{message}"
            );
            assert_eq!(
                parse_memory_limit("2GiB", Err(unread), NAME).unwrap(),
                2 << 30
            );
            assert_eq!(parse_memory_limit("0", Err(unread), NAME).unwrap(), 0);
        }
    }

    #[test]
    fn sizes_print_in_the_largest_unit_that_fits() {
        assert_eq!(format_size(4 << 30), "4 GiB");
        assert_eq!(format_size(3 << 29), "1.5 GiB");
        assert_eq!(format_size(128 << 20), "128 MiB");
        assert_eq!(format_size(1000), "1000 B");
    }
}
