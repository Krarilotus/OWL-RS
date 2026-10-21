//! The `\uXXXX` and `\UXXXXXXXX` escapes of N-Triples, Turtle, SPARQL and the formats that
//! borrow them (SPARQL TSV results, RDF4J's protocol parameters): one strict reading for all
//! of them, so that a malformed escape is an error everywhere rather than another character
//! somewhere.

/// The character of the `\uXXXX` or `\UXXXXXXXX` escape at `bytes[at]` (on the `\`):
/// exactly four or eight ASCII hex digits naming a Unicode scalar value. `None` for anything
/// else: fewer digits, a sign, a surrogate, past U+10FFFF.
pub fn unicode_escape(bytes: &[u8], at: usize) -> Option<char> {
    let digits = match bytes.get(at + 1)? {
        b'u' => 4,
        b'U' => 8,
        _ => return None,
    };
    let hex = bytes.get(at + 2..at + 2 + digits)?;
    hex_code_point(std::str::from_utf8(hex).ok()?)
}

/// The character whose code point `hex` is: ASCII hex digits only (no sign, no space), at
/// most eight, naming a Unicode scalar value.
fn hex_code_point(hex: &str) -> Option<char> {
    if hex.is_empty() || hex.len() > 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    char::from_u32(u32::from_str_radix(hex, 16).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_are_read_strictly() {
        for (text, expected) in [
            (r"\u0041", Some('A')),
            (r"\U0001F980", Some('🦀')),
            (r"\u12", None),
            (r"\u12x4", None),
            (r"\u+041", None),
            (r"\U+0001F98", None),
            (r"\uD800", None),
            (r"\U00110000", None),
            (r"\x0041", None),
        ] {
            assert_eq!(unicode_escape(text.as_bytes(), 0), expected, "{text}");
        }
        assert_eq!(hex_code_point("41"), Some('A'));
        assert_eq!(hex_code_point("+41"), None);
        assert_eq!(hex_code_point(""), None);
    }
}
