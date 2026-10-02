//! Text shared by the syntaxes: what an IRI can hold, and the escapes of IRIs and strings.

/// Bytes an IRI reference can't hold as written (`[^#x00-#x20<>"{}|^`\]`): `true` to reject.
pub(crate) const NOT_IN_IRI: [bool; 256] = {
    let mut table = [false; 256];
    let mut b = 0;
    while b <= 0x20 {
        table[b] = true;
        b += 1;
    }
    let others = *b"<>\"{}|^`\\";
    let mut i = 0;
    while i < others.len() {
        table[others[i] as usize] = true;
        i += 1;
    }
    table
};

/// The character of a `\uXXXX` or `\UXXXXXXXX` escape at `bytes[at]` (on '\').
pub(crate) fn unicode_escape(bytes: &[u8], at: usize) -> Option<char> {
    let digits = match bytes.get(at + 1)? {
        b'u' => 4,
        b'U' => 8,
        _ => return None,
    };
    let hex = bytes.get(at + 2..at + 2 + digits)?;
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let code = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
    char::from_u32(code)
}

/// The width of the escape at `bytes[at]`: 6 for `\u`, 10 for `\U`, 2 for the others.
fn escape_width(bytes: &[u8], at: usize) -> usize {
    match bytes.get(at + 1) {
        Some(b'u') => 6,
        Some(b'U') => 10,
        _ => 2,
    }
}

/// Appends the content of an IRI reference with its `\u`/`\U` escapes decoded, checking
/// that neither it nor an escape holds a character IRIs can't. `Err(at)`: where it fails.
pub(crate) fn decode_iri(text: &str, out: &mut String) -> Result<(), usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            let c = unicode_escape(bytes, i).ok_or(i)?;
            if (c as u32) < 0x80 && NOT_IN_IRI[c as usize] {
                return Err(i);
            }
            out.push(c);
            i += escape_width(bytes, i);
        } else if NOT_IN_IRI[b as usize] {
            return Err(i);
        } else {
            let next = bytes[i..]
                .iter()
                .position(|&b| b == b'\\' || NOT_IN_IRI[b as usize])
                .map_or(bytes.len(), |k| i + k);
            out.push_str(&text[i..next]);
            i = next;
        }
    }
    Ok(())
}

/// Appends a string's content with its escapes (`ECHAR`, `UCHAR`) decoded.
/// `Err(at)`: where an escape is invalid.
pub(crate) fn decode_string(text: &str, out: &mut String) -> Result<(), usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(k) = memchr::memchr(b'\\', &bytes[i..]) else {
            out.push_str(&text[i..]);
            break;
        };
        out.push_str(&text[i..i + k]);
        let at = i + k;
        let escaped = match bytes.get(at + 1) {
            Some(b't') => '\t',
            Some(b'b') => '\u{8}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b'f') => '\u{c}',
            Some(b'"') => '"',
            Some(b'\'') => '\'',
            Some(b'\\') => '\\',
            Some(b'u' | b'U') => unicode_escape(bytes, at).ok_or(at)?,
            _ => return Err(at),
        };
        out.push(escaped);
        i = at + escape_width(bytes, at);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_decode() {
        let mut out = String::new();
        decode_string(r#"a\tbé\U0001F980\"\\"#, &mut out).unwrap();
        assert_eq!(out, "a\tbé🦀\"\\");
        out.clear();
        assert_eq!(decode_string(r"bad\q", &mut out), Err(3));
        out.clear();
        decode_iri(r"http://e/Ax", &mut out).unwrap();
        assert_eq!(out, "http://e/Ax");
        out.clear();
        assert!(decode_iri(r"http://e/ ", &mut out).is_err());
        assert!(decode_iri("http://e/a b", &mut String::new()).is_err());
        assert_eq!(unicode_escape(br"\u+041", 0), None);
    }
}
