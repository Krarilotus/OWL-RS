//! Where a document stops, and what its values are, on documents written by hand:
//!
//! - the parser's documented contract (`RdfParser::recovering`): a read error always stops;
//!   a syntax error stops unless the format goes on after one, whichever layer found it;
//! - a statement that fails gives none of its triples (Turtle 1.2 §7: only a conforming
//!   statement's triples are in the graph);
//! - XML 1.0 §3.3.3 (attribute-value normalisation), §4.5 (an entity's replacement text),
//!   §2.2 (legal characters) and §4.1 (a reference must name a declared entity), for
//!   RDF/XML's property attributes, namespace declarations and text;
//! - RDF4J's Binary RDF: a variable-length integer holds at most 32 bits.
//!
//! Each table checks every row and reports all that fail (`crate::table`).

use std::collections::VecDeque;
use std::io::{self, Read};

use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::n3::N3Parser;
use nrese_rdf_io::{RdfFormat, RdfParseError, RdfParser};

use crate::table::{Table, same};

/// Each item until the end, or the first nine (a parser that never stops shows as nine).
fn drain<T>(items: impl Iterator<Item = Result<T, RdfParseError>>) -> Vec<Result<T, String>> {
    items
        .take(9)
        .map(|item| item.map_err(|e| e.to_string()))
        .collect()
}

/// `Ok` or `Err` per item: the shape of an outcome.
fn shape<T>(items: &[Result<T, String>]) -> Vec<bool> {
    items.iter().map(Result::is_ok).collect()
}

/// The items have the shape `expected`.
fn shaped<T: std::fmt::Debug>(
    items: &[Result<T, String>],
    expected: &[bool],
) -> Result<(), String> {
    if shape(items) == expected {
        Ok(())
    } else {
        Err(format!("want {expected:?}, got {items:?}"))
    }
}

fn iri(text: &str) -> NamedNode {
    NamedNode::new_unchecked(text)
}

fn triple(s: &str, p: &str, o: impl Into<Term>) -> Quad {
    Quad::new(iri(s), iri(p), o, GraphName::DefaultGraph)
}

fn recovering_if(parser: RdfParser, recovering: bool) -> RdfParser {
    if recovering {
        parser.recovering()
    } else {
        parser
    }
}

// --- A failed statement gives none of its triples ---------------------------------------

#[test]
fn a_failed_statement_gives_none_of_its_triples() {
    let after = " <http://e/a> <http://e/b> <http://e/c> .";
    let abc = triple("http://e/a", "http://e/b", iri("http://e/c"));
    let mut table = Table::default();
    // (format, statement, whether recovering goes on with the next statement: not when
    // the failing token is the statement's own `.`, which recovery then can't find)
    for (format, statement, resumes) in [
        (RdfFormat::Turtle, "<http://e/s> <http://e/p> 1, .", false),
        (RdfFormat::Turtle, "<http://e/s> <http://e/p> 1, ] .", true),
        (
            RdfFormat::Turtle,
            "<http://e/s> <http://e/p> <<( <http://e/a> <http://e/b> <http://e/c> )>>, ] .",
            true,
        ),
        (
            RdfFormat::Turtle,
            "<http://e/s> <http://e/p> 1; <http://e/q> ] .",
            true,
        ),
        (
            RdfFormat::TriG,
            "<http://e/g> { <http://e/s> <http://e/p> 1, ] . }",
            true,
        ),
    ] {
        let document = format!("{statement}{after}");
        let strict = drain(RdfParser::from_format(format).for_slice(document.as_bytes()));
        table.row(false, ("strict", statement), shaped(&strict, &[false]));
        if !resumes {
            continue;
        }
        // Recovering skips the statement, all of it, and goes on (recovery cleared the
        // queue before too).
        let recovering = drain(
            RdfParser::from_format(format)
                .recovering()
                .for_slice(document.as_bytes()),
        );
        let outcome = shaped(&recovering, &[false, true])
            .and_then(|()| same(recovering[1].as_ref().ok(), Some(&abc)));
        table.row(true, ("recovering", statement), outcome);
    }
    // N3's own parser too.
    let n3 = drain(N3Parser::new().for_slice(b"<http://e/s> <http://e/p> 1, ."));
    table.row(false, "N3Parser <s> <p> 1, .", shaped(&n3, &[false]));
    table.done();
}

/// Recovery resumes right after the failing statement's end: a `.` with nothing of the
/// statement open is its end (Turtle 1.2 §6.5 `triples '.'`; `RdfParser::recovering`: the
/// rest of the statement is skipped, up to the `.` ending it). What of the statement is
/// open when it fails stays open while its rest is skipped, `[`, `(`, `<<(`, `<<` and `{|`
/// alike (review p2-a1b r1, A1B-R1-F1). Read from a slice, a reader, and a reader giving
/// one byte at a time. Guards held before the change too.
#[test]
fn recovery_resumes_after_the_failing_statements_own_end() {
    let abc = triple("http://e/a", "http://e/b", iri("http://e/c"));
    let def = triple("http://e/d", "http://e/e", iri("http://e/f"));
    let in_g = Quad::new(
        iri("http://e/a"),
        iri("http://e/b"),
        iri("http://e/c"),
        iri("http://e/g"),
    );
    let after = triple("urn:after", "urn:p", iri("urn:o"));
    let after_in_g = Quad::new(iri("urn:after"), iri("urn:p"), iri("urn:o"), iri("urn:g"));
    let mut table = Table::default();
    // (guard, format, document, recovering's items: Err as None)
    let cases: Vec<(bool, RdfFormat, &'static str, Vec<Option<&Quad>>)> = vec![
        // The statement fails inside its open `[` or `(`: the statement-shaped `urn:u`
        // sequence inside it is skipped with it, up to the `]` or `)` and the `.` after.
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> . <urn:a> <urn:b> <urn:c> . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> ( 1 . <urn:a> <urn:b> <urn:c> . <urn:u> <urn:v> <urn:w> . ) . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> . <urn:a> <urn:b> <urn:c> . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> ( 1 . <urn:a> <urn:b> <urn:c> . <urn:u> <urn:v> <urn:w> . ) . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        // The same inside an open triple term or annotation.
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <<( <urn:a> . <urn:u> <urn:v> <urn:w> . )>> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <urn:o> {| <urn:q> . <urn:u> <urn:v> <urn:w> . |} . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // RDF 1.2's delimiters opened in the skipped rest count as brackets too.
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <urn:o> <urn:x> <<( <urn:a> <urn:b> . <urn:u> <urn:v> <urn:w> . )>> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <urn:o> <urn:x> << <urn:a> <urn:b> . <urn:u> <urn:v> <urn:w> . >> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <urn:o> <urn:x> {| <urn:a> <urn:b> . <urn:u> <urn:v> <urn:w> . |} . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // The `}` closing the TriG block ends the statement, open brackets or not.
        (
            true,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> . } <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // A quote with no closing quote anywhere after it (a missing closer): the quote
        // alone is skipped and its text read as tokens, the `[` still open. The `(` in it is
        // opened by the skip; the `]` matches nothing the skip opened, so it closes the `[`
        // (and the `(` in it), and the `.` after it ends the statement (the policy of
        // `turtle_tests::recovering_parsers_skip_bad_statements_and_go_on`).
        (
            true,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> \"open ; <urn:r> ( 1 2 ] .\n<urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // The same when the quote comes while the rest is skipped (sol-loop A1B-R2-F1).
        (
            true,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> . \"open ; <urn:r> ( 1 2 ] .\n<urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // A missing closing quote with a statement in its text, inside the `[`: the `[`
        // stays open over it, so the statement is skipped with the rest (fix check r2).
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> \"open . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // Bytes the lexer can't read keep what is open: a stray `%` in the failed
        // statement, or met while its rest is skipped (fix check r2, A1B-R1-F1).
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> % . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> . % . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> % . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> . % . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        // A closer closes an open bracket of its own kind, or nothing: a `)` with only `[`
        // open leaves them open, whether the `[` was the failed statement's or the skip's
        // (fix check r3: A1B-R3-F1 and R2-11).
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> . [ ) . <urn:u> <urn:v> <urn:w> . ] ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> . [ ) . <urn:u> <urn:v> <urn:w> . ] ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> . ) . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> . ) . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        // The same when the failing token is itself the wrong closer (taken where `]` was
        // expected), or a stray opener (taken where `.` was expected).
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> <urn:o> ) . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> <urn:o> ) . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <urn:o> [ ) . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> <urn:o> [ ) . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        // An open `<<(` isn't closed by a `]`.
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <<( <urn:a> <urn:b> . ] . <urn:u> <urn:v> <urn:w> . )>> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> <<( <urn:a> <urn:b> . ] . <urn:u> <urn:v> <urn:w> . )>> . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        // A closer of an outer open bracket closes it and the unfinished ones inside it (the
        // salvage policy), whether met while skipping or as the failing token. Before, the
        // `]` closed one bracket of two, so `urn:after` was skipped too: over-skipping, not
        // a wrong answer.
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> ( 1 . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> ( 1 . ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> ( 1 ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> ( 1 ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        // A brace as the failing token counts as one met while skipping: a `{` stays open
        // over the `.` inside it; a `}` ends the TriG block it is in, so what follows is in
        // the default graph (and in Turtle, with no block, closes nothing).
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> { <urn:q> . <urn:u> <urn:v> <urn:w> . } . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> { <urn:q> . <urn:u> <urn:v> <urn:w> . } . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> } <http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        // Before, the `[` kept the skip going to the end, `urn:after` with it: over-skipping.
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> <urn:o> } <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            true,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> } . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // A TriG graph block whose name fails (an undeclared prefix, an IRI that can't be
        // one) is skipped through its own `}`: the statement after the block isn't part of
        // it. Before, the `{` was read as a brace of the failed statement, which went on to
        // the next `.` and took that statement with it.
        (
            false,
            RdfFormat::TriG,
            "ex:g { <urn:u> <urn:v> <urn:w> . } <http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "GRAPH <a b> { <urn:u> <urn:v> <urn:w> . } <http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        // A name that fails without a block after it is a failed statement, as before.
        (
            true,
            RdfFormat::TriG,
            "ex:s <urn:p> <urn:o> . <http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        // A SPARQL-style directive that fails ends after its IRI (PREFIX, BASE) or its string
        // (VERSION), as one that parses does: the statement after it isn't part of it.
        // Before, the failed directive went on to the next `.` and took that statement with
        // it.
        (
            false,
            RdfFormat::Turtle,
            "PREFIX ex: <a b>\n<http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "PREFIX ex <x>\n<http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "BASE <a b>\n<http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "VERSION 12 \"1.2\"\n<http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        // With no IRI (or string) to end it, it ends at a `.` with nothing open, as before.
        (
            true,
            RdfFormat::Turtle,
            "VERSION 12\n<urn:u> <urn:v> <urn:w> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // In its `@` form a directive ends at its `.`, failed or not, as before.
        (
            true,
            RdfFormat::Turtle,
            "@prefix ex: <a b> <urn:u> .\n<http://e/a> <http://e/b> <http://e/c> . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&abc), Some(&after)],
        ),
        // A string with a line break in it, closed later: its end is known, so it is
        // skipped whole and the `] .` and statement in it are text, not structure (fix
        // check r2). LF and CR, both quotes, and in a TriG block.
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> \"a\n] . <urn:u> <urn:v> <urn:w> .\" ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> \"a\r] . <urn:u> <urn:v> <urn:w> .\" ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> 'a\n] . <urn:u> <urn:v> <urn:w> .' ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> \"a\n] . <urn:u> <urn:v> <urn:w> .\" ] . <urn:after> <urn:p> <urn:o> . }",
            vec![None, Some(&after_in_g)],
        ),
        // The same string met while the rest is skipped.
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> . \"a\n] . <urn:u> <urn:v> <urn:w> .\" ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // A long string with no closing quotes says nothing of a later short string: that
        // one's closing quote is still searched for and found (sol-loop r2 S1).
        (
            false,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> \"\"\" \"a\n] . <urn:u> <urn:v> <urn:w> .\" ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // An escaped quote before the closing one is part of the string (the lexer's rule).
        (
            true,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> \"a\n\\\" ] . <urn:u> <urn:v> <urn:w> .\" ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // Brackets the skip opened itself stay open past unreadable bytes (sol-loop
        // A1B-R3-F1).
        (
            true,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> <urn:o> <urn:x> [ % . <urn:u> <urn:v> <urn:w> . ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // A stray closer opens nothing: the next `.` outside brackets ends the statement.
        (
            true,
            RdfFormat::Turtle,
            "<urn:s> <urn:p> 1, ] . <urn:after> <urn:p> <urn:o> .",
            vec![None, Some(&after)],
        ),
        // The failing token is the statement's own `.`.
        (
            false,
            RdfFormat::Turtle,
            "<http://e/s> <http://e/p> 1, . <http://e/a> <http://e/b> <http://e/c> .",
            vec![None, Some(&abc)],
        ),
        (
            false,
            RdfFormat::Turtle,
            "<http://e/s> . <http://e/a> <http://e/b> <http://e/c> .",
            vec![None, Some(&abc)],
        ),
        (
            false,
            RdfFormat::TriG,
            "<http://e/g> { <http://e/s> <http://e/p> 1, . <http://e/a> <http://e/b> <http://e/c> . }",
            vec![None, Some(&in_g)],
        ),
        // A `.` inside brackets doesn't end the statement: skipped to the one that does.
        (
            true,
            RdfFormat::Turtle,
            "<http://e/s> <http://e/p> [ <http://e/q> . ] . <http://e/a> <http://e/b> <http://e/c> .",
            vec![None, Some(&abc)],
        ),
        (
            true,
            RdfFormat::Turtle,
            "<http://e/s> <http://e/p> ( 1 . ) . <http://e/a> <http://e/b> <http://e/c> .",
            vec![None, Some(&abc)],
        ),
        // Bytes the lexer can't read right after a statement start a bad statement, which
        // ends at its `.`: the earlier statement's `.` doesn't count for it.
        (
            true,
            RdfFormat::Turtle,
            "<http://e/a> <http://e/b> <http://e/c> . %% <http://e/d> <http://e/e> <http://e/f> . <http://e/d> <http://e/e> <http://e/f> .",
            vec![Some(&abc), None, Some(&def)],
        ),
        // A statement failing at its first token.
        (
            true,
            RdfFormat::Turtle,
            "] <http://e/s> . <http://e/a> <http://e/b> <http://e/c> .",
            vec![None, Some(&abc)],
        ),
    ];
    for (guard, format, document, expected) in cases {
        let parser = || RdfParser::from_format(format).recovering();
        let bytes = || Pieces(document.as_bytes().chunks(1).map(Ok).collect());
        for (path, items) in [
            ("slice", drain(parser().for_slice(document.as_bytes()))),
            ("reader", drain(parser().for_reader(document.as_bytes()))),
            ("byte reader", drain(parser().for_reader(bytes()))),
        ] {
            let got: Vec<Option<&Quad>> = items.iter().map(|item| item.as_ref().ok()).collect();
            // No quad from inside the failed statement, and all of the output as expected.
            let inside = got
                .iter()
                .flatten()
                .any(|quad| quad.subject.to_string() == "<urn:u>");
            let outcome = match inside {
                true => Err(format!("a quad from inside the failed statement: {got:?}")),
                false => same(got, expected.clone()),
            };
            table.row(guard, ("recovering", path, document), outcome);
        }
        // Strict: one error, then the end.
        let strict = drain(RdfParser::from_format(format).for_slice(document.as_bytes()));
        let strict_expected: Vec<bool> = match expected.first() {
            Some(Some(_)) => vec![true, false],
            _ => vec![false],
        };
        table.row(
            true,
            ("strict", document),
            shaped(&strict, &strict_expected),
        );
    }
    table.done();
}

/// `document` parsed by `parser` from a slice, a reader, and a reader giving one byte at a
/// time.
fn on_three_paths(
    parser: impl Fn() -> RdfParser,
    document: &'static [u8],
) -> [(&'static str, Vec<Result<Quad, String>>); 3] {
    let bytes = || Pieces(document.chunks(1).map(Ok).collect());
    [
        ("slice", drain(parser().for_slice(document))),
        ("reader", drain(parser().for_reader(document))),
        ("byte reader", drain(parser().for_reader(bytes()))),
    ]
}

/// An opener refused by the nesting limit is in the document all the same: it stays open
/// while the failed statement's rest is skipped, so a `.` before its `]` ends nothing and the
/// two `)` close nothing (fix check r3; `RdfParser::recovering`). Turtle and a TriG block, on
/// all three paths.
#[test]
fn an_opener_refused_by_the_nesting_limit_stays_open() {
    let after = triple("urn:after", "urn:p", iri("urn:o"));
    let after_in_g = Quad::new(iri("urn:after"), iri("urn:p"), iri("urn:o"), iri("urn:g"));
    let mut table = Table::default();
    for (format, document, expected) in [
        (
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> [ ) ) . <urn:u> <urn:v> <urn:w> . ] ] . <urn:after> <urn:p> <urn:o> .",
            &after,
        ),
        (
            RdfFormat::TriG,
            "<urn:g> { <urn:s> <urn:p> [ <urn:q> [ ) ) . <urn:u> <urn:v> <urn:w> . ] ] . <urn:after> <urn:p> <urn:o> . }",
            &after_in_g,
        ),
    ] {
        let parser = || {
            RdfParser::from_format(format)
                .with_max_nesting(1)
                .recovering()
        };
        for (path, items) in on_three_paths(parser, document.as_bytes()) {
            let got: Vec<Option<&Quad>> = items.iter().map(|item| item.as_ref().ok()).collect();
            let inside = got
                .iter()
                .flatten()
                .any(|quad| quad.subject.to_string() == "<urn:u>");
            let outcome = match inside {
                true => Err(format!("a quad from inside the failed statement: {got:?}")),
                false => same(got, vec![None, Some(expected)]),
            };
            table.row(false, (path, document), outcome);
        }
    }
    table.done();
}

/// Each of the ten delimiters, the two braces and `.`, at every token position of statements
/// using every grammar routine that opens one (objects, lists, triple terms, reified triples,
/// annotations, reifiers, `[]`, datatypes, directives, TriG graph names), replacing the token
/// there or put before it, then a valid statement: recovering runs to the end on all three
/// paths without a panic. In a debug build `resync` checks that the brackets noted open when
/// a statement fails agree with those it took, and that it ended at its failing token exactly
/// when that is a `.` taken with nothing open, so a grammar site that noted nothing fails
/// here (fix check r3; P, r5). A guard: before, there was no such check.
#[test]
fn recovery_knows_every_bracket_open_wherever_a_statement_fails() {
    let mut table = Table::default();
    for (format, document) in crate::recovery_inputs::sweep() {
        let document: &'static str = Box::leak(document.into_boxed_str());
        let parser = || RdfParser::from_format(format).recovering();
        let bytes = || Pieces(document.as_bytes().chunks(1).map(Ok).collect());
        // Every item to the end: the delimiter can come after the seed's own quads (fix
        // check r3's sol-loop, S1). A parser that doesn't end fails the row.
        const BOUND: usize = 1_000;
        let outcome = std::panic::catch_unwind(|| {
            [
                parser().for_slice(document.as_bytes()).take(BOUND).count(),
                parser().for_reader(document.as_bytes()).take(BOUND).count(),
                parser().for_reader(bytes()).take(BOUND).count(),
            ]
        })
        .map_err(|panic| {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()));
            format!("panicked: {message:?}")
        })
        .and_then(|counts| match counts.iter().all(|&n| n < BOUND) {
            true => Ok(()),
            false => Err(format!("no end within {BOUND} items: {counts:?}")),
        });
        table.row(true, (format, document), outcome);
    }
    table.done();
}

/// Recovery over large malformed input gives the expected output on every path (sol-loop r2
/// S2–S4: unmatched closers, many escaped quotes after a missing one, one long malformed
/// string from a reader giving a byte at a time). The work stays linear by construction (a
/// count per bracket pair; no search after one has reached the end; reading at least as much
/// again before searching again); these rows check the output. Guards: they held before.
/// The escaped quotes are fewer: before, each was searched to the end again, so the parent's
/// red run would take quadratic time on them.
#[test]
fn recovery_over_large_malformed_input() {
    let after = triple("urn:after", "urn:p", iri("urn:o"));
    let n = 100_000;
    let quotes = 5_000;
    let documents: [(&str, Vec<u8>); 3] = [
        (
            "unmatched closers",
            [
                b"<urn:s> % ".to_vec(),
                b"[".repeat(n),
                b")".repeat(n),
                b"]".repeat(n),
                b" . <urn:after> <urn:p> <urn:o> .".to_vec(),
            ]
            .concat(),
        ),
        (
            "escaped quotes after a missing one",
            [
                b"<urn:s> <urn:p> [ <urn:q> \" ".to_vec(),
                b"\\\" ".repeat(quotes),
                b"] . <urn:after> <urn:p> <urn:o> .".to_vec(),
            ]
            .concat(),
        ),
        (
            "a long malformed string",
            [
                b"<urn:s> <urn:p> [ <urn:q> \"\n".to_vec(),
                b"a".repeat(n),
                b"\"] . <urn:after> <urn:p> <urn:o> .".to_vec(),
            ]
            .concat(),
        ),
    ];
    let mut table = Table::default();
    for (case, document) in documents {
        let document: &'static [u8] = Box::leak(document.into_boxed_slice());
        let parser = || RdfParser::from_format(RdfFormat::Turtle).recovering();
        let bytes = || Pieces(document.chunks(1).map(Ok).collect());
        for (path, items) in [
            ("slice", drain(parser().for_slice(document))),
            ("reader", drain(parser().for_reader(document))),
            ("byte reader", drain(parser().for_reader(bytes()))),
        ] {
            let got: Vec<Option<&Quad>> = items.iter().map(|item| item.as_ref().ok()).collect();
            table.row(true, (case, path), same(got, vec![None, Some(&after)]));
        }
    }
    table.done();
}

/// A read error met while recovery reads ahead for a malformed string's end loses nothing:
/// the string's end and the statement after it were read before the error, so they come
/// first, then the read error (`RdfParser::recovering`: a read error always stops). Where
/// recovery itself can't read on, the read error comes right after the syntax error
/// (sol-loop r2 S5).
#[test]
fn a_read_error_during_recovery_comes_after_what_was_read() {
    let first: &'static [u8] = concat!(
        "<urn:s> <urn:p> \"a\n",
        "aaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaa"
    )
    .as_bytes();
    let rest: &'static [u8] = b"\" . <urn:after> <urn:p> <urn:o> . ";
    let after = triple("urn:after", "urn:p", iri("urn:o"));
    // An item as syntax error, read error, or quad.
    let kind = |item: &Result<Quad, RdfParseError>| match item {
        Err(RdfParseError::Syntax(_)) => "syntax".to_owned(),
        Err(RdfParseError::Io(_)) => "read".to_owned(),
        Ok(quad) => format!("{quad:?}"),
    };
    let quad = format!("{after:?}");
    let mut table = Table::default();
    for (case, pieces, expected) in [
        (
            "the end read before the error",
            vec![Ok(first), Ok(rest), Err(io::ErrorKind::Other)],
            vec!["syntax", quad.as_str(), "read"],
        ),
        (
            "the end not read",
            vec![Ok(first), Err(io::ErrorKind::Other)],
            vec!["syntax", "read"],
        ),
    ] {
        let items: Vec<String> = RdfParser::from_format(RdfFormat::Turtle)
            .recovering()
            .for_reader(Pieces(pieces.into()))
            .take(9)
            .map(|item| kind(&item))
            .collect();
        let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
        table.row(false, case, same(items, expected));
    }
    table.done();
}

/// A `[` that never closes: no later `.` can be shown to be outside it, so recovery
/// reports everything after the statement's start as its skipped statement, one error, and
/// emits nothing (fix check r2; bulk load quarantines that span). Slice, reader, and a
/// reader giving one byte at a time. A guard: it held before.
#[test]
fn an_unclosed_bracket_reports_the_rest_as_its_statement() {
    let document =
        "<urn:s> <urn:p> [ <urn:q> . <urn:a> <urn:b> <urn:c> . <urn:after> <urn:p> <urn:o> .";
    let parser = || RdfParser::from_format(RdfFormat::Turtle).recovering();
    let bytes = || Pieces(document.as_bytes().chunks(1).map(Ok).collect());
    let mut table = Table::default();
    for (path, items) in [
        (
            "slice",
            parser()
                .for_slice(document.as_bytes())
                .take(9)
                .collect::<Vec<_>>(),
        ),
        (
            "reader",
            parser().for_reader(document.as_bytes()).take(9).collect(),
        ),
        (
            "byte reader",
            parser().for_reader(bytes()).take(9).collect(),
        ),
    ] {
        let outcome = match items.as_slice() {
            [Err(RdfParseError::Syntax(error))] => match error.statement() {
                Some(statement) => same(
                    (statement.span.start.offset, statement.span.end.offset),
                    (0, document.len() as u64),
                ),
                None => Err("no skipped statement".to_owned()),
            },
            other => Err(format!("want one syntax error, got {other:?}")),
        };
        table.row(true, ("unclosed to the end", path), outcome);
    }
    table.done();
}

// --- A read error always stops ------------------------------------------------------------

/// A reader that gives its pieces in order: bytes, or a failure of a kind.
struct Pieces(VecDeque<Result<&'static [u8], io::ErrorKind>>);

impl Read for Pieces {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let Some(piece) = self.0.front_mut() else {
            return Ok(0);
        };
        match piece {
            Err(kind) => {
                let kind = *kind;
                self.0.pop_front();
                Err(io::Error::new(kind, "a test fault"))
            }
            Ok(bytes) => {
                let n = bytes.len().min(out.len());
                out[..n].copy_from_slice(&bytes[..n]);
                *bytes = &bytes[n..];
                if bytes.is_empty() {
                    self.0.pop_front();
                }
                Ok(n)
            }
        }
    }
}

/// A reader that fails on every call.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("broken"))
    }
}

#[test]
fn a_read_error_ends_the_document() {
    use io::ErrorKind::{Interrupted, Other};
    const AB: &[u8] = b"<urn:a> <urn:p> <urn:b> .\n";
    let mut table = Table::default();
    for format in [
        RdfFormat::NTriples,
        RdfFormat::NQuads,
        RdfFormat::Turtle,
        RdfFormat::TriG,
    ] {
        // Turtle and TriG reported a failure met inside a statement already.
        let statement_wise = matches!(format, RdfFormat::Turtle | RdfFormat::TriG);
        for recovering in [false, true] {
            let parser = || recovering_if(RdfParser::from_format(format), recovering);
            for (guard, what, pieces, expected) in [
                // A failure, then a reader that would go on: the document has ended.
                (
                    false,
                    "fails, then reads",
                    vec![Err(Other), Ok(AB)],
                    vec![false],
                ),
                // A line, then a failure inside the next line.
                (
                    statement_wise,
                    "reads, fails inside a statement, reads",
                    vec![
                        Ok(AB),
                        Ok(&b"<urn:c> <urn:p>"[..]),
                        Err(Other),
                        Ok(&b" <urn:d> .\n"[..]),
                    ],
                    vec![true, false],
                ),
                // An interrupted read is retried.
                (
                    true,
                    "interrupted, then reads",
                    vec![Err(Interrupted), Ok(AB)],
                    vec![true],
                ),
            ] {
                let items = drain(parser().for_reader(Pieces(pieces.into())));
                table.row(guard, (format, recovering, what), shaped(&items, &expected));
            }
            let items = drain(parser().for_reader(Broken));
            table.row(
                statement_wise,
                (format, recovering, "always fails"),
                shaped(&items, &[false]),
            );
        }
    }
    table.done();
}

// --- An error the parser's own layer finds stops a strict format --------------------------

#[test]
fn a_statement_in_a_named_graph_stops_a_strict_format() {
    let default = triple("urn:a", "urn:p", iri("urn:b"));
    let trig = "<urn:g> { <urn:s> <urn:p> <urn:o> . } <urn:a> <urn:p> <urn:b> .";
    let mut table = Table::default();
    for (guard, format, document, recovering, expected) in [
        (false, RdfFormat::TriG, trig, false, vec![false]),
        (true, RdfFormat::TriG, trig, true, vec![false, true]),
        // A line-based format goes on after a bad line anyway.
        (
            true,
            RdfFormat::NQuads,
            "<urn:s> <urn:p> <urn:o> <urn:g> .\n<urn:a> <urn:p> <urn:b> .\n",
            false,
            vec![false, true],
        ),
    ] {
        let parser = recovering_if(
            RdfParser::from_format(format).without_named_graphs(),
            recovering,
        );
        let items = drain(parser.for_slice(document.as_bytes()));
        let outcome = shaped(&items, &expected).and_then(|()| match items.get(1) {
            Some(Ok(quad)) => same(quad, &default),
            _ => Ok(()),
        });
        table.row(guard, (format, recovering), outcome);
    }
    table.done();
}

#[test]
fn n3_that_isnt_rdf_stops_the_document() {
    let items = drain(
        RdfParser::from_format(RdfFormat::N3)
            .for_slice(b"?s <urn:p> <urn:o> . <urn:a> <urn:p> <urn:b> ."),
    );
    assert_eq!(shaped(&items, &[false]), Ok(()));
}

// --- Binary RDF: a variable-length integer holds 32 bits ----------------------------------

#[test]
fn a_binary_value_id_beyond_32_bits_is_an_error() {
    let mut document = b"BRDF".to_vec();
    document.extend_from_slice(&2i32.to_be_bytes());
    document.push(5);
    document.extend_from_slice(b"UTF-8");
    // Value 0 is the IRI urn:a.
    document.extend_from_slice(&[3, 0, 1, 5]);
    document.extend_from_slice(b"urn:a");
    // A statement of value 0 thrice, the first id written in five bytes (a valid
    // encoding), then one whose subject id is 2^32: bits a 32-bit integer can't hold.
    for subject in [
        &[0x80, 0x80, 0x80, 0x80, 0x00][..],
        &[0x80, 0x80, 0x80, 0x80, 0x10],
    ] {
        document.push(1);
        document.push(6);
        document.extend_from_slice(subject);
        document.extend_from_slice(&[6, 0, 6, 0, 0]);
    }
    document.push(127);
    let a = triple("urn:a", "urn:a", iri("urn:a"));
    let mut table = Table::default();
    for reader in [false, true] {
        let parser = RdfParser::from_format(RdfFormat::BinaryRdf);
        let items = match reader {
            false => drain(parser.for_slice(&document)),
            true => drain(parser.for_reader(document.as_slice())),
        };
        let outcome =
            shaped(&items, &[true, false]).and_then(|()| same(items[0].as_ref().ok(), Some(&a)));
        table.row(false, ("reader", reader), outcome);
    }
    table.done();
}

// --- RDF/XML: references in attribute values and text -------------------------------------

/// A document whose `http://e/s` has the property attribute `e:p="{value}"`, after the
/// entity declarations `entities`.
fn property_attribute(entities: &str, value: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF [ {entities} ]>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="http://e/">
  <rdf:Description rdf:about="http://e/s" e:p="{value}"/>
</rdf:RDF>"#
    )
}

fn rdf_xml(document: &str) -> Vec<Result<Quad, String>> {
    drain(RdfParser::from_format(RdfFormat::RdfXml).for_slice(document.as_bytes()))
}

fn literal(value: &str) -> Quad {
    triple(
        "http://e/s",
        "http://e/p",
        Literal::new_simple_literal(value),
    )
}

#[test]
fn an_attribute_value_is_normalised_as_xml_says() {
    let mut table = Table::default();
    for (guard, entities, value, expected) in [
        // A character reference adds its character, whitespace or not.
        (false, "", "a&#xA;b", "a\nb"),
        (false, "", "a&#x9;b&#xD;c&#32;d", "a\tb\rc d"),
        // Whitespace characters written in the value become spaces; CR LF is one break.
        (true, "", "a\nb\tc", "a b c"),
        (false, "", "a\r\nb", "a b"),
        // An entity adds its replacement text, normalised in turn: the LF its declaration
        // made becomes a space, a reference its declaration made is read on inclusion.
        (true, r#"<!ENTITY nl "&#xA;">"#, "a&nl;b", "a b"),
        (false, r#"<!ENTITY ref "&#38;#xA;">"#, "a&ref;b", "a\nb"),
        // An ampersand a reference made is text, never a reference again.
        (true, "", "a&amp;#xA;b", "a&#xA;b"),
        (true, r#"<!ENTITY amp2 "&amp;#xA;">"#, "a&amp2;b", "a&#xA;b"),
        // An entity's value may name one declared after it.
        (
            false,
            r#"<!ENTITY a "x&b;y"> <!ENTITY b "z">"#,
            "&a;",
            "xzy",
        ),
    ] {
        let document = property_attribute(entities, value);
        let outcome = same(rdf_xml(&document), vec![Ok(literal(expected))]);
        table.row(guard, (entities, value), outcome);
    }
    table.done();
}

#[test]
fn a_reference_xml_does_not_allow_is_an_error() {
    let mut table = Table::default();
    for (entities, value) in [
        ("", "&nope;"),
        ("", "&#0;"),
        ("", "&#xFFFE;"),
        ("", "&#xZZ;"),
        ("", "&#X41;"),
        ("", "&#;"),
        (r#"<!ENTITY a "&a;">"#, "&a;"),
        (r#"<!ENTITY a "&b;"> <!ENTITY b "&a;">"#, "&a;"),
        (r#"<!ENTITY a "&nope;">"#, "&a;"),
    ] {
        let document = property_attribute(entities, value);
        table.row(
            false,
            (entities, value),
            shaped(&rdf_xml(&document), &[false]),
        );
    }
    table.done();
}

#[test]
fn an_entity_in_text_is_its_replacement_text() {
    let mut table = Table::default();
    let document = r#"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF [ <!ENTITY a "x&b;&#xA;y"> <!ENTITY b "z"> <!ENTITY e "http://e/"> ]>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="&e;">
  <rdf:Description rdf:about="&e;s"><e:p>&a;</e:p></rdf:Description>
</rdf:RDF>"#;
    let outcome = same(rdf_xml(document), vec![Ok(literal("xz\ny"))]);
    table.row(false, "&a; naming &b; declared after it", outcome);
    // A character reference in text names a legal XML character, as in an attribute
    // (quick-xml refused `&#0;` and `&#X41;` before already).
    for (guard, reference) in [(false, "&#xFFFE;"), (true, "&#0;"), (true, "&#X41;")] {
        let document = format!(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="http://e/">
  <rdf:Description rdf:about="http://e/s"><e:p>{reference}</e:p></rdf:Description>
</rdf:RDF>"#
        );
        table.row(guard, reference, shaped(&rdf_xml(&document), &[false]));
    }
    table.done();
}

#[test]
fn a_bad_namespace_declaration_skips_its_node_element_when_recovering() {
    let document = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:e="http://e/">
  <rdf:Description rdf:about="http://e/bad" xmlns:x="&nope;"><e:p><rdf:Description rdf:about="http://e/inner"/></e:p></rdf:Description>
  <rdf:Description rdf:about="http://e/s" e:p="ok"/>
</rdf:RDF>"#;
    let items = drain(
        RdfParser::from_format(RdfFormat::RdfXml)
            .recovering()
            .for_slice(document.as_bytes()),
    );
    let outcome = shaped(&items, &[false, true])
        .and_then(|()| same(items[1].as_ref().ok(), Some(&literal("ok"))));
    assert_eq!(outcome, Ok(()));
}
