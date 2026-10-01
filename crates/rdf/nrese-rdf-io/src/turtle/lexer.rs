//! The tokens of Turtle and TriG (RDF 1.1), and of Notation3 (N3), from the unconsumed
//! bytes of the input. N3's own tokens are only recognised in N3 mode; each starts with a
//! byte Turtle doesn't allow there, so the Turtle path doesn't pay for them.
//!
//! [`lex`] skips whitespace and comments, then reads one token and returns its kind and
//! where its text is. When the bytes end inside a token and more input may come, it says
//! so ([`Lexed::NeedMore`]), and the caller refills and asks again: tokens never span a
//! refill. Text is not decoded here (escapes, prefixes, relative IRIs are the parser's).

use std::ops::Range;

/// What a token is. Text positions are in the token's [`Token::text`] range unless noted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `<…>`: the text is between the brackets; `escaped` if it holds `\u` or `\U`.
    IriRef {
        escaped: bool,
    },
    /// `prefix:local`: the colon is at `colon` (from the text's start); `escaped` if the
    /// local part holds `\` escapes.
    PrefixedName {
        colon: usize,
        escaped: bool,
    },
    /// `_:label`: the text is the label.
    BlankLabel,
    /// A string: the text is between the quotes; `escaped` if it holds `\`.
    String {
        escaped: bool,
    },
    /// `@tag` after a string: the text is the tag.
    LangTag,
    Integer,
    Decimal,
    Double,
    True,
    False,
    /// `a`, for `rdf:type`.
    A,
    AtPrefix,
    AtBase,
    /// SPARQL-style `PREFIX`, `BASE`, and TriG's `GRAPH` (any letter case).
    Prefix,
    Base,
    Graph,
    Dot,
    Semicolon,
    Comma,
    OpenBracket,
    CloseBracket,
    OpenParen,
    CloseParen,
    OpenBrace,
    CloseBrace,
    /// `^^`
    Datatype,
    // N3 only.
    /// `?name`: the text is the name.
    Variable,
    /// `=>`
    Implies,
    /// `<=`
    ImpliedBy,
    /// `=`
    Equals,
    /// `<-`
    Inverse,
    /// `!`
    Bang,
    /// `^`
    Caret,
    /// The words `has`, `is`, `of`, and `id` (in `[ id <iri> … ]`).
    Has,
    Is,
    Of,
    Id,
    Eof,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    /// Where the token starts (after whitespace and comments), from the input's start.
    pub(crate) start: usize,
    /// Its text (see [`Kind`]), from the input's start.
    pub(crate) text: Range<usize>,
    /// Bytes consumed, whitespace before it included.
    pub(crate) consumed: usize,
}

pub(crate) enum Lexed {
    Token(Token),
    /// The input ends inside a token; more may come.
    NeedMore,
    /// Not a token: what is wrong, and at which byte.
    Error(usize, &'static str),
}

/// Reads the next token of `input`. `eof`: no more input will come. `after_string`: the
/// previous token was a string, so `@` starts a language tag (not `@prefix`). `n3`: N3's
/// tokens too.
pub(crate) fn lex(input: &[u8], eof: bool, after_string: bool, n3: bool) -> Lexed {
    let len = input.len();
    let mut i = 0;
    // Whitespace and comments.
    loop {
        while i < len && matches!(input[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        if i < len && input[i] == b'#' {
            match memchr::memchr2(b'\n', b'\r', &input[i..]) {
                Some(k) => i += k,
                None if eof => i = len,
                None => return Lexed::NeedMore,
            }
            continue;
        }
        break;
    }
    if i == len {
        return if eof {
            token(Kind::Eof, i, i..i, i)
        } else {
            Lexed::NeedMore
        };
    }
    let start = i;
    let next = |k: usize| input.get(start + k).copied();
    // A byte after the token's first, or "need more" if it may still come.
    macro_rules! need {
        ($k:expr) => {
            match next($k) {
                Some(b) => Some(b),
                None if eof => None,
                None => return Lexed::NeedMore,
            }
        };
    }
    match input[start] {
        b'<' if n3 => n3_angle(input, start, eof),
        b'=' if n3 => match need!(1) {
            Some(b'>') => token(Kind::Implies, start, start..start + 2, start + 2),
            _ => token(Kind::Equals, start, start..start + 1, start + 1),
        },
        b'!' if n3 => token(Kind::Bang, start, start..start + 1, start + 1),
        b'?' if n3 => variable(input, start, eof),
        b'<' => match memchr::memchr(b'>', &input[start..]) {
            Some(k) => {
                let text = start + 1..start + k;
                let escaped = input[text.clone()].contains(&b'\\');
                token(Kind::IriRef { escaped }, start, text, start + k + 1)
            }
            None if eof => Lexed::Error(start, "an IRI without its closing '>'"),
            None => Lexed::NeedMore,
        },
        b'"' | b'\'' => string(input, start, eof),
        b'_' => match need!(1) {
            Some(b':') => blank_label(input, start, eof),
            _ => Lexed::Error(start, "expected '_:' for a blank node"),
        },
        b'@' => {
            let mut end = start + 1;
            while end < len && input[end].is_ascii_alphabetic() {
                end += 1;
            }
            if end == len && !eof {
                return Lexed::NeedMore;
            }
            if after_string {
                return language_tag(input, start, eof);
            }
            match &input[start + 1..end] {
                b"prefix" => token(Kind::AtPrefix, start, start..end, end),
                b"base" => token(Kind::AtBase, start, start..end, end),
                _ => Lexed::Error(start, "expected @prefix or @base"),
            }
        }
        b'.' => match need!(1) {
            Some(b'0'..=b'9') => number(input, start, eof),
            _ => token(Kind::Dot, start, start..start + 1, start + 1),
        },
        b'+' | b'-' | b'0'..=b'9' => number(input, start, eof),
        b';' => token(Kind::Semicolon, start, start..start + 1, start + 1),
        b',' => token(Kind::Comma, start, start..start + 1, start + 1),
        b'[' => token(Kind::OpenBracket, start, start..start + 1, start + 1),
        b']' => token(Kind::CloseBracket, start, start..start + 1, start + 1),
        b'(' => token(Kind::OpenParen, start, start..start + 1, start + 1),
        b')' => token(Kind::CloseParen, start, start..start + 1, start + 1),
        b'{' => token(Kind::OpenBrace, start, start..start + 1, start + 1),
        b'}' => token(Kind::CloseBrace, start, start..start + 1, start + 1),
        b'^' => match need!(1) {
            Some(b'^') => token(Kind::Datatype, start, start..start + 2, start + 2),
            _ if n3 => token(Kind::Caret, start, start..start + 1, start + 1),
            _ => Lexed::Error(start, "expected '^^'"),
        },
        b':' => prefixed_name(input, start, start, eof),
        _ => name(input, start, eof, n3),
    }
}

/// In N3, `<` starts an IRI, `<=` or `<-`. An IRI is the longest match: `<=x>` is one.
fn n3_angle(input: &[u8], start: usize, eof: bool) -> Lexed {
    let mut i = start + 1;
    while let Some(&b) = input.get(i) {
        match b {
            b'>' => {
                let text = start + 1..i;
                let escaped = input[text.clone()].contains(&b'\\');
                return token(Kind::IriRef { escaped }, start, text, i + 1);
            }
            // Not in an IRI reference: so not one.
            b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | 0..=0x20 => break,
            _ => i += 1,
        }
    }
    if i == input.len() && !eof {
        return Lexed::NeedMore;
    }
    match input.get(start + 1) {
        Some(b'=') => token(Kind::ImpliedBy, start, start..start + 2, start + 2),
        Some(b'-') => token(Kind::Inverse, start, start..start + 2, start + 2),
        _ => Lexed::Error(start, "an IRI without its closing '>'"),
    }
}

/// `?name`: `'?' PN_CHARS_U PN_CHARS*`.
fn variable(input: &[u8], start: usize, eof: bool) -> Lexed {
    let name = start + 1;
    match char_at(input, name, eof) {
        Ok(Some(c)) if is_pn_chars_u(c) => {}
        Ok(_) => return Lexed::Error(name, "a variable without a name"),
        Err(lexed) => return lexed,
    }
    let mut i = name;
    loop {
        match char_at(input, i, eof) {
            Ok(Some(c)) if is_pn_chars(c) => i += c.len_utf8(),
            Ok(_) => break,
            Err(lexed) => return lexed,
        }
    }
    token(Kind::Variable, start, name..i, i)
}

fn token(kind: Kind, start: usize, text: Range<usize>, consumed: usize) -> Lexed {
    Lexed::Token(Token {
        kind,
        start,
        text,
        consumed,
    })
}

/// The character at `input[i]`: `Ok(None)` at the end, `Err` if more input is needed.
fn char_at(input: &[u8], i: usize, eof: bool) -> Result<Option<char>, Lexed> {
    let Some(&lead) = input.get(i) else {
        return if eof { Ok(None) } else { Err(Lexed::NeedMore) };
    };
    let width = match lead {
        0x00..=0x7F => return Ok(Some(char::from(lead))),
        0xF0.. => 4,
        0xE0.. => 3,
        _ => 2,
    };
    match input.get(i..i + width) {
        Some(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => Ok(s.chars().next()),
            Err(_) => Err(Lexed::Error(i, "invalid UTF-8")),
        },
        None if eof => Err(Lexed::Error(i, "invalid UTF-8")),
        None => Err(Lexed::NeedMore),
    }
}

/// Strings, short (`"…"`, `'…'`) and long (`"""…"""`, `'''…'''`).
fn string(input: &[u8], start: usize, eof: bool) -> Lexed {
    let quote = input[start];
    let len = input.len();
    // Long if three quotes open it ("" alone is the empty string).
    if start + 2 >= len && !eof {
        return Lexed::NeedMore;
    }
    let long = input.get(start + 1) == Some(&quote) && input.get(start + 2) == Some(&quote);
    let content = if long { start + 3 } else { start + 1 };
    let mut i = content;
    let mut escaped = false;
    loop {
        // A short string can't hold a raw line break ('\n' found here, '\r' checked below).
        let found = if long {
            memchr::memchr2(quote, b'\\', &input[i..])
        } else {
            memchr::memchr3(quote, b'\\', b'\n', &input[i..])
        };
        let Some(k) = found else {
            if !long && input[i..].contains(&b'\r') {
                return Lexed::Error(start, "a line break in a short string");
            }
            return if eof {
                Lexed::Error(start, "a string without its closing quote")
            } else {
                Lexed::NeedMore
            };
        };
        let at = i + k;
        let b = input[at];
        if !long && (b == b'\n' || input[i..at].contains(&b'\r')) {
            return Lexed::Error(at, "a line break in a short string");
        }
        if b == b'\\' {
            escaped = true;
            if at + 1 >= len {
                return if eof {
                    Lexed::Error(at, "an escape at the end of the input")
                } else {
                    Lexed::NeedMore
                };
            }
            i = at + 2;
            continue;
        }
        // A quote: the end of a short string, or maybe of a long one.
        if !long {
            return token(Kind::String { escaped }, start, content..at, at + 1);
        }
        if at + 2 >= len {
            if !eof {
                return Lexed::NeedMore;
            }
            return Lexed::Error(start, "a long string without its closing quotes");
        }
        if input[at + 1] == quote && input[at + 2] == quote {
            return token(Kind::String { escaped }, start, content..at, at + 3);
        }
        i = at + 1;
    }
}

fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}

fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || matches!(c, '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// `_:label`: `(PN_CHARS_U | [0-9]) ((PN_CHARS | '.')* PN_CHARS)?`.
fn blank_label(input: &[u8], start: usize, eof: bool) -> Lexed {
    let label = start + 2;
    match char_at(input, label, eof) {
        Ok(Some(c)) if is_pn_chars_u(c) || c.is_ascii_digit() => {}
        Ok(_) => return Lexed::Error(label, "an invalid blank node label"),
        Err(lexed) => return lexed,
    }
    let mut end = label;
    let mut i = label;
    loop {
        match char_at(input, i, eof) {
            Ok(Some(c)) if is_pn_chars(c) || c == '.' || i == label => {
                i += c.len_utf8();
                if c != '.' {
                    end = i;
                }
            }
            Ok(_) => break,
            Err(lexed) => return lexed,
        }
    }
    // Trailing dots end the statement.
    token(Kind::BlankLabel, start, label..end, end)
}

/// `@tag` after a string: `[a-zA-Z]+ ('-' [a-zA-Z0-9]+)*`.
fn language_tag(input: &[u8], start: usize, eof: bool) -> Lexed {
    let len = input.len();
    let mut end = start + 1;
    while end < len && input[end].is_ascii_alphabetic() {
        end += 1;
    }
    if end == start + 1 {
        return Lexed::Error(start, "an empty language tag");
    }
    loop {
        if end == len && !eof {
            return Lexed::NeedMore;
        }
        if input.get(end) != Some(&b'-') {
            break;
        }
        let part = end + 1;
        let mut stop = part;
        while stop < len && input[stop].is_ascii_alphanumeric() {
            stop += 1;
        }
        if stop == len && !eof {
            return Lexed::NeedMore;
        }
        if stop == part {
            return Lexed::Error(end, "an empty language subtag");
        }
        end = stop;
    }
    token(Kind::LangTag, start, start + 1..end, end)
}

/// `INTEGER`, `DECIMAL` or `DOUBLE`. A '.' not followed by a digit (nor by an exponent
/// after digits) ends the statement, and isn't part of the number.
fn number(input: &[u8], start: usize, eof: bool) -> Lexed {
    let len = input.len();
    let digits = |from: usize| {
        let mut i = from;
        while i < len && input[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let mut i = start;
    if matches!(input[i], b'+' | b'-') {
        i += 1;
    }
    let whole_end = digits(i);
    let whole = whole_end - i;
    i = whole_end;
    if i == len && !eof {
        return Lexed::NeedMore;
    }
    let mut kind = Kind::Integer;
    if input.get(i) == Some(&b'.') {
        if i + 1 >= len && !eof {
            return Lexed::NeedMore;
        }
        let fraction_end = digits(i + 1);
        let fraction = fraction_end - (i + 1);
        if fraction_end == len && !eof {
            return Lexed::NeedMore;
        }
        if fraction > 0 {
            kind = Kind::Decimal;
            i = fraction_end;
        } else if whole > 0 && matches!(input.get(i + 1), Some(b'e' | b'E')) {
            // "1.e5": a double; the exponent is read below.
            i += 1;
        }
    }
    if whole == 0 && kind == Kind::Integer {
        return Lexed::Error(start, "a number without digits");
    }
    if matches!(input.get(i), Some(b'e' | b'E')) {
        let mut e = i + 1;
        if matches!(input.get(e), Some(b'+' | b'-')) {
            e += 1;
        }
        let exponent_end = digits(e);
        if exponent_end == len && !eof {
            return Lexed::NeedMore;
        }
        if exponent_end > e {
            kind = Kind::Double;
            i = exponent_end;
        } else if input[i - 1] == b'.' {
            return Lexed::Error(i, "an exponent without digits");
        }
    }
    token(kind, start, start..i, i)
}

/// A name: a prefixed name, or a keyword (`a`, `true`, `false`, `PREFIX`, `BASE`, `GRAPH`;
/// in N3 also `has`, `is`, `of`, `id`).
fn name(input: &[u8], start: usize, eof: bool, n3: bool) -> Lexed {
    match char_at(input, start, eof) {
        Ok(Some(c)) if is_pn_chars_base(c) => {}
        Ok(_) => return Lexed::Error(start, "unexpected character"),
        Err(lexed) => return lexed,
    }
    // PN_PREFIX: PN_CHARS_BASE ((PN_CHARS | '.')* PN_CHARS)?
    let mut i = start;
    loop {
        match char_at(input, i, eof) {
            Ok(Some(c)) if is_pn_chars(c) || c == '.' => i += c.len_utf8(),
            Ok(Some(':')) => return prefixed_name(input, start, i, eof),
            Ok(_) => break,
            Err(lexed) => return lexed,
        }
    }
    // A keyword: trailing dots end the statement.
    let mut end = i;
    while input[end - 1] == b'.' {
        end -= 1;
    }
    let word = &input[start..end];
    let kind = match word {
        b"a" => Kind::A,
        b"true" => Kind::True,
        b"false" => Kind::False,
        _ if word.eq_ignore_ascii_case(b"PREFIX") => Kind::Prefix,
        _ if word.eq_ignore_ascii_case(b"BASE") => Kind::Base,
        _ if word.eq_ignore_ascii_case(b"GRAPH") && !n3 => Kind::Graph,
        b"has" if n3 => Kind::Has,
        b"is" if n3 => Kind::Is,
        b"of" if n3 => Kind::Of,
        b"id" if n3 => Kind::Id,
        _ => return Lexed::Error(start, "a name without ':' that is no keyword"),
    };
    token(kind, start, start..end, end)
}

/// `prefix:local` with the colon at `colon` (`start == colon` for the empty prefix).
/// `PN_LOCAL ::= (PN_CHARS_U | ':' | [0-9] | PLX) ((PN_CHARS | '.' | ':' | PLX)* (PN_CHARS | ':' | PLX))?`
fn prefixed_name(input: &[u8], start: usize, colon: usize, eof: bool) -> Lexed {
    if colon > start && input[colon - 1] == b'.' {
        return Lexed::Error(colon - 1, "a prefix can't end with '.'");
    }
    let local = colon + 1;
    let mut i = local;
    let mut end = local;
    let mut escaped = false;
    loop {
        let c = match char_at(input, i, eof) {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(lexed) => return lexed,
        };
        let first = i == local;
        if c == '%' {
            // PERCENT: '%' HEX HEX
            match (input.get(i + 1), input.get(i + 2)) {
                (Some(a), Some(b)) if a.is_ascii_hexdigit() && b.is_ascii_hexdigit() => {
                    i += 3;
                    end = i;
                }
                (None, _) | (_, None) if !eof => return Lexed::NeedMore,
                _ => return Lexed::Error(i, "an invalid percent encoding"),
            }
        } else if c == '\\' {
            // PN_LOCAL_ESC
            match input.get(i + 1) {
                Some(
                    b'_' | b'~' | b'.' | b'-' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*'
                    | b'+' | b',' | b';' | b'=' | b'/' | b'?' | b'#' | b'@' | b'%',
                ) => {
                    escaped = true;
                    i += 2;
                    end = i;
                }
                None if !eof => return Lexed::NeedMore,
                _ => return Lexed::Error(i, "an invalid escape in a local name"),
            }
        } else if (first && (is_pn_chars_u(c) || c == ':' || c.is_ascii_digit()))
            || (!first && (is_pn_chars(c) || c == ':'))
        {
            i += c.len_utf8();
            end = i;
        } else if !first && c == '.' {
            i += 1;
        } else {
            break;
        }
    }
    // Trailing dots (left out of `end`) end the statement.
    token(
        Kind::PrefixedName {
            colon: colon - start,
            escaped,
        },
        start,
        start..end,
        end,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens of a complete input, as (kind, text).
    fn tokens(text: &str) -> Vec<(Kind, String)> {
        let bytes = text.as_bytes();
        let mut at = 0;
        let mut out = Vec::new();
        let mut after_string = false;
        loop {
            match lex(&bytes[at..], true, after_string, false) {
                Lexed::Token(t) => {
                    let piece =
                        String::from_utf8(bytes[at + t.text.start..at + t.text.end].to_vec())
                            .unwrap();
                    if t.kind == Kind::Eof {
                        return out;
                    }
                    after_string = matches!(t.kind, Kind::String { .. });
                    out.push((t.kind, piece));
                    at += t.consumed;
                }
                Lexed::NeedMore => panic!("need more at the end"),
                Lexed::Error(i, why) => panic!("{why} at {}", at + i),
            }
        }
    }

    #[test]
    fn statements_tokenise() {
        let t = tokens(
            "@prefix ex: <http://e/> .\nex:s a ex:C ; ex:p \"x\"@en-GB, 'y', \"\"\"long \"q\" \"\"\" , 1, -2.5, 3e4, .5, 1.e2, true .# c\n",
        );
        let kinds: Vec<Kind> = t.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds[..4],
            [
                Kind::AtPrefix,
                Kind::PrefixedName {
                    colon: 2,
                    escaped: false
                },
                Kind::IriRef { escaped: false },
                Kind::Dot
            ]
        );
        assert!(t.contains(&(Kind::LangTag, "en-GB".into())));
        assert!(t.contains(&(Kind::String { escaped: false }, "long \"q\" ".into())));
        assert!(t.contains(&(Kind::Decimal, "-2.5".into())));
        assert!(t.contains(&(Kind::Double, "3e4".into())));
        assert!(t.contains(&(Kind::Decimal, ".5".into())));
        assert!(t.contains(&(Kind::Double, "1.e2".into())));
        assert_eq!(t.last().unwrap().0, Kind::Dot);
    }

    #[test]
    fn dots_end_statements() {
        assert_eq!(
            tokens("ex:a.b. _:x. 1. a."),
            [
                (
                    Kind::PrefixedName {
                        colon: 2,
                        escaped: false
                    },
                    "ex:a.b".into()
                ),
                (Kind::Dot, ".".into()),
                (Kind::BlankLabel, "x".into()),
                (Kind::Dot, ".".into()),
                (Kind::Integer, "1".into()),
                (Kind::Dot, ".".into()),
                (Kind::A, "a".into()),
                (Kind::Dot, ".".into()),
            ]
        );
    }

    #[test]
    fn a_token_split_by_the_buffer_asks_for_more() {
        for (text, cut) in [
            ("<http://e/x>", 5),
            ("\"abc\"", 3),
            ("ex:local", 4),
            ("12.5", 3),
            ("\"\"\"a\"\"\"", 5),
            ("^^", 1),
            ("@en-GB", 4),
        ] {
            assert!(
                matches!(
                    lex(&text.as_bytes()[..cut], false, text.starts_with('@'), false),
                    Lexed::NeedMore
                ),
                "{text}"
            );
            assert!(
                matches!(
                    lex(text.as_bytes(), true, text.starts_with('@'), false),
                    Lexed::Token(_)
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn bad_tokens_are_errors() {
        for text in [
            "\"open", "<open", "_:", "ex.:x", "@pre", "^x", "\"a\nb\"", "word",
        ] {
            assert!(
                matches!(lex(text.as_bytes(), true, false, false), Lexed::Error(..)),
                "{text}"
            );
        }
    }
}
