//! A triple term keyed by its text: the first RDF 1.2 version's dictionary key for one, its
//! tag and the term as `Term`'s `Display` writes it, N-Triples 1.2 (`<<( s p o )>>`, IRIs in
//! `<>`, blank nodes as `_:`, quoted literals with their escapes, `@tag`, `@tag--dir` and
//! `^^<datatype>`). No path writes one now; a store of that version still holds them.
//!
//! Read back here, not by the N-Triples parser, so that what a decode holds has a bound and the
//! stored term comes back whatever its depth (that version set no limit; the parser takes 64
//! levels): [`decode`] allocates the term's strings, each at exactly its length, which is at
//! most its source text's (escapes only shorten), and one box a triple term, each of which has
//! at least [`TRIPLE_TEXT`] bytes of text of its own; nothing else, and no stack: only an object
//! nests, so the chain of triple terms is built from the outside in, through the slot of the
//! innermost one's object.
//!
//! Text that is no term, which that writer never wrote (a damaged key), is the simple literal
//! of the text, as the parser's fallback was: the decode has no error to give (`decode` answers
//! a term or none, and the key is one), and the literal keeps it total and bounded, the text.

use nrese_rdf::vocab::rdf;
use nrese_rdf::{BaseDirection, BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};

/// The text a triple term has read of its own when [`decode`] boxes it, at least: `<<(` and the
/// two bytes at least (`<>`, or `_:` and a byte) of its subject and of its predicate. Its
/// closing `)>>` isn't counted, since a damaged text may lack it (sol-loop r4 S3), nor its
/// object, which may be a triple term's, nor the spaces the writer puts between them, since
/// [`decode`] reads text without them too.
const TRIPLE_TEXT: usize = 3 + 2 + 2;

/// The most bytes [`decode`] holds for a text of `len` bytes, from its length alone: its
/// strings, together at most the text, and a box for each triple term it can hold; or the
/// fallback literal, the text.
pub(super) fn bound(len: usize) -> usize {
    len.saturating_add((len / TRIPLE_TEXT).saturating_mul(size_of::<Triple>()))
}

/// The term `text` writes, or the simple literal of `text` where it can't be read. What was
/// read before that is let go first, one level at a time: a damaged key's chain may be deeper
/// than a recursive drop has stack for.
pub(super) fn decode(text: &str) -> Term {
    let mut at = 0;
    let mut term = placeholder();
    if read(text, &mut at, &mut term).is_some() && {
        space(text, &mut at);
        at == text.len()
    } {
        return term;
    }
    let_go(term);
    Literal::new_simple_literal(text).into()
}

/// Drops `term` a triple term at a time, its object taken out first.
fn let_go(mut term: Term) {
    while let Term::Triple(triple) = term {
        term = triple.object;
    }
}

/// Reads the term at `*at` into `slot`: a run of triple terms, each the object of the one
/// before, read from the outside in, then the innermost object, then their closings. Each triple
/// term is boxed with a placeholder object (an empty IRI, which allocates nothing) that the
/// next one, or the innermost object, replaces. `None` where the text is no term; `slot` then
/// holds what was read.
fn read(text: &str, at: &mut usize, slot: &mut Term) -> Option<()> {
    space(text, at);
    let mut slot = slot;
    let mut open = 0usize;
    while text.get(*at..)?.starts_with("<<(") {
        *at += 3;
        space(text, at);
        let subject: NamedOrBlankNode = match text.as_bytes().get(*at)? {
            b'<' if !text[*at..].starts_with("<<(") => iri(text, at)?.into(),
            b'_' => blank(text, at)?.into(),
            _ => return None,
        };
        space(text, at);
        let predicate = iri(text, at)?;
        space(text, at);
        *slot = Triple::new(subject, predicate, placeholder()).into();
        slot = match slot {
            Term::Triple(triple) => &mut triple.object,
            _ => unreachable!("a triple term was just put here"),
        };
        open += 1;
    }
    *slot = match text.as_bytes().get(*at)? {
        b'<' => iri(text, at)?.into(),
        b'_' => blank(text, at)?.into(),
        b'"' => literal(text, at)?.into(),
        _ => return None,
    };
    for _ in 0..open {
        space(text, at);
        text.get(*at..)?.starts_with(")>>").then_some(())?;
        *at += 3;
    }
    Some(())
}

/// What a triple term holds until its object is read: allocates nothing.
fn placeholder() -> Term {
    NamedNode::new_unchecked(String::new()).into()
}

/// Spaces and tabs at `*at`, skipped.
fn space(text: &str, at: &mut usize) {
    let bytes = text.as_bytes();
    while matches!(bytes.get(*at), Some(b' ' | b'\t')) {
        *at += 1;
    }
}

/// `<iri>`, its `\u` and `\U` escapes read.
fn iri(text: &str, at: &mut usize) -> Option<NamedNode> {
    let start = *at + 1;
    let end = start + text.get(start..)?.find('>')?;
    *at = end + 1;
    Some(NamedNode::new_unchecked(unescaped(
        &text[start..end],
        false,
    )?))
}

/// `_:label`, up to the next space or tab, or what no label holds (`<`, `"`, `)`), or the end.
fn blank(text: &str, at: &mut usize) -> Option<BlankNode> {
    let start = text.get(*at..)?.strip_prefix("_:").map(|_| *at + 2)?;
    let len = text[start..]
        .find([' ', '\t', '<', '"', ')'])
        .unwrap_or(text.len() - start);
    if len == 0 {
        return None;
    }
    *at = start + len;
    Some(BlankNode::new_unchecked(&text[start..start + len]))
}

/// `"value"`, its escapes read, then `@tag`, `@tag--dir`, `^^<datatype>` or nothing. The tag
/// in lower case, as the parser gave it.
fn literal(text: &str, at: &mut usize) -> Option<Literal> {
    let bytes = text.as_bytes();
    let start = *at + 1;
    let mut end = start;
    loop {
        match bytes.get(end)? {
            b'"' => break,
            b'\\' => end += 2,
            _ => end += 1,
        }
    }
    let value = unescaped(text.get(start..end)?, true)?;
    *at = end + 1;
    let rest = &text[*at..];
    if let Some(tagged) = rest.strip_prefix('@') {
        let len = tagged
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(tagged.len());
        *at += 1 + len;
        let (tag, direction) = match tagged[..len].split_once("--") {
            None => (&tagged[..len], None),
            Some((tag, "ltr")) => (tag, Some(BaseDirection::Ltr)),
            Some((tag, "rtl")) => (tag, Some(BaseDirection::Rtl)),
            Some(_) => return None,
        };
        if tag.is_empty() {
            return None;
        }
        let tag = tag.to_ascii_lowercase();
        return Some(match direction {
            None => Literal::new_language_tagged_literal_unchecked(value, tag),
            Some(direction) => {
                Literal::new_directional_language_tagged_literal_unchecked(value, tag, direction)
            }
        });
    }
    if rest.starts_with("^^<") {
        *at += 2;
        let datatype = iri(text, at)?;
        // These a literal gets from a language tag only.
        if datatype == rdf::LANG_STRING || datatype == rdf::DIR_LANG_STRING {
            return None;
        }
        return Some(Literal::new_typed_literal(value, datatype));
    }
    Some(Literal::new_simple_literal(value))
}

/// `source` with its escapes read (`\u` and `\U` always, and with `echar` the one-character
/// ones of a quoted string), in a `String` of exactly its length; `None` for an escape that
/// isn't one.
fn unescaped(source: &str, echar: bool) -> Option<String> {
    let mut len = 0;
    pieces(source, echar, |piece| len += piece.len())?;
    let mut out = String::with_capacity(len);
    pieces(source, echar, |piece| piece.push_to(&mut out))?;
    Some(out)
}

/// A run of `source` as it is, or the character an escape writes.
enum Piece<'s> {
    Run(&'s str),
    Char(char),
}

impl Piece<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Run(run) => run.len(),
            Self::Char(c) => c.len_utf8(),
        }
    }

    fn push_to(&self, out: &mut String) {
        match self {
            Self::Run(run) => out.push_str(run),
            Self::Char(c) => out.push(*c),
        }
    }
}

/// Calls `f` with each piece of `source`: the one walk that both [`unescaped`]'s length and
/// its text come from.
fn pieces<'s>(source: &'s str, echar: bool, mut f: impl FnMut(Piece<'s>)) -> Option<()> {
    let mut rest = source;
    while let Some(at) = rest.find('\\') {
        if at > 0 {
            f(Piece::Run(&rest[..at]));
        }
        let escape = &rest[at + 1..];
        let (c, len) = match escape.as_bytes().first()? {
            b'u' => (hex(escape.get(1..5)?)?, 5),
            b'U' => (hex(escape.get(1..9)?)?, 9),
            b't' if echar => ('\t', 1),
            b'b' if echar => ('\u{8}', 1),
            b'n' if echar => ('\n', 1),
            b'r' if echar => ('\r', 1),
            b'f' if echar => ('\u{c}', 1),
            b'"' if echar => ('"', 1),
            b'\'' if echar => ('\'', 1),
            b'\\' if echar => ('\\', 1),
            _ => return None,
        };
        f(Piece::Char(c));
        rest = &escape[len..];
    }
    if !rest.is_empty() {
        f(Piece::Run(rest));
    }
    Some(())
}

/// The character `digits` (hexadecimal) names.
fn hex(digits: &str) -> Option<char> {
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    char::from_u32(u32::from_str_radix(digits, 16).ok()?)
}

#[cfg(test)]
mod tests {
    use nrese_rdf::vocab::xsd;

    use super::*;

    /// How these keys were read before: by the N-Triples parser, as the object of a line, the
    /// text itself as a simple literal where that failed. The oracle the decode must match.
    fn parsed(text: &str) -> Term {
        let line = format!("<urn:x> <urn:x> {text} .");
        nrese_rdf_io::RdfParser::from_format(nrese_rdf_io::RdfFormat::NTriples)
            .for_slice(line.as_bytes())
            .next()
            .and_then(Result::ok)
            .map_or_else(
                || Literal::new_simple_literal(text).into(),
                |quad| quad.object,
            )
    }

    /// Package C3: for every triple term the writer can key by its text (here 20,000 random
    /// ones: nesting to 5, absolute IRIs with non-ASCII, blank nodes, literals with every
    /// character the writer escapes, well-formed language tags in either case, base
    /// directions, datatypes, `xsd:string` among them), the decode is the parser's term, and
    /// it holds no more than [`bound`] of the text's length (its strings' capacities and its
    /// boxes); for text that is no term, which the writer never wrote, both give the text as a
    /// literal.
    #[test]
    fn a_text_reads_back_as_the_parser_read_it() {
        let mut random = Random(0x2545_f491_4f6c_dd1d);
        for round in 0..20_000 {
            let mut term = random.object(0);
            if !matches!(term, Term::Triple(_)) {
                let (s, p) = (
                    NamedNode::new_unchecked("urn:s"),
                    NamedNode::new_unchecked("urn:p"),
                );
                term = Triple::new(s, p, term).into();
            }
            let text = term.to_string();
            let decoded = decode(&text);
            assert_eq!(decoded, parsed(&text), "round {round}: {text}");
            assert!(
                decoded.heap_bytes() <= bound(text.len()),
                "round {round}: {text}"
            );
        }
        for text in [
            "<<( <urn:s> <urn:p> )>>",
            "<<( <urn:s> <urn:p> <urn:o>",
            "x",
        ] {
            assert_eq!(decode(text), parsed(text), "{text}");
            assert_eq!(decode(text), Literal::new_simple_literal(text).into());
        }
    }

    /// Package C3, review r1 R1: the first RDF 1.2 version wrote triple terms of any depth
    /// (`Triple::new` and its interning set no limit), and its stores still open, so their text
    /// reads back as the term at any depth: 65 levels (one past the 64 the N-Triples parser
    /// takes, which gave the text back as a literal) and 1,000, each against the term built
    /// apart, and within [`bound`].
    #[test]
    fn a_triple_term_reads_back_at_any_depth() {
        for depth in [64, 65, 1_000] {
            let mut expected: Term = NamedNode::new_unchecked("urn:o").into();
            for level in 0..depth {
                let s = NamedNode::new_unchecked(format!("urn:s{level}"));
                expected = Triple::new(s, NamedNode::new_unchecked("urn:p"), expected).into();
            }
            let text = expected.to_string();
            let decoded = decode(&text);
            assert!(decoded == expected, "depth {depth}: not the term");
            assert!(decoded.heap_bytes() <= bound(text.len()), "depth {depth}");
        }
    }

    /// A damaged key: 200,000 triple terms opened and none closed, no object. The decode is the
    /// text as a literal, the chain it read let go a level at a time (dropped whole, it would
    /// recurse 200,000 deep).
    #[test]
    fn a_damaged_deep_text_is_let_go_without_recursion() {
        let text = "<<( <urn:s> <urn:p> ".repeat(200_000);
        assert!(decode(&text) == Literal::new_simple_literal(&text).into());
    }

    /// Package C3, sol-loop r4 S3: a triple term is boxed once its opening, subject and
    /// predicate are read, before its closing, so damaged text of only those, `<<(<><>` (7
    /// bytes) once and repeated, holds a box for each. Measured (`nrese_exec::measure`): the
    /// decode's peak is within [`bound`] of the text's length.
    #[test]
    fn a_text_of_openings_alone_holds_no_more_than_its_bound() {
        if !nrese_exec::measure::alone() {
            return;
        }
        for repeats in [1, 2, 100, 10_000] {
            let text = "<<(<><>".repeat(repeats);
            let (decoded, peak) = nrese_exec::measure::measure(|| decode(&text));
            assert!(decoded == Literal::new_simple_literal(&text).into());
            assert!(
                peak <= bound(text.len()),
                "{repeats} openings: peak {peak}, bound {}",
                bound(text.len())
            );
        }
    }

    /// Package C3 (sol-loop r2 S2): the text was the term the dictionary interned, as `Display`
    /// writes it, and terms are interned unchecked, so it may hold what the N-Triples parser
    /// refuses: a tag that isn't well-formed BCP 47, a relative IRI, a blank label the grammar
    /// has no room for. The parser gave the text back as a literal, not the term; the decode
    /// gives the term (its tag in lower case, as for every tag).
    #[test]
    fn a_term_the_parser_refused_reads_back_as_interned() {
        let s = NamedNode::new_unchecked("urn:s");
        let p = NamedNode::new_unchecked("urn:p");
        let tagged = |tag| Literal::new_language_tagged_literal_unchecked("", tag);
        let terms: [Term; 4] = [
            Triple::new(s.clone(), p.clone(), tagged("en-a")).into(),
            Triple::new(s.clone(), NamedNode::new_unchecked("p"), tagged("x")).into(),
            Triple::new(
                BlankNode::new_unchecked("-a"),
                p.clone(),
                BlankNode::new_unchecked("b."),
            )
            .into(),
            Triple::new(
                s,
                p,
                Literal::new_typed_literal("1", NamedNode::new_unchecked("int")),
            )
            .into(),
        ];
        for term in terms {
            let text = term.to_string();
            assert_eq!(decode(&text), term, "{text}");
            assert_eq!(
                parsed(&text),
                Literal::new_simple_literal(&text).into(),
                "{text}"
            );
        }
    }

    /// Xorshift, from a fixed seed: the same terms every run.
    struct Random(u64);

    /// Every character the writer escapes, the quote and backslash it escapes by name, and
    /// others it doesn't.
    const LITERAL: [char; 18] = [
        'a', 'Z', '0', ' ', '"', '\\', '\n', '\t', '\r', '\u{8}', '\u{c}', '\u{1}', '\u{7f}',
        '\u{fffe}', '\u{ffff}', 'é', '😀', '\'',
    ];
    const IRI: [char; 6] = ['a', 'z', '0', 'é', 'ü', '9'];
    /// Well-formed BCP 47 tags, in either case: the parser refuses others.
    const TAGS: [&str; 6] = [
        "en",
        "EN-gb",
        "de-CH-1996",
        "zh-Hant-TW",
        "x-private",
        "sr-Latn",
    ];

    impl Random {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }

        fn text(&mut self, most: u64, chars: &[char]) -> String {
            (0..self.below(most + 1))
                .map(|_| chars[self.below(chars.len() as u64) as usize])
                .collect()
        }

        fn iri(&mut self) -> NamedNode {
            NamedNode::new_unchecked(format!("urn:x:{}", self.text(12, &IRI)))
        }

        fn object(&mut self, depth: u64) -> Term {
            match self.below(if depth < 5 { 6 } else { 5 }) {
                0 => self.iri().into(),
                1 => BlankNode::new_unchecked(format!("b{}", self.below(1000))).into(),
                2 => Literal::new_simple_literal(self.text(40, &LITERAL)).into(),
                3 => {
                    let tag = TAGS[self.below(TAGS.len() as u64) as usize];
                    let value = self.text(20, &LITERAL);
                    match self.below(3) {
                        0 => Literal::new_language_tagged_literal_unchecked(value, tag),
                        1 => Literal::new_directional_language_tagged_literal_unchecked(
                            value,
                            tag,
                            BaseDirection::Ltr,
                        ),
                        _ => Literal::new_directional_language_tagged_literal_unchecked(
                            value,
                            tag,
                            BaseDirection::Rtl,
                        ),
                    }
                    .into()
                }
                4 => {
                    let datatype = match self.below(2) {
                        0 => xsd::STRING.into_owned(),
                        _ => self.iri(),
                    };
                    Literal::new_typed_literal(self.text(10, &LITERAL), datatype).into()
                }
                _ => {
                    let subject: NamedOrBlankNode = match self.below(2) {
                        0 => self.iri().into(),
                        _ => BlankNode::new_unchecked(format!("s{}", self.below(100))).into(),
                    };
                    let predicate = self.iri();
                    Triple::new(subject, predicate, self.object(depth + 1)).into()
                }
            }
        }
    }
}
