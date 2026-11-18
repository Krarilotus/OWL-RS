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

use std::fmt;

use nrese_rdf::vocab::xsd;
use nrese_rdf::{Literal, LiteralRef};

use super::{PAYLOAD_BITS, TermId, TermKind};

const INT_MIN: i64 = -(1 << (PAYLOAD_BITS - 1));
const INT_MAX: i64 = (1 << (PAYLOAD_BITS - 1)) - 1;

const DECIMAL_MANTISSA_BITS: u32 = 56;
const DECIMAL_MANTISSA_MASK: u64 = (1 << DECIMAL_MANTISSA_BITS) - 1;
/// Exclusive bound on the absolute mantissa, so it fits 56-bit two's complement.
const DECIMAL_MANTISSA_LIMIT: i64 = 1 << (DECIMAL_MANTISSA_BITS - 1);
const DECIMAL_MAX_SCALE: u32 = 15;

/// Returns the inline id for `literal` if its datatype is inlinable and its lexical form is
/// canonical and in range. Integer-derived datatypes only with `derived` (a store that
/// keeps them in its dictionary passes `false`). O(len(lexical)).
pub(crate) fn try_inline_literal(literal: LiteralRef<'_>, derived: bool) -> Option<TermId> {
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
    } else if derived && let Some(code) = DERIVED.iter().position(|d| d.0 == datatype) {
        (TermKind::DerivedInteger, encode_derived(lexical, code)?)
    } else {
        return None;
    };
    Some(TermId::new(kind, payload))
}

/// Materialises an inline id back into its literal; `None` for non-inline kinds. It allocates
/// exactly [`inline_bound`]: its lexical form, in a `String` of [`lexical_len`]'s capacity,
/// written in place, and its datatype's IRI.
pub(crate) fn inline_to_literal(id: TermId) -> Option<Literal> {
    let datatype = inline_datatype(id)?;
    let mut lexical = String::with_capacity(lexical_len(id)?);
    write_lexical(id, &mut lexical)?;
    Some(Literal::new_typed_literal(lexical, datatype))
}

/// The bytes [`inline_to_literal`] allocates for `id`, exactly, allocating nothing; `None`
/// where it gives no literal.
pub(crate) fn inline_bound(id: TermId) -> Option<usize> {
    Some(inline_datatype(id)?.as_str().len() + lexical_len(id)?)
}

/// The length of the inline literal `id`'s lexical form, counted as [`write_lexical`] writes
/// it, with nothing allocated.
fn lexical_len(id: TermId) -> Option<usize> {
    struct Count(usize);
    impl fmt::Write for Count {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            self.0 += s.len();
            Ok(())
        }
    }
    let mut count = Count(0);
    write_lexical(id, &mut count)?;
    Some(count.0)
}

/// Writes the inline literal `id`'s lexical form to `out`, straight from its payload, through
/// no string of its own; `None` for non-inline kinds (or where `out` fails, which a `String`
/// and [`lexical_len`]'s count never do). The one formatting of every inline kind: its count
/// and its text can't differ.
fn write_lexical(id: TermId, out: &mut impl fmt::Write) -> Option<()> {
    let payload = id.payload();
    match id.kind() {
        TermKind::Integer => write!(out, "{}", decode_integer(payload)),
        TermKind::Boolean => write!(out, "{}", payload != 0),
        TermKind::Decimal => write_decimal(payload, out),
        TermKind::Date => write_date(payload, out),
        TermKind::DateTime => write_date_time(payload, out),
        TermKind::DerivedInteger => write!(out, "{}", decode_derived(payload).0),
        TermKind::DefaultGraph
        | TermKind::Iri
        | TermKind::BlankNode
        | TermKind::String
        | TermKind::LangString
        | TermKind::TypedLiteral
        | TermKind::Triple => return None,
    }
    .ok()
}

/// The datatype of the inline literal `id`; `None` for non-inline kinds and an unknown
/// derived datatype's code.
fn inline_datatype(id: TermId) -> Option<nrese_rdf::NamedNodeRef<'static>> {
    Some(match id.kind() {
        TermKind::Integer => xsd::INTEGER,
        TermKind::Boolean => xsd::BOOLEAN,
        TermKind::Decimal => xsd::DECIMAL,
        TermKind::Date => xsd::DATE,
        TermKind::DateTime => xsd::DATE_TIME,
        TermKind::DerivedInteger => DERIVED.get(decode_derived(id.payload()).1)?.0,
        TermKind::DefaultGraph
        | TermKind::Iri
        | TermKind::BlankNode
        | TermKind::String
        | TermKind::LangString
        | TermKind::TypedLiteral
        | TermKind::Triple => return None,
    })
}

// --- xsd:integer ------------------------------------------------------------------------

/// Offset binary (`value - INT_MIN`), so payload order is numeric order and a FILTER range
/// over integers is an id range.
fn encode_integer(lexical: &str) -> Option<u64> {
    let value: i64 = lexical.parse().ok()?;
    ((INT_MIN..=INT_MAX).contains(&value) && is_canonical_integer(lexical))
        .then(|| value.wrapping_sub(INT_MIN) as u64)
}

/// The id of an inline integer, if `value` is in the inline range. Executors use it to turn
/// numeric bounds into id bounds.
pub(crate) fn integer_id(value: i64) -> Option<TermId> {
    (INT_MIN..=INT_MAX)
        .contains(&value)
        .then(|| TermId::new(TermKind::Integer, value.wrapping_sub(INT_MIN) as u64))
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
    (payload as i64).wrapping_add(INT_MIN)
}

// --- datatypes derived from xsd:integer -----------------------------------------------------

/// The integer-derived datatypes, by code, with their value spaces (XSD 1.1 §3.4.14-3.4.26).
const DERIVED: [(nrese_rdf::NamedNodeRef<'static>, i64, i64); 12] = [
    (xsd::NON_POSITIVE_INTEGER, i64::MIN, 0),
    (xsd::NEGATIVE_INTEGER, i64::MIN, -1),
    (xsd::LONG, i64::MIN, i64::MAX),
    (xsd::INT, i32::MIN as i64, i32::MAX as i64),
    (xsd::SHORT, i16::MIN as i64, i16::MAX as i64),
    (xsd::BYTE, i8::MIN as i64, i8::MAX as i64),
    (xsd::NON_NEGATIVE_INTEGER, 0, i64::MAX),
    (xsd::UNSIGNED_LONG, 0, i64::MAX),
    (xsd::UNSIGNED_INT, 0, u32::MAX as i64),
    (xsd::UNSIGNED_SHORT, 0, u16::MAX as i64),
    (xsd::UNSIGNED_BYTE, 0, u8::MAX as i64),
    (xsd::POSITIVE_INTEGER, 1, i64::MAX),
];
/// Bits of the datatype code, below the value.
const DERIVED_CODE_BITS: u32 = 4;
/// The inline value range: the payload's bits above the code, offset binary.
const DERIVED_MIN: i64 = -(1 << (PAYLOAD_BITS - DERIVED_CODE_BITS - 1));
const DERIVED_MAX: i64 = (1 << (PAYLOAD_BITS - DERIVED_CODE_BITS - 1)) - 1;

/// A canonical literal of the derived datatype `code`, valid for it and in the inline
/// range: the value (offset binary) above the code. Others stay in the dictionary, as
/// they are (an ill-typed `"300"^^xsd:byte` included).
fn encode_derived(lexical: &str, code: usize) -> Option<u64> {
    let (_, min, max) = DERIVED[code];
    let value: i64 = lexical.parse().ok()?;
    ((min..=max).contains(&value)
        && (DERIVED_MIN..=DERIVED_MAX).contains(&value)
        && is_canonical_integer(lexical))
    .then(|| (value.wrapping_sub(DERIVED_MIN) as u64) << DERIVED_CODE_BITS | code as u64)
}

/// The value and the datatype code of a derived integer's payload.
pub(crate) fn decode_derived(payload: u64) -> (i64, usize) {
    let value = ((payload >> DERIVED_CODE_BITS) as i64).wrapping_add(DERIVED_MIN);
    (value, (payload & ((1 << DERIVED_CODE_BITS) - 1)) as usize)
}

/// The derived-integer ids with a value in `low..=high` (clamped to the inline range).
pub(crate) fn derived_range(low: i64, high: i64) -> Option<(TermId, TermId)> {
    let (low, high) = (low.max(DERIVED_MIN), high.min(DERIVED_MAX));
    (low <= high).then(|| {
        let at = |value: i64, code: u64| {
            TermId::new(
                TermKind::DerivedInteger,
                (value.wrapping_sub(DERIVED_MIN) as u64) << DERIVED_CODE_BITS | code,
            )
        };
        (at(low, 0), at(high, (1 << DERIVED_CODE_BITS) - 1))
    })
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

/// The mantissa's digits with a point `scale` digits from the right: its integer part (at
/// least `0`), then its fraction, `scale` digits zero-padded (`.0` for scale 0).
fn write_decimal(payload: u64, out: &mut impl fmt::Write) -> fmt::Result {
    let scale = (payload >> DECIMAL_MANTISSA_BITS) as u32;
    let mantissa = sign_extend(payload & DECIMAL_MANTISSA_MASK, DECIMAL_MANTISSA_BITS);
    let sign = if mantissa < 0 { "-" } else { "" };
    let digits = mantissa.unsigned_abs();
    if scale == 0 {
        return write!(out, "{sign}{digits}.0");
    }
    // The scale is 4 bits, at most 15: 10^15 fits a u64.
    let unit = 10u64.pow(scale);
    let width = scale as usize;
    write!(
        out,
        "{sign}{}.{:0width$}",
        digits / unit,
        digits % unit,
        width = width
    )
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

/// The timezone field of a date or dateTime payload (the lowest bits of both).
pub(crate) fn timezone_code(payload: u64) -> u64 {
    payload & ((1 << TIMEZONE_BITS) - 1)
}

/// A `Date` id bound one local day before (`later == false`) or after `id`, for FILTER range
/// scans: a date with a timezone and one without compare indeterminately when their local
/// dates are less than a day (±14 h) apart, so a range over dates is widened by a day and
/// its edges are checked by the full comparison. The bound needn't be a real date; it only
/// has to order correctly (fields are compared as year, month, day, timezone).
pub(crate) fn date_widened(id: TermId, later: bool) -> Option<TermId> {
    if id.kind() != TermKind::Date {
        return None;
    }
    let date = id.payload() >> TIMEZONE_BITS;
    let (year, month, day) = (date >> 9, (date >> 5) & 0xf, date & 0x1f);
    let last_day = |y: u64, m: u64| u64::from(days_in_month(y as u32, m as u32));
    let (year, month, day, timezone) = if later {
        let (y, m, d) = if day < last_day(year, month) {
            (year, month, day + 1)
        } else if month < 12 {
            (year, month + 1, 1)
        } else {
            (year + 1, 1, 1)
        };
        (y, m, d, (1 << TIMEZONE_BITS) - 1)
    } else {
        let (y, m, d) = if day > 1 {
            (year, month, day - 1)
        } else if month > 1 {
            (year, month - 1, last_day(year, month - 1))
        } else if year > 0 {
            (year - 1, 12, 31)
        } else {
            (0, 0, 0)
        };
        (y, m, d, 0)
    };
    Some(TermId::new(
        TermKind::Date,
        (((year << 9) | (month << 5) | day) << TIMEZONE_BITS) | timezone,
    ))
}

/// A `DateTime` id bound two local days before (`later == false`) or after `id`, at its
/// clock and fraction, with the smallest or largest timezone code, for FILTER range scans.
/// Inline dateTimes sort by local date and time, then timezone code, but compare by instant
/// (with a timezone on one side only, within ±14 h of it). Both sides can be ±14 h off UTC,
/// so a value that passes can lie up to 28 h of local time beyond the bound: under the
/// engine's order and under XPath's implicit timezone alike. Two days cover that, and the
/// full comparison decides the edges. The days step as [`date_widened`]'s, across months,
/// leap days and years; past the inline years its bound is below 0000-01-01 or in year
/// 10000, beyond every inline dateTime.
pub(crate) fn date_time_widened(id: TermId, later: bool) -> Option<TermId> {
    if id.kind() != TermKind::DateTime {
        return None;
    }
    let bits = id.payload() >> TIMEZONE_BITS;
    let (date, time) = (bits >> 27, bits & ((1 << 27) - 1));
    // The date as a `Date` id, moved a day at a time by the date code.
    let mut day = TermId::new(TermKind::Date, date << TIMEZONE_BITS);
    for _ in 0..2 {
        day = date_widened(day, later)?;
    }
    let date = day.payload() >> TIMEZONE_BITS;
    let timezone = if later { (1 << TIMEZONE_BITS) - 1 } else { 0 };
    Some(TermId::new(
        TermKind::DateTime,
        (((date << 27) | time) << TIMEZONE_BITS) | timezone,
    ))
}

/// Bits below the year in a date payload (month, day, timezone) and in a dateTime payload
/// (month, day, time of day, timezone).
const DATE_YEAR_SHIFT: u32 = 9 + TIMEZONE_BITS;
const DATE_TIME_YEAR_SHIFT: u32 = 9 + 27 + TIMEZONE_BITS;

/// See [`TermId::year_range`].
pub(crate) fn year_range(kind: TermKind, low: u32, high: u32) -> Option<(TermId, TermId)> {
    let shift = match kind {
        TermKind::Date => DATE_YEAR_SHIFT,
        TermKind::DateTime => DATE_TIME_YEAR_SHIFT,
        _ => return None,
    };
    if low > high || high > 9999 {
        return None;
    }
    Some((
        TermId::new(kind, u64::from(low) << shift),
        TermId::new(kind, ((u64::from(high) + 1) << shift) - 1),
    ))
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

fn write_date_part(bits: u64, out: &mut impl fmt::Write) -> fmt::Result {
    let (year, month, day) = (bits >> 9, (bits >> 5) & 0xf, bits & 0x1f);
    write!(out, "{year:04}-{month:02}-{day:02}")
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
    // Only a `±hh:mm` suffix is a timezone; a date's own `-MM-DD` is not. The sign is
    // read as a byte first: an ASCII sign byte is always a char boundary, so the split
    // below cannot land inside a multi-byte character of an ill-typed lexical form.
    let bytes = s.as_bytes();
    let sign = match (bytes[split], bytes[split + 3]) {
        (b'+', b':') => 1,
        (b'-', b':') => -1,
        _ => return Some((s, 0)),
    };
    let (rest, zone) = s.split_at(split);
    let (hours, minutes) = (digits(zone, 1..3)?, digits(zone, 4..6)?);
    let offset = i64::from(hours * 60 + minutes);
    if minutes >= 60 || offset == 0 || offset > 14 * 60 || offset % 15 != 0 {
        return None; // zero must be written "Z"; other offsets aren't inlined
    }
    Some((rest, (1 + 56 + sign * offset / 15) as u64))
}

fn write_timezone(code: u64, out: &mut impl fmt::Write) -> fmt::Result {
    if code == 0 {
        return Ok(());
    }
    let offset = (code as i64 - 1 - 56) * 15;
    if offset == 0 {
        return out.write_str("Z");
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.abs();
    write!(out, "{sign}{:02}:{:02}", offset / 60, offset % 60)
}

fn encode_date(lexical: &str) -> Option<u64> {
    let (date, timezone) = split_timezone(lexical)?;
    Some((parse_date_part(date)? << TIMEZONE_BITS) | timezone)
}

fn write_date(payload: u64, out: &mut impl fmt::Write) -> fmt::Result {
    write_date_part(payload >> TIMEZONE_BITS, out)?;
    write_timezone(payload & ((1 << TIMEZONE_BITS) - 1), out)
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

fn write_date_time(payload: u64, out: &mut impl fmt::Write) -> fmt::Result {
    let timezone = payload & ((1 << TIMEZONE_BITS) - 1);
    let bits = payload >> TIMEZONE_BITS;
    let (date, time) = (bits >> 27, bits & ((1 << 27) - 1));
    let (hour, minute, second, millisecond) = (
        time >> 22,
        (time >> 16) & 0x3f,
        (time >> 10) & 0x3f,
        time & 0x3ff,
    );
    write_date_part(date, out)?;
    write!(out, "T{hour:02}:{minute:02}:{second:02}")?;
    if millisecond > 0 {
        // `.` and the milliseconds in at least three digits, their trailing zeros cut.
        let (mut value, mut width) = (millisecond, if millisecond >= 1000 { 4 } else { 3 });
        while value % 10 == 0 {
            value /= 10;
            width -= 1;
        }
        write!(out, ".{value:0width$}")?;
    }
    write_timezone(timezone, out)
}

#[cfg(test)]
mod tests {
    use nrese_rdf::NamedNodeRef;

    use super::*;

    fn inline(lexical: &str, datatype: NamedNodeRef<'_>) -> Option<TermId> {
        try_inline_literal(LiteralRef::new_typed_literal(lexical, datatype), true)
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

    /// Id order is numeric order, which lets executors turn a numeric FILTER into an id
    /// range; `integer_id` agrees with the lexical path.
    #[test]
    fn integer_ids_sort_by_value() {
        let mut values = vec![
            INT_MIN,
            INT_MIN + 1,
            -1_000_000,
            -2,
            -1,
            0,
            1,
            2,
            7,
            1 << 40,
            INT_MAX,
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            values.push((state as i64) >> 4); // uniform over the 60-bit range
        }
        let mut ids: Vec<(i64, TermId)> = values
            .iter()
            .map(|&v| {
                let id = inline(&v.to_string(), xsd::INTEGER).expect("in range");
                assert_eq!(integer_id(v), Some(id));
                assert_eq!(id.as_inline_integer(), Some(v));
                (v, id)
            })
            .collect();
        ids.sort_by_key(|&(_, id)| id);
        assert!(
            ids.windows(2).all(|w| w[0].0 <= w[1].0),
            "id order must be value order"
        );
        assert_eq!(integer_id(INT_MAX + 1), None);
        assert_eq!(integer_id(INT_MIN - 1), None);
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

    /// The widened bounds enclose every date of the neighbouring local days, whatever its
    /// timezone, across month and year boundaries.
    #[test]
    fn widened_date_bounds_enclose_neighbouring_days() {
        let id = |s: &str| inline(s, xsd::DATE).unwrap();
        for (date, previous, next) in [
            ("2000-03-01", "2000-02-29", "2000-03-02"),
            ("2000-01-01", "1999-12-31", "2000-01-02"),
            ("1999-12-31", "1999-12-30", "2000-01-01"),
            ("2001-04-30", "2001-04-29", "2001-05-01"),
        ] {
            let low = date_widened(id(date), false).unwrap();
            let high = date_widened(id(date), true).unwrap();
            for zone in ["", "Z", "+14:00", "-14:00", "+05:30"] {
                let p = id(&format!("{previous}{zone}"));
                let n = id(&format!("{next}{zone}"));
                let d = id(&format!("{date}{zone}"));
                assert!(
                    low <= p && p <= high && low <= d && d <= high && low <= n && n <= high,
                    "{date}{zone}"
                );
            }
            assert!(date_widened(id("2000-01-05"), false).unwrap() > id("2000-01-03Z"));
            assert!(date_widened(id("2000-01-05"), true).unwrap() < id("2000-01-07"));
        }
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
    fn year_ranges_hold_exactly_the_years_dates() {
        for (lexical, datatype, year) in [
            ("1879-03-14", xsd::DATE, 1879),
            ("1879-12-31-14:00", xsd::DATE, 1879),
            ("1879-01-01+14:00", xsd::DATE, 1879),
            ("1879-12-31T23:59:59.999-14:00", xsd::DATE_TIME, 1879),
            ("1879-01-01T00:00:00Z", xsd::DATE_TIME, 1879),
            ("0000-01-01T00:00:00", xsd::DATE_TIME, 0),
            ("9999-12-31", xsd::DATE, 9999),
        ] {
            let id =
                try_inline_literal(LiteralRef::new_typed_literal(lexical, datatype), true).unwrap();
            let within = |low, high| {
                let (first, last) = year_range(id.kind(), low, high).unwrap();
                first <= id && id <= last
            };
            assert!(within(year, year), "{lexical}");
            if year < 9999 {
                assert!(!within(year + 1, 9999), "{lexical}");
            }
            if year > 0 {
                assert!(!within(0, year - 1), "{lexical}");
            }
        }
        assert!(year_range(TermKind::Integer, 0, 1).is_none());
        assert!(year_range(TermKind::Date, 0, 10_000).is_none());
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

    /// RDF 1.1 Concepts §3.3: a literal whose lexical form is not in its datatype's
    /// lexical space is ill-typed but still a legal term, so it stays in the dictionary
    /// with its lexical form. In each row a multi-byte character covers the byte where a
    /// `±hh:mm` timezone would start (`"éabcde"` is 7 bytes, `len - 6 == 1` is inside `é`).
    #[test]
    fn non_ascii_lexicals_stay_in_the_dictionary() {
        let lexicals = [
            "éabcde",
            "2026-09-2é+5:00",
            "x€bcdef",
            "2026-09-25T14:03:0é+1:00",
        ];
        // Every row is checked, so a failure lists each wrong one (inlined or a panic).
        let wrong: Vec<String> = [xsd::DATE, xsd::DATE_TIME]
            .into_iter()
            .flat_map(|datatype| lexicals.map(|lexical| (lexical, datatype)))
            .filter_map(|(lexical, datatype)| {
                match std::panic::catch_unwind(|| inline(lexical, datatype)) {
                    Ok(None) => None,
                    Ok(Some(_)) => Some(format!("{lexical} ^^ {datatype} inlined")),
                    Err(_) => Some(format!("{lexical} ^^ {datatype} panicked")),
                }
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
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

    #[test]
    fn integer_derived_literals_are_inline_and_sort_by_value() {
        let id = |lexical: &str, datatype| {
            try_inline_literal(LiteralRef::new_typed_literal(lexical, datatype), true)
        };
        let values = [
            ("-129", xsd::LONG),
            ("-5", xsd::INT),
            ("0", xsd::NON_NEGATIVE_INTEGER),
            ("0", xsd::UNSIGNED_BYTE),
            ("7", xsd::BYTE),
            ("300", xsd::INT),
            ("70000", xsd::UNSIGNED_INT),
        ];
        let ids: Vec<TermId> = values.iter().map(|&(l, d)| id(l, d).unwrap()).collect();
        assert!(ids.is_sorted(), "ids sort by value: {ids:?}");
        for (&(lexical, datatype), &id) in values.iter().zip(&ids) {
            assert_eq!(id.kind(), TermKind::DerivedInteger);
            assert_eq!(id.as_inline_integer(), Some(lexical.parse().unwrap()));
            let literal = inline_to_literal(id).unwrap();
            assert_eq!((literal.value(), literal.datatype()), (lexical, datatype));
        }
        // Not canonical, outside the datatype or the inline range: in the dictionary.
        for (lexical, datatype) in [
            ("+5", xsd::INT),
            ("05", xsd::INT),
            ("-0", xsd::INT),
            ("300", xsd::BYTE),
            ("-1", xsd::NON_NEGATIVE_INTEGER),
            ("0", xsd::POSITIVE_INTEGER),
            ("1", xsd::NEGATIVE_INTEGER),
            ("9223372036854775807", xsd::LONG),
            ("x", xsd::INT),
        ] {
            assert_eq!(id(lexical, datatype), None, "{lexical}^^{datatype}");
        }
        // A store that keeps them in its dictionary.
        let literal = LiteralRef::new_typed_literal("5", xsd::INT);
        assert_eq!(try_inline_literal(literal, false), None);
        // Ranges by value cover every datatype.
        let (low, high) = derived_range(-5, 7).unwrap();
        assert!(ids[1] >= low && ids[4] <= high && ids[0] < low && ids[5] > high);
    }

    /// Package C3: a charged decode charges [`inline_bound`] before [`inline_to_literal`]
    /// allocates, so the bound is what the literal holds (its strings' capacities), exactly, for
    /// every inline kind and any payload: the longest fields, each field at its largest (a year
    /// of 14 bits, milliseconds of 10, the last timezone code, the widest mantissa and scale),
    /// and payloads no lexical form encodes. `None` exactly where no literal is decoded.
    #[test]
    fn an_inline_bound_is_what_its_literal_holds() {
        let kinds = [
            TermKind::Integer,
            TermKind::Boolean,
            TermKind::Decimal,
            TermKind::Date,
            TermKind::DateTime,
            TermKind::DerivedInteger,
            TermKind::String,
            TermKind::Triple,
        ];
        let all = (1u64 << super::super::PAYLOAD_BITS) - 1;
        let mut payloads = vec![0, 1, all, all >> 1, all >> 4, 0x3ff << TIMEZONE_BITS];
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            payloads.push(state & all);
        }
        let canonical = [
            inline("-576460752303423488", xsd::INTEGER),
            inline("-36028797018963.967", xsd::DECIMAL),
            inline("9999-12-31-14:00", xsd::DATE),
            inline("9999-12-31T23:59:59.999+14:00", xsd::DATE_TIME),
            inline("-128", xsd::BYTE),
        ];
        let mut decoded = 0;
        let ids = kinds
            .iter()
            .flat_map(|&kind| payloads.iter().map(move |&p| TermId::new(kind, p)))
            .chain(canonical.into_iter().map(Option::unwrap));
        for id in ids {
            let literal = inline_to_literal(id);
            assert_eq!(
                inline_bound(id),
                literal.as_ref().map(Literal::heap_bytes),
                "{id:?}: {literal:?}"
            );
            decoded += usize::from(literal.is_some());
        }
        assert!(decoded > 100_000, "{decoded} decoded");
    }
}
