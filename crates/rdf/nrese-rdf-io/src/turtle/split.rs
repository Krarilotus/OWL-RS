//! Exact statement boundaries in Turtle and TriG, for parsing in parallel.
//!
//! A [`Skimmer`] reads the document once, sequentially, but only far enough to know what
//! is a string (four quote forms, with escapes), an IRI or a comment, and what is open:
//! - brackets (`[ ]`, `( )`, `<<( )>>`, `<< >>`, `{| |}`), read as the lexer reads them
//!   (`lexer::delimiter`) and held as recovery holds them (`lexer::Brackets`): a closer
//!   closes an open bracket of its own kind, or nothing;
//! - TriG blocks (a `{` after a graph block's head, as the lexer reads it), and any other
//!   `{` as a brace, as recovery reads one;
//! - a name holding `\` is read by the lexer itself, token by token.
//!
//! It costs a table lookup per byte outside those, and `memchr` inside them. A `.` with
//! nothing open followed by whitespace (or the end, or a comment), and a `}` that closes a
//! TriG block, end a statement for certain. Every other `.` is left alone: a boundary missed
//! costs nothing, a wrong one corrupts the parse. So where it is unsure the skimmer holds
//! more open than a recovering parser would, never less, and every cut it makes is where the
//! sequential recovering parse starts a statement. The skim also records every `@prefix`,
//! `@base`, `PREFIX` and `BASE` directive, and RDF 1.2's `@version` and `VERSION`, so each
//! chunk starts with exactly the prefixes, base and version in force where it begins,
//! wherever the directives are in the document.
//!
//! Oxigraph's Turtle splitter guesses instead (a `.` after which three triples parse) and
//! says it can fail or give wrong results on directives after the start or Turtle inside
//! literals; this one can't.

use super::lexer::{Brackets, Delimiter, Kind, Lexed, characters, delimiter, is_space, lex};

/// Bytes the skimmer must look at outside strings, IRIs and comments.
const fn interesting() -> [bool; 256] {
    let mut table = [false; 256];
    let bytes = b"\"'<>|#.[]{}()\\";
    let mut i = 0;
    while i < bytes.len() {
        table[bytes[i] as usize] = true;
        i += 1;
    }
    table
}
const INTERESTING: [bool; 256] = interesting();

/// The bytes that always end a token before them (the lexer's whitespace, delimiters,
/// quotes, `<`, `#`): a name holding `\` starts after the last one.
fn ends_token(b: u8) -> bool {
    is_space(b)
        || matches!(
            b,
            b'"' | b'\'' | b'<' | b'>' | b'|' | b'#' | b'[' | b']' | b'{' | b'}' | b'(' | b')'
        )
}

/// Whether a SPARQL-style directive's text so far is a `VERSION` (ending with its short
/// string; `PREFIX` and `BASE` end with their IRI).
fn version(text: &[u8]) -> bool {
    text.len() >= 7 && text[..7].eq_ignore_ascii_case(b"version")
}

/// Whether the directive text's end, from its opening quote (just before `from`), is a short
/// string the lexer reads as one token, by its own rules (escapes, line breaks).
fn short_string(text: &[u8], from: usize) -> bool {
    let Some(start) = from.checked_sub(1) else {
        return false;
    };
    let string = &text[start..];
    matches!(
        lex(string, true, false, false),
        Lexed::Token(token)
            if matches!(token.kind, Kind::String { .. })
                && token.start == 0
                && token.consumed == string.len()
    )
}

/// What the tokens of a statement before a `{` make of it (TriG): a graph block's head
/// (nothing, a graph name, `GRAPH` and a graph name), the start of one, or neither.
#[derive(PartialEq)]
enum Head {
    Block,
    Start,
    Not,
}

/// [`Head`] of `text`, read by the lexer; `eof`: `text` ends there (at the `{`).
fn head(text: &[u8], eof: bool) -> Head {
    let mut kinds = [Kind::Eof; 3];
    let mut n = 0;
    let mut at = 0;
    loop {
        match lex(&text[at..], eof, false, false) {
            Lexed::Token(token) if token.kind == Kind::Eof => break,
            Lexed::Token(token) => {
                if n == kinds.len() {
                    return Head::Not;
                }
                kinds[n] = token.kind;
                n += 1;
                at += token.consumed;
            }
            Lexed::NeedMore => break,
            Lexed::Error(..) => return Head::Not,
        }
    }
    let name = |kind: &Kind| {
        matches!(
            kind,
            Kind::IriRef { .. } | Kind::PrefixedName { .. } | Kind::BlankLabel
        )
    };
    match &kinds[..n] {
        [] | [Kind::OpenBracket, Kind::CloseBracket] => Head::Block,
        [Kind::Graph, Kind::OpenBracket, Kind::CloseBracket] => Head::Block,
        [kind] | [Kind::Graph, kind] if name(kind) => Head::Block,
        [Kind::Graph] | [Kind::OpenBracket] | [Kind::Graph, Kind::OpenBracket] => Head::Start,
        _ => Head::Not,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    /// After a `.` with nothing open: a boundary if whitespace, a comment or the end follows.
    Dot,
    Comment,
    Iri,
    /// In a short string with this quote; `escaped`: the previous byte was `\`.
    Short {
        quote: u8,
        escaped: bool,
    },
    /// In a long string with this quote: `quotes` closing quotes seen in a row.
    Long {
        quote: u8,
        quotes: u8,
        escaped: bool,
    },
    /// After one or two opening quotes: a long string if a third follows.
    Opening {
        quote: u8,
        count: u8,
    },
}

/// A cut: where a chunk starts, as its parser counts positions there (`for_chunk`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cut {
    /// Its byte offset in the document.
    pub(crate) offset: u64,
    /// The line it is in (from 0), and the offset of that line's start.
    pub(crate) line: u64,
    pub(crate) line_start: u64,
    /// The characters on that line before it: a column counts characters (`TextPosition`).
    pub(crate) column: u64,
}

impl Cut {
    /// The document's start: the first chunk's.
    pub(crate) const START: Cut = Cut {
        offset: 0,
        line: 0,
        line_start: 0,
        column: 0,
    };
}

/// The skim of one document: where to cut, and the directives.
#[derive(Debug, Default)]
pub(crate) struct Skim {
    /// The chosen cuts.
    pub(crate) cuts: Vec<Cut>,
    /// Each directive: where it ends (offset of the byte after it) and its text.
    pub(crate) directives: Vec<(u64, String)>,
}

/// Reads a document in pieces and finds cuts near the targets.
pub(crate) struct Skimmer {
    state: State,
    /// TriG: `{` may open a graph block.
    trig: bool,
    /// What is open, as a recovering parser holds it: brackets (`lexer::Brackets`); `{`
    /// opened inside a statement; a TriG block.
    open: Brackets,
    braces: u64,
    in_block: bool,
    /// Where the current statement starts; while its tokens so far may be a TriG block's
    /// head (at most a graph name after `GRAPH`), its bytes before the current input.
    statement_start: u64,
    head: Vec<u8>,
    /// No `{` can open a block in this statement any more.
    no_head: bool,
    /// The bytes of the last input not read yet: a token whose reading needs what comes
    /// after them (a word at the input's end, a delimiter cut by it). They end at the next
    /// byte that ends a token (`ends_token`).
    pending: Vec<u8>,
    /// Whether a byte order mark at the document's start has been looked for (the parser
    /// reads past one there).
    started: bool,
    /// In a SPARQL-style directive's short string: where in the directive's text it starts
    /// (a `VERSION` ends with it only if it holds no line break: one that does is a token the
    /// lexer can't read, which recovery skips).
    string_from: usize,
    /// The document offset of the next byte read.
    offset: u64,
    line: u64,
    line_start: u64,
    /// The characters from `line_start` to `offset` (a cut later in the line, in the next
    /// input, counts its column from them).
    line_chars: u64,
    /// Where cuts are wanted, ascending; the first one not yet reached.
    targets: Vec<u64>,
    next_target: usize,
    skim: Skim,
    /// At the start of a statement, its first token not read yet (it tells a directive).
    start: bool,
    /// The directive being read: its text so far, and whether it ends at a '.' (`@` forms)
    /// or after its IRI or, for `VERSION`, its string (SPARQL forms).
    directive: Option<(Vec<u8>, bool)>,
}

impl Skimmer {
    /// A skimmer for a Turtle (or, with `trig`, TriG) document of `length` bytes cut into up
    /// to `parts` chunks.
    pub(crate) fn new(length: u64, parts: usize, trig: bool) -> Self {
        let parts = parts.max(1) as u64;
        Self {
            state: State::Normal,
            trig,
            open: Brackets::default(),
            braces: 0,
            in_block: false,
            statement_start: 0,
            head: Vec::new(),
            no_head: false,
            pending: Vec::new(),
            started: false,
            string_from: 0,
            offset: 0,
            line: 0,
            line_start: 0,
            line_chars: 0,
            targets: (1..parts).map(|k| length * k / parts).collect(),
            next_target: 0,
            skim: Skim::default(),
            start: true,
            directive: None,
        }
    }

    /// A statement ended just before `at` (the offset of the next byte): a cut there if a
    /// target has been reached, and a new statement starts.
    fn boundary(&mut self, at: u64) {
        if let Some((text, _)) = self.directive.take() {
            self.skim
                .directives
                .push((at, String::from_utf8_lossy(&text).into_owned()));
        }
        if self.next_target < self.targets.len() && at >= self.targets[self.next_target] {
            // The column is counted where the piece is read to its end (`scan`, `finish`).
            self.skim.cuts.push(Cut {
                offset: at,
                line: self.line,
                line_start: self.line_start,
                column: 0,
            });
            while self.next_target < self.targets.len() && self.targets[self.next_target] <= at {
                self.next_target += 1;
            }
        }
        self.start = true;
        self.statement_start = at;
        self.head.clear();
        self.no_head = false;
    }

    /// Nothing open: a `.` here ends the statement.
    #[inline(always)]
    fn level(&self) -> bool {
        self.open.is_empty() && self.braces == 0 && !self.in_block
    }

    /// A statement ends whatever is open in it (a SPARQL-style directive's end).
    fn close_all(&mut self) {
        self.open.clear();
        self.braces = 0;
    }

    /// A delimiter token at `bytes[at..]` applied to what is open, as a recovering parser
    /// applies it (`skip_token`); `true` when it ends a TriG block (and the statement in it).
    #[inline(always)]
    fn delimit(&mut self, kind: Kind, bytes: &[u8], at: usize) -> bool {
        match kind {
            Kind::OpenBrace | Kind::CloseBrace => self.brace(kind, bytes, at),
            _ => {
                self.open.apply(kind);
                false
            }
        }
    }

    /// `delimit` for a brace: a TriG block's or a statement's (once per block or less: out of
    /// the skim's loop).
    #[inline(never)]
    fn brace(&mut self, kind: Kind, bytes: &[u8], at: usize) -> bool {
        match kind {
            Kind::OpenBrace => {
                if self.trig
                    && !self.in_block
                    && self.open.is_empty()
                    && self.braces == 0
                    && !self.no_head
                    && self.block_head(bytes, at)
                {
                    self.in_block = true;
                } else {
                    self.braces += 1;
                }
                self.no_head = true;
                false
            }
            Kind::CloseBrace if self.braces > 0 => {
                self.braces -= 1;
                false
            }
            Kind::CloseBrace if self.in_block => {
                self.in_block = false;
                self.open.clear();
                true
            }
            _ => false,
        }
    }

    /// The lines of `bytes[*counted..upto]` (`bytes` the input from document offset `base`),
    /// counted with memchr; `*counted` moves to `upto`.
    fn lines(&mut self, bytes: &[u8], base: u64, counted: &mut usize, upto: usize) {
        if upto > *counted {
            let piece = &bytes[*counted..upto];
            let breaks = memchr::memchr_iter(b'\n', piece).count() as u64;
            if breaks > 0 {
                let last = memchr::memrchr(b'\n', piece).unwrap_or(0);
                self.line += breaks;
                self.line_start = base + (*counted + last + 1) as u64;
            }
            *counted = upto;
        }
    }

    /// A `\` at `bytes[i]` outside strings, IRIs and comments: the name it is in, read by the
    /// lexer from the token's start (after the last byte that ends a token), token by token,
    /// a byte it can't read skipped as a recovering parser skips it; the delimiters and `.`
    /// among them applied. Where reading goes on, and whether it stopped for want of more
    /// input (to hold from there). Rare: out of the skim's loop.
    #[cold]
    #[inline(never)]
    fn escaped_name(
        &mut self,
        bytes: &[u8],
        i: usize,
        eof: bool,
        base: u64,
        counted: &mut usize,
    ) -> (usize, bool) {
        let n = bytes.len();
        self.start = false;
        let mut from = i;
        while from > 0 && !ends_token(bytes[from - 1]) {
            from -= 1;
        }
        let mut at = from;
        let mut more = false;
        while at < n && !ends_token(bytes[at]) {
            match lex(&bytes[at..], eof, false, false) {
                Lexed::Token(token) => {
                    let end = at + token.consumed;
                    match token.kind {
                        // A `.` with nothing open ends the statement if whitespace or a
                        // comment follows (`State::Dot`).
                        Kind::Dot if self.level() => {
                            if end == n {
                                self.state = State::Dot;
                            } else if is_space(bytes[end]) || bytes[end] == b'#' {
                                self.lines(bytes, base, counted, end);
                                self.boundary(base + end as u64);
                            }
                        }
                        opener!() | closer!() | Kind::OpenBrace | Kind::CloseBrace => {
                            let ended = self.delimit(token.kind, bytes, at);
                            if ended {
                                self.lines(bytes, base, counted, end);
                                self.boundary(base + end as u64);
                            }
                        }
                        _ => {}
                    }
                    at = end;
                }
                Lexed::NeedMore => {
                    more = true;
                    break;
                }
                // A byte the lexer can't read: skipped, as recovery does.
                Lexed::Error(..) => at += 1,
            }
        }
        if let Some((text, _)) = &mut self.directive {
            if at > i + 1 {
                text.extend_from_slice(&bytes[i + 1..at]);
            } else {
                // Held before the `\`: its bytes are read again.
                text.truncate(text.len() - (i + 1 - at));
            }
        }
        (at, more)
    }

    /// Whether the statement's tokens before the `{` at `bytes[at]` are a graph block's head.
    /// Once per TriG block: out of the skim's loop.
    #[cold]
    #[inline(never)]
    fn block_head(&self, bytes: &[u8], at: usize) -> bool {
        let joined;
        let text = match self.statement_start.checked_sub(self.offset) {
            Some(from) => &bytes[from as usize..at],
            None => {
                joined = [&self.head[..], &bytes[..at]].concat();
                &joined[..]
            }
        };
        head(text, true) == Head::Block
    }

    /// Feeds the next bytes of the document.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        let mut bytes = bytes;
        while !self.pending.is_empty() {
            // What was held back goes on to the next byte that ends a token: until it comes,
            // the input only grows it (each byte is looked at once). With that byte and two
            // more (a delimiter's own length), it can be read.
            let Some(k) = bytes.iter().position(|&c| ends_token(c)) else {
                self.pending.extend_from_slice(bytes);
                return;
            };
            let k = (k + 3).min(bytes.len());
            let mut joined = std::mem::take(&mut self.pending);
            joined.extend_from_slice(&bytes[..k]);
            let read = self.scan(&joined, false);
            self.pending = joined.split_off(read);
            bytes = &bytes[k..];
            if bytes.is_empty() {
                return;
            }
        }
        let read = self.scan(bytes, false);
        self.pending.extend_from_slice(&bytes[read..]);
    }

    /// Reads `bytes`, the document's next bytes from `self.offset`: how many it read (the
    /// rest is held back, see `pending`). `eof`: nothing comes after them.
    fn scan(&mut self, bytes: &[u8], eof: bool) -> usize {
        let base = self.offset;
        // The cuts made in this input: their columns are counted at its end.
        let first_cut = self.skim.cuts.len();
        // Lines: counted per piece with memchr (vectorised), and at each boundary from the
        // piece's start up to it.
        let mut counted = 0_usize;
        let mut i = 0;
        let n = bytes.len();
        let count_lines = |me: &mut Self, counted: &mut usize, upto: usize| {
            me.lines(bytes, base, counted, upto);
        };
        // The document's start: a byte order mark is no text, as for the parser.
        if !self.started && base == 0 {
            const BOM: &[u8] = "\u{FEFF}".as_bytes();
            if n < BOM.len() && !eof && BOM.starts_with(bytes) {
                return 0;
            }
            self.started = true;
            if bytes.starts_with(BOM) {
                i = BOM.len();
                self.statement_start = i as u64;
                self.start = true;
            }
        }
        // A word at the input's end is held back (it may go on, its start is needed to read
        // a `\` in it): it starts after the last byte that ends a token.
        let tail = if eof {
            n
        } else {
            bytes
                .iter()
                .rposition(|&c| ends_token(c))
                .map_or(0, |k| k + 1)
        };
        // Where reading stopped for want of more input.
        let mut held = None;
        // A delimiter at `bytes[i]` (a byte `lexer::delimiter` reads), applied to what is
        // open; `break`s the loop (`$done`) to hold it when it is cut by the input's end.
        macro_rules! delimiter_arm {
            ($me:ident, $done:lifetime) => {
                match delimiter(bytes, i, eof, false) {
                    Some(Delimiter::Token(kind, len)) => {
                        if let Some((text, _)) = &mut $me.directive {
                            text.extend_from_slice(&bytes[i + 1..i + len]);
                        }
                        let ended = $me.delimit(kind, bytes, i);
                        i += len;
                        if ended {
                            count_lines($me, &mut counted, i);
                            $me.boundary(base + i as u64);
                        }
                    }
                    // A `>` or `|` that starts no delimiter: a byte the lexer can't read,
                    // skipped.
                    Some(Delimiter::Bad(_)) => i += 1,
                    Some(Delimiter::NeedMore) => {
                        if let Some((text, _)) = &mut $me.directive {
                            text.pop();
                        }
                        held = Some(i);
                        break $done;
                    }
                    // A `<` that starts an IRI.
                    None => {
                        $me.state = State::Iri;
                        i += 1;
                    }
                }
            };
        }
        // The loop reads a view of the input, and asks nothing per byte about holding: first
        // the bytes before the held word (`tail`), then, only if a token begun before the word
        // goes on into it, all of them (see below). The view starts where the input does, so
        // its offsets are the input's (`delimiter_arm` reads the whole input).
        let whole = bytes;
        let mut view = &whole[..tail];
        'scan: loop {
            let bytes = view;
            let n = bytes.len();
            while i < n {
                debug_assert!(
                    i < tail || self.state != State::Normal,
                    "reading the held word"
                );
                // A statement's first token: is it a directive? Past the lexer's whitespace and
                // comments, the lexer reads it, where a directive's keyword can start: `@`
                // (`@prefix`, `@base`, `@version`) or the first letter of `PREFIX`, `BASE`,
                // `VERSION` in any case. (What is open is asked only while a statement's start
                // is being read: this runs for every byte.) A `VERSION "1.2"` read as a
                // statement ran on to the next `.` and swallowed the directive after it (a
                // release whose second line declared `prov:` failed to load in parallel, 4
                // October 2026).
                if self.start && matches!(self.state, State::Normal) && self.level() {
                    let b = bytes[i];
                    if is_space(b) {
                        i += 1;
                        continue;
                    }
                    // A comment before the statement is read below, and the start is kept.
                    if b != b'#' {
                        self.start = false;
                        if matches!(b, b'@' | b'p' | b'P' | b'b' | b'B' | b'v' | b'V') {
                            match lex(&bytes[i..], eof, false, false) {
                                Lexed::Token(token) => match token.kind {
                                    Kind::AtPrefix | Kind::AtBase | Kind::AtVersion => {
                                        self.directive = Some((Vec::new(), true));
                                    }
                                    Kind::Prefix | Kind::Base | Kind::Version => {
                                        self.directive = Some((Vec::new(), false));
                                    }
                                    _ => {}
                                },
                                // The token goes on in the next input: read it again then.
                                Lexed::NeedMore => {
                                    self.start = true;
                                    held = Some(i);
                                    break 'scan;
                                }
                                // A byte the lexer can't read: the statement fails there.
                                Lexed::Error(..) => {}
                            }
                        }
                    }
                }
                let b = bytes[i];
                if let Some((text, _)) = &mut self.directive {
                    text.push(b);
                }
                match self.state {
                    State::Normal => {
                        if !INTERESTING[b as usize] {
                            // The fast path: skip to the next byte that matters, up to the held
                            // word (a directive's text is copied byte by byte above, but
                            // directives are few).
                            if self.directive.is_none() && !self.start {
                                let rest = &bytes[i + 1..];
                                let skip = rest
                                    .iter()
                                    .position(|&c| INTERESTING[c as usize])
                                    .unwrap_or(rest.len());
                                i += 1 + skip;
                            } else {
                                i += 1;
                            }
                            continue;
                        }
                        match b {
                            b'"' | b'\'' => {
                                self.state = State::Opening { quote: b, count: 1 };
                                i += 1;
                            }
                            b'#' => {
                                self.state = State::Comment;
                                i += 1;
                            }
                            b'.' => {
                                if self.level() {
                                    self.state = State::Dot;
                                }
                                i += 1;
                            }
                            b'\\' => {
                                let (at, more) =
                                    self.escaped_name(bytes, i, eof, base, &mut counted);
                                if more {
                                    held = Some(at);
                                    break 'scan;
                                }
                                i = at;
                            }
                            // The delimiters, read by the lexer's `delimiter`.
                            _ => delimiter_arm!(self, 'scan),
                        }
                    }
                    State::Dot => {
                        self.state = State::Normal;
                        // The byte is read again in the normal state (a '#' starts a comment):
                        // take it back from a directive's text.
                        if let Some((text, _)) = &mut self.directive {
                            text.pop();
                        }
                        if is_space(b) || b == b'#' {
                            count_lines(self, &mut counted, i);
                            self.boundary(base + i as u64);
                        }
                    }
                    State::Comment => match memchr::memchr2(b'\n', b'\r', &bytes[i..]) {
                        Some(k) => {
                            if let Some((text, _)) = &mut self.directive {
                                text.extend_from_slice(&bytes[i + 1..i + k + 1]);
                            }
                            self.state = State::Normal;
                            i += k + 1;
                        }
                        None => {
                            if let Some((text, _)) = &mut self.directive {
                                text.extend_from_slice(&bytes[i + 1..]);
                            }
                            i = n;
                        }
                    },
                    State::Iri => match memchr::memchr(b'>', &bytes[i..]) {
                        Some(k) => {
                            if let Some((text, _)) = &mut self.directive {
                                text.extend_from_slice(&bytes[i + 1..i + k + 1]);
                            }
                            self.state = State::Normal;
                            i += k + 1;
                            // A SPARQL-style `PREFIX` or `BASE` ends with its IRI, whatever is open
                            // (a recovering parser ends a failed one there too).
                            if matches!(&self.directive, Some((text, false)) if !version(text)) {
                                let at = base + i as u64;
                                count_lines(self, &mut counted, i);
                                self.close_all();
                                self.boundary(at);
                            }
                        }
                        None => {
                            if let Some((text, _)) = &mut self.directive {
                                text.extend_from_slice(&bytes[i + 1..]);
                            }
                            i = n;
                        }
                    },
                    State::Opening { quote, count } => {
                        if b == quote && count == 1 {
                            self.state = State::Opening { quote, count: 2 };
                            i += 1;
                        } else if b == quote && count == 2 {
                            self.state = State::Long {
                                quote,
                                quotes: 0,
                                escaped: false,
                            };
                            i += 1;
                        } else {
                            // The byte is read again in the next state: take it back from a
                            // directive's text (a `VERSION` string held it twice).
                            if let Some((text, _)) = &mut self.directive {
                                text.pop();
                                self.string_from = text.len();
                            }
                            self.state = if count == 2 {
                                // `""`: the empty string; this byte is after it.
                                State::Normal
                            } else {
                                State::Short {
                                    quote,
                                    escaped: false,
                                }
                            };
                        }
                    }
                    State::Short { quote, escaped } => {
                        if escaped {
                            self.state = State::Short {
                                quote,
                                escaped: false,
                            };
                            i += 1;
                            continue;
                        }
                        match memchr::memchr2(quote, b'\\', &bytes[i..]) {
                            Some(k) => {
                                if let Some((text, _)) = &mut self.directive {
                                    text.extend_from_slice(&bytes[i + 1..i + k + 1]);
                                }
                                let closed = bytes[i + k] == quote;
                                self.state = if closed {
                                    State::Normal
                                } else {
                                    State::Short {
                                        quote,
                                        escaped: true,
                                    }
                                };
                                i += k + 1;
                                // A SPARQL-style `VERSION` ends with its string (only short
                                // strings may follow it), whatever is open; only with one the lexer
                                // reads as a string token (one it can't read, a raw line break in
                                // it, recovery skips, and the directive goes on to a `.`).
                                let string_from = self.string_from;
                                if closed
                                    && matches!(&self.directive, Some((text, false))
                                    if version(text) && short_string(text, string_from))
                                {
                                    let at = base + i as u64;
                                    count_lines(self, &mut counted, i);
                                    self.close_all();
                                    self.boundary(at);
                                }
                            }
                            None => {
                                // The string goes on in the next piece: keep this piece's part.
                                if let Some((text, _)) = &mut self.directive {
                                    text.extend_from_slice(&bytes[i + 1..]);
                                }
                                i = n;
                            }
                        }
                    }
                    State::Long {
                        quote,
                        quotes,
                        escaped,
                    } => {
                        if escaped {
                            self.state = State::Long {
                                quote,
                                quotes: 0,
                                escaped: false,
                            };
                            i += 1;
                        } else if b == b'\\' {
                            self.state = State::Long {
                                quote,
                                quotes: 0,
                                escaped: true,
                            };
                            i += 1;
                        } else if b == quote {
                            if quotes == 2 {
                                self.state = State::Normal;
                            } else {
                                self.state = State::Long {
                                    quote,
                                    quotes: quotes + 1,
                                    escaped: false,
                                };
                            }
                            i += 1;
                        } else {
                            self.state = State::Long {
                                quote,
                                quotes: 0,
                                escaped: false,
                            };
                            // Skip to the next quote or escape.
                            match memchr::memchr2(quote, b'\\', &bytes[i..]) {
                                Some(k) => i += k,
                                None => i = n,
                            }
                        }
                    }
                }
            }
            if n == whole.len() {
                break;
            }
            // At `tail`: the held word, whose bytes end no token (as `tail` is after the last
            // one that does).
            match self.state {
                State::Normal => {
                    held = Some(i);
                    break;
                }
                // A `.` or an empty string `""` just before it: the word's first byte ends
                // nothing there, and the word is held.
                State::Dot | State::Opening { count: 2, .. } => {
                    self.state = State::Normal;
                    held = Some(i);
                    break;
                }
                // A token begun before it (a string, an IRI, a comment, or a quote not known to
                // open an empty string yet) reads on: only a byte that ends a token (a quote, a
                // `>`, a line break) ends one of them, so none ends in the word, and the normal
                // state isn't reached again before the input's end.
                _ => view = whole,
            }
        }
        let read = held.unwrap_or(n);
        count_lines(self, &mut counted, read);
        // Columns in characters: each cut's from its line's start (in this input, or before
        // it: `line_chars`), then `line_chars` for the next input. Only the bytes of a cut's
        // line before it are counted, and of the last line read.
        let since =
            |line_start: u64, to: usize, chars_before: u64| match line_start.checked_sub(base) {
                Some(from) => characters(&bytes[from as usize..to]),
                None => chars_before + characters(&bytes[..to]),
            };
        for k in first_cut..self.skim.cuts.len() {
            let cut = self.skim.cuts[k];
            self.skim.cuts[k].column = since(
                cut.line_start,
                (cut.offset - base) as usize,
                self.line_chars,
            );
        }
        self.line_chars = since(self.line_start, read, self.line_chars);
        // TriG: the statement's bytes so far, while they may be a block's head (a `{` later
        // asks; see `block_head`).
        if self.trig && !self.no_head && !self.in_block {
            let from = self.statement_start.saturating_sub(base) as usize;
            if from < read {
                self.head.extend_from_slice(&bytes[from..read]);
                // Inside a token (an IRI, a string, a comment) the head is told when it ends:
                // a long one isn't read again at every input.
                if self.state == State::Normal && head(&self.head, false) == Head::Not {
                    self.head.clear();
                    self.no_head = true;
                }
            }
        }
        self.offset = base + read as u64;
        read
    }

    /// Ends the document.
    pub(crate) fn finish(mut self) -> Skim {
        let pending = std::mem::take(&mut self.pending);
        let read = self.scan(&pending, true);
        debug_assert_eq!(read, pending.len(), "nothing is held at the end");
        if self.state == State::Dot {
            let cuts = self.skim.cuts.len();
            self.boundary(self.offset);
            // A cut at the end: after all the characters read on its line.
            if self.skim.cuts.len() > cuts {
                self.skim.cuts[cuts].column = self.line_chars;
            }
        }
        self.skim
    }
}

/// The directives in force before `offset`, as one text a parser can read.
pub(crate) fn directives_before(skim: &Skim, offset: u64) -> String {
    let mut text = String::new();
    for (end, directive) in &skim.directives {
        if *end <= offset {
            text.push_str(directive);
            text.push('\n');
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skim(text: &str, parts: usize, piece: usize) -> Skim {
        skim_as(text, parts, piece, false)
    }

    fn skim_as(text: &str, parts: usize, piece: usize, trig: bool) -> Skim {
        let mut skimmer = Skimmer::new(text.len() as u64, parts, trig);
        for chunk in text.as_bytes().chunks(piece.max(1)) {
            skimmer.feed(chunk);
        }
        skimmer.finish()
    }

    /// The cuts don't depend on the pieces the input comes in, where a delimiter, a name with
    /// `\` or a block's head is cut by a piece's end too (fix of the splitter's witnesses).
    /// A guard: written with the fix, the integration tests (`split_contract_tests`) give
    /// the cuts' meaning.
    #[test]
    fn the_same_cuts_whatever_the_pieces() {
        let documents = [
            (
                false,
                "<urn:s> <urn:p> [ <urn:q> . ) . <urn:u> <urn:v> <urn:w> . ] . <urn:a> <urn:b> <urn:c> .\n",
            ),
            (
                false,
                "<urn:s> <urn:p> [ <urn:q> > . <urn:u> <urn:v> <urn:w> . ] . <urn:a> <urn:b> <urn:c> .\n",
            ),
            (
                false,
                "<urn:s> <urn:p> <urn:o> \\( . <urn:u> <urn:v> <urn:w> . ) . <urn:a> <urn:b> <urn:c> .\n",
            ),
            (
                false,
                "@prefix ex: <http://e/> .\nex:Foo_\\(bar\\) ex:p ex:o\\. .\nex:a ex:p <<( ex:a ex:b ex:c )>> {| ex:q ex:r |} .\n",
            ),
            (
                true,
                "ex:g { <urn:a> <urn:b> <urn:c> . } GRAPH [ ] { <urn:d> <urn:e> <urn:f> . }\n<urn:s> <urn:p> { <urn:q> . } <urn:x> <urn:y> <urn:z> .\n<urn:g> { <urn:s> <urn:p> [ <urn:q> <urn:o> }\n<urn:a> <urn:b> <urn:c> .\n",
            ),
            // A name with a `.` and then an escaped closer, cut by a piece after the `.`
            // (sol-loop r1 S1); a byte order mark cut by a piece (S4).
            (
                false,
                "@prefix : <urn:> .\n<urn:s> <urn:p> ( :foo.bar\\) . <urn:u> <urn:v> <urn:w> . ) . <urn:a> <urn:b> <urn:c> .\n",
            ),
            (
                true,
                "\u{FEFF}<urn:g> { <urn:s> <urn:p> <urn:o> . }\n<urn:h> { <urn:a> <urn:b> <urn:c> . }\n",
            ),
            // Pieces ending right after an empty string or a `.` with a name's byte next, or
            // inside a string, a long string, a comment: the word held at a piece's end.
            (
                false,
                "<urn:s> <urn:p> \"\"@en . <urn:s> <urn:p> 1.5 . <urn:s> <urn:p> \"\"\"a\"b\"\"c\"\"\" . # c.d\n<urn:s> <urn:p> \"x\\\"y\" . <urn:a> <urn:b> <urn:c> .\n",
            ),
        ];
        for (trig, text) in documents {
            let whole = skim_as(text, text.len(), text.len(), trig);
            assert!(!whole.cuts.is_empty(), "{text:?}");
            for piece in [1, 2, 3, 7, 64] {
                let other = skim_as(text, text.len(), piece, trig);
                assert_eq!(other.cuts, whole.cuts, "pieces of {piece}: {text:?}");
            }
        }
    }

    #[test]
    fn cuts_only_at_statement_ends() {
        let text = "@prefix ex: <http://e/> .\nex:a ex:p \"x . y\" , '''a . \"b\" ''' .\nex:b ex:p ex:c.d , 1.5 , [ ex:q 2 . ] .\n# a comment . here\nPREFIX f: <http://f/>\nf:a ex:p ( 1 2 ) .\nex:e ex:p ex:f\\.g .\n";
        let every = skim(text, text.len(), 1);
        let cuts: Vec<&str> = every
            .cuts
            .iter()
            .map(|cut| &text[..cut.offset as usize])
            .collect();
        // Every cut is at the end of a statement (a valid prefix of the document).
        for cut in &cuts {
            assert!(cut.ends_with('.') || cut.ends_with('>'), "{cut:?}");
        }
        assert!(cuts.iter().any(|c| c.ends_with("1.5 , [ ex:q 2 . ] .")));
        assert!(!cuts.iter().any(|c| c.ends_with("\"x .")));
        assert!(!cuts.iter().any(|c| c.ends_with("ex:c.")));
        assert_eq!(
            every
                .directives
                .iter()
                .map(|(_, d)| d.trim())
                .collect::<Vec<_>>(),
            ["@prefix ex: <http://e/> .", "PREFIX f: <http://f/>"]
        );
        // Directives without whitespace after the keyword; `PREFIX:x` is a prefixed name.
        let tight = "BASE<http://e/>\n@prefix:<http://c/>.\nPREFIX:a <http://e/p> <http://e/o> .\n@prefix\nd:<http://d/>.\n";
        let found = skim(tight, tight.len(), 1);
        assert_eq!(
            found
                .directives
                .iter()
                .map(|(_, d)| d.trim())
                .collect::<Vec<_>>(),
            [
                "BASE<http://e/>",
                "@prefix:<http://c/>.",
                "@prefix\nd:<http://d/>."
            ]
        );
        // The same cuts whatever the pieces the input comes in.
        for piece in [1, 2, 3, 7, 64] {
            let other = skim(text, text.len(), piece);
            assert_eq!(other.cuts, every.cuts, "pieces of {piece}");
        }
    }

    /// RDF 1.2's version directives are directives: `VERSION "1.2"` ends with its string
    /// and doesn't swallow the directive after it (a file declaring `prov:` on its
    /// second line failed to load in parallel).
    #[test]
    fn version_directives_are_directives() {
        let text = "VERSION \"1.2\"\n@prefix prov: <http://www.w3.org/ns/prov#> .\n@version '1.2' .\nversion\"1.2\"\nPREFIX ex: <http://e/>\nex:a prov:wasGeneratedBy ex:b .\n";
        for piece in [1, 2, 3, 7, 64] {
            let found = skim(text, text.len(), piece);
            assert_eq!(
                found
                    .directives
                    .iter()
                    .map(|(_, d)| d.trim())
                    .collect::<Vec<_>>(),
                [
                    "VERSION \"1.2\"",
                    "@prefix prov: <http://www.w3.org/ns/prov#> .",
                    "@version '1.2' .",
                    "version\"1.2\"",
                    "PREFIX ex: <http://e/>"
                ],
                "pieces of {piece}"
            );
        }
    }
}
