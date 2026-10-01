//! The JSON Canonicalization Scheme (RFC 8785): one text per JSON value, for JSON-LD's
//! `@json` literals. Numbers are written as ECMAScript writes doubles; keys are sorted by
//! their UTF-16 code units.

use crate::value::Value;
use crate::writer::write_string;

/// The digits of the shortest decimal that reads back as `x` (finite, positive), and `n`
/// such that `x` = 0.digits × 10ⁿ.
pub fn shortest_digits(x: f64) -> (String, i32) {
    // `{:e}` gives the shortest round-trip digits, as `d.ddde±x`.
    let formatted = format!("{x:e}");
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    (digits.to_owned(), exponent.parse::<i32>().unwrap_or(0) + 1)
}

/// Appends `x` as ECMAScript's `Number.prototype.toString` writes it (ECMA-262
/// §6.1.6.1.20), which RFC 8785 adopts. `x` must be finite.
pub fn write_number(x: f64, out: &mut String) {
    if x == 0.0 {
        out.push('0');
        return;
    }
    if x < 0.0 {
        out.push('-');
    }
    let (digits, n) = shortest_digits(x.abs());
    let k = digits.len() as i32;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        let e = n - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&e.unsigned_abs().to_string());
    }
}

/// A number JCS can't write: beyond the range of a double.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the number {0} is out of the range of a double")]
pub struct OutOfRange(pub String);

/// Appends the canonical text of `value`.
pub fn write_canonical(value: &Value<'_>, out: &mut String) -> Result<(), OutOfRange> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Boolean(true) => out.push_str("true"),
        Value::Boolean(false) => out.push_str("false"),
        Value::Number(text) => match text.parse::<f64>() {
            Ok(x) if x.is_finite() => write_number(x, out),
            _ => return Err(OutOfRange(text.to_string())),
        },
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_canonical(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(x: f64) -> String {
        let mut out = String::new();
        write_number(x, &mut out);
        out
    }

    #[test]
    fn numbers_as_ecmascript_writes_them() {
        // RFC 8785 Appendix B, and ECMAScript's thresholds.
        for (x, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (9007199254740992.0, "9007199254740992"),
            (0.1, "0.1"),
            (333333333.3333333, "333333333.3333333"),
            (295147905179352830000.0, "295147905179352830000"),
            (4.5, "4.5"),
            (2e-3, "0.002"),
            (0.000001000000000000001, "0.000001000000000000001"),
            (1e-6 + 1e-30, "0.000001"),
        ] {
            assert_eq!(number(x), text, "{x:e}");
        }
    }

    #[test]
    fn canonical_documents() {
        let value =
            Value::parse(r#"{"b": [1.0E2, "€", false], "a": {"€": 1, "\r": 2, "1": null}}"#)
                .unwrap();
        let mut out = String::new();
        write_canonical(&value, &mut out).unwrap();
        assert_eq!(out, r#"{"a":{"\r":2,"1":null,"€":1},"b":[100,"€",false]}"#);
        // UTF-16 order: U+1F600 (D83D DE00) sorts before U+FB33.
        let value = Value::parse("{\"\u{FB33}\": 1, \"\u{1F600}\": 2}").unwrap();
        out.clear();
        write_canonical(&value, &mut out).unwrap();
        assert_eq!(out, "{\"\u{1F600}\":2,\"\u{FB33}\":1}");
        assert!(write_canonical(&Value::Number("1e400".into()), &mut out).is_err());
    }
}
