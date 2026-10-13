//! Input: the bytes of a document, from a slice or a reader, a line at a time.

use std::fs::File;
use std::io::{self, Read};
use std::ops::Range;
use std::path::PathBuf;

use memchr::{memchr2, memchr2_iter};

use crate::RdfParseError;

/// Where the input comes from.
enum Source<'a, R> {
    Slice(&'a [u8]),
    Reader {
        reader: R,
        buffer: Vec<u8>,
        /// The unread bytes are `buffer[start..end]`.
        start: usize,
        end: usize,
        eof: bool,
    },
}

/// The lines of the document before a chunk's first line, counted only when an error has to
/// name its line: a parallel parse cuts the document at byte offsets, and counting the line
/// ends before every cut would put a pass over the whole input on the parse's path.
pub(crate) enum Before<'a> {
    /// A whole document, or a chunk whose first line is known.
    Lines(u64),
    /// The document's bytes before the chunk.
    Bytes(&'a [u8]),
    /// The file's first `.1` bytes.
    File(PathBuf, u64),
}

impl Before<'_> {
    /// The number of lines before the chunk: its line ends, `\n` or `\r` each, as
    /// [`Lines::next`] counts them (a chunk starts after one).
    fn count(&self) -> io::Result<u64> {
        match self {
            Before::Lines(n) => Ok(*n),
            Before::Bytes(bytes) => Ok(memchr2_iter(b'\n', b'\r', bytes).count() as u64),
            Before::File(path, length) => {
                let mut file = File::open(path)?.take(*length);
                let mut buffer = vec![0_u8; 1 << 20];
                let mut lines = 0;
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) => return Ok(lines),
                        Ok(n) => lines += memchr2_iter(b'\n', b'\r', &buffer[..n]).count() as u64,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }
}

/// Lines of a document (ending at `\n` or `\r`, as N-Triples' `EOL` allows). A line is
/// returned as a range, to borrow with [`Lines::slice`] until the next call: callers can
/// skip lines without holding a borrow. The reader's buffer grows for a line longer than it.
pub(crate) struct Lines<'a, R> {
    source: Source<'a, R>,
    /// The document's lines before these: an error's line is the document's.
    before: Before<'a>,
    /// The byte offset of the next line, and the number of lines returned so far.
    offset: u64,
    line: u64,
    /// The offset of the line returned last.
    line_start: u64,
    /// Where the slice's next line starts.
    position: usize,
}

const INITIAL_BUFFER: usize = 64 * 1024;

impl<'a> Lines<'a, io::Empty> {
    /// The lines of `bytes`, which start at byte `offset` of the document.
    pub(crate) fn from_slice(bytes: &'a [u8], offset: u64) -> Self {
        Self {
            source: Source::Slice(bytes),
            before: Before::Lines(0),
            offset,
            line: 0,
            line_start: offset,
            position: 0,
        }
    }
}

impl<R: Read> Lines<'static, R> {
    /// The lines `reader` gives, which start at byte `offset` of the document.
    pub(crate) fn from_reader(reader: R, offset: u64) -> Self {
        Self {
            source: Source::Reader {
                reader,
                buffer: vec![0; INITIAL_BUFFER],
                start: 0,
                end: 0,
                eof: false,
            },
            before: Before::Lines(0),
            offset,
            line: 0,
            line_start: offset,
            position: 0,
        }
    }
}

impl<'a, R: Read> Lines<'a, R> {
    /// These lines as a chunk after the document's lines `before`.
    pub(crate) fn after(mut self, before: Before<'a>) -> Self {
        self.before = before;
        self
    }

    /// `error`, a syntax error's line counted in the document (an I/O error as it is; if the
    /// lines before can't be counted, the chunk's own line stays, rather than no error).
    pub(crate) fn in_document(&self, error: RdfParseError) -> RdfParseError {
        match (error, &self.before) {
            (RdfParseError::Syntax(syntax), Before::Lines(0)) => RdfParseError::Syntax(syntax),
            (RdfParseError::Syntax(syntax), before) => match before.count() {
                Ok(lines) => RdfParseError::Syntax(syntax.after_lines(lines)),
                Err(_) => RdfParseError::Syntax(syntax),
            },
            (error, _) => error,
        }
    }
}

impl<R: Read> Lines<'_, R> {
    /// The number of the line returned last (from 0 in this input), and its byte offset.
    pub(crate) fn position(&self) -> (u64, u64) {
        (self.line.saturating_sub(1), self.line_start)
    }

    /// The line `range` (from [`Lines::next`]).
    pub(crate) fn slice(&self, range: Range<usize>) -> &[u8] {
        match &self.source {
            Source::Slice(bytes) => &bytes[range],
            Source::Reader { buffer, .. } => &buffer[range],
        }
    }

    /// The next line, without its terminator; `None` at the end.
    pub(crate) fn next(&mut self) -> io::Result<Option<Range<usize>>> {
        let (from, length, consumed) = match &mut self.source {
            Source::Slice(bytes) => {
                let rest = &bytes[self.position..];
                if rest.is_empty() {
                    return Ok(None);
                }
                let (length, consumed) = match memchr2(b'\n', b'\r', rest) {
                    Some(i) => (i, i + 1),
                    None => (rest.len(), rest.len()),
                };
                let from = self.position;
                self.position += consumed;
                (from, length, consumed)
            }
            Source::Reader {
                reader,
                buffer,
                start,
                end,
                eof,
            } => {
                let found = loop {
                    if let Some(i) = memchr2(b'\n', b'\r', &buffer[*start..*end]) {
                        break (*start, i, i + 1);
                    }
                    if *eof {
                        if start == end {
                            return Ok(None);
                        }
                        break (*start, *end - *start, *end - *start);
                    }
                    // Keep the partial line, and make room for more.
                    if *start > 0 {
                        buffer.copy_within(*start..*end, 0);
                        *end -= *start;
                        *start = 0;
                    }
                    if *end == buffer.len() {
                        buffer.resize(buffer.len() * 2, 0);
                    }
                    match reader.read(&mut buffer[*end..]) {
                        Ok(0) => *eof = true,
                        Ok(n) => *end += n,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error),
                    }
                };
                *start = found.0 + found.2;
                found
            }
        };
        self.line_start = self.offset;
        self.offset += consumed as u64;
        self.line += 1;
        Ok(Some(from..from + length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all<R: Read>(mut lines: Lines<'_, R>) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(range) = lines.next().unwrap() {
            out.push(String::from_utf8(lines.slice(range).to_vec()).unwrap());
        }
        out
    }

    #[test]
    fn lines_end_at_either_terminator() {
        let text = b"a\nbb\r\nccc\rd";
        let expected = ["a", "bb", "", "ccc", "d"];
        assert_eq!(all(Lines::from_slice(text, 0)), expected);
        assert_eq!(all(Lines::from_reader(&text[..], 0)), expected);
    }

    #[test]
    fn a_line_longer_than_the_buffer_grows_it() {
        let long = "x".repeat(INITIAL_BUFFER * 3);
        let text = format!("{long}\nshort\n");
        let lines = all(Lines::from_reader(text.as_bytes(), 0));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), INITIAL_BUFFER * 3);
    }

    /// A reader that returns one byte per call.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            match self.0.split_first() {
                Some((&b, rest)) if !out.is_empty() => {
                    out[0] = b;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn short_reads_make_no_difference() {
        let text = b"one\ntwo\nthree";
        assert_eq!(
            all(Lines::from_reader(Trickle(text), 0)),
            ["one", "two", "three"]
        );
        let mut lines = Lines::from_reader(&text[..], 100);
        lines.next().unwrap();
        lines.next().unwrap();
        assert_eq!(lines.position(), (1, 104));
    }
}
