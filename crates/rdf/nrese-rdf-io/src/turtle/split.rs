//! Exact statement boundaries in Turtle and TriG, for parsing in parallel.
//!
//! A [`Skimmer`] reads the document once, sequentially, but only far enough to know what
//! is a string (four quote forms, with escapes), an IRI, a comment or an escaped character
//! in a name, and how deep `[`, `(` and `{` nest: a table lookup per byte outside those,
//! `memchr` inside them. A `.` at depth 0 followed by whitespace (or the end, or a
//! comment), and a `}` that closes a TriG block, end a statement for certain; every other
//! `.` is left alone (a boundary missed costs nothing, a wrong one would corrupt the parse).
//! The skim also records every `@prefix`, `@base`, `PREFIX` and `BASE` directive, so each
//! chunk starts with exactly the prefixes and base in force where it begins, wherever the
//! directives are in the document.
//!
//! Oxigraph's Turtle splitter guesses instead (a `.` after which three triples parse) and
//! says it can fail or give wrong results on directives after the start or Turtle inside
//! literals; this one can't, for a valid document. On an invalid document the chunks still
//! report its error, maybe after other statements than a sequential parse would.

/// Bytes the skimmer must look at outside strings, IRIs and comments.
const fn interesting() -> [bool; 256] {
    let mut table = [false; 256];
    let bytes = b"\"'<#.[]{}()\\";
    let mut i = 0;
    while i < bytes.len() {
        table[bytes[i] as usize] = true;
        i += 1;
    }
    table
}
const INTERESTING: [bool; 256] = interesting();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    /// After a `.` at depth 0: a boundary if whitespace, a comment or the end follows.
    Dot,
    /// After `\` outside a string: the next byte is escaped.
    Escape,
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

/// The skim of one document: where to cut, and the directives.
#[derive(Debug, Default)]
pub(crate) struct Skim {
    /// The chosen cuts: byte offset, and the line and line start there.
    pub(crate) cuts: Vec<(u64, u64, u64)>,
    /// Each directive: where it ends (offset of the byte after it) and its text.
    pub(crate) directives: Vec<(u64, String)>,
}

/// Reads a document in pieces and finds cuts near the targets.
pub(crate) struct Skimmer {
    state: State,
    depth: i64,
    /// The document offset of the next byte fed.
    offset: u64,
    line: u64,
    line_start: u64,
    /// Where cuts are wanted, ascending; the first one not yet reached.
    targets: Vec<u64>,
    next_target: usize,
    skim: Skim,
    /// At the start of a statement: the first bytes, to tell a directive (up to 7 bytes),
    /// and where the statement began.
    start: Option<(u64, Vec<u8>)>,
    /// The directive being read: its text so far, and whether it ends at a '.' (`@` forms)
    /// or after its IRI (SPARQL forms).
    directive: Option<(Vec<u8>, bool)>,
}

impl Skimmer {
    /// A skimmer for a document of `length` bytes cut into up to `parts` chunks.
    pub(crate) fn new(length: u64, parts: usize) -> Self {
        let parts = parts.max(1) as u64;
        Self {
            state: State::Normal,
            depth: 0,
            offset: 0,
            line: 0,
            line_start: 0,
            targets: (1..parts).map(|k| length * k / parts).collect(),
            next_target: 0,
            skim: Skim::default(),
            start: Some((0, Vec::new())),
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
            self.skim.cuts.push((at, self.line, self.line_start));
            while self.next_target < self.targets.len() && self.targets[self.next_target] <= at {
                self.next_target += 1;
            }
        }
        self.start = Some((at, Vec::new()));
    }

    /// Feeds the next bytes of the document.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        let base = self.offset;
        // Lines: counted per piece with memchr (vectorised), and at each boundary from the
        // piece's start up to it.
        let mut counted = 0_usize;
        let mut i = 0;
        let n = bytes.len();
        let count_lines = |me: &mut Self, counted: &mut usize, upto: usize| {
            if upto > *counted {
                let piece = &bytes[*counted..upto];
                let breaks = memchr::memchr_iter(b'\n', piece).count() as u64;
                if breaks > 0 {
                    let last = memchr::memrchr(b'\n', piece).unwrap_or(0);
                    me.line += breaks;
                    me.line_start = base + (*counted + last + 1) as u64;
                }
                *counted = upto;
            }
        };
        while i < n {
            // A statement's first bytes: is it a directive?
            if let Some((_, first)) = &mut self.start
                && self.depth == 0
                && matches!(self.state, State::Normal)
            {
                let b = bytes[i];
                if first.is_empty() && b.is_ascii_whitespace() {
                    i += 1;
                    continue;
                }
                if first.is_empty() && b == b'#' {
                    // A comment before the statement.
                } else {
                    first.push(b);
                    // As the lexer reads them: `@` and a run of letters that is `prefix` or
                    // `base`; or a name that is `PREFIX` or `BASE` in any case and not
                    // followed by `:` (that would be a prefixed name). No whitespace needs
                    // to follow (`BASE<…>`, `@prefix:<…>`).
                    let (run, dotted) = match first.split_first() {
                        Some((b'@', rest)) => (rest, true),
                        _ => (&first[..], false),
                    };
                    let in_run = |c: u8| {
                        if dotted {
                            c.is_ascii_alphabetic()
                        } else {
                            c.is_ascii_alphanumeric()
                                || matches!(c, b'_' | b'-' | b'.')
                                || c >= 0x80
                        }
                    };
                    let keywords: [&[u8]; 2] = [b"prefix", b"base"];
                    let same = |word: &[u8], keyword: &[u8]| {
                        if dotted {
                            word == keyword
                        } else {
                            word.eq_ignore_ascii_case(keyword)
                        }
                    };
                    let (&last, word) = run.split_last().unwrap_or((&b' ', &[]));
                    if run.is_empty() {
                        // Only the `@` so far.
                    } else if in_run(last) {
                        // Still in the run: a keyword can still come of it?
                        let could_be = keywords.iter().any(|k| {
                            run.len() <= k.len()
                                && (if dotted {
                                    k.starts_with(run)
                                } else {
                                    k[..run.len()].eq_ignore_ascii_case(run)
                                })
                        });
                        if !could_be {
                            self.start = None;
                        }
                    } else if keywords.iter().any(|k| same(word, k)) && (dotted || last != b':') {
                        let mut text = std::mem::take(first);
                        text.truncate(text.len() - 1);
                        self.start = None;
                        self.directive = Some((text, dotted));
                        // Re-read this byte as part of the directive.
                        continue;
                    } else {
                        self.start = None;
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
                        // The fast path: skip to the next byte that matters (a directive's
                        // text is copied byte by byte above, but directives are few).
                        if self.directive.is_none() && self.start.is_none() {
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
                        b'"' | b'\'' => self.state = State::Opening { quote: b, count: 1 },
                        b'<' => self.state = State::Iri,
                        b'#' => self.state = State::Comment,
                        b'\\' => self.state = State::Escape,
                        b'[' | b'(' | b'{' => self.depth += 1,
                        b']' | b')' => self.depth -= 1,
                        b'}' => {
                            self.depth -= 1;
                            if self.depth == 0 {
                                count_lines(self, &mut counted, i + 1);
                                self.boundary(base + i as u64 + 1);
                            }
                        }
                        b'.' if self.depth == 0 => self.state = State::Dot,
                        _ => {}
                    }
                    i += 1;
                }
                State::Dot => {
                    self.state = State::Normal;
                    // The byte is read again in the normal state (a '#' starts a comment):
                    // take it back from a directive's text.
                    if let Some((text, _)) = &mut self.directive {
                        text.pop();
                    }
                    if b.is_ascii_whitespace() || b == b'#' {
                        count_lines(self, &mut counted, i);
                        self.boundary(base + i as u64);
                    }
                }
                State::Escape => {
                    self.state = State::Normal;
                    i += 1;
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
                        // A SPARQL-style directive ends with its IRI.
                        if matches!(&self.directive, Some((text, false)) if text.iter().filter(|&&c| c == b'<').count() >= 1)
                        {
                            let at = base + i as u64;
                            count_lines(self, &mut counted, i);
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
                    } else if count == 2 {
                        // `""`: the empty string; this byte is after it.
                        self.state = State::Normal;
                    } else {
                        self.state = State::Short {
                            quote,
                            escaped: false,
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
                            self.state = if bytes[i + k] == quote {
                                State::Normal
                            } else {
                                State::Short {
                                    quote,
                                    escaped: true,
                                }
                            };
                            i += k + 1;
                        }
                        None => i = n,
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
        count_lines(self, &mut counted, n);
        self.offset = base + n as u64;
    }

    /// Ends the document.
    pub(crate) fn finish(mut self) -> Skim {
        if self.state == State::Dot {
            self.boundary(self.offset);
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
        let mut skimmer = Skimmer::new(text.len() as u64, parts);
        for chunk in text.as_bytes().chunks(piece.max(1)) {
            skimmer.feed(chunk);
        }
        skimmer.finish()
    }

    #[test]
    fn cuts_only_at_statement_ends() {
        let text = "@prefix ex: <http://e/> .\nex:a ex:p \"x . y\" , '''a . \"b\" ''' .\nex:b ex:p ex:c.d , 1.5 , [ ex:q 2 . ] .\n# a comment . here\nPREFIX f: <http://f/>\nf:a ex:p ( 1 2 ) .\nex:e ex:p ex:f\\.g .\n";
        let every = skim(text, text.len(), 1);
        let cuts: Vec<&str> = every
            .cuts
            .iter()
            .map(|&(at, _, _)| &text[..at as usize])
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
}
