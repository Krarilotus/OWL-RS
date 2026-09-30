//! Whether a lexical form is in a datatype's lexical space (`sh:datatype` rejects
//! ill-formed literals of the datatypes SPARQL supports).

use std::str::FromStr;

use oxsdatatypes::{
    Boolean, Date, DateTime, DayTimeDuration, Double, Duration, Float, GDay, GMonth, GMonthDay,
    GYear, GYearMonth, Time, YearMonthDuration,
};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// `[+-]?[0-9]+`
fn is_integer(lexical: &str) -> bool {
    let digits = lexical.strip_prefix(['+', '-']).unwrap_or(lexical);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)`
fn is_decimal(lexical: &str) -> bool {
    let unsigned = lexical.strip_prefix(['+', '-']).unwrap_or(lexical);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty())
}

/// An integer within `min..=max` (`None`: unbounded on that side).
fn integer_within(lexical: &str, min: Option<i128>, max: Option<i128>) -> bool {
    if !is_integer(lexical) {
        return false;
    }
    match i128::from_str(lexical.strip_prefix('+').unwrap_or(lexical)) {
        Ok(value) => min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max),
        // Beyond 128 bits: outside every bounded type, inside an unbounded side.
        Err(_) => {
            if lexical.starts_with('-') {
                min.is_none()
            } else {
                max.is_none()
            }
        }
    }
}

/// Whether `lexical` is a lexical form of `datatype`. Datatypes this function doesn't know
/// accept everything.
pub(crate) fn well_formed(datatype: &str, lexical: &str) -> bool {
    let Some(local) = datatype.strip_prefix(XSD) else {
        return true;
    };
    match local {
        "boolean" => Boolean::from_str(lexical).is_ok(),
        "integer" => is_integer(lexical),
        "decimal" => is_decimal(lexical),
        "float" => Float::from_str(lexical).is_ok(),
        "double" => Double::from_str(lexical).is_ok(),
        "long" => integer_within(lexical, Some(i64::MIN.into()), Some(i64::MAX.into())),
        "int" => integer_within(lexical, Some(i32::MIN.into()), Some(i32::MAX.into())),
        "short" => integer_within(lexical, Some(i16::MIN.into()), Some(i16::MAX.into())),
        "byte" => integer_within(lexical, Some(i8::MIN.into()), Some(i8::MAX.into())),
        "unsignedLong" => integer_within(lexical, Some(0), Some(u64::MAX.into())),
        "unsignedInt" => integer_within(lexical, Some(0), Some(u32::MAX.into())),
        "unsignedShort" => integer_within(lexical, Some(0), Some(u16::MAX.into())),
        "unsignedByte" => integer_within(lexical, Some(0), Some(u8::MAX.into())),
        "nonNegativeInteger" => integer_within(lexical, Some(0), None),
        "positiveInteger" => integer_within(lexical, Some(1), None),
        "nonPositiveInteger" => integer_within(lexical, None, Some(0)),
        "negativeInteger" => integer_within(lexical, None, Some(-1)),
        "date" => Date::from_str(lexical).is_ok(),
        "dateTime" => DateTime::from_str(lexical).is_ok(),
        "time" => Time::from_str(lexical).is_ok(),
        "gYear" => GYear::from_str(lexical).is_ok(),
        "gYearMonth" => GYearMonth::from_str(lexical).is_ok(),
        "gMonth" => GMonth::from_str(lexical).is_ok(),
        "gMonthDay" => GMonthDay::from_str(lexical).is_ok(),
        "gDay" => GDay::from_str(lexical).is_ok(),
        "duration" => Duration::from_str(lexical).is_ok(),
        "dayTimeDuration" => DayTimeDuration::from_str(lexical).is_ok(),
        "yearMonthDuration" => YearMonthDuration::from_str(lexical).is_ok(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::well_formed;

    #[test]
    fn lexical_spaces() {
        let xsd = |local: &str| format!("http://www.w3.org/2001/XMLSchema#{local}");
        for (datatype, lexical, expected) in [
            ("integer", "42", true),
            ("integer", "+0042", true),
            (
                "integer",
                "123456789012345678901234567890123456789012",
                true,
            ),
            ("integer", "4.2", false),
            ("integer", "aldi", false),
            ("integer", "", false),
            ("decimal", "4.", true),
            ("decimal", ".5", true),
            ("decimal", ".", false),
            ("decimal", "1e3", false),
            ("double", "1e3", true),
            ("double", "INF", true),
            ("boolean", "1", true),
            ("boolean", "yes", false),
            ("byte", "127", true),
            ("byte", "128", false),
            ("unsignedByte", "-1", false),
            (
                "nonNegativeInteger",
                "99999999999999999999999999999999999999999999",
                true,
            ),
            ("negativeInteger", "0", false),
            (
                "negativeInteger",
                "-99999999999999999999999999999999999999999999",
                true,
            ),
            ("date", "2026-09-30", true),
            ("date", "2026-13-01", false),
            ("dateTime", "2026-09-30T10:00:00Z", true),
            ("dateTime", "2026-09-30", false),
            ("string", "anything", true),
        ] {
            assert_eq!(
                well_formed(&xsd(datatype), lexical),
                expected,
                "{datatype} {lexical:?}"
            );
        }
        assert!(well_formed("http://example.com/custom", "whatever"));
    }
}
