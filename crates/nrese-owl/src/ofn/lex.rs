//! The tokens of the OWL 2 Functional-Style Syntax (W3C *OWL 2 Structural Specification*,
//! §2.3 and §2.4): parentheses, `=`, full IRIs, names (keywords, prefixed names, numbers,
//! blank node labels), quoted strings with their `^^` datatype or `@` language tag;
//! white space and `#` comments between them. Tokens borrow from the text and carry their
//! byte offset (a diagnostic's line and column are computed from it).

use std::borrow::Cow;

/// A token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tok<'t> {
    Open,
    Close,
    Equals,
    /// `<…>`, without the brackets.
    Iri(&'t str),
    /// A keyword, a prefixed name (`p:local`, `:local`, `p:`), a number, a blank node
    /// label (`_:x`).
    Name(&'t str),
    /// A quoted string, unescaped.
    Str(Cow<'t, str>),
    /// `^^`.
    Carets,
    /// `@tag`, without the `@`.
    Lang(&'t str),
    End,
    /// Something no token starts with (the character), or an unterminated string or IRI.
    Bad(&'static str),
}

pub(super) struct Lexer<'t> {
    text: &'t str,
    at: usize,
    /// Parentheses open now (of the tokens taken).
    pub depth: u32,
}

/// The characters that end a name.
fn ends_name(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'<' | b'>' | b'"' | b'=' | b'^' | b'@'
    )
}

impl<'t> Lexer<'t> {
    pub fn new(text: &'t str) -> Self {
        Self {
            text,
            at: 0,
            depth: 0,
        }
    }

    pub fn text(&self) -> &'t str {
        self.text
    }

    /// Skips white space and comments; the offset of what follows.
    fn skip(&mut self) -> usize {
        let bytes = self.text.as_bytes();
        while self.at < bytes.len() {
            match bytes[self.at] {
                b' ' | b'\t' | b'\n' | b'\r' => self.at += 1,
                b'#' => {
                    while self.at < bytes.len() && bytes[self.at] != b'\n' {
                        self.at += 1;
                    }
                }
                _ => break,
            }
        }
        self.at
    }

    /// The next token and its offset.
    pub fn next(&mut self) -> (Tok<'t>, usize) {
        let start = self.skip();
        let bytes = self.text.as_bytes();
        let Some(&b) = bytes.get(start) else {
            return (Tok::End, start);
        };
        let tok = match b {
            b'(' => {
                self.at += 1;
                self.depth += 1;
                Tok::Open
            }
            b')' => {
                self.at += 1;
                self.depth = self.depth.saturating_sub(1);
                Tok::Close
            }
            b'=' => {
                self.at += 1;
                Tok::Equals
            }
            b'<' => match bytes[start + 1..].iter().position(|&c| c == b'>') {
                Some(end) => {
                    self.at = start + 1 + end + 1;
                    Tok::Iri(&self.text[start + 1..start + 1 + end])
                }
                None => {
                    self.at = bytes.len();
                    Tok::Bad("an IRI without its closing '>'")
                }
            },
            b'^' if bytes.get(start + 1) == Some(&b'^') => {
                self.at += 2;
                Tok::Carets
            }
            b'@' => {
                let from = start + 1;
                let mut end = from;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-')
                {
                    end += 1;
                }
                self.at = end;
                if end == from {
                    Tok::Bad("an '@' without a language tag")
                } else {
                    Tok::Lang(&self.text[from..end])
                }
            }
            b'"' => self.string(start),
            b'>' | b'^' => {
                self.at += 1;
                Tok::Bad("a character no token starts with")
            }
            _ => {
                let mut end = start;
                while end < bytes.len() && !ends_name(bytes[end]) {
                    end += 1;
                }
                self.at = end;
                Tok::Name(&self.text[start..end])
            }
        };
        (tok, start)
    }

    /// A quoted string from `start` (its opening quote): `\"` and `\\` are the escapes.
    fn string(&mut self, start: usize) -> Tok<'t> {
        let bytes = self.text.as_bytes();
        let mut i = start + 1;
        let mut escaped = false;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    self.at = i + 1;
                    let raw = &self.text[start + 1..i];
                    if !escaped {
                        return Tok::Str(Cow::Borrowed(raw));
                    }
                    let mut out = String::with_capacity(raw.len());
                    let mut chars = raw.chars();
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            match chars.next() {
                                Some(e @ ('"' | '\\')) => out.push(e),
                                _ => return Tok::Bad("an escape other than \\\" or \\\\"),
                            }
                        } else {
                            out.push(c);
                        }
                    }
                    return Tok::Str(Cow::Owned(out));
                }
                b'\\' => {
                    escaped = true;
                    i += 2;
                }
                _ => i += 1,
            }
        }
        self.at = bytes.len();
        Tok::Bad("a string without its closing quote")
    }
}

/// The line and column (from 1, the column in characters) of byte offset `at`.
pub(super) fn position(text: &str, at: usize) -> (u32, u32) {
    let before = &text[..at.min(text.len())];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].chars().count() as u32 + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<Tok<'_>> {
        let mut l = Lexer::new(text);
        let mut out = Vec::new();
        loop {
            let (t, _) = l.next();
            if t == Tok::End {
                return out;
            }
            out.push(t);
        }
    }

    #[test]
    fn tokens() {
        assert_eq!(
            all(
                "Prefix(:=<http://x/>) # a comment\nC(:a \"q\\\"\"^^xsd:string \"b\"@en-GB _:n 12)"
            ),
            vec![
                Tok::Name("Prefix"),
                Tok::Open,
                Tok::Name(":"),
                Tok::Equals,
                Tok::Iri("http://x/"),
                Tok::Close,
                Tok::Name("C"),
                Tok::Open,
                Tok::Name(":a"),
                Tok::Str(Cow::Owned("q\"".into())),
                Tok::Carets,
                Tok::Name("xsd:string"),
                Tok::Str(Cow::Borrowed("b")),
                Tok::Lang("en-GB"),
                Tok::Name("_:n"),
                Tok::Name("12"),
                Tok::Close,
            ]
        );
        assert_eq!(
            all("\"open"),
            vec![Tok::Bad("a string without its closing quote")]
        );
        assert_eq!(position("ab\ncdé f", 8), (2, 5));
    }
}
