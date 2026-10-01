//! The terminals of the grammar (SPARQL 1.1 §19.8, with the SPARQL 1.2 additions): IRIs,
//! prefixed names, variables, blank nodes, strings, numbers, language tags.

use nrese_rdf::vocab::xsd;
use nrese_rdf::{BaseDirection, BlankNode, Iri, Literal, NamedNode, Variable};

use super::{ParseResult, Parser};

impl<'a> Parser<'a> {
    // --- IRIs -----------------------------------------------------------------------

    /// Whether an IRI (`<…>`, not `<<`) or a prefixed name comes next.
    pub(super) fn at_iri(&mut self) -> bool {
        match self.peek() {
            Some(b'<') => self.byte_at(self.pos + 1) != Some(b'<'),
            Some(b':') => true,
            Some(b) if b.is_ascii_alphabetic() || b >= 0x80 => self.at_prefixed_name(),
            _ => false,
        }
    }

    /// An IRI or prefixed name, if one comes next.
    pub(super) fn try_iri(&mut self) -> ParseResult<Option<NamedNode>> {
        if !self.at_iri() {
            return Ok(None);
        }
        if self.bytes[self.pos] == b'<' {
            self.iriref().map(Some)
        } else {
            self.prefixed_name().map(Some)
        }
    }

    pub(super) fn iri(&mut self) -> ParseResult<NamedNode> {
        match self.try_iri()? {
            Some(iri) => Ok(iri),
            None => Err(self.expected("an IRI")),
        }
    }

    /// The content of an `IRIREF`, escapes decoded, not yet resolved.
    pub(super) fn iriref_text(&mut self) -> ParseResult<String> {
        self.ws();
        let start = self.pos;
        if self.byte_at(start) != Some(b'<') {
            return Err(self.expected("an IRI"));
        }
        let mut out = String::new();
        let mut i = start + 1;
        let mut run = i;
        loop {
            let Some(b) = self.byte_at(i) else {
                return Err(self.error_at(start, "an IRI isn't closed with '>'"));
            };
            match b {
                b'>' => {
                    out.push_str(&self.text[run..i]);
                    self.pos = i + 1;
                    return Ok(out);
                }
                b'\\' => {
                    out.push_str(&self.text[run..i]);
                    let (c, len) = self.unicode_escape(i)?;
                    out.push(c);
                    i += len;
                    run = i;
                }
                b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | 0..=0x20 => {
                    return Err(self.error_at(
                        i,
                        format!("the character {:?} can't stand in an IRI", char::from(b)),
                    ));
                }
                _ => i += 1,
            }
        }
    }

    /// An `IRIREF`, resolved against the base IRI.
    pub(super) fn iriref(&mut self) -> ParseResult<NamedNode> {
        let start = self.peek_offset();
        let text = self.iriref_text()?;
        self.resolve(&text, start)
    }

    pub(super) fn resolve(&self, iri: &str, at: usize) -> ParseResult<NamedNode> {
        let resolved = match &self.base {
            Some(base) => base.resolve(iri),
            None => Iri::parse(iri.to_owned()),
        };
        match resolved {
            Ok(iri) => Ok(NamedNode::new_from_iri(iri)),
            Err(error) if self.base.is_none() => Err(self.error_at(
                at,
                format!("<{iri}> is not an absolute IRI and there is no base IRI: {error}"),
            )),
            Err(error) => Err(self.error_at(at, format!("an invalid IRI <{iri}>: {error}"))),
        }
    }

    /// Whether an `IRIREF` token starts at `at` (the longest-token rule: `<a>` is an IRI
    /// even where `<` could be an operator).
    pub(super) fn iriref_token_at(&self, at: usize) -> bool {
        let mut i = at + 1;
        loop {
            match self.byte_at(i) {
                Some(b'>') => return true,
                Some(b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | 0..=0x20) | None => {
                    return false;
                }
                Some(b'\\') => {
                    if !matches!(self.byte_at(i + 1), Some(b'u' | b'U')) {
                        return false;
                    }
                    i += 2;
                }
                Some(_) => i += 1,
            }
        }
    }

    /// `\uXXXX` or `\UXXXXXXXX` at `at`: the character and the escape's length.
    fn unicode_escape(&self, at: usize) -> ParseResult<(char, usize)> {
        let digits = match self.byte_at(at + 1) {
            Some(b'u') => 4,
            Some(b'U') => 8,
            _ => {
                return Err(self.error_at(at, "only \\u and \\U escapes may stand in an IRI"));
            }
        };
        let hex = self
            .bytes
            .get(at + 2..at + 2 + digits)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .ok_or_else(|| {
                self.error_at(
                    at,
                    format!("a \\u escape needs {digits} hexadecimal digits"),
                )
            })?;
        let value = hex.iter().fold(0_u32, |acc, &h| {
            acc * 16 + char::from(h).to_digit(16).unwrap_or_default()
        });
        let c = char::from_u32(value).ok_or_else(|| {
            self.error_at(
                at,
                format!("\\u{value:X} is not a character (a surrogate?)"),
            )
        })?;
        Ok((c, 2 + digits))
    }

    /// `PNAME_NS` (the prefix, without the colon), at a prefixed name.
    pub(super) fn pname_ns(&mut self) -> ParseResult<&'a str> {
        self.ws();
        let start = self.pos;
        let mut i = start;
        let first = self.char_at(i);
        if let Some(c) = first.filter(|&c| is_pn_chars_base(c)) {
            i += c.len_utf8();
            let mut last_dot = false;
            while let Some(c) = self.char_at(i) {
                if is_pn_chars(c) {
                    last_dot = false;
                } else if c == '.' {
                    last_dot = true;
                } else {
                    break;
                }
                i += c.len_utf8();
            }
            if last_dot {
                return Err(self.error_at(i - 1, "a prefix can't end with '.'"));
            }
        }
        if self.byte_at(i) != Some(b':') {
            return Err(self.error_at(start, "expected a prefix followed by ':'"));
        }
        self.pos = i + 1;
        Ok(&self.text[start..i])
    }

    /// `PNAME_LN` or `PNAME_NS` as an IRI.
    fn prefixed_name(&mut self) -> ParseResult<NamedNode> {
        let start = self.peek_offset();
        let prefix = self.pname_ns()?;
        let Some(namespace) = self.prefixes.get(prefix) else {
            return Err(self.error_at(start, format!("the prefix '{prefix}:' is not declared")));
        };
        let extendable = namespace.extendable;
        let namespace_len = namespace.iri.len();
        let mut iri = namespace.iri.clone();
        self.pn_local(&mut iri)?;
        let plain = iri.as_bytes()[namespace_len..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
        if extendable && plain {
            return Ok(NamedNode::new_unchecked(iri));
        }
        NamedNode::new(iri)
            .map_err(|e| self.error_at(start, format!("the prefixed name is no IRI: {e}")))
    }

    /// `PN_LOCAL`, appended to `out` with its `\` escapes removed (percent escapes stay).
    fn pn_local(&mut self, out: &mut String) -> ParseResult<()> {
        let mut i = self.pos;
        let mut first = true;
        // Where the name ends if what follows the last accepted character are dots.
        let mut end = i;
        let mut accepted_len = out.len();
        while let Some(c) = self.char_at(i) {
            match c {
                '%' => {
                    let hex = self.bytes.get(i + 1..i + 3);
                    if !hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                        return Err(self.error_at(i, "'%' must be followed by two hex digits"));
                    }
                    out.push_str(&self.text[i..i + 3]);
                    i += 3;
                }
                '\\' => {
                    let Some(e) = self.char_at(i + 1).filter(|&e| is_local_escape(e)) else {
                        return Err(self.error_at(i, "an invalid escape in a local name"));
                    };
                    out.push(e);
                    i += 2;
                }
                '.' if !first => {
                    out.push('.');
                    i += 1;
                    continue; // a dot only counts if a name character follows
                }
                c if (first && (is_pn_chars_u(c) || c == ':' || c.is_ascii_digit()))
                    || (!first && (is_pn_chars(c) || c == ':')) =>
                {
                    out.push(c);
                    i += c.len_utf8();
                }
                _ => break,
            }
            first = false;
            end = i;
            accepted_len = out.len();
        }
        out.truncate(accepted_len);
        self.pos = end;
        Ok(())
    }

    fn char_at(&self, offset: usize) -> Option<char> {
        self.text.get(offset..)?.chars().next()
    }

    // --- Variables and blank nodes ----------------------------------------------------

    /// A variable (`?x` or `$x`), if one comes next.
    pub(super) fn try_variable(&mut self) -> ParseResult<Option<Variable>> {
        if !matches!(self.peek(), Some(b'?' | b'$')) {
            return Ok(None);
        }
        let start = self.pos + 1;
        let mut i = start;
        while let Some(c) = self.char_at(i) {
            let ok = if i == start {
                is_pn_chars_u(c) || c.is_ascii_digit()
            } else {
                is_pn_chars_u(c)
                    || c.is_ascii_digit()
                    || c == '\u{B7}'
                    || ('\u{300}'..='\u{36F}').contains(&c)
                    || ('\u{203F}'..='\u{2040}').contains(&c)
            };
            if !ok {
                break;
            }
            i += c.len_utf8();
        }
        if i == start {
            return Ok(None); // a lone '?' is a path modifier, not a variable
        }
        let name = &self.text[start..i];
        self.note_name(name);
        self.pos = i;
        Ok(Some(Variable::new_unchecked(name)))
    }

    pub(super) fn variable(&mut self) -> ParseResult<Variable> {
        match self.try_variable()? {
            Some(v) => Ok(v),
            None => Err(self.expected("a variable")),
        }
    }

    /// Whether a variable (not a lone `?`) comes next.
    pub(super) fn at_variable(&mut self) -> bool {
        matches!(self.peek(), Some(b'?' | b'$'))
            && self
                .char_at(self.pos + 1)
                .is_some_and(|c| is_pn_chars_u(c) || c.is_ascii_digit())
    }

    /// A blank node label (`_:x`) or `[]`, if one comes next. Labels belong to one group:
    /// one already used in a closed group is an error.
    pub(super) fn try_blank_node(&mut self) -> ParseResult<Option<BlankNode>> {
        match self.peek() {
            Some(b'_') if self.byte_at(self.pos + 1) == Some(b':') => {}
            Some(b'[') => {
                let save = self.pos;
                self.pos += 1;
                if self.eat("]") {
                    return Ok(Some(self.fresh_blank_node()));
                }
                self.pos = save;
                return Ok(None);
            }
            _ => return Ok(None),
        }
        let at = self.pos;
        let start = self.pos + 2;
        let mut i = start;
        let mut end = start;
        while let Some(c) = self.char_at(i) {
            let ok = if i == start {
                is_pn_chars_u(c) || c.is_ascii_digit()
            } else {
                is_pn_chars(c) || c == '.'
            };
            if !ok {
                break;
            }
            i += c.len_utf8();
            if c != '.' {
                end = i;
            }
        }
        if end == start {
            return Err(self.error_at(at, "a blank node label is empty"));
        }
        let label = &self.text[start..end];
        self.pos = end;
        self.note_name(label);
        if self.used_blank_nodes.contains(label) {
            return Err(self.error_at(
                at,
                format!("the blank node _:{label} is already used in another group"),
            ));
        }
        self.current_blank_nodes.insert(label.to_owned());
        Ok(Some(BlankNode::new_unchecked(label)))
    }

    /// Closes the blank node scope of a group: its labels may not be used again.
    pub(super) fn close_blank_node_scope(&mut self) {
        self.used_blank_nodes
            .extend(std::mem::take(&mut self.current_blank_nodes));
    }

    // --- Literals ---------------------------------------------------------------------

    /// Whether a string starts next.
    pub(super) fn at_string(&mut self) -> bool {
        matches!(self.peek(), Some(b'"' | b'\''))
    }

    /// A string literal of any of the four forms, escapes decoded.
    pub(super) fn string(&mut self) -> ParseResult<String> {
        self.ws();
        let start = self.pos;
        let Some(quote) = self.byte_at(start).filter(|b| matches!(b, b'"' | b'\'')) else {
            return Err(self.expected("a string"));
        };
        let long = self.byte_at(start + 1) == Some(quote) && self.byte_at(start + 2) == Some(quote);
        let mut i = start + if long { 3 } else { 1 };
        let mut out = String::new();
        let mut run = i;
        loop {
            let Some(b) = self.byte_at(i) else {
                return Err(self.error_at(start, "a string isn't closed"));
            };
            if b == quote {
                if !long {
                    out.push_str(&self.text[run..i]);
                    self.pos = i + 1;
                    return Ok(out);
                }
                if self.byte_at(i + 1) == Some(quote) && self.byte_at(i + 2) == Some(quote) {
                    // A long string may end in up to two more quotes: `""""` ends with `"`.
                    let mut end = i;
                    while self.byte_at(end + 3) == Some(quote) && end < i + 2 {
                        end += 1;
                    }
                    out.push_str(&self.text[run..end]);
                    self.pos = end + 3;
                    return Ok(out);
                }
                i += 1;
                continue;
            }
            match b {
                b'\\' => {
                    out.push_str(&self.text[run..i]);
                    let simple = match self.byte_at(i + 1) {
                        Some(b't') => Some('\t'),
                        Some(b'b') => Some('\u{8}'),
                        Some(b'n') => Some('\n'),
                        Some(b'r') => Some('\r'),
                        Some(b'f') => Some('\u{C}'),
                        Some(b'"') => Some('"'),
                        Some(b'\'') => Some('\''),
                        Some(b'\\') => Some('\\'),
                        _ => None,
                    };
                    if let Some(c) = simple {
                        out.push(c);
                        i += 2;
                    } else if matches!(self.byte_at(i + 1), Some(b'u' | b'U')) {
                        let (c, len) = self.unicode_escape(i)?;
                        out.push(c);
                        i += len;
                    } else {
                        return Err(self.error_at(i, "an invalid escape in a string"));
                    }
                    run = i;
                }
                b'\n' | b'\r' if !long => {
                    return Err(self.error_at(i, "a line break in a short string"));
                }
                _ => i += 1,
            }
        }
    }

    /// `String (LANGDIR | '^^' iri)?`, if a string comes next.
    pub(super) fn try_rdf_literal(&mut self) -> ParseResult<Option<Literal>> {
        if !self.at_string() {
            return Ok(None);
        }
        let value = self.string()?;
        // The tag follows the string directly.
        if self.byte_at(self.pos) == Some(b'@') {
            return self.language_tag(value).map(Some);
        }
        if self.eat("^^") {
            let datatype = self.iri()?;
            return Ok(Some(Literal::new_typed_literal(value, datatype)));
        }
        Ok(Some(Literal::new_simple_literal(value)))
    }

    /// `LANGDIR` after a string: `@tag` and, in SPARQL 1.2, `--ltr` or `--rtl`.
    fn language_tag(&mut self, value: String) -> ParseResult<Literal> {
        let start = self.pos + 1;
        let mut i = start;
        while self.byte_at(i).is_some_and(|b| b.is_ascii_alphabetic()) {
            i += 1;
        }
        if i == start {
            return Err(self.error_at(self.pos, "a language tag is empty"));
        }
        while self.byte_at(i) == Some(b'-')
            && self
                .byte_at(i + 1)
                .is_some_and(|b| b.is_ascii_alphanumeric())
        {
            i += 2;
            while self.byte_at(i).is_some_and(|b| b.is_ascii_alphanumeric()) {
                i += 1;
            }
        }
        let tag = &self.text[start..i];
        let mut direction = None;
        if self.bytes[i..].starts_with(b"--") {
            let d = i + 2;
            let mut e = d;
            while self.byte_at(e).is_some_and(|b| b.is_ascii_alphabetic()) {
                e += 1;
            }
            if !self.options.sparql_12 {
                return Err(self.error_at(i, "base directions need SPARQL 1.2"));
            }
            direction = Some(match &self.text[d..e] {
                "ltr" => BaseDirection::Ltr,
                "rtl" => BaseDirection::Rtl,
                other => {
                    return Err(self.error_at(
                        d,
                        format!("'{other}' is no base direction ('ltr' or 'rtl')"),
                    ));
                }
            });
            i = e;
        }
        let literal = match direction {
            Some(direction) => {
                Literal::new_directional_language_tagged_literal(value, tag, direction)
            }
            None => Literal::new_language_tagged_literal(value, tag),
        }
        .map_err(|e| self.error_at(start, format!("an invalid language tag '{tag}': {e}")))?;
        self.pos = i;
        Ok(literal)
    }

    /// Whether a number starts next, possibly signed: a sign counts only directly before
    /// the digits (`-1` is a number, `- 1` a negation).
    pub(super) fn at_number(&mut self, signed: bool) -> bool {
        let Some(b) = self.peek() else { return false };
        let i = if signed && matches!(b, b'+' | b'-') {
            self.pos + 1
        } else {
            self.pos
        };
        match self.byte_at(i) {
            Some(b'0'..=b'9') => true,
            Some(b'.') => self.byte_at(i + 1).is_some_and(|b| b.is_ascii_digit()),
            _ => false,
        }
    }

    /// `INTEGER`, `DECIMAL` or `DOUBLE`, with a sign directly before if `signed`.
    pub(super) fn numeric_literal(&mut self, signed: bool) -> ParseResult<Literal> {
        self.ws();
        let start = self.pos;
        let mut i = start;
        if signed && matches!(self.byte_at(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let digits = |p: &Self, mut i: usize| {
            while p.byte_at(i).is_some_and(|b| b.is_ascii_digit()) {
                i += 1;
            }
            i
        };
        // The end of an exponent starting at `e`, if one does.
        let exponent = |p: &Self, e: usize| {
            if !matches!(p.byte_at(e), Some(b'e' | b'E')) {
                return None;
            }
            let mut d = e + 1;
            if matches!(p.byte_at(d), Some(b'+' | b'-')) {
                d += 1;
            }
            let end = digits(p, d);
            (end > d).then_some(end)
        };
        let whole_end = digits(self, i);
        let has_whole = whole_end > i;
        let (end, datatype) = if self.byte_at(whole_end) == Some(b'.') {
            let fraction_end = digits(self, whole_end + 1);
            let has_fraction = fraction_end > whole_end + 1;
            match exponent(self, fraction_end) {
                // `1.5e3`, `.5e3`, `1.e3`
                Some(e) if has_whole || has_fraction => (e, xsd::DOUBLE),
                _ if has_fraction => (fraction_end, xsd::DECIMAL),
                // `1.` is the integer 1 and a dot that ends a triple
                _ => (whole_end, xsd::INTEGER),
            }
        } else {
            match exponent(self, whole_end) {
                Some(e) if has_whole => (e, xsd::DOUBLE),
                _ => (whole_end, xsd::INTEGER),
            }
        };
        if end == i || (datatype == xsd::INTEGER && !has_whole) {
            return Err(self.expected("a number"));
        }
        self.pos = end;
        Ok(Literal::new_typed_literal(&self.text[start..end], datatype))
    }

    /// `true` or `false` (any case), if one comes next.
    pub(super) fn try_boolean(&mut self) -> Option<Literal> {
        for value in ["true", "false"] {
            if self.keyword(value) {
                return Some(Literal::new_typed_literal(value, xsd::BOOLEAN));
            }
        }
        None
    }
}

/// `PN_CHARS_BASE`.
pub(super) fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// `PN_CHARS_U`.
fn is_pn_chars_u(c: char) -> bool {
    c == '_' || is_pn_chars_base(c)
}

/// `PN_CHARS`.
fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{B7}'
        || ('\u{300}'..='\u{36F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

/// The characters `PN_LOCAL_ESC` may escape.
fn is_local_escape(c: char) -> bool {
    matches!(
        c,
        '_' | '~'
            | '.'
            | '-'
            | '!'
            | '$'
            | '&'
            | '\''
            | '('
            | ')'
            | '*'
            | '+'
            | ','
            | ';'
            | '='
            | '/'
            | '?'
            | '#'
            | '@'
            | '%'
    )
}
