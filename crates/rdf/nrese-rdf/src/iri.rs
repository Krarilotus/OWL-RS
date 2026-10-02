//! IRIs (RFC 3987): validation, and resolution of references against a base (RFC 3986
//! §5.2, with the IRI characters of RFC 3987).
//!
//! [`Iri::parse`] accepts absolute IRIs; [`Iri::resolve`] takes any IRI reference (absolute,
//! or relative such as `../a#b`) and gives the absolute IRI it denotes against `self`.
//! [`Iri::resolve_into`] writes into a caller's buffer, for parsers that resolve an IRI per
//! triple.
//!
//! Validation is one pass over the bytes: a table lookup per ASCII byte, a range check
//! per other character. Resolution writes the result once and knows its parts, so it
//! doesn't parse the result again.

use std::fmt;

/// Why a text isn't an IRI: what is wrong, and the byte offset where it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid IRI: {kind} at byte {position}")]
pub struct IriParseError {
    kind: &'static str,
    position: usize,
}

impl IriParseError {
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn position(&self) -> usize {
        self.position
    }
}

fn error<T>(kind: &'static str, position: usize) -> Result<T, IriParseError> {
    Err(IriParseError { kind, position })
}

/// An absolute IRI, validated, with the positions of its parts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Iri<T> {
    iri: T,
    parts: Parts,
}

/// Where the parts of an IRI reference end (byte offsets): scheme (with its `:`; 0 if
/// none), authority (with its `//`; equal to the scheme's end if none), path, query (with
/// its `?`); the fragment runs to the end.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
struct Parts {
    scheme_end: usize,
    authority_end: usize,
    path_end: usize,
    query_end: usize,
}

impl Parts {
    fn has_authority(&self) -> bool {
        self.authority_end > self.scheme_end
    }
}

impl<T: AsRef<str>> Iri<T> {
    /// `iri` if it is an absolute IRI.
    pub fn parse(iri: T) -> Result<Self, IriParseError> {
        let parts = parse_reference(iri.as_ref().as_bytes(), true)?;
        if parts.scheme_end == 0 {
            return error("no scheme", 0);
        }
        Ok(Self { iri, parts })
    }

    /// Without validating the characters: `iri` must be an absolute IRI. The parts are
    /// still found, so the accessors and [`Iri::resolve`] work.
    pub fn parse_unchecked(iri: T) -> Self {
        let parts = parse_reference(iri.as_ref().as_bytes(), false).unwrap_or_default();
        Self { iri, parts }
    }

    pub fn as_str(&self) -> &str {
        self.iri.as_ref()
    }

    pub fn into_inner(self) -> T {
        self.iri
    }

    /// The scheme, without its `:`.
    pub fn scheme(&self) -> &str {
        &self.as_str()[..self.parts.scheme_end.saturating_sub(1)]
    }

    /// The authority, without its `//`, if there is one.
    pub fn authority(&self) -> Option<&str> {
        self.parts
            .has_authority()
            .then(|| &self.as_str()[self.parts.scheme_end + 2..self.parts.authority_end])
    }

    pub fn path(&self) -> &str {
        &self.as_str()[self.parts.authority_end..self.parts.path_end]
    }

    /// The query, without its `?`, if there is one.
    pub fn query(&self) -> Option<&str> {
        (self.parts.query_end > self.parts.path_end)
            .then(|| &self.as_str()[self.parts.path_end + 1..self.parts.query_end])
    }

    /// The fragment, without its `#`, if there is one.
    pub fn fragment(&self) -> Option<&str> {
        let s = self.as_str();
        (s.len() > self.parts.query_end).then(|| &s[self.parts.query_end + 1..])
    }

    /// The absolute IRI `reference` denotes with `self` as base (RFC 3986 §5.2.2).
    pub fn resolve(&self, reference: &str) -> Result<Iri<String>, IriParseError> {
        let mut out = String::with_capacity(self.as_str().len() + reference.len());
        let parts = self.resolve_parts(reference, &mut out, true)?;
        Ok(Iri { iri: out, parts })
    }

    /// Like [`Iri::resolve`], appending the IRI to `out` (cleared first).
    pub fn resolve_into(&self, reference: &str, out: &mut String) -> Result<(), IriParseError> {
        out.clear();
        self.resolve_parts(reference, out, true).map(|_| ())
    }

    /// Like [`Iri::resolve`] without validating `reference`'s characters.
    pub fn resolve_unchecked(&self, reference: &str) -> Iri<String> {
        let mut out = String::with_capacity(self.as_str().len() + reference.len());
        let parts = self
            .resolve_parts(reference, &mut out, false)
            .unwrap_or_default();
        Iri { iri: out, parts }
    }

    fn resolve_parts(
        &self,
        reference: &str,
        out: &mut String,
        validate: bool,
    ) -> Result<Parts, IriParseError> {
        let r = parse_reference(reference.as_bytes(), validate)?;
        let base = self.as_str();
        let b = self.parts;
        let r_path = &reference[r.authority_end..r.path_end];
        let r_query = (r.query_end > r.path_end).then(|| &reference[r.path_end..r.query_end]);
        let r_fragment = &reference[r.query_end..];

        let mut parts = Parts::default();
        if r.scheme_end > 0 {
            // T.scheme = R.scheme; T.authority = R.authority; T.path = remove_dot_segments(R.path)
            out.push_str(&reference[..r.authority_end]);
            parts.scheme_end = r.scheme_end;
            parts.authority_end = out.len();
            push_without_dot_segments(r_path, out);
        } else {
            out.push_str(&base[..b.scheme_end]);
            parts.scheme_end = out.len();
            if r.has_authority() {
                out.push_str(&reference[..r.authority_end]);
                parts.authority_end = out.len();
                push_without_dot_segments(r_path, out);
            } else {
                out.push_str(&base[b.scheme_end..b.authority_end]);
                parts.authority_end = out.len();
                let b_path = &base[b.authority_end..b.path_end];
                if r_path.is_empty() {
                    out.push_str(b_path);
                    parts.path_end = out.len();
                    // T.query = R.query if defined, else Base.query.
                    out.push_str(r_query.unwrap_or(&base[b.path_end..b.query_end]));
                    parts.query_end = out.len();
                    out.push_str(r_fragment);
                    return Ok(parts);
                } else if r_path.starts_with('/') {
                    push_without_dot_segments(r_path, out);
                } else {
                    // Merge (§5.2.3): the base path up to its last '/', then the
                    // reference's; then remove dot segments (§5.2.4) if there are any.
                    let directory = if b.has_authority() && b_path.is_empty() {
                        "/"
                    } else {
                        b_path.rfind('/').map_or("", |i| &b_path[..=i])
                    };
                    if !merge_leading_dots(directory, r_path, out) {
                        let start = out.len();
                        out.push_str(directory);
                        out.push_str(r_path);
                        if has_dot_segment(directory) || has_dot_segment(r_path) {
                            remove_dot_segments(out, start);
                        }
                    }
                }
            }
        }
        // Without an authority a path can't start with "//"; "/." keeps its meaning.
        if parts.authority_end == parts.scheme_end && out[parts.authority_end..].starts_with("//") {
            out.insert_str(parts.authority_end, "/.");
        }
        parts.path_end = out.len();
        if let Some(query) = r_query {
            out.push_str(query);
        }
        parts.query_end = out.len();
        out.push_str(r_fragment);
        Ok(parts)
    }
}

impl<T: AsRef<str>> fmt::Display for Iri<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<T: AsRef<str>> fmt::Debug for Iri<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.as_str())
    }
}

impl<T: AsRef<str>> AsRef<str> for Iri<T> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Whether a segment of `path` is `.` or `..`.
fn has_dot_segment(path: &str) -> bool {
    path.as_bytes().contains(&b'.') && path.split('/').any(|s| s == "." || s == "..")
}

/// `path` without dot segments, appended to `out`.
fn push_without_dot_segments(path: &str, out: &mut String) {
    let start = out.len();
    out.push_str(path);
    if has_dot_segment(path) {
        remove_dot_segments(out, start);
    }
}

/// The merge of an absolute base directory (ending in '/') and a relative reference path
/// whose only dot segments lead it (`../../x`, `./x`), written straight to `out`: the
/// directory without a segment per `..`, then the rest. `false` (nothing written) for any
/// other shape, which [`remove_dot_segments`] handles.
fn merge_leading_dots(directory: &str, reference: &str, out: &mut String) -> bool {
    if !directory.starts_with('/') || has_dot_segment(directory) {
        return false;
    }
    let mut rest = reference;
    let mut ups = 0;
    loop {
        if let Some(r) = rest.strip_prefix("../") {
            ups += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("./") {
            rest = r;
        } else {
            break;
        }
    }
    if has_dot_segment(rest) {
        return false;
    }
    let mut keep = directory.len();
    for _ in 0..ups {
        if keep <= 1 {
            break;
        }
        keep = directory[..keep - 1].rfind('/').map_or(1, |i| i + 1);
    }
    out.push_str(&directory[..keep]);
    out.push_str(rest);
    true
}

/// RFC 3986 §5.2.4 on `out[start..]`, in place: a read and a write position in one
/// buffer (the output never outgrows the input it has consumed), so no allocation.
fn remove_dot_segments(out: &mut String, start: usize) {
    let mut bytes = std::mem::take(out).into_bytes();
    let end = bytes.len();
    let (mut read, mut write) = (start, start);
    // The output without its last segment and that segment's '/'.
    let pop = |bytes: &[u8], write: usize| {
        bytes[start..write]
            .iter()
            .rposition(|&b| b == b'/')
            .map_or(start, |i| start + i)
    };
    while read < end {
        let input = &bytes[read..end];
        if input.starts_with(b"../") {
            read += 3;
        } else if input.starts_with(b"./") || input.starts_with(b"/./") {
            read += 2;
        } else if input == b"/." {
            // "/." becomes "/": reuse its last byte.
            bytes[end - 1] = b'/';
            read = end - 1;
        } else if input.starts_with(b"/../") {
            read += 3;
            write = pop(&bytes, write);
        } else if input == b"/.." {
            bytes[end - 1] = b'/';
            read = end - 1;
            write = pop(&bytes, write);
        } else if input == b"." || input == b".." {
            read = end;
        } else {
            let first = usize::from(input[0] == b'/');
            let length = input[first..]
                .iter()
                .position(|&b| b == b'/')
                .map_or(input.len(), |i| i + first);
            bytes.copy_within(read..read + length, write);
            write += length;
            read += length;
        }
    }
    bytes.truncate(write);
    // Whole segments, cut at ASCII '/', are copied: the bytes stay UTF-8.
    *out = String::from_utf8(bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
}

// ---------------------------------------------------------------------------------------
// Validation

/// Character classes of ASCII bytes, as bit flags: the grammar's classes, and each
/// delimiter its own bit, so that a scan tests one mask per byte.
const UNRESERVED: u16 = 1; // ALPHA DIGIT - . _ ~
const SUB_DELIM: u16 = 2; // ! $ & ' ( ) * + , ; =
const SCHEME: u16 = 4; // ALPHA DIGIT + - .
const HEX: u16 = 8;
const DIGIT: u16 = 16;
const COLON: u16 = 32;
const AT: u16 = 64;
const SLASH: u16 = 128;
const QUESTION: u16 = 256;
const HASH: u16 = 512;

/// What each component allows besides percent encodings (and `iprivate` in the query).
const HOST: u16 = UNRESERVED | SUB_DELIM;
const USERINFO: u16 = HOST | COLON;
const PATH: u16 = HOST | COLON | AT | SLASH;
const QUERY: u16 = PATH | QUESTION;

const fn classes() -> [u16; 128] {
    let mut table = [0u16; 128];
    let mut b = 0;
    while b < 128 {
        let c = b as u8;
        let mut class = 0;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') {
            class |= UNRESERVED;
        }
        if matches!(
            c,
            b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
        ) {
            class |= SUB_DELIM;
        }
        if c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.') {
            class |= SCHEME;
        }
        if c.is_ascii_hexdigit() {
            class |= HEX;
        }
        if c.is_ascii_digit() {
            class |= DIGIT;
        }
        class |= match c {
            b':' => COLON,
            b'@' => AT,
            b'/' => SLASH,
            b'?' => QUESTION,
            b'#' => HASH,
            _ => 0,
        };
        table[b] = class;
        b += 1;
    }
    table
}

static CLASSES: [u16; 128] = classes();

fn is(b: u8, class: u16) -> bool {
    b < 128 && CLASSES[b as usize] & class != 0
}

fn is_ucschar(c: char) -> bool {
    matches!(c,
        '\u{A0}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFEF}'
        | '\u{10000}'..='\u{1FFFD}' | '\u{20000}'..='\u{2FFFD}' | '\u{30000}'..='\u{3FFFD}'
        | '\u{40000}'..='\u{4FFFD}' | '\u{50000}'..='\u{5FFFD}' | '\u{60000}'..='\u{6FFFD}'
        | '\u{70000}'..='\u{7FFFD}' | '\u{80000}'..='\u{8FFFD}' | '\u{90000}'..='\u{9FFFD}'
        | '\u{A0000}'..='\u{AFFFD}' | '\u{B0000}'..='\u{BFFFD}' | '\u{C0000}'..='\u{CFFFD}'
        | '\u{D0000}'..='\u{DFFFD}' | '\u{E1000}'..='\u{EFFFD}')
}

fn is_iprivate(c: char) -> bool {
    matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}

/// The character at `text[i]`, a UTF-8 lead byte of a valid string (so the continuation
/// bytes are there), decoded from the bytes.
fn decode(text: &[u8], i: usize) -> char {
    let tail = |k: usize| u32::from(text[i + k] & 0x3F);
    let lead = u32::from(text[i]);
    let code = match lead {
        0xF0.. => ((lead & 0x07) << 18) | (tail(1) << 12) | (tail(2) << 6) | tail(3),
        0xE0.. => ((lead & 0x0F) << 12) | (tail(1) << 6) | tail(2),
        _ => ((lead & 0x1F) << 6) | tail(1),
    };
    char::from_u32(code).unwrap_or('\u{FFFD}')
}

/// Scans `text[i..end]` up to a byte of the classes `stop`, checking each character
/// against `allowed` (and `iprivate` if `private`) when `validate`; percent encodings are
/// always allowed. Returns where it stopped.
fn scan(
    text: &[u8],
    mut i: usize,
    end: usize,
    stop: u16,
    allowed: u16,
    private: bool,
    validate: bool,
) -> Result<usize, IriParseError> {
    while i < end {
        let b = text[i];
        if b < 128 {
            let class = CLASSES[b as usize];
            if class & stop != 0 {
                break;
            }
            if !validate || class & allowed != 0 {
                i += 1;
            } else if b == b'%' {
                if i + 2 >= end || !is(text[i + 1], HEX) || !is(text[i + 2], HEX) {
                    return error("a malformed percent encoding", i);
                }
                i += 3;
            } else {
                return error("a character IRIs don't allow", i);
            }
        } else {
            let c = decode(text, i);
            if validate && !(is_ucschar(c) || (private && is_iprivate(c))) {
                return error("a character IRIs don't allow", i);
            }
            i += c.len_utf8();
        }
    }
    Ok(i)
}

/// The parts of an IRI reference (RFC 3987 `IRI-reference`), in one pass over the bytes;
/// validated if `validate`.
fn parse_reference(text: &[u8], validate: bool) -> Result<Parts, IriParseError> {
    let len = text.len();
    // Scheme: a letter, then scheme characters, then ':'.
    let mut scheme_end = 0;
    if text.first().is_some_and(u8::is_ascii_alphabetic) {
        let mut i = 1;
        while i < len && is(text[i], SCHEME) {
            i += 1;
        }
        if i < len && text[i] == b':' {
            scheme_end = i + 1;
        }
    }
    // Authority: after "//", up to '/', '?' or '#'.
    let mut authority_end = scheme_end;
    if text[scheme_end..].starts_with(b"//") {
        let start = scheme_end + 2;
        authority_end = if validate {
            authority(text, start)?
        } else {
            scan(text, start, len, SLASH | QUESTION | HASH, 0, false, false)?
        };
    }
    // Path. A relative path's first segment can't hold a ':' (it would read as a scheme).
    let path_end = if validate && scheme_end == 0 && authority_end == 0 {
        let first = scan(
            text,
            0,
            len,
            SLASH | QUESTION | HASH | COLON,
            PATH,
            false,
            true,
        )?;
        if text.get(first) == Some(&b':') {
            return error("a ':' in the first segment of a relative path", first);
        }
        scan(text, first, len, QUESTION | HASH, PATH, false, true)?
    } else {
        scan(
            text,
            authority_end,
            len,
            QUESTION | HASH,
            PATH,
            false,
            validate,
        )?
    };
    // Query, then fragment.
    let query_end = if text.get(path_end) == Some(&b'?') {
        scan(text, path_end + 1, len, HASH, QUERY, true, validate)?
    } else {
        path_end
    };
    if query_end < len {
        // text[query_end] is '#'; a second '#' is invalid.
        scan(text, query_end + 1, len, 0, QUERY, false, validate)?;
    }
    Ok(Parts {
        scheme_end,
        authority_end,
        path_end,
        query_end,
    })
}

/// `iauthority = [ iuserinfo "@" ] ihost [ ":" port ]` from `start`, validated in one pass;
/// returns where it ends (at '/', '?', '#' or the end).
fn authority(text: &[u8], start: usize) -> Result<usize, IriParseError> {
    let len = text.len();
    let delimiters = SLASH | QUESTION | HASH;
    let mut host = start;
    if text.get(start) != Some(&b'[') {
        // A reg-name, or the userinfo: they differ by the ':' a userinfo may hold.
        let i = scan(text, start, len, delimiters | COLON | AT, HOST, false, true)?;
        match text.get(i) {
            Some(b'@') => host = i + 1,
            Some(b':') => {
                let j = scan(text, i + 1, len, delimiters | AT, USERINFO, false, true)?;
                if text.get(j) == Some(&b'@') {
                    host = j + 1;
                } else {
                    return port(text, i, j);
                }
            }
            _ => return Ok(i),
        }
    }
    if text.get(host) == Some(&b'[') {
        let Some(close) = text[host..].iter().position(|&b| b == b']') else {
            return error("an unclosed IP literal", host);
        };
        let close = host + close;
        check_ip_literal(&text[host + 1..close], host + 1)?;
        let end = scan(text, close + 1, len, delimiters, 0, false, false)?;
        return port(text, close + 1, end);
    }
    let i = scan(text, host, len, delimiters | COLON, HOST, false, true)?;
    let end = scan(text, i, len, delimiters, 0, false, false)?;
    port(text, i, end)
}

/// `[ ":" port ]` in `text[start..end]`; returns `end`.
fn port(text: &[u8], start: usize, end: usize) -> Result<usize, IriParseError> {
    if start == end {
        return Ok(end);
    }
    if text[start] != b':' {
        return error("a character after the host", start);
    }
    match text[start + 1..end].iter().position(|&b| !is(b, DIGIT)) {
        Some(i) => error("a port that isn't a number", start + 1 + i),
        None => Ok(end),
    }
}

/// `IPv6address / IPvFuture` (the text between the brackets).
fn check_ip_literal(ip: &[u8], offset: usize) -> Result<(), IriParseError> {
    if let [b'v' | b'V', rest @ ..] = ip {
        // IPvFuture = "v" 1*HEXDIG "." 1*( unreserved / sub-delims / ":" )
        let hex = rest.iter().take_while(|&&b| is(b, HEX)).count();
        let tail = &rest[hex..];
        let valid = hex > 0
            && tail.first() == Some(&b'.')
            && tail.len() > 1
            && tail[1..]
                .iter()
                .all(|&b| is(b, UNRESERVED | SUB_DELIM) || b == b':');
        return if valid {
            Ok(())
        } else {
            error("an invalid IPvFuture", offset)
        };
    }
    if is_ipv6(ip) {
        Ok(())
    } else {
        error("an invalid IPv6 address", offset)
    }
}

/// RFC 3986 `IPv6address`: eight 16-bit groups, the last two possibly an IPv4 address,
/// with at most one `::` standing for one or more zero groups.
fn is_ipv6(ip: &[u8]) -> bool {
    let text = match std::str::from_utf8(ip) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let groups = |part: &str, last: bool| -> Option<usize> {
        if part.is_empty() {
            return Some(0);
        }
        let pieces: Vec<&str> = part.split(':').collect();
        let mut count = 0;
        for (i, piece) in pieces.iter().enumerate() {
            if last && i == pieces.len() - 1 && piece.contains('.') {
                is_ipv4(piece).then_some(())?;
                count += 2;
            } else if (1..=4).contains(&piece.len()) && piece.bytes().all(|b| is(b, HEX)) {
                count += 1;
            } else {
                return None;
            }
        }
        Some(count)
    };
    match text.split_once("::") {
        Some((head, tail)) => {
            if tail.contains("::") {
                return false;
            }
            match (groups(head, tail.is_empty()), groups(tail, true)) {
                (Some(h), Some(t)) => h + t <= 7,
                _ => false,
            }
        }
        None => groups(text, true) == Some(8),
    }
}

/// `dec-octet "." dec-octet "." dec-octet "." dec-octet`, without leading zeros.
fn is_ipv4(text: &str) -> bool {
    let octets: Vec<&str> = text.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|o| {
            !o.is_empty()
                && o.len() <= 3
                && o.bytes().all(|b| b.is_ascii_digit())
                && (o.len() == 1 || !o.starts_with('0'))
                && o.parse::<u16>().is_ok_and(|v| v <= 255)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_iris_parse_and_others_do_not() {
        for ok in [
            "http://example.com/a/b?c=d#e",
            "urn:isbn:0451450523",
            "http://[::1]:8080/x",
            "http://[2001:db8::7]/c=GB?objectClass?one",
            "http://[::ffff:192.0.2.128]/",
            "http://[1:2:3:4:5:6:7:8]/",
            "http://[v7.fe80::a+en1]/",
            "http://user:pw@host.example/p%20q",
            "http://example.com/Übung",
            "http://example.com/?q=\u{E000}",
            "file:///C:/data/x.nt",
            "mailto:a@b.c",
            "a:",
            "a:b:c",
            "http://h:/",
            "tag:x.org,2001:a/b",
        ] {
            let iri = Iri::parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert_eq!(iri.as_str(), ok);
        }
        for bad in [
            "",
            "no-scheme",
            ":no-scheme",
            "http://example.com/a b",
            "http://example.com/<x>",
            "http://example.com/%zz",
            "http://example.com/%a",
            "http://example.com/%",
            "1http://x",
            "http://host:port/",
            "http://[::1/",
            "http://[1:2:3:4:5:6:7:8:9]/",
            "http://[1::2::3]/",
            "http://[::256.0.0.1]/",
            "http://[::01.0.0.1]/",
            "http://[zz::1]/",
            "http://[::1]x/",
            "http://[v.x]/",
            "http://example.com/#\u{E000}",
            "http://example.com/{x}",
            "http://example.com/\\",
        ] {
            assert!(Iri::parse(bad).is_err(), "{bad}");
        }
        let iri = Iri::parse("http://h:1/p/q?x#f").unwrap();
        assert_eq!(iri.scheme(), "http");
        assert_eq!(iri.authority(), Some("h:1"));
        assert_eq!(iri.path(), "/p/q");
        assert_eq!(iri.query(), Some("x"));
        assert_eq!(iri.fragment(), Some("f"));
        let bare = Iri::parse("urn:x").unwrap();
        assert_eq!(
            (bare.authority(), bare.path(), bare.query(), bare.fragment()),
            (None, "x", None, None)
        );
    }

    #[test]
    fn errors_say_where() {
        let e = Iri::parse("http://e/a b").unwrap_err();
        assert_eq!(e.position(), 10);
        assert_eq!(e.kind(), "a character IRIs don't allow");
    }

    /// RFC 3986 §5.4: every normal and abnormal example.
    #[test]
    fn references_resolve_as_rfc_3986_shows() {
        let base = Iri::parse("http://a/b/c/d;p?q").unwrap();
        for (reference, expected) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("g;x?y#s", "http://a/b/c/g;x?y#s"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("../../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            (".g", "http://a/b/c/.g"),
            ("g..", "http://a/b/c/g.."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("g?y/./x", "http://a/b/c/g?y/./x"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/./x", "http://a/b/c/g#s/./x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
            ("http:g", "http:g"),
        ] {
            let resolved = base
                .resolve(reference)
                .unwrap_or_else(|e| panic!("{reference}: {e}"));
            assert_eq!(resolved.as_str(), expected, "{reference}");
            // The parts of the result are those a fresh parse finds.
            assert_eq!(
                resolved,
                Iri::parse(expected.to_owned()).unwrap(),
                "{reference}"
            );
            let mut buffer = String::from("old");
            base.resolve_into(reference, &mut buffer).unwrap();
            assert_eq!(buffer, expected);
        }
        assert!(base.resolve("a b").is_err());
        assert!(base.resolve("a:b c").is_err());
        assert_eq!(base.resolve_unchecked("a b").as_str(), "http://a/b/c/a b");
    }

    #[test]
    fn resolution_edge_cases() {
        let cases = [
            ("a:/b", "..//c", "a:/.//c"),
            ("a:b", "c", "a:c"),
            ("a:b/c", "d", "a:b/d"),
            ("http://a", "b", "http://a/b"),
            ("http://a", "?q", "http://a?q"),
            ("http://a/b?q#f", "", "http://a/b?q"),
            ("urn:x:y", "#f", "urn:x:y#f"),
        ];
        for (base, reference, expected) in cases {
            let resolved = Iri::parse(base).unwrap().resolve(reference).unwrap();
            assert_eq!(resolved.as_str(), expected, "{base} + {reference}");
            assert_eq!(resolved, Iri::parse(expected.to_owned()).unwrap());
        }
    }
}
