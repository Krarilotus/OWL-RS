//! Inline value encoding for literals whose value fits into a [`TermId`] payload.
//!
//! A literal is inlined only if its lexical form is the *canonical* form of its value (XSD
//! 1.1 canonical mappings), so decoding reproduces the exact lexical form and RDF term
//! identity is preserved. Every other literal, including valid non-canonical ones such as
//! `"01"^^xsd:integer`, lives in the dictionary.
//!
//! | Kind | Inlined values | Payload |
//! |---|---|---|
//! | `Integer` | `xsd:integer` in the 60-bit range | two's complement |
//! | `Boolean` | `true`, `false` | 1, 0 |
//! | `Decimal` | `xsd:decimal` with \|mantissa\| < 2⁵⁵ and ≤ 15 fraction digits | scale (4 bits), 56-bit two's-complement mantissa |
//! | `Date` | `xsd:date`, years 0000–9999 | year, month, day, timezone |
//! | `DateTime` | `xsd:dateTime`, years 0000–9999, ≤ 3 fraction digits | year … millisecond, timezone |
//!
//! Timezones are inlined when they are a multiple of 15 minutes (all real-world offsets);
//! canonically, a zero offset is written `Z`.

use oxrdf::vocab::xsd;
use oxrdf::{Literal, LiteralRef};

use super::{PAYLOAD_BITS, TermId, TermKind};

const INT_MIN: i64 = -(1 << (PAYLOAD_BITS - 1));
const INT_MAX: i64 = (1 << (PAYLOAD_BITS - 1)) - 1;
const PAYLOAD_MASK: u64 = (1 << PAYLOAD_BITS) - 1;

const DECIMAL_MANTISSA_BITS: u32 = 56;
const DECIMAL_MANTISSA_MASK: u64 = (1 << DECIMAL_MANTISSA_BITS) - 1;
/// Exclusive bound on the absolute mantissa, so it fits 56-bit two's complement.
const DECIMAL_MANTISSA_LIMIT: i64 = 1 << (DECIMAL_MANTISSA_BITS - 1);
const DECIMAL_MAX_SCALE: u32 = 15;

/// Returns the inline id for `literal` if its datatype is inlinable and its lexical form is
/// canonical and in range. O(len(lexical)).
pub(crate) fn try_inline_literal(literal: LiteralRef<'_>) -> Option<TermId> {
    if literal.language().is_some() {
        return None;
    }
    let datatype = literal.datatype();
    let lexical = literal.value();
    let (kind, payload) = if datatype == xsd::INTEGER {
        (TermKind::Integer, encode_integer(lexical)?)
    } else if datatype == xsd::BOOLEAN {
        let payload = match lexical {
            "true" => 1,
            "false" => 0,
            _ => return None,
        };
        (TermKind::Boolean, payload)
    } else if datatype == xsd::DECIMAL {
        (TermKind::Decimal, encode_decimal(lexical)?)
    } else if datatype == xsd::DATE {
        (TermKind::Date, encode_date(lexical)?)
    } else if datatype == xsd::DATE_TIME {
        (TermKind::DateTime, encode_date_time(lexical)?)
    } else {
        return None;
    };
    Some(TermId::new(kind, payload))
}

/// Materialises an inline id back into its literal; `None` for non-inline kinds.
pub(crate) fn inline_to_literal(id: TermId) -> Option<Literal> {
    let payload = id.payload();
    let (lexical, datatype) = match id.kind() {
        TermKind::Integer => (decode_integer(payload).to_string(), xsd::INTEGER),
        TermKind::Boolean => ((payload != 0).to_string(), xsd::BOOLEAN),
        TermKind::Decimal => (decode_decimal(payload), xsd::DECIMAL),
        TermKind::Date => (decode_date(payload), xsd::DATE),
        TermKind::DateTime => (decode_date_time(payload), xsd::DATE_TIME),
        TermKind::DefaultGraph | TermKind::Iri | TermKind::BlankNode | TermKind::Literal => {
            return None;
        }
    };
    Some(Literal::new_typed_literal(lexical, datatype))
}

// --- xsd:integer ------------------------------------------------------------------------

fn encode_integer(lexical: &str) -> Option<u64> {
    let value: i64 = lexical.parse().ok()?;
    ((INT_MIN..=INT_MAX).contains(&value) && is_canonical_integer(lexical))
        .then_some((value as u64) & PAYLOAD_MASK)
}

/// Canonical integer digits: optional '-', no '+', no leading zeros, no "-0".
fn is_canonical_integer(lexical: &str) -> bool {
    let digits = lexical.strip_prefix('-').unwrap_or(lexical);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
        && lexical != "-0"
}

pub(crate) fn decode_integer(payload: u64) -> i64 {
    sign_extend(payload, PAYLOAD_BITS)
}

fn sign_extend(value: u64, bits: u32) -> i64 {
    ((value << (64 - bits)) as i64) >> (64 - bits)
}

// --- xsd:decimal ------------------------------------------------------------------------

/// Canonical decimals (XSD 1.1): digits on both sides of the point, no leading zeros
/// before it, no trailing zeros after it except the single `0` of an integral value
/// (`"1.0"`), no `+`, no `"-0.0"`.
fn encode_decimal(lexical: &str) -> Option<u64> {
    let (negative, unsigned) = match lexical.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, lexical),
    };
    let (integer, fraction) = unsigned.split_once('.')?;
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(integer) || !all_digits(fraction) {
        return None;
    }
    if integer.len() > 1 && integer.starts_with('0') {
        return None;
    }
    let fraction = match fraction {
        "0" => "",
        f if f.ends_with('0') => return None,
        f => f,
    };
    if negative && integer == "0" && fraction.is_empty() {
        return None; // "-0.0"
    }
    let scale = u32::try_from(fraction.len()).ok()?;
    if scale > DECIMAL_MAX_SCALE {
        return None;
    }
    let mut mantissa: i64 = 0;
    for digit in integer.bytes().chain(fraction.bytes()) {
        mantissa = mantissa
            .checked_mul(10)?
            .checked_add(i64::from(digit - b'0'))?;
        if mantissa >= DECIMAL_MANTISSA_LIMIT {
            return None;
        }
    }
    let mantissa = if negative { -mantissa } else { mantissa };
    Some((u64::from(scale) << DECIMAL_MANTISSA_BITS) | (mantissa as u64 & DECIMAL_MANTISSA_MASK))
}

fn decode_decimal(payload: u64) -> String {
    let scale = (payload >> DECIMAL_MANTISSA_BITS) as usize;
    let mantissa = sign_extend(payload & DECIMAL_MANTISSA_MASK, DECIMAL_MANTISSA_BITS);
    let sign = if mantissa < 0 { "-" } else { "" };
    let digits = format!("{:0width$}", mantissa.unsigned_abs(), width = scale + 1);
    if scale == 0 {
        format!("{sign}{digits}.0")
    } else {
        let (integer, fraction) = digits.split_at(digits.len() - scale);
        format!("{sign}{integer}.{fraction}")
    }
}

// --- xsd:date and xsd:dateTime ----------------------------------------------------------

/// Payload layouts, most significant field first:
/// - date: year 14 | month 4 | day 5 | timezone 8 (31 bits)
/// - dateTime: year 14 | month 4 | day 5 | hour 5 | minute 6 | second 6 | millisecond 10 |
///   timezone 8 (58 bits)
const TIMEZONE_BITS: u32 = 8;

/// A fixed-width decimal field at `range` of `s`; `None` if not all digits.
fn digits(s: &str, range: std::ops::Range<usize>) -> Option<u32> {
    let field = s.get(range)?;
    field
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| field.parse().ok())?
}

/// `YYYY-MM-DD` with a year 0000–9999 and a real calendar day; returns the date bits.
fn parse_date_part(s: &str) -> Option<u64> {
    if s.len() != 10 || s.as_bytes()[4] != b'-' || s.as_bytes()[7] != b'-' {
        return None;
    }
    let (year, month, day) = (digits(s, 0..4)?, digits(s, 5..7)?, digits(s, 8..10)?);
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    Some((u64::from(year) << 9) | (u64::from(month) << 5) | u64::from(day))
}

fn format_date_part(bits: u64) -> String {
    let (year, month, day) = (bits >> 9, (bits >> 5) & 0xf, bits & 0x1f);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Proleptic Gregorian calendar, as XSD uses (year 0000 is a leap year).
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Splits a canonical timezone suffix off `s`: `Z`, `±hh:mm` (non-zero, ≤ 14:00, a
/// multiple of 15 minutes), or none. Returns the rest and the 8-bit timezone code
/// (0 = none, otherwise 1 + offset/15min + 56).
fn split_timezone(s: &str) -> Option<(&str, u64)> {
    if let Some(rest) = s.strip_suffix('Z') {
        return Some((rest, 1 + 56));
    }
    let Some(split) = s.len().checked_sub(6) else {
        return Some((s, 0));
    };
    let (rest, zone) = s.split_at(split);
    // Only a `±hh:mm` suffix is a timezone; a date's own `-MM-DD` is not.
    let sign = match (zone.as_bytes()[0], zone.as_bytes()[3]) {
        (b'+', b':') => 1,
        (b'-', b':') => -1,
        _ => return Some((s, 0)),
    };
    let (hours, minutes) = (digits(zone, 1..3)?, digits(zone, 4..6)?);
    let offset = i64::from(hours * 60 + minutes);
    if minutes >= 60 || offset == 0 || offset > 14 * 60 || offset % 15 != 0 {
        return None; // zero must be written "Z"; other offsets aren't inlined
    }
    Some((rest, (1 + 56 + sign * offset / 15) as u64))
}

fn format_timezone(code: u64) -> String {
    if code == 0 {
        return String::new();
    }
    let offset = (code as i64 - 1 - 56) * 15;
    if offset == 0 {
        return "Z".to_owned();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.abs();
    format!("{sign}{:02}:{:02}", offset / 60, offset % 60)
}

fn encode_date(lexical: &str) -> Option<u64> {
    let (date, timezone) = split_timezone(lexical)?;
    Some((parse_date_part(date)? << TIMEZONE_BITS) | timezone)
}

fn decode_date(payload: u64) -> String {
    let timezone = payload & ((1 << TIMEZONE_BITS) - 1);
    format!(
        "{}{}",
        format_date_part(payload >> TIMEZONE_BITS),
        format_timezone(timezone)
    )
}

/// `YYYY-MM-DDThh:mm:ss[.f{1,3}]` plus timezone. Canonical: hour < 24 (`24:00:00` is the
/// next day), no fraction for whole seconds, no trailing zeros in the fraction.
fn encode_date_time(lexical: &str) -> Option<u64> {
    let (rest, timezone) = split_timezone(lexical)?;
    let (date, time) = rest.split_once('T')?;
    let date = parse_date_part(date)?;
    let (clock, millisecond) = match time.split_once('.') {
        None => (time, 0),
        Some((clock, fraction)) => {
            if fraction.is_empty() || fraction.len() > 3 || fraction.ends_with('0') {
                return None;
            }
            let value = digits(fraction, 0..fraction.len())?;
            (clock, value * 10u32.pow(3 - fraction.len() as u32))
        }
    };
    if clock.len() != 8 || clock.as_bytes()[2] != b':' || clock.as_bytes()[5] != b':' {
        return None;
    }
    let (hour, minute, second) = (
        digits(clock, 0..2)?,
        digits(clock, 3..5)?,
        digits(clock, 6..8)?,
    );
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let time = (u64::from(hour) << 22)
        | (u64::from(minute) << 16)
        | (u64::from(second) << 10)
        | u64::from(millisecond);
    Some((((date << 27) | time) << TIMEZONE_BITS) | timezone)
}

fn decode_date_time(payload: u64) -> String {
    let timezone = payload & ((1 << TIMEZONE_BITS) - 1);
    let bits = payload >> TIMEZONE_BITS;
    let (date, time) = (bits >> 27, bits & ((1 << 27) - 1));
    let (hour, minute, second, millisecond) = (
        time >> 22,
        (time >> 16) & 0x3f,
        (time >> 10) & 0x3f,
        time & 0x3ff,
    );
    let fraction = match millisecond {
        0 => String::new(),
        ms => format!(".{ms:03}").trim_end_matches('0').to_owned(),
    };
    format!(
        "{}T{hour:02}:{minute:02}:{second:02}{fraction}{}",
        format_date_part(date),
        format_timezone(timezone)
    )
}

#[cfg(test)]
mod tests {
    use oxrdf::NamedNodeRef;

    use super::*;

    fn inline(lexical: &str, datatype: NamedNodeRef<'_>) -> Option<TermId> {
        try_inline_literal(LiteralRef::new_typed_literal(lexical, datatype))
    }

    fn assert_roundtrips(datatype: NamedNodeRef<'_>, lexicals: &[&str]) {
        for &lexical in lexicals {
            let id = inline(lexical, datatype)
                .unwrap_or_else(|| panic!("{lexical} ^^ {datatype} should inline"));
            let literal = inline_to_literal(id).unwrap();
            assert_eq!(
                (literal.value(), literal.datatype()),
                (lexical, datatype),
                "roundtrip"
            );
        }
    }

    fn assert_not_inlined(datatype: NamedNodeRef<'_>, lexicals: &[&str]) {
        for &lexical in lexicals {
            assert!(
                inline(lexical, datatype).is_none(),
                "{lexical} ^^ {datatype} must stay in the dictionary"
            );
        }
    }

    #[test]
    fn integers() {
        let max = INT_MAX.to_string();
        let min = INT_MIN.to_string();
        assert_roundtrips(
            xsd::INTEGER,
            &["0", "1", "-1", "42", "-123456789", &max, &min],
        );
        let too_big = (INT_MAX as i128 + 1).to_string();
        assert_not_inlined(
            xsd::INTEGER,
            &["01", "+1", "-0", "00", " 1", "", "-", &too_big, "abc"],
        );
    }

    #[test]
    fn booleans() {
        assert_roundtrips(xsd::BOOLEAN, &["true", "false"]);
        assert_not_inlined(xsd::BOOLEAN, &["1", "0", "TRUE"]);
        assert_eq!(
            inline("true", xsd::BOOLEAN).unwrap().as_inline_boolean(),
            Some(true)
        );
    }

    #[test]
    fn decimals() {
        assert_roundtrips(
            xsd::DECIMAL,
            &[
                "0.0",
                "1.0",
                "-1.0",
                "0.5",
                "-0.5",
                "10.25",
                "0.001",
                "-123456.789",
                "36028797018963967.0", // 2^55 - 1
                "0.000000000000001",   // 15 fraction digits
            ],
        );
        assert_not_inlined(
            xsd::DECIMAL,
            &[
                "1",
                "1.",
                ".5",
                "01.0",
                "+1.0",
                "1.50",
                "1.00",
                "-0.0",
                "00.0",
                "1e3",
                "36028797018963968.0", // 2^55
                "0.0000000000000001",  // 16 fraction digits
                "1.0.0",
                "-",
                "",
            ],
        );
    }

    #[test]
    fn dates() {
        assert_roundtrips(
            xsd::DATE,
            &[
                "2026-09-25",
                "0000-01-01",
                "9999-12-31",
                "2024-02-29",
                "2000-02-29",
                "1500-03-01Z",
                "2026-09-25+02:00",
                "2026-09-25-05:30",
                "2026-09-25+14:00",
                "2026-09-25-14:00",
                "2026-09-25+05:45",
            ],
        );
        assert_not_inlined(
            xsd::DATE,
            &[
                "2026-9-25",
                "2026-13-01",
                "2026-00-10",
                "2026-04-31",
                "2023-02-29",
                "1900-02-29",
                "12026-01-01",
                "-0001-01-01",
                "2026-09-25+00:00",
                "2026-09-25-00:00",
                "2026-09-25+14:15",
                "2026-09-25+01:10",
                "2026-09-25+1:00",
                "2026-09-25T00:00:00",
            ],
        );
    }

    #[test]
    fn date_times() {
        assert_roundtrips(
            xsd::DATE_TIME,
            &[
                "2026-09-25T14:03:07",
                "2026-09-25T14:03:07Z",
                "2026-09-25T14:03:07.5",
                "2026-09-25T14:03:07.05+01:00",
                "2026-09-25T14:03:07.123-09:30",
                "0000-01-01T00:00:00",
                "9999-12-31T23:59:59.999+14:00",
            ],
        );
        assert_not_inlined(
            xsd::DATE_TIME,
            &[
                "2026-09-25T24:00:00",
                "2026-09-25T14:60:00",
                "2026-09-25T14:03:60",
                "2026-09-25T14:03:07.",
                "2026-09-25T14:03:07.50",
                "2026-09-25T14:03:07.0",
                "2026-09-25T14:03:07.1234",
                "2026-09-25T14:03",
                "2026-09-25 14:03:07",
                "2026-09-25T14:03:07+00:00",
                "2026-02-30T00:00:00",
            ],
        );
    }

    /// The invariant that matters: whatever gets inlined decodes to exactly its input.
    /// Random near-miss strings over each datatype's alphabet.
    #[test]
    fn anything_inlined_roundtrips_exactly() {
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let seeds = [
            (xsd::DECIMAL, "-1230.4500"),
            (xsd::INTEGER, "-120034"),
            (xsd::DATE, "2024-02-29+05:45"),
            (xsd::DATE_TIME, "2026-09-25T14:03:07.120Z"),
        ];
        let alphabet = b"0123456789-+.:TZ";
        let mut inlined = 0;
        for _ in 0..200_000 {
            let (datatype, seed) = seeds[next(seeds.len() as u64) as usize];
            let mut bytes = seed.as_bytes().to_vec();
            for _ in 0..=next(3) {
                let position = next(bytes.len() as u64 + 1) as usize;
                let byte = alphabet[next(alphabet.len() as u64) as usize];
                match next(3) {
                    0 if position < bytes.len() => bytes[position] = byte,
                    1 if position < bytes.len() => {
                        bytes.remove(position);
                    }
                    _ => bytes.insert(position, byte),
                }
            }
            let lexical = String::from_utf8(bytes).unwrap();
            if let Some(id) = inline(&lexical, datatype) {
                inlined += 1;
                let literal = inline_to_literal(id).unwrap();
                assert_eq!(literal.value(), lexical, "{datatype}");
            }
        }
        assert!(
            inlined > 1_000,
            "the mutations should often stay valid: {inlined}"
        );
    }
}
