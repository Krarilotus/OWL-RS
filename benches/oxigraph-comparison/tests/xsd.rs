//! `nrese_xsd` against `oxsdatatypes` on generated lexical forms and operations. Where
//! they differ by design, the difference is checked against the specification instead
//! (see `docs/plan/2026-10-01-oxigraph-migration.md`, `nrese-xsd`).

use std::collections::BTreeMap;
use std::fmt::Display;
use std::str::FromStr;

use oxigraph_comparison::{decimal_lexical, numeric_lexical, rng, temporal_lexical};

#[derive(Default)]
struct Differences(BTreeMap<String, Vec<String>>);

impl Differences {
    fn add(&mut self, kind: &str, example: String) {
        let examples = self.0.entry(kind.to_owned()).or_default();
        if examples.len() < 6 {
            examples.push(example);
        }
    }

    fn assert_empty(&self, what: &str) {
        let report: Vec<String> = self
            .0
            .iter()
            .map(|(kind, examples)| format!("{kind}:\n    {}", examples.join("\n    ")))
            .collect();
        assert!(self.0.is_empty(), "{what}\n{}", report.join("\n"));
    }
}

/// Parses `text` with both, and compares acceptance and output.
fn compare<A, B>(
    datatype: &str,
    text: &str,
    differences: &mut Differences,
    explain: impl Fn(&str, Option<&str>, Option<&str>) -> bool,
) where
    A: FromStr + Display,
    B: FromStr + Display,
{
    let ours = A::from_str(text).ok().map(|v| v.to_string());
    let theirs = B::from_str(text).ok().map(|v| v.to_string());
    if ours != theirs && !explain(text, ours.as_deref(), theirs.as_deref()) {
        differences.add(
            &format!(
                "{datatype}: {}",
                match (&ours, &theirs) {
                    (Some(_), None) => "accepted only by nrese",
                    (None, Some(_)) => "accepted only by oxsdatatypes",
                    _ => "written differently",
                }
            ),
            format!("{text:?}: nrese {ours:?}, oxsdatatypes {theirs:?}"),
        );
    }
}

/// Rust's float parser, which `oxsdatatypes` uses, takes `inf`, `infinity` and `nan` in
/// any case and `+NaN`; XSD takes only `INF`, `+INF`, `-INF` and `NaN`.
fn rust_only_special(text: &str) -> bool {
    let unsigned = text.trim_start_matches(['+', '-']).to_ascii_lowercase();
    matches!(unsigned.as_str(), "inf" | "infinity" | "nan")
        && !matches!(text, "INF" | "+INF" | "-INF" | "NaN")
}

/// XPath writes magnitudes below 10⁻⁶ and from 10⁶ up in scientific notation (`1.0E6`);
/// `oxsdatatypes` writes Rust's positional form (`1000000`). The value must be the same.
fn scientific_by_xpath(ours: Option<&str>, theirs: Option<&str>) -> bool {
    match (ours, theirs) {
        (Some(a), Some(b)) => a.contains('E') && a.parse::<f64>().ok() == b.parse::<f64>().ok(),
        _ => false,
    }
}

#[test]
fn numbers_parse_and_print_alike() {
    let mut rng = rng(1);
    let mut differences = Differences::default();
    for _ in 0..300_000 {
        let text = numeric_lexical(&mut rng);
        compare::<nrese_xsd::Integer, oxsdatatypes::Integer>(
            "integer",
            &text,
            &mut differences,
            |_, _, _| false,
        );
        // Decimals: oxsdatatypes rejects more than 18 fractional digits, unless they are
        // zeros; nrese-xsd truncates them (the lexical form is valid XSD).
        compare::<nrese_xsd::Decimal, oxsdatatypes::Decimal>(
            "decimal",
            &text,
            &mut differences,
            |text, ours, theirs| {
                let fraction = text.split_once('.').map_or("", |(_, f)| f);
                ours.is_some() && theirs.is_none() && fraction.trim_end_matches('0').len() > 18
            },
        );
        compare::<nrese_xsd::Double, oxsdatatypes::Double>(
            "double",
            &text,
            &mut differences,
            |text, ours, theirs| {
                (ours.is_none() && rust_only_special(text)) || scientific_by_xpath(ours, theirs)
            },
        );
        compare::<nrese_xsd::Float, oxsdatatypes::Float>(
            "float",
            &text,
            &mut differences,
            |text, ours, theirs| {
                (ours.is_none() && rust_only_special(text)) || scientific_by_xpath(ours, theirs)
            },
        );
        compare::<nrese_xsd::Boolean, oxsdatatypes::Boolean>(
            "boolean",
            &text,
            &mut differences,
            |_, _, _| false,
        );
    }
    differences.assert_empty("numbers");
}

#[test]
fn dates_times_and_durations_parse_and_print_alike() {
    let mut rng = rng(2);
    let mut differences = Differences::default();
    // Each is a departure of oxsdatatypes from XSD 1.1 that nrese-xsd doesn't share, or a
    // documented choice of nrese-xsd; each is checked against the rule, not waved through.
    let long_fraction = |text: &str, ours: Option<&str>, theirs: Option<&str>| {
        // oxsdatatypes rejects fractional seconds past 18 digits; nrese-xsd truncates them.
        (ours.is_some() && theirs.is_none() && text.contains(".0000000000000000001"))
            // §3.3.7: hour 24 only as 24:00:00 (endOfDayFrag); oxsdatatypes takes 24:18:00.
            || (ours.is_none() && theirs.is_some() && end_of_day_misused(text))
            // §3.3.6: a T must be followed by a time part; oxsdatatypes takes P9YT.
            || (ours.is_none() && theirs.is_some() && text.ends_with('T'))
            // §3.3.12: --02-29 is a gMonthDay, with or without a timezone.
            || (ours.is_some() && theirs.is_none() && text.starts_with("--02-29"))
            // Years up to 0 with fractional seconds: oxsdatatypes writes the next minute
            // (-0045-01-03T04:13:59.5 as ...04:14:59.5); nrese-xsd writes what was read.
            || (ours == Some(text.replace("+00:00", "Z").as_str()) && (text.starts_with('-') || text.starts_with("0000")))
    };
    for _ in 0..300_000 {
        let text = temporal_lexical(&mut rng);
        compare::<nrese_xsd::DateTime, oxsdatatypes::DateTime>(
            "dateTime",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::Date, oxsdatatypes::Date>(
            "date",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::Time, oxsdatatypes::Time>(
            "time",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::GYearMonth, oxsdatatypes::GYearMonth>(
            "gYearMonth",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::GYear, oxsdatatypes::GYear>(
            "gYear",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::GMonthDay, oxsdatatypes::GMonthDay>(
            "gMonthDay",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::GDay, oxsdatatypes::GDay>(
            "gDay",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::GMonth, oxsdatatypes::GMonth>(
            "gMonth",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::Duration, oxsdatatypes::Duration>(
            "duration",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::YearMonthDuration, oxsdatatypes::YearMonthDuration>(
            "yearMonthDuration",
            &text,
            &mut differences,
            long_fraction,
        );
        compare::<nrese_xsd::DayTimeDuration, oxsdatatypes::DayTimeDuration>(
            "dayTimeDuration",
            &text,
            &mut differences,
            long_fraction,
        );
    }
    differences.assert_empty("dates, times and durations");
}

/// An hour of 24 with minutes or seconds other than zero.
fn end_of_day_misused(text: &str) -> bool {
    let time = text.split_once('T').map_or(text, |(_, t)| t);
    time.starts_with("24:") && !time.starts_with("24:00:00")
        || time.starts_with("24:00:00.")
            && time[9..]
                .trim_start_matches('0')
                .starts_with(|c: char| c.is_ascii_digit())
}

/// Decimal arithmetic. Where the two differ, ours must equal the exact result truncated
/// to 18 fractional digits (computed here with integers), and theirs must not.
#[test]
fn decimal_arithmetic_is_exact_to_truncation() {
    let mut rng = rng(3);
    let mut differences = Differences::default();
    for _ in 0..200_000 {
        let (a, b) = (decimal_lexical(&mut rng), decimal_lexical(&mut rng));
        let (x, y): (nrese_xsd::Decimal, nrese_xsd::Decimal) =
            (a.parse().unwrap(), b.parse().unwrap());
        let (p, q): (oxsdatatypes::Decimal, oxsdatatypes::Decimal) =
            (a.parse().unwrap(), b.parse().unwrap());
        for (op, ours, theirs) in [
            (
                "+",
                x.checked_add(y).map(|v| v.to_string()),
                p.checked_add(q).map(|v| v.to_string()),
            ),
            (
                "-",
                x.checked_sub(y).map(|v| v.to_string()),
                p.checked_sub(q).map(|v| v.to_string()),
            ),
            (
                "*",
                x.checked_mul(y).map(|v| v.to_string()),
                p.checked_mul(q).map(|v| v.to_string()),
            ),
            (
                "/",
                x.checked_div(y).map(|v| v.to_string()),
                p.checked_div(q).map(|v| v.to_string()),
            ),
        ] {
            if ours == theirs {
                continue;
            }
            // Past what i128 holds here the exact result isn't computed: not judged.
            let Some(exact) = exact(&a, op, &b) else {
                continue;
            };
            if ours != exact {
                differences.add(
                    &format!("{op}: nrese not exact"),
                    format!(
                        "{a} {op} {b}: nrese {ours:?}, exact {exact:?}, oxsdatatypes {theirs:?}"
                    ),
                );
            }
        }
        for (name, ours, theirs) in [
            (
                "round",
                x.checked_round(),
                p.checked_round().map(|v| v.to_string().parse().unwrap()),
            ),
            (
                "ceil",
                x.checked_ceil(),
                p.checked_ceil().map(|v| v.to_string().parse().unwrap()),
            ),
            (
                "floor",
                x.checked_floor(),
                p.checked_floor().map(|v| v.to_string().parse().unwrap()),
            ),
        ] {
            // fn:round is floor(x + 0.5) (halves towards positive infinity); oxsdatatypes
            // looks at the first fractional digit only (round(-8.52) = -8).
            let by_definition = x
                .checked_add("0.5".parse::<nrese_xsd::Decimal>().unwrap())
                .and_then(|v| v.checked_floor());
            if ours != theirs && !(name == "round" && ours == by_definition) {
                differences.add(
                    name,
                    format!("{a}: nrese {ours:?}, oxsdatatypes {theirs:?}"),
                );
            }
        }
        let (ours, theirs) = (
            f64::from(nrese_xsd::Double::from(x)),
            f64::from(oxsdatatypes::Double::from(p)),
        );
        let correctly_rounded: f64 = a.parse().unwrap();
        if ours != theirs && ours != correctly_rounded {
            differences.add(
                "to double: nrese not correctly rounded",
                format!("{a}: nrese {ours:e}, oxsdatatypes {theirs:e}"),
            );
        }
    }
    differences.assert_empty("decimal arithmetic");
}

/// `a op b` truncated to 18 fractional digits (`Some(None)`: no result, as for a zero
/// divisor), or `None` where an intermediate exceeds `i128`.
fn exact(a: &str, op: &str, b: &str) -> Option<Option<String>> {
    let scale = |t: &str| -> i128 {
        let (negative, t) = t.strip_prefix('-').map_or((false, t), |t| (true, t));
        let (whole, fraction) = t.split_once('.').unwrap_or((t, ""));
        let mut digits = format!("{whole}{fraction}");
        for _ in fraction.len()..11 {
            digits.push('0');
        }
        let v: i128 = digits.parse().unwrap();
        if negative { -v } else { v }
    };
    // Values times 10^11.
    let (x, y) = (scale(a), scale(b));
    let e18 = 1_000_000_000_000_000_000_i128;
    let raw = match op {
        "+" => (x + y) * 10_000_000,
        "-" => (x - y) * 10_000_000,
        // x*y is the value times 10^22: divide by 10^4 to get 10^18, truncating.
        "*" => x.checked_mul(y)? / 10_000,
        _ => {
            if y == 0 {
                return Some(None);
            }
            // (x / y) * 10^18, from x * 10^18 / y when that fits.
            x.checked_mul(e18)? / y
        }
    };
    let text = format!(
        "{}{}.{:018}",
        if raw < 0 { "-" } else { "" },
        raw.unsigned_abs() / e18 as u128,
        raw.unsigned_abs() % e18 as u128
    );
    Some(
        nrese_xsd::Decimal::from_str(&text)
            .ok()
            .map(|d| d.to_string()),
    )
}
