//! Data values of the OWL 2 datatype map and the lexical-to-value mapping of literals.
//!
//! Equality of `Value`s is OWL 2's: identity of data values. One number of `owl:real` is
//! one value whatever its datatype (`"1"^^xsd:integer` and `"1.0"^^xsd:decimal`); floats,
//! doubles and reals are pairwise disjoint; `+0` and `-0` are equal but not identical, so
//! two values; NaN is one value; two date-times at one instant with different offsets are
//! two values; a language tag is compared in lower case.

use std::str::FromStr;

use super::datatype::Datatype;
use super::rational::{NumberError, Rational};
use super::text;
use crate::{DateTime, Double, Float};

/// A data value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Value {
    /// A number of `owl:real` (every one a literal can write is rational).
    Real(Rational),
    /// An `xsd:float` by its bits (one NaN).
    Float(u32),
    /// An `xsd:double` by its bits (one NaN).
    Double(u64),
    /// A string without a language tag (`xsd:string`).
    String(String),
    /// A string with a language tag, the tag in lower case.
    LangString(String, String),
    Boolean(bool),
    /// An `xsd:dateTime`: the instant (seconds × 10¹⁸ since the epoch, UTC; local time for
    /// one without a timezone) and the timezone offset in minutes.
    DateTime(i128, Option<i16>),
    HexBinary(Vec<u8>),
    Base64Binary(Vec<u8>),
    AnyUri(String),
    /// An `rdf:XMLLiteral` by its canonical form.
    XmlLiteral(String),
}

/// Why a literal has no value here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteralError {
    /// The lexical form isn't one of the datatype's: the literal is ill-typed.
    IllTyped(String),
    /// Valid, but its value is beyond what is represented here (a number past 128 bits,
    /// more than 18 fractional digits of a second, an XML literal not decided).
    Unsupported(String),
}

const NAN32: u32 = 0x7fc0_0000;
const NAN64: u64 = 0x7ff8_0000_0000_0000;

impl Value {
    /// The value of the literal with lexical form `lexical` and datatype `datatype`
    /// (`language`: its tag, for a language-tagged literal).
    pub fn parse(
        lexical: &str,
        datatype: Datatype,
        language: Option<&str>,
    ) -> Result<Value, LiteralError> {
        let ill = |why: &str| LiteralError::IllTyped(format!("{lexical:?} as {datatype:?}: {why}"));
        let number = |r: Result<Rational, NumberError>| match r {
            Ok(r) => Ok(r),
            Err(NumberError::Range) => Err(LiteralError::Unsupported(format!(
                "{lexical:?}: a number beyond 128 bits"
            ))),
            Err(NumberError::Lexical(e)) => Err(ill(&e.to_string())),
        };
        if let Some(tag) = language {
            return match datatype {
                // XML's Char only: the map's text model has no other strings (`text`, the
                // regular alphabet). RDF 1.1 Concepts §3.3 gives rdf:langString all of Unicode,
                // rdf:PlainLiteral XML's Char; telling the two apart needs that model widened.
                Datatype::LangString | Datatype::PlainLiteral if !tag.is_empty() => {
                    if !lexical.chars().all(text::is_xml_char) {
                        return Err(ill("a character XML doesn't allow"));
                    }
                    Ok(Value::LangString(
                        lexical.to_owned(),
                        tag.to_ascii_lowercase(),
                    ))
                }
                _ => Err(ill("a language tag on a datatype without one")),
            };
        }
        let string = |s: &str| -> Result<String, LiteralError> {
            if s.chars().all(text::is_xml_char) {
                Ok(s.to_owned())
            } else {
                Err(ill("a character XML doesn't allow"))
            }
        };
        Ok(match datatype {
            Datatype::Literal | Datatype::Real | Datatype::LangString => {
                return Err(ill("the datatype has no lexical forms without a tag"));
            }
            Datatype::Rational => Value::Real(number(Rational::parse_rational(lexical))?),
            Datatype::Decimal => Value::Real(number(Rational::parse_decimal(lexical))?),
            d if d.integer_bounds().is_some() => {
                let (lo, hi) = d.integer_bounds().unwrap_or((None, None));
                let r = match Rational::parse_integer(lexical) {
                    // Past 128 bits, its sign alone puts it outside a bound the type has.
                    Err(NumberError::Range) => {
                        let negative = lexical.starts_with('-');
                        if (negative && lo.is_some()) || (!negative && hi.is_some()) {
                            return Err(ill("out of the datatype's range"));
                        }
                        number(Err(NumberError::Range))?
                    }
                    r => number(r)?,
                };
                let i = r.numerator();
                if lo.is_some_and(|lo| i < lo) || hi.is_some_and(|hi| i > hi) {
                    return Err(ill("out of the datatype's range"));
                }
                Value::Real(r)
            }
            Datatype::Float => {
                let f = Float::from_str(lexical).map_err(|e| ill(&e.to_string()))?;
                let x = f32::from(f);
                Value::Float(if x.is_nan() { NAN32 } else { x.to_bits() })
            }
            Datatype::Double => {
                let f = Double::from_str(lexical).map_err(|e| ill(&e.to_string()))?;
                let x = f64::from(f);
                Value::Double(if x.is_nan() { NAN64 } else { x.to_bits() })
            }
            Datatype::PlainLiteral => {
                // `text@tag`; an empty tag: a string without one.
                let (text, tag) = lexical
                    .rsplit_once('@')
                    .ok_or_else(|| ill("no '@' before the language tag"))?;
                if tag.is_empty() {
                    Value::String(string(text)?)
                } else if text::is_language(tag) {
                    Value::LangString(string(text)?, tag.to_ascii_lowercase())
                } else {
                    return Err(ill("not a language tag"));
                }
            }
            d if d.string_floor().is_some() => {
                let s = string(lexical)?;
                if text::region(&s) < d.string_floor().unwrap_or(0) {
                    return Err(ill("not in the datatype's lexical space"));
                }
                Value::String(s)
            }
            Datatype::Boolean => match lexical {
                "true" | "1" => Value::Boolean(true),
                "false" | "0" => Value::Boolean(false),
                _ => return Err(ill("not true, false, 1 or 0")),
            },
            Datatype::HexBinary => Value::HexBinary(hex(lexical).ok_or_else(|| ill("not hex"))?),
            Datatype::Base64Binary => {
                Value::Base64Binary(base64(lexical).ok_or_else(|| ill("not base64"))?)
            }
            Datatype::AnyUri => Value::AnyUri(string(lexical)?),
            Datatype::DateTime | Datatype::DateTimeStamp => {
                // A valid form past what is represented is unsupported, never ill-typed; a
                // dateTimeStamp without its timezone is ill-typed whatever its year.
                let d = DateTime::from_str(lexical).map_err(|e| match e.unrepresented() {
                    Some(false) if datatype == Datatype::DateTimeStamp => {
                        ill("a dateTimeStamp needs a timezone")
                    }
                    Some(_) => LiteralError::Unsupported(format!("{lexical:?}: {e}")),
                    None => ill(&e.to_string()),
                })?;
                // The lexical space first (XML Schema 1.1 Part 2 §3.4.28.1), then what is
                // represented.
                let (instant, timezone) = d.timeline();
                if datatype == Datatype::DateTimeStamp && timezone.is_none() {
                    return Err(ill("a dateTimeStamp needs a timezone"));
                }
                if fraction_digits(lexical) > 18 {
                    return Err(LiteralError::Unsupported(format!(
                        "{lexical:?}: more than 18 fractional digits of a second"
                    )));
                }
                Value::DateTime(instant, timezone)
            }
            Datatype::XmlLiteral => match super::xml::classify(lexical) {
                Ok(form) => Value::XmlLiteral(form),
                Err(super::xml::NotCanonical::Malformed) => {
                    return Err(ill("not well-balanced, self-contained XML content"));
                }
                Err(super::xml::NotCanonical::Undecided) => {
                    return Err(LiteralError::Unsupported(format!(
                        "{lexical:?}: an XML literal not decided"
                    )));
                }
            },
            _ => return Err(ill("not a datatype of the map")),
        })
    }
}

/// The digits after the point of a date-time's seconds, but trailing zeros.
fn fraction_digits(lexical: &str) -> usize {
    let Some(t) = lexical.find('T') else {
        return 0;
    };
    let time = &lexical[t..];
    let Some(dot) = time.find('.') else {
        return 0;
    };
    let digits: String = time[dot + 1..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.trim_end_matches('0').len()
}

/// XML Schema's `hexBinary`: pairs of hexadecimal digits, nothing else (§3.3.15.2; Rust's
/// `from_str_radix` alone would also take a sign).
fn hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// XML Schema's `base64Binary`: groups of four, a single space allowed between
/// characters, `=` padding only at the end.
fn base64(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b' ') || bytes.last() == Some(&b' ') || s.contains("  ") {
        return None;
    }
    let chars: Vec<u8> = bytes.iter().copied().filter(|&b| b != b' ').collect();
    if !chars.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(chars.len() / 4 * 3);
    for (i, quad) in chars.chunks(4).enumerate() {
        let last = i + 1 == chars.len() / 4;
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &quad[..4 - pad] {
            n = (n << 6) | value(c)?;
        }
        n <<= 6 * pad as u32;
        let b = n.to_be_bytes();
        out.extend_from_slice(&b[1..4 - pad]);
        // The bits a padded quad drops must be zero (the canonical encoding).
        if pad > 0 && (n >> (8 * pad)) << (8 * pad) != n {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(lexical: &str, d: Datatype) -> Value {
        Value::parse(lexical, d, None).unwrap()
    }

    #[test]
    fn identity_across_and_within_datatypes() {
        assert_eq!(v("1", Datatype::Integer), v("1.0", Datatype::Decimal));
        assert_eq!(v("1", Datatype::Byte), v("01", Datatype::Integer));
        assert_eq!(v("0.5", Datatype::Decimal), v("1/2", Datatype::Rational));
        assert_ne!(v("1", Datatype::Integer), v("1", Datatype::Float));
        assert_ne!(v("1", Datatype::Float), v("1", Datatype::Double));
        assert_ne!(v("0", Datatype::Float), v("-0", Datatype::Float));
        assert_eq!(v("NaN", Datatype::Float), v("NaN", Datatype::Float));
        assert_ne!(
            v("2000-01-01T00:00:00Z", Datatype::DateTime),
            v("2000-01-01T01:00:00+01:00", Datatype::DateTime)
        );
        assert_eq!(
            v("2000-01-01T00:00:00Z", Datatype::DateTime),
            v("2000-01-01T00:00:00.000Z", Datatype::DateTimeStamp)
        );
        assert_eq!(
            Value::parse("a", Datatype::LangString, Some("EN")),
            Ok(Value::LangString("a".into(), "en".into()))
        );
        assert!(matches!(
            Value::parse("a\u{1}b", Datatype::String, None),
            Err(LiteralError::IllTyped(_))
        ));
        assert_eq!(
            v("a@En", Datatype::PlainLiteral),
            Value::LangString("a".into(), "en".into())
        );
        assert_eq!(v("a@", Datatype::PlainLiteral), v("a", Datatype::String));
        assert_ne!(v("a", Datatype::String), v("a", Datatype::AnyUri));
        assert_eq!(v("0F", Datatype::HexBinary), Value::HexBinary(vec![15]));
        assert_eq!(
            v("TWFu", Datatype::Base64Binary),
            Value::Base64Binary(b"Man".to_vec())
        );
        assert_eq!(
            v("TWE=", Datatype::Base64Binary),
            Value::Base64Binary(b"Ma".to_vec())
        );
        assert_ne!(
            v("0F", Datatype::HexBinary),
            v("Dw==", Datatype::Base64Binary)
        );
    }

    #[test]
    fn ill_typed_and_unsupported_literals() {
        // Every row is checked and the wrong ones reported together.
        let ill_typed: Vec<String> = [
            ("128", Datatype::Byte),
            ("-1", Datatype::NonNegativeInteger),
            ("1.5", Datatype::Integer),
            ("abc", Datatype::Integer),
            ("1", Datatype::Real),
            (" a", Datatype::Token),
            ("1a", Datatype::Name),
            ("2000-01-01T00:00:00", Datatype::DateTimeStamp),
            ("2000-13-01T00:00:00Z", Datatype::DateTime),
            // A year past what is represented doesn't excuse the rest of the form (XML
            // Schema 1.1 Part 2 §3.3.7.2, §3.4.28.1): every other part is still checked.
            ("100000000001-13-01T00:00:00Z", Datatype::DateTime),
            ("100000000001-01-01T00:00:00", Datatype::DateTimeStamp),
            ("100000000000-12-31T24:00:00", Datatype::DateTimeStamp),
            // Leap years by the year itself: 100000000001 and 100000000100 aren't.
            ("100000000001-02-29T00:00:00Z", Datatype::DateTime),
            ("100000000100-02-29T00:00:00Z", Datatype::DateTime),
            ("yes", Datatype::Boolean),
            ("F", Datatype::HexBinary),
            ("TQ=", Datatype::Base64Binary),
            ("TR==", Datatype::Base64Binary),
        ]
        .into_iter()
        .filter_map(|(lexical, d)| {
            let got = Value::parse(lexical, d, None);
            (!matches!(got, Err(LiteralError::IllTyped(_))))
                .then(|| format!("{lexical} as {d:?}: {got:?}"))
        })
        .collect();
        assert!(ill_typed.is_empty(), "{ill_typed:#?}");
        assert!(matches!(
            Value::parse("1".repeat(50).as_str(), Datatype::Integer, None),
            Err(LiteralError::Unsupported(_))
        ));
        assert!(matches!(
            Value::parse(
                "2000-01-01T00:00:00.1234567890123456789Z",
                Datatype::DateTime,
                None
            ),
            Err(LiteralError::Unsupported(_))
        ));
        // A year has no bound (XML Schema 1.1 Part 2 §3.3.7.2): one past what is represented
        // is valid, so unsupported, never ill-typed. Every row is checked.
        let wrong: Vec<String> = [
            ("100000000001-01-01T00:00:00Z", Datatype::DateTime),
            ("100000000001-01-01T00:00:00Z", Datatype::DateTimeStamp),
            ("-100000000001-01-01T00:00:00", Datatype::DateTime),
            ("99999999999999999999-01-01T00:00:00Z", Datatype::DateTime),
            ("100000000000-12-31T24:00:00Z", Datatype::DateTime),
            ("100000000000-12-31T24:00:00", Datatype::DateTime),
            // 100000000400 is a leap year (divisible by 400), and so is its negation.
            ("100000000400-02-29T00:00:00Z", Datatype::DateTime),
            ("-100000000400-02-29T00:00:00Z", Datatype::DateTime),
        ]
        .into_iter()
        .filter_map(|(lexical, d)| {
            let got = Value::parse(lexical, d, None);
            (!matches!(got, Err(LiteralError::Unsupported(_))))
                .then(|| format!("{lexical} as {d:?}: {got:?}"))
        })
        .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Each datatype's lexical space is checked whole before a value is decided or found
    /// beyond what is represented (XSDVAL review r1): a form outside it is ill-typed, never
    /// unsupported, and a std parser's wider grammar decides nothing. Sources: XML Schema 1.1
    /// Part 2 §3.4.28.1 (dateTimeStamp needs a timezone), §3.3.15.2 (hexBinary: hex digit
    /// pairs), §3.4.13 ff. (an integer type's bounds), OWL 2 Structural Specification §4.1
    /// (owl:rational: a nonzero denominator), RDF 1.1 Concepts (rdf:XMLLiteral:
    /// well-balanced, self-contained XML content). Every row is checked.
    #[test]
    fn the_lexical_space_is_checked_before_what_is_represented() {
        #[derive(Debug, PartialEq)]
        enum Is {
            Ill,
            Unsupported,
            Valid,
        }
        let big = "9".repeat(50);
        let long_fraction = "2000-01-01T00:00:00.1234567890123456789";
        let rows: Vec<(String, Datatype, Is)> = vec![
            // An early representation-only return.
            (long_fraction.to_owned(), Datatype::DateTimeStamp, Is::Ill),
            (
                format!("{long_fraction}Z"),
                Datatype::DateTimeStamp,
                Is::Unsupported,
            ),
            (
                long_fraction.to_owned(),
                Datatype::DateTime,
                Is::Unsupported,
            ),
            (format!("{big}/x"), Datatype::Rational, Is::Ill),
            (format!("{big}/0"), Datatype::Rational, Is::Ill),
            (format!("{big}/1"), Datatype::Rational, Is::Unsupported),
            (big.clone(), Datatype::Byte, Is::Ill),
            (big.clone(), Datatype::UnsignedLong, Is::Ill),
            (big.clone(), Datatype::NonPositiveInteger, Is::Ill),
            (format!("-{big}"), Datatype::NonNegativeInteger, Is::Ill),
            (big.clone(), Datatype::NonNegativeInteger, Is::Unsupported),
            (format!("-{big}"), Datatype::Integer, Is::Unsupported),
            ("<!-- c --><a".to_owned(), Datatype::XmlLiteral, Is::Ill),
            ("<a>".to_owned(), Datatype::XmlLiteral, Is::Ill),
            ("&nbsp;".to_owned(), Datatype::XmlLiteral, Is::Ill),
            ("<p:a/>".to_owned(), Datatype::XmlLiteral, Is::Ill),
            (
                "<!-- c --><a/>".to_owned(),
                Datatype::XmlLiteral,
                Is::Unsupported,
            ),
            ("<a/>".to_owned(), Datatype::XmlLiteral, Is::Valid),
            ("&#x+41;".to_owned(), Datatype::XmlLiteral, Is::Ill),
            ("<!--c-->&#x+41;".to_owned(), Datatype::XmlLiteral, Is::Ill),
            ("<a /x></a>".to_owned(), Datatype::XmlLiteral, Is::Ill),
            ("<:a/>".to_owned(), Datatype::XmlLiteral, Is::Ill),
            (
                "<a xmlns:p=''><p:b/></a>".to_owned(),
                Datatype::XmlLiteral,
                Is::Ill,
            ),
            ("&#x41;".to_owned(), Datatype::XmlLiteral, Is::Valid),
            // A std parser's grammar wider than the datatype's.
            ("+F".to_owned(), Datatype::HexBinary, Is::Ill),
            ("00+F".to_owned(), Datatype::HexBinary, Is::Ill),
            ("0f".to_owned(), Datatype::HexBinary, Is::Valid),
            ("0F".to_owned(), Datatype::HexBinary, Is::Valid),
            (String::new(), Datatype::HexBinary, Is::Valid),
            ("inf".to_owned(), Datatype::Double, Is::Ill),
            ("Infinity".to_owned(), Datatype::Double, Is::Ill),
            ("nan".to_owned(), Datatype::Float, Is::Ill),
            ("1_0".to_owned(), Datatype::Double, Is::Ill),
            (" 1".to_owned(), Datatype::Double, Is::Ill),
            ("+INF".to_owned(), Datatype::Double, Is::Valid),
            ("-INF".to_owned(), Datatype::Float, Is::Valid),
            ("NaN".to_owned(), Datatype::Double, Is::Valid),
        ];
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|(lexical, d, expected)| {
                let got = Value::parse(lexical, *d, None);
                let is = match &got {
                    Ok(_) => Is::Valid,
                    Err(LiteralError::IllTyped(_)) => Is::Ill,
                    Err(LiteralError::Unsupported(_)) => Is::Unsupported,
                };
                (is != *expected).then(|| format!("{lexical:.60} as {d:?}: {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
