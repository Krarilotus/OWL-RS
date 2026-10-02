//! SPARQL Query Results TSV (§3 of the CSV/TSV specification): terms in SPARQL syntax,
//! one solution per line. Read and written.

use std::io::BufRead;

use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{
    BaseDirection, BlankNode, Literal, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, Term,
    TermRef, Triple, Variable,
};

use crate::error::{QueryResultsParseError, QueryResultsSyntaxError, TextPosition};

// ---------------------------------------------------------------------------------------
// Writing

pub(crate) fn write_head(out: &mut Vec<u8>, variables: &[Variable]) {
    for (i, variable) in variables.iter().enumerate() {
        if i > 0 {
            out.push(b'\t');
        }
        out.push(b'?');
        out.extend_from_slice(variable.as_str().as_bytes());
    }
    out.push(b'\n');
}

pub(crate) fn write_row(out: &mut Vec<u8>, row: &[Option<TermRef<'_>>]) {
    for (i, value) in row.iter().enumerate() {
        if i > 0 {
            out.push(b'\t');
        }
        if let Some(value) = value {
            write_term(out, *value);
        }
    }
    out.push(b'\n');
}

/// A term in the syntax TSV uses (SPARQL's, without prefixes): numbers and booleans in
/// their short form when their lexical form is one.
pub(crate) fn write_term(out: &mut Vec<u8>, term: TermRef<'_>) {
    match term {
        TermRef::NamedNode(iri) => {
            out.push(b'<');
            out.extend_from_slice(iri.as_str().as_bytes());
            out.push(b'>');
        }
        TermRef::BlankNode(b) => {
            out.extend_from_slice(b"_:");
            out.extend_from_slice(b.as_str().as_bytes());
        }
        TermRef::Literal(literal) => {
            let value = literal.value();
            if let Some(language) = literal.language() {
                write_quoted(out, value);
                out.push(b'@');
                out.extend_from_slice(language.as_bytes());
                if let Some(direction) = literal.direction() {
                    out.extend_from_slice(b"--");
                    out.extend_from_slice(direction.as_str().as_bytes());
                }
                return;
            }
            let datatype = literal.datatype();
            let short = (datatype == xsd::BOOLEAN && matches!(value, "true" | "false"))
                || (datatype == xsd::INTEGER && is_integer(value))
                || (datatype == xsd::DECIMAL && is_decimal(value))
                || (datatype == xsd::DOUBLE && is_double(value));
            if short {
                out.extend_from_slice(value.as_bytes());
            } else {
                write_quoted(out, value);
                if datatype != xsd::STRING {
                    out.extend_from_slice(b"^^<");
                    out.extend_from_slice(datatype.as_str().as_bytes());
                    out.push(b'>');
                }
            }
        }
        TermRef::Triple(triple) => {
            out.extend_from_slice(b"<<( ");
            write_term(out, NamedOrBlankNodeRef::from(&triple.subject).into());
            out.push(b' ');
            write_term(out, (&triple.predicate).into());
            out.push(b' ');
            write_term(out, (&triple.object).into());
            out.extend_from_slice(b" )>>");
        }
    }
}

/// A string with `\t \n \r \" \\` escaped; the rest as it is.
fn write_quoted(out: &mut Vec<u8>, text: &str) {
    out.push(b'"');
    // Most values have nothing to escape: find that out with one bit test per byte, with
    // no early exit so it vectorises.
    const ESCAPED: u128 = 1 << b'\t' | 1 << b'\n' | 1 << b'\r' | 1 << b'"' | 1 << b'\\';
    if !text
        .bytes()
        .fold(false, |found, b| found | (b < 128 && ESCAPED >> b & 1 == 1))
    {
        out.extend_from_slice(text.as_bytes());
        out.push(b'"');
        return;
    }
    let mut run = 0;
    for (i, b) in text.bytes().enumerate() {
        let escaped: &[u8] = match b {
            b'\t' => b"\\t",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            _ => continue,
        };
        out.extend_from_slice(&text.as_bytes()[run..i]);
        out.extend_from_slice(escaped);
        run = i + 1;
    }
    out.extend_from_slice(&text.as_bytes()[run..]);
    out.push(b'"');
}

/// `[+-]? [0-9]+`
fn is_integer(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `[+-]? [0-9]* '.' [0-9]+`
fn is_decimal(text: &str) -> bool {
    let rest = text.strip_prefix(['+', '-']).unwrap_or(text);
    let Some((whole, fraction)) = rest.split_once('.') else {
        return false;
    };
    whole.bytes().all(|b| b.is_ascii_digit())
        && !fraction.is_empty()
        && fraction.bytes().all(|b| b.is_ascii_digit())
}

/// `[+-]? ([0-9]+ '.' [0-9]* | '.' [0-9]+ | [0-9]+) [eE] [+-]? [0-9]+`
fn is_double(text: &str) -> bool {
    let rest = text.strip_prefix(['+', '-']).unwrap_or(text);
    let Some(e) = rest.find(['e', 'E']) else {
        return false;
    };
    let (mantissa, exponent) = (&rest[..e], &rest[e + 1..]);
    let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (!whole.is_empty() || !fraction.is_empty())
                && whole.bytes().all(|b| b.is_ascii_digit())
                && fraction.bytes().all(|b| b.is_ascii_digit())
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|b| b.is_ascii_digit()),
    };
    mantissa_ok && !exponent.is_empty() && exponent.bytes().all(|b| b.is_ascii_digit())
}

// ---------------------------------------------------------------------------------------
// Reading

/// What the first line says: the variables, or a boolean result.
pub(crate) enum Head {
    Variables(Vec<Variable>),
    Boolean(bool),
}

/// The head line (`?a\t?b`, or `true`/`false` for a boolean).
pub(crate) fn parse_head(line: &str) -> Result<Head, QueryResultsSyntaxError> {
    let line = line.trim_end_matches(['\r', '\n']);
    match line {
        "true" => return Ok(Head::Boolean(true)),
        "false" => return Ok(Head::Boolean(false)),
        "" => return Ok(Head::Variables(Vec::new())),
        _ => {}
    }
    line.split('\t')
        .map(|name| {
            let bare = name
                .strip_prefix(['?', '$'])
                .ok_or_else(|| error(0, format!("a variable without ? or $: {name:?}")))?;
            Variable::new(bare).map_err(|e| error(0, e.to_string()))
        })
        .collect::<Result<_, _>>()
        .map(Head::Variables)
}

/// The values of a solution line (`line_number` from 0, for errors).
pub(crate) fn parse_row(
    line: &str,
    width: usize,
    line_number: u64,
) -> Result<Vec<Option<Term>>, QueryResultsSyntaxError> {
    let line = line.trim_end_matches(['\r', '\n']);
    let mut values = Vec::with_capacity(width);
    // A line with only the empty string is one unbound value per variable (one variable)
    // or a solution with no variables.
    if width == 0 {
        return if line.is_empty() {
            Ok(values)
        } else {
            Err(error(line_number, "a value but no variables"))
        };
    }
    for field in line.split('\t') {
        values.push(if field.is_empty() {
            None
        } else {
            let mut cursor = Cursor { text: field, at: 0 };
            let term = cursor
                .term(0)
                .map_err(|message| error(line_number, format!("{message} in {field:?}")))?;
            cursor.skip_space();
            if cursor.at != field.len() {
                return Err(error(
                    line_number,
                    format!("text after the term in {field:?}"),
                ));
            }
            Some(term)
        });
    }
    if values.len() != width {
        return Err(error(
            line_number,
            format!("{} values for {width} variables", values.len()),
        ));
    }
    Ok(values)
}

fn error(line: u64, message: impl Into<String>) -> QueryResultsSyntaxError {
    let at = TextPosition {
        line,
        column: 0,
        offset: 0,
    };
    QueryResultsSyntaxError::located(message, at..at)
}

/// How deep triple terms may nest: deeper input is an error, not a stack overflow.
const MAX_NESTING: usize = 64;

struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Cursor<'a> {
    /// The text not read yet (borrowed from the line, not the cursor).
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn skip_space(&mut self) {
        while self.rest().starts_with(' ') {
            self.at += 1;
        }
    }

    fn term(&mut self, depth: usize) -> Result<Term, String> {
        self.skip_space();
        let rest = self.rest();
        if rest.starts_with("<<(") {
            if depth >= MAX_NESTING {
                return Err("triple terms nested too deeply".to_owned());
            }
            self.at += 3;
            let subject = NamedOrBlankNode::try_from(self.term(depth + 1)?)
                .map_err(|e| format!("a triple term's subject: {e}"))?;
            let Term::NamedNode(predicate) = self.term(depth + 1)? else {
                return Err("a triple term's predicate must be an IRI".to_owned());
            };
            let object = self.term(depth + 1)?;
            self.skip_space();
            if !self.rest().starts_with(")>>") {
                return Err("expected ')>>'".to_owned());
            }
            self.at += 3;
            return Ok(Triple::new(subject, predicate, object).into());
        }
        match rest.as_bytes().first() {
            Some(b'<') => Ok(self.iri()?.into()),
            Some(b'_') => {
                let label = rest
                    .strip_prefix("_:")
                    .ok_or("expected '_:' for a blank node")?;
                let end = label.find([' ', ')']).unwrap_or(label.len());
                let node = BlankNode::new(&label[..end]).map_err(|e| e.to_string())?;
                self.at += 2 + end;
                Ok(node.into())
            }
            Some(b'"' | b'\'') => self.literal(),
            Some(_) => {
                let end = rest.find([' ', ')']).unwrap_or(rest.len());
                let word = &rest[..end];
                let datatype = match word {
                    "true" | "false" => xsd::BOOLEAN,
                    w if is_integer(w) => xsd::INTEGER,
                    w if is_decimal(w) => xsd::DECIMAL,
                    w if is_double(w) => xsd::DOUBLE,
                    _ => return Err(format!("not a term: {word:?}")),
                };
                self.at += end;
                Ok(Literal::new_typed_literal(word, datatype).into())
            }
            None => Err("a missing term".to_owned()),
        }
    }

    fn iri(&mut self) -> Result<NamedNode, String> {
        let rest = self.rest();
        let end = rest.find('>').ok_or("an IRI without its closing '>'")?;
        let raw = &rest[1..end];
        let iri = if raw.contains('\\') {
            unescape(raw, false)?
        } else {
            raw.to_owned()
        };
        self.at += end + 1;
        NamedNode::new(iri).map_err(|e| e.to_string())
    }

    fn literal(&mut self) -> Result<Term, String> {
        let rest = self.rest();
        let quote = rest.as_bytes()[0];
        // The closing quote: the first one not escaped.
        let mut end = None;
        let mut escaped = false;
        for (i, b) in rest.bytes().enumerate().skip(1) {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                end = Some(i);
                break;
            }
        }
        let end = end.ok_or("a string without its closing quote")?;
        let raw = &rest[1..end];
        let value = if raw.contains('\\') {
            unescape(raw, true)?
        } else {
            raw.to_owned()
        };
        self.at += end + 1;
        let rest = self.rest();
        if let Some(tag) = rest.strip_prefix('@') {
            let stop = tag.find([' ', ')']).unwrap_or(tag.len());
            let tag = &tag[..stop];
            self.at += 1 + stop;
            let (language, direction) = match tag.split_once("--") {
                Some((language, direction)) => (
                    language,
                    Some(
                        direction
                            .parse::<BaseDirection>()
                            .map_err(|e| e.to_string())?,
                    ),
                ),
                None => (tag, None),
            };
            let literal = match direction {
                Some(direction) => {
                    Literal::new_directional_language_tagged_literal(value, language, direction)
                }
                None => Literal::new_language_tagged_literal(value, language),
            };
            return literal.map(Term::from).map_err(|e| e.to_string());
        }
        if rest.starts_with("^^<") {
            self.at += 2;
            let datatype = self.iri()?;
            if datatype == rdf::LANG_STRING || datatype == rdf::DIR_LANG_STRING {
                return Err("a language-string datatype without a language tag".to_owned());
            }
            return Ok(Literal::new_typed_literal(value, datatype).into());
        }
        Ok(Literal::new_simple_literal(value).into())
    }
}

/// `\uXXXX`, `\UXXXXXXXX`, and in strings also `\t \b \n \r \f \" \' \\`.
fn unescape(text: &str, string: bool) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let escaped = chars.next().ok_or("an escape at the end")?;
        match escaped {
            'u' | 'U' => {
                let digits: String = chars
                    .by_ref()
                    .take(if escaped == 'u' { 4 } else { 8 })
                    .collect();
                let code = u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("an invalid escape \\{escaped}{digits}"))?;
                out.push(code);
            }
            't' if string => out.push('\t'),
            'b' if string => out.push('\u{8}'),
            'n' if string => out.push('\n'),
            'r' if string => out.push('\r'),
            'f' if string => out.push('\u{c}'),
            '"' | '\'' | '\\' if string => out.push(escaped),
            other => return Err(format!("an invalid escape \\{other}")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Line sources: a slice, or a reader

/// The next line of `bytes` from `*at` (without its line break), or `None` at the end.
pub(crate) fn next_slice_line<'a>(
    bytes: &'a [u8],
    at: &mut usize,
) -> Option<Result<&'a str, QueryResultsSyntaxError>> {
    if *at >= bytes.len() {
        return None;
    }
    let rest = &bytes[*at..];
    let end = memchr::memchr(b'\n', rest).unwrap_or(rest.len());
    *at += (end + 1).min(rest.len());
    Some(
        std::str::from_utf8(&rest[..end])
            .map_err(|_| QueryResultsSyntaxError::msg("invalid UTF-8")),
    )
}

/// The next line of `reader` into `buffer`; `false` at the end.
pub(crate) fn next_reader_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut String,
) -> Result<bool, QueryResultsParseError> {
    buffer.clear();
    Ok(reader.read_line(buffer)? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(term: Term) {
        let mut out = Vec::new();
        write_term(&mut out, term.as_ref());
        let text = String::from_utf8(out).unwrap();
        let values = parse_row(&text, 1, 0).unwrap();
        assert_eq!(values, vec![Some(term)], "{text}");
    }

    #[test]
    fn terms_round_trip() {
        let s = NamedNode::new_unchecked("http://e/s");
        for term in [
            Term::from(s.clone()),
            BlankNode::new_unchecked("b1").into(),
            Literal::new_simple_literal("tab\tnew\nline \"q\" \\ é").into(),
            Literal::new_language_tagged_literal_unchecked("x", "en-gb").into(),
            Literal::new_directional_language_tagged_literal_unchecked(
                "x",
                "ar",
                BaseDirection::Rtl,
            )
            .into(),
            Literal::new_typed_literal("12", xsd::INTEGER).into(),
            Literal::new_typed_literal("-1.5", xsd::DECIMAL).into(),
            Literal::new_typed_literal("1.0E3", xsd::DOUBLE).into(),
            Literal::new_typed_literal("true", xsd::BOOLEAN).into(),
            Literal::new_typed_literal("01", xsd::BOOLEAN).into(),
            Literal::new_typed_literal("x", NamedNode::new_unchecked("http://e/dt")).into(),
            Triple::new(
                BlankNode::new_unchecked("b"),
                s.clone(),
                Triple::new(s.clone(), s.clone(), Literal::new_simple_literal("o")),
            )
            .into(),
        ] {
            round_trip(term);
        }
    }

    #[test]
    fn heads_rows_and_errors() {
        assert!(matches!(parse_head("?a\t$b").unwrap(), Head::Variables(v) if v.len() == 2));
        assert!(matches!(parse_head("true").unwrap(), Head::Boolean(true)));
        assert!(parse_head("a\tb").is_err());
        assert_eq!(parse_row("\t<http://e/x>", 2, 1).unwrap()[0], None);
        assert!(parse_row("<http://e/x>", 2, 1).is_err());
        assert!(parse_row("\"unterminated", 1, 1).is_err());
        assert!(
            parse_row(
                "\"x\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#langString>",
                1,
                1
            )
            .is_err()
        );
        assert!(parse_row("\"x\"@en--LTR", 1, 1).is_err());
    }
}
