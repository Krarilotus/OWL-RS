//! The pull parser: JSON text in, one event at a time out.
//!
//! One lexer serves two front ends: [`SliceJsonParser`] over a `&str` (strings without
//! escapes are borrowed from it for its whole lifetime) and [`ReaderJsonParser`] over any
//! `Read` (borrowed from its buffer until the next event). The lexer never consumes part
//! of a token: when the text in hand ends inside one, it asks for more, and the reader
//! front end refills and asks again.
//!
//! Strict RFC 8259: no trailing commas, comments, leading zeros, control characters in
//! strings or lone surrogates; a byte order mark at the start is skipped (§8.1). Nesting
//! deeper than a limit is an error, so a hostile document can't exhaust the stack of code
//! that walks the tree recursively (dropping one included).

use std::borrow::Cow;
use std::io::Read;
use std::ops::Range;

use crate::error::{JsonParseError, JsonSyntaxError};
use crate::value::{Object, Value};

/// How deep arrays and objects may nest by default.
pub const MAX_DEPTH: usize = 128;

/// One step through a JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonEvent<'a> {
    String(Cow<'a, str>),
    /// A number as written (it matches the grammar; its value is the reader's business).
    Number(Cow<'a, str>),
    Boolean(bool),
    Null,
    StartArray,
    EndArray,
    StartObject,
    ObjectKey(Cow<'a, str>),
    EndObject,
    /// The end of the document (or of the single value asked for).
    Eof,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// A value: the document's, an array item after a comma, or an entry's after the colon.
    Value,
    /// The first item of an array, or its end.
    ItemOrEnd,
    /// The first key of an object, or its end.
    KeyOrEnd,
    /// A key, after a comma.
    Key,
    /// The colon after a key.
    Colon,
    /// A comma, or the end of the container.
    CommaOrEnd,
    /// Nothing: the document's value is complete.
    Done,
}

/// An event as positions in the text.
#[derive(Clone, Copy, Debug)]
enum Raw {
    String {
        start: usize,
        end: usize,
        escaped: bool,
    },
    Key {
        start: usize,
        end: usize,
        escaped: bool,
    },
    Number {
        start: usize,
        end: usize,
    },
    Boolean(bool),
    Null,
    StartArray,
    EndArray,
    StartObject,
    EndObject,
    Eof,
}

/// The grammar's state, independent of where the text comes from.
#[derive(Debug, Clone)]
struct Lexer {
    expect: Expect,
    /// The open containers: `true` for an object.
    stack: Vec<bool>,
    max_depth: usize,
    /// Stop after one value, without looking at what follows.
    single: bool,
    /// The absolute offset of the text's first byte (the reader drops what it has read).
    base: u64,
    line: u64,
    line_start: u64,
    /// Where the last event's token started, in the text.
    token_start: usize,
}

impl Lexer {
    fn new(single: bool) -> Self {
        Self {
            expect: Expect::Value,
            stack: Vec::new(),
            max_depth: MAX_DEPTH,
            single,
            base: 0,
            line: 0,
            line_start: 0,
            token_start: 0,
        }
    }

    fn error(&self, message: impl Into<String>, at: usize) -> JsonSyntaxError {
        let offset = self.base + at as u64;
        JsonSyntaxError::new(
            message,
            self.line,
            offset.saturating_sub(self.line_start),
            offset,
        )
    }

    fn after_value(&mut self) {
        self.expect = if self.stack.is_empty() {
            Expect::Done
        } else {
            Expect::CommaOrEnd
        };
    }

    fn open(&mut self, object: bool, at: usize) -> Result<(), JsonSyntaxError> {
        if self.stack.len() >= self.max_depth {
            return Err(self.error(
                format!("nested deeper than {} arrays and objects", self.max_depth),
                at,
            ));
        }
        self.stack.push(object);
        self.expect = if object {
            Expect::KeyOrEnd
        } else {
            Expect::ItemOrEnd
        };
        Ok(())
    }

    fn close(&mut self) {
        self.stack.pop();
        self.after_value();
    }

    /// The next event from `text` at `*pos`; `None` if the text ends inside a token and
    /// more may come (`eof` false).
    fn step(
        &mut self,
        text: &str,
        pos: &mut usize,
        eof: bool,
    ) -> Result<Option<Raw>, JsonSyntaxError> {
        let bytes = text.as_bytes();
        loop {
            if self.expect == Expect::Done && self.single {
                return Ok(Some(Raw::Eof));
            }
            // Whitespace (the only place a line can end).
            while let Some(&b) = bytes.get(*pos) {
                match b {
                    b' ' | b'\t' | b'\r' => *pos += 1,
                    b'\n' => {
                        *pos += 1;
                        self.line += 1;
                        self.line_start = self.base + *pos as u64;
                    }
                    _ => break,
                }
            }
            let at = *pos;
            self.token_start = at;
            let Some(&b) = bytes.get(at) else {
                if !eof {
                    return Ok(None);
                }
                if self.expect == Expect::Done {
                    return Ok(Some(Raw::Eof));
                }
                return Err(self.error("the document ends early", at));
            };
            match self.expect {
                Expect::Done => {
                    return Err(self.error("there is more after the document's value", at));
                }
                Expect::Colon => {
                    if b != b':' {
                        return Err(self.error("a ':' should follow the key", at));
                    }
                    *pos += 1;
                    self.expect = Expect::Value;
                    continue;
                }
                Expect::CommaOrEnd => {
                    let object = self.stack.last() == Some(&true);
                    match b {
                        b',' => {
                            *pos += 1;
                            self.expect = if object { Expect::Key } else { Expect::Value };
                            continue;
                        }
                        b']' if !object => {
                            *pos += 1;
                            self.close();
                            return Ok(Some(Raw::EndArray));
                        }
                        b'}' if object => {
                            *pos += 1;
                            self.close();
                            return Ok(Some(Raw::EndObject));
                        }
                        _ => {
                            let what = if object { "'}'" } else { "']'" };
                            return Err(self.error(format!("a ',' or {what} should follow"), at));
                        }
                    }
                }
                Expect::ItemOrEnd if b == b']' => {
                    *pos += 1;
                    self.close();
                    return Ok(Some(Raw::EndArray));
                }
                Expect::KeyOrEnd if b == b'}' => {
                    *pos += 1;
                    self.close();
                    return Ok(Some(Raw::EndObject));
                }
                Expect::Key | Expect::KeyOrEnd => {
                    if b != b'"' {
                        return Err(self.error("an object key should be a string", at));
                    }
                    let Some((end, escaped)) = self.string(bytes, at + 1, eof)? else {
                        return Ok(None);
                    };
                    *pos = end + 1;
                    self.expect = Expect::Colon;
                    return Ok(Some(Raw::Key {
                        start: at + 1,
                        end,
                        escaped,
                    }));
                }
                Expect::Value | Expect::ItemOrEnd => {}
            }
            // A value.
            let raw = match b {
                b'{' => {
                    self.open(true, at)?;
                    *pos += 1;
                    return Ok(Some(Raw::StartObject));
                }
                b'[' => {
                    self.open(false, at)?;
                    *pos += 1;
                    return Ok(Some(Raw::StartArray));
                }
                b'"' => {
                    let Some((end, escaped)) = self.string(bytes, at + 1, eof)? else {
                        return Ok(None);
                    };
                    *pos = end + 1;
                    Raw::String {
                        start: at + 1,
                        end,
                        escaped,
                    }
                }
                b'-' | b'0'..=b'9' => {
                    let Some(end) = self.number(bytes, at, eof)? else {
                        return Ok(None);
                    };
                    *pos = end;
                    Raw::Number { start: at, end }
                }
                b't' | b'f' | b'n' => {
                    let (word, raw): (&[u8], Raw) = match b {
                        b't' => (b"true", Raw::Boolean(true)),
                        b'f' => (b"false", Raw::Boolean(false)),
                        _ => (b"null", Raw::Null),
                    };
                    let rest = &bytes[at..];
                    if rest.len() < word.len() && word.starts_with(rest) && !eof {
                        return Ok(None);
                    }
                    if !rest.starts_with(word) {
                        return Err(self.error("not a JSON value", at));
                    }
                    *pos = at + word.len();
                    raw
                }
                _ => return Err(self.error("not a JSON value", at)),
            };
            self.after_value();
            return Ok(Some(raw));
        }
    }

    /// The end of the string whose content starts at `start` (the closing quote's index),
    /// and whether it has escapes; `None` if the text ends first and more may come.
    fn string(
        &self,
        bytes: &[u8],
        start: usize,
        eof: bool,
    ) -> Result<Option<(usize, bool)>, JsonSyntaxError> {
        let mut i = start;
        let mut escaped = false;
        loop {
            let Some(j) = string_stop(bytes, i) else {
                return if eof {
                    Err(self.error("a string isn't closed", start - 1))
                } else {
                    Ok(None)
                };
            };
            match bytes[j] {
                b'"' => return Ok(Some((j, escaped))),
                b'\\' => {}
                _ => {
                    return Err(self.error("a control character in a string must be escaped", j));
                }
            }
            escaped = true;
            let Some(&e) = bytes.get(j + 1) else {
                return if eof {
                    Err(self.error("a string isn't closed", start - 1))
                } else {
                    Ok(None)
                };
            };
            i = match e {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => j + 2,
                b'u' => {
                    let Some(hex) = bytes.get(j + 2..j + 6) else {
                        return if eof {
                            Err(self.error("a \\u escape needs four hexadecimal digits", j))
                        } else {
                            Ok(None)
                        };
                    };
                    if !hex.iter().all(u8::is_ascii_hexdigit) {
                        return Err(self.error("a \\u escape needs four hexadecimal digits", j));
                    }
                    j + 6
                }
                _ => return Err(self.error("not a JSON escape sequence", j)),
            };
        }
    }

    /// The end of the number starting at `start`; `None` if the text ends where the
    /// number could go on and more may come.
    fn number(
        &self,
        bytes: &[u8],
        start: usize,
        eof: bool,
    ) -> Result<Option<usize>, JsonSyntaxError> {
        let digits = |mut i: usize| {
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            i
        };
        let bad = || Err(self.error("not a JSON number", start));
        let mut i = start;
        if bytes[i] == b'-' {
            i += 1;
        }
        match bytes.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => i = digits(i + 1),
            Some(_) => return bad(),
            None if eof => return bad(),
            None => return Ok(None),
        }
        if bytes.get(i) == Some(&b'.') {
            let end = digits(i + 1);
            if end == i + 1 {
                return if end == bytes.len() && !eof {
                    Ok(None)
                } else {
                    bad()
                };
            }
            i = end;
        }
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            let end = digits(i);
            if end == i {
                return if end == bytes.len() && !eof {
                    Ok(None)
                } else {
                    bad()
                };
            }
            i = end;
        }
        if i == bytes.len() && !eof {
            return Ok(None);
        }
        Ok(Some(i))
    }

    /// The event for `raw`, its strings borrowed from `text` unless they have escapes.
    fn event<'t>(&self, text: &'t str, raw: Raw) -> Result<JsonEvent<'t>, JsonSyntaxError> {
        Ok(match raw {
            Raw::String {
                start,
                end,
                escaped,
            } => JsonEvent::String(self.decoded(text, start, end, escaped)?),
            Raw::Key {
                start,
                end,
                escaped,
            } => JsonEvent::ObjectKey(self.decoded(text, start, end, escaped)?),
            Raw::Number { start, end } => JsonEvent::Number(Cow::Borrowed(&text[start..end])),
            Raw::Boolean(b) => JsonEvent::Boolean(b),
            Raw::Null => JsonEvent::Null,
            Raw::StartArray => JsonEvent::StartArray,
            Raw::EndArray => JsonEvent::EndArray,
            Raw::StartObject => JsonEvent::StartObject,
            Raw::EndObject => JsonEvent::EndObject,
            Raw::Eof => JsonEvent::Eof,
        })
    }

    fn decoded<'t>(
        &self,
        text: &'t str,
        start: usize,
        end: usize,
        escaped: bool,
    ) -> Result<Cow<'t, str>, JsonSyntaxError> {
        let content = &text[start..end];
        if !escaped {
            return Ok(Cow::Borrowed(content));
        }
        let mut out = String::with_capacity(content.len());
        unescape(content, &mut out).map_err(|(message, i)| self.error(message, start + i))?;
        Ok(Cow::Owned(out))
    }
}

/// The first byte at or after `from` that ends a string's plain run: `"`, `\` or a control
/// character. Eight bytes at a time in a word (most strings are short, where a vector
/// search costs more than it saves): per byte, a high bit where it equals `"` or `\` or is
/// below 0x20. Borrows can set bits above a byte that matches, never below the first one,
/// so the lowest bit is exact.
fn string_stop(bytes: &[u8], from: usize) -> Option<usize> {
    const ONES: u64 = u64::from_ne_bytes([1; 8]);
    const HIGH: u64 = ONES << 7;
    let zero_byte = |x: u64| x.wrapping_sub(ONES) & !x & HIGH;
    let mut i = from;
    while let Some(word) = bytes.get(i..i + 8) {
        let x = u64::from_le_bytes(word.try_into().expect("eight bytes"));
        let stops = zero_byte(x ^ (ONES * u64::from(b'"')))
            | zero_byte(x ^ (ONES * u64::from(b'\\')))
            | (x.wrapping_sub(ONES * 0x20) & !x & HIGH);
        if stops != 0 {
            return Some(i + (stops.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    bytes[i..]
        .iter()
        .position(|&b| b == b'"' || b == b'\\' || b < 0x20)
        .map(|k| i + k)
}

/// Decodes the escapes of a string's content (already checked to be well formed but for
/// surrogates); an error is a message and the offset in `content`.
fn unescape(content: &str, out: &mut String) -> Result<(), (&'static str, usize)> {
    let bytes = content.as_bytes();
    let mut i = 0;
    while let Some(k) = memchr::memchr(b'\\', &bytes[i..]) {
        out.push_str(&content[i..i + k]);
        let j = i + k;
        let simple = match bytes[j + 1] {
            b'"' => Some('"'),
            b'\\' => Some('\\'),
            b'/' => Some('/'),
            b'b' => Some('\u{8}'),
            b'f' => Some('\u{c}'),
            b'n' => Some('\n'),
            b'r' => Some('\r'),
            b't' => Some('\t'),
            _ => None,
        };
        if let Some(c) = simple {
            out.push(c);
            i = j + 2;
            continue;
        }
        let unit = hex4(&bytes[j + 2..j + 6]);
        i = j + 6;
        let c = match unit {
            0xD800..=0xDBFF => {
                // A high surrogate: a low one must follow.
                if bytes.get(i..i + 2) != Some(b"\\u") {
                    return Err(("a lone surrogate in a \\u escape", j));
                }
                let low = bytes
                    .get(i + 2..i + 6)
                    .filter(|h| h.iter().all(u8::is_ascii_hexdigit));
                let Some(low) = low.map(hex4).filter(|l| (0xDC00..=0xDFFF).contains(l)) else {
                    return Err(("a lone surrogate in a \\u escape", j));
                };
                i += 6;
                char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
            }
            0xDC00..=0xDFFF => return Err(("a lone surrogate in a \\u escape", j)),
            _ => char::from_u32(unit),
        };
        out.push(c.ok_or(("not a character", j))?);
    }
    out.push_str(&content[i..]);
    Ok(())
}

fn hex4(hex: &[u8]) -> u32 {
    hex.iter().fold(0, |acc, &h| {
        let digit = match h {
            b'0'..=b'9' => h - b'0',
            b'a'..=b'f' => h - b'a' + 10,
            _ => h - b'A' + 10,
        };
        acc * 16 + u32::from(digit)
    })
}

/// Where a [`SliceJsonParser`] is in its text.
#[derive(Debug, Clone)]
pub struct JsonParserState {
    pos: usize,
    lexer: Lexer,
}

/// A JSON parser over text in memory: events borrow from the text.
#[derive(Debug, Clone)]
pub struct SliceJsonParser<'a> {
    text: &'a str,
    pos: usize,
    lexer: Lexer,
}

impl<'a> SliceJsonParser<'a> {
    /// The parser of a whole document.
    pub fn new(text: &'a str) -> Self {
        let pos = if text.starts_with('\u{FEFF}') { 3 } else { 0 };
        Self {
            text,
            pos,
            lexer: Lexer::new(false),
        }
    }

    /// The parser of a document in bytes, which must be UTF-8.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, JsonSyntaxError> {
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(Self::new(text)),
            Err(error) => {
                let at = error.valid_up_to();
                let before = &bytes[..at];
                let line = memchr::memchr_iter(b'\n', before).count() as u64;
                let line_start = memchr::memrchr(b'\n', before).map_or(0, |i| i + 1);
                Err(JsonSyntaxError::new(
                    "the text isn't UTF-8",
                    line,
                    (at - line_start) as u64,
                    at as u64,
                ))
            }
        }
    }

    /// The parser of the one value starting at byte `offset` of `text` (after whitespace):
    /// it ends with that value, whatever follows. Positions stay those in `text`.
    pub fn value_at(text: &'a str, offset: usize) -> Self {
        let before = &text.as_bytes()[..offset];
        let mut lexer = Lexer::new(true);
        lexer.line = memchr::memchr_iter(b'\n', before).count() as u64;
        lexer.line_start = memchr::memrchr(b'\n', before).map_or(0, |i| i as u64 + 1);
        Self {
            text,
            pos: offset,
            lexer,
        }
    }

    /// How deep arrays and objects may nest ([`MAX_DEPTH`] by default).
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.lexer.max_depth = depth;
        self
    }

    /// Where the parser is, to go on later over the same text with [`Self::resume`]
    /// (the parser can then be dropped, and the text borrowed again).
    pub fn state(&self) -> JsonParserState {
        JsonParserState {
            pos: self.pos,
            lexer: self.lexer.clone(),
        }
    }

    /// The parser where `state` left it; `text` must be the text it was taken from.
    pub fn resume(text: &'a str, state: JsonParserState) -> Self {
        Self {
            text,
            pos: state.pos,
            lexer: state.lexer,
        }
    }

    pub fn text(&self) -> &'a str {
        self.text
    }

    /// The byte offset after the last event.
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// The byte offset where the last event's token started.
    pub fn token_start(&self) -> usize {
        self.lexer.token_start
    }

    pub fn next_event(&mut self) -> Result<JsonEvent<'a>, JsonSyntaxError> {
        let raw = self
            .lexer
            .step(self.text, &mut self.pos, true)?
            .expect("no more text to wait for");
        self.lexer.event(self.text, raw)
    }

    /// An error at the start of the last event's token.
    pub fn error(&self, message: impl Into<String>) -> JsonSyntaxError {
        self.lexer.error(message, self.lexer.token_start)
    }

    /// The next value as a tree; `None` (the bracket consumed) at the end of the array or
    /// object it would be in, or at the end of the document.
    pub fn next_value(&mut self) -> Result<Option<Value<'a>>, JsonSyntaxError> {
        let first = self.next_event()?;
        build_value(first, || self.next_event())
    }

    /// Skips the next value; its byte range, or `None` as for [`Self::next_value`].
    pub fn skip_value(&mut self) -> Result<Option<Range<usize>>, JsonSyntaxError> {
        let mut depth = 0_usize;
        let mut start = None;
        loop {
            let event = self.next_event()?;
            let start = *start.get_or_insert(self.lexer.token_start);
            match event {
                JsonEvent::StartArray | JsonEvent::StartObject => depth += 1,
                JsonEvent::EndArray | JsonEvent::EndObject | JsonEvent::Eof if depth == 0 => {
                    return Ok(None);
                }
                JsonEvent::EndArray | JsonEvent::EndObject => depth -= 1,
                JsonEvent::Eof => return Err(self.error("the document ends early")),
                _ => {}
            }
            if depth == 0 {
                return Ok(Some(start..self.pos));
            }
        }
    }
}

/// A JSON parser over a reader, buffered: events borrow from the buffer until the next
/// one.
pub struct ReaderJsonParser<R: Read> {
    reader: R,
    buffer: String,
    /// Bytes read that end inside a character.
    pending: Vec<u8>,
    pos: usize,
    eof: bool,
    started: bool,
    lexer: Lexer,
}

const READ_SIZE: usize = 64 * 1024;

impl<R: Read> ReaderJsonParser<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: String::new(),
            pending: Vec::new(),
            pos: 0,
            eof: false,
            started: false,
            lexer: Lexer::new(false),
        }
    }

    /// How deep arrays and objects may nest ([`MAX_DEPTH`] by default).
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.lexer.max_depth = depth;
        self
    }

    pub fn next_event(&mut self) -> Result<JsonEvent<'_>, JsonParseError> {
        let raw = loop {
            match self.lexer.step(&self.buffer, &mut self.pos, self.eof)? {
                Some(raw) => break raw,
                None => self.fill()?,
            }
        };
        Ok(self.lexer.event(&self.buffer, raw)?)
    }

    /// The next value as a tree (owned); `None` as for [`SliceJsonParser::next_value`].
    pub fn next_value(&mut self) -> Result<Option<Value<'static>>, JsonParseError> {
        let first = self.next_event()?.into_owned();
        build_value(first, || {
            Ok::<_, JsonParseError>(self.next_event()?.into_owned())
        })
    }

    /// Drops what has been read and appends what the reader gives.
    fn fill(&mut self) -> Result<(), JsonParseError> {
        if self.pos > 0 {
            self.buffer.drain(..self.pos);
            self.lexer.base += self.pos as u64;
            self.pos = 0;
        }
        let have = self.pending.len();
        self.pending.resize(have + READ_SIZE, 0);
        let read = loop {
            match self.reader.read(&mut self.pending[have..]) {
                Ok(read) => break read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.pending.truncate(have);
                    return Err(error.into());
                }
            }
        };
        self.pending.truncate(have + read);
        if read == 0 {
            self.eof = true;
            if !self.pending.is_empty() {
                let at = self.buffer.len();
                return Err(self.lexer.error("the text isn't UTF-8", at).into());
            }
            return Ok(());
        }
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => {
                let at = self.buffer.len() + error.valid_up_to();
                return Err(self.lexer.error("the text isn't UTF-8", at).into());
            }
        };
        let text = std::str::from_utf8(&self.pending[..valid]).unwrap_or_default();
        let text = if self.started {
            text
        } else if !text.is_empty() {
            self.started = true;
            text.strip_prefix('\u{FEFF}').unwrap_or(text)
        } else {
            text
        };
        self.buffer.push_str(text);
        self.pending.drain(..valid);
        Ok(())
    }
}

impl JsonEvent<'_> {
    pub fn into_owned(self) -> JsonEvent<'static> {
        match self {
            Self::String(s) => JsonEvent::String(Cow::Owned(s.into_owned())),
            Self::Number(n) => JsonEvent::Number(Cow::Owned(n.into_owned())),
            Self::ObjectKey(k) => JsonEvent::ObjectKey(Cow::Owned(k.into_owned())),
            Self::Boolean(b) => JsonEvent::Boolean(b),
            Self::Null => JsonEvent::Null,
            Self::StartArray => JsonEvent::StartArray,
            Self::EndArray => JsonEvent::EndArray,
            Self::StartObject => JsonEvent::StartObject,
            Self::EndObject => JsonEvent::EndObject,
            Self::Eof => JsonEvent::Eof,
        }
    }
}

/// The value starting with `first`, the rest from `next`; `None` if `first` closes a
/// container or ends the document. Iterative: depth costs no stack.
fn build_value<'a, E>(
    first: JsonEvent<'a>,
    mut next: impl FnMut() -> Result<JsonEvent<'a>, E>,
) -> Result<Option<Value<'a>>, E> {
    enum Open<'a> {
        Array(Vec<Value<'a>>),
        Object(Object<'a>, Cow<'a, str>),
    }
    let mut stack: Vec<Open<'a>> = Vec::new();
    let mut event = first;
    loop {
        let value = match event {
            JsonEvent::EndArray | JsonEvent::EndObject | JsonEvent::Eof if stack.is_empty() => {
                return Ok(None);
            }
            JsonEvent::String(s) => Value::String(s),
            JsonEvent::Number(n) => Value::Number(n),
            JsonEvent::Boolean(b) => Value::Boolean(b),
            JsonEvent::Null => Value::Null,
            JsonEvent::StartArray => {
                stack.push(Open::Array(Vec::new()));
                event = next()?;
                continue;
            }
            JsonEvent::StartObject => {
                stack.push(Open::Object(Object::new(), Cow::Borrowed("")));
                event = next()?;
                continue;
            }
            JsonEvent::ObjectKey(key) => {
                if let Some(Open::Object(_, pending)) = stack.last_mut() {
                    *pending = key;
                }
                event = next()?;
                continue;
            }
            JsonEvent::EndArray => match stack.pop() {
                Some(Open::Array(items)) => Value::Array(items),
                _ => unreachable!("the lexer balances brackets"),
            },
            JsonEvent::EndObject => match stack.pop() {
                Some(Open::Object(mut object, _)) => {
                    object.keep_last_duplicates();
                    Value::Object(object)
                }
                _ => unreachable!("the lexer balances brackets"),
            },
            JsonEvent::Eof => unreachable!("the lexer reports an early end as an error"),
        };
        match stack.last_mut() {
            None => return Ok(Some(value)),
            Some(Open::Array(items)) => items.push(value),
            Some(Open::Object(object, key)) => {
                object.push(std::mem::take(key), value);
            }
        }
        event = next()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(text: &str) -> Result<Vec<JsonEvent<'_>>, JsonSyntaxError> {
        let mut parser = SliceJsonParser::new(text);
        let mut out = Vec::new();
        loop {
            let event = parser.next_event()?;
            if event == JsonEvent::Eof {
                return Ok(out);
            }
            out.push(event);
        }
    }

    /// The reader front end, fed one byte per read, gives the same events.
    fn trickled(text: &str) -> Result<Vec<JsonEvent<'static>>, String> {
        struct OneByte<'b>(&'b [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let Some((&first, rest)) = self.0.split_first() else {
                    return Ok(0);
                };
                buf[0] = first;
                self.0 = rest;
                Ok(1)
            }
        }
        let mut parser = ReaderJsonParser::new(OneByte(text.as_bytes()));
        let mut out = Vec::new();
        loop {
            let event = parser.next_event().map_err(|e| e.to_string())?.into_owned();
            if event == JsonEvent::Eof {
                return Ok(out);
            }
            out.push(event);
        }
    }

    #[test]
    fn valid_documents() {
        let text =
            " {\"a\": [1, -2.5e+3, true, false, null, \"x\\n\\u00e9\\ud83d\\ude00\"], \"\": {}} ";
        let expected = vec![
            JsonEvent::StartObject,
            JsonEvent::ObjectKey("a".into()),
            JsonEvent::StartArray,
            JsonEvent::Number("1".into()),
            JsonEvent::Number("-2.5e+3".into()),
            JsonEvent::Boolean(true),
            JsonEvent::Boolean(false),
            JsonEvent::Null,
            JsonEvent::String("x\né😀".into()),
            JsonEvent::EndArray,
            JsonEvent::ObjectKey("".into()),
            JsonEvent::StartObject,
            JsonEvent::EndObject,
            JsonEvent::EndObject,
        ];
        assert_eq!(events(text).unwrap(), expected);
        assert_eq!(trickled(text).unwrap(), expected);
        for scalar in ["0", "-0", "1e5", "\"\"", "null", "[]", "\u{FEFF}[1]"] {
            assert!(events(scalar).is_ok(), "{scalar}");
            assert!(trickled(scalar).is_ok(), "{scalar}");
        }
    }

    #[test]
    fn invalid_documents() {
        for text in [
            "",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "1.",
            ".5",
            "1e",
            "-",
            "+1",
            "[1 2]",
            "{\"a\" 1}",
            "{a:1}",
            "\"\\x\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"a\nb\"",
            "tru",
            "nul",
            "[",
            "{",
            "\"abc",
            "1 2",
            "[]]",
            "NaN",
            "'a'",
            "/* */ 1",
            "\"\\u12\"",
        ] {
            assert!(events(text).is_err(), "{text:?}");
            assert!(trickled(text).is_err(), "{text:?}");
        }
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(events(&deep).is_err());
        let mut parser = SliceJsonParser::new(&deep).with_max_depth(300);
        while parser.next_event().unwrap() != JsonEvent::Eof {}
    }

    #[test]
    fn errors_say_where() {
        let error = events("[1,\n  2,\n  x]").unwrap_err();
        assert_eq!((error.line(), error.column(), error.offset()), (2, 2, 11));
        let error = SliceJsonParser::from_bytes(b"[\"\xff\"]").unwrap_err();
        assert_eq!(error.offset(), 2);
    }

    #[test]
    fn values_one_at_a_time() {
        let text = "[{\"a\": [1, {\"b\": null}]}, \"s\", 3] ";
        let mut parser = SliceJsonParser::new(text);
        assert_eq!(parser.next_event().unwrap(), JsonEvent::StartArray);
        let first = parser.next_value().unwrap().unwrap();
        assert_eq!(first.to_string(), "{\"a\":[1,{\"b\":null}]}");
        let range = parser.skip_value().unwrap().unwrap();
        assert_eq!(&text[range], "\"s\"");
        assert_eq!(
            parser.next_value().unwrap(),
            Some(Value::Number("3".into()))
        );
        assert_eq!(parser.next_value().unwrap(), None);
        assert_eq!(parser.next_event().unwrap(), JsonEvent::Eof);
        // One value in the middle of a text.
        let mut parser = SliceJsonParser::value_at(text, 1);
        assert!(parser.next_value().unwrap().is_some());
        assert_eq!(parser.next_event().unwrap(), JsonEvent::Eof);
    }

    #[test]
    fn the_word_search_finds_what_a_byte_search_finds() {
        // Every stop byte at every offset and alignment, behind every kind of byte (the
        // borrows of the word arithmetic come from below the first match).
        let fillers = [b'a', 0x7f, 0x80, 0xff, 0x20, 0x21, 0x5b, 0x5d];
        for stop in [b'"', b'\\', 0x00, 0x1f, 0x0a] {
            for filler in fillers {
                for len in 0..20 {
                    for at in 0..=len {
                        let mut bytes = vec![filler; len];
                        if at < len {
                            bytes[at] = stop;
                            if at + 1 < len {
                                bytes[at + 1] = 0x01;
                            }
                        }
                        for from in 0..=len.min(9) {
                            let expected = bytes[from..]
                                .iter()
                                .position(|&b| b == b'"' || b == b'\\' || b < 0x20)
                                .map(|k| from + k);
                            assert_eq!(
                                super::string_stop(&bytes, from),
                                expected,
                                "{bytes:?} from {from}"
                            );
                        }
                    }
                }
            }
        }
    }
}
