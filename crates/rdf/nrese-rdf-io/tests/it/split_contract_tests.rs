//! Parallel parsing gives what a sequential parse gives: a Turtle or TriG document split
//! into chunks (`RdfParser::split_slice_for_parallel_parsing`) and parsed recovering, the
//! chunks' items in order, equals the sequential recovering parse of the whole document
//! (`RdfParser::recovering`: a failed statement is skipped through its end, and nothing in
//! it is emitted; docs/design/quarantine.md, quarantine isolation). Bulk load splits every
//! Turtle and TriG file so. A cut is only right where the sequential parse starts a
//! statement; with `parts` the document's length, the skimmer takes every cut it would
//! ever take, so each one is checked.

use nrese_rdf_io::{RdfFormat, RdfParseError, RdfParser};

use crate::table::{Table, same};

/// An item as text: a quad, or an error with its position and skipped statement.
fn text<T: std::fmt::Debug>(item: Result<T, RdfParseError>) -> String {
    match item {
        Ok(quad) => format!("{quad:?}"),
        Err(error) => format!("error: {error}"),
    }
}

/// Each parser's items, at most `BOUND` each (a parser that doesn't end shows as that).
const BOUND: usize = 10_000;

/// The items with every blank node renamed by its first occurrence (`b0`, `b1`, ...): the
/// generated ones (`[]`, lists) are named from a key random per parser, so only their
/// pattern is the same in a chunked and a sequential parse.
fn canonical(items: Vec<String>) -> Vec<String> {
    const ID: &str = "BlankNode { id: \"";
    let mut names = std::collections::HashMap::new();
    items
        .into_iter()
        .map(|item| {
            let mut out = String::new();
            let mut rest = item.as_str();
            while let Some(at) = rest.find(ID) {
                let (before, after) = rest.split_at(at + ID.len());
                out.push_str(before);
                let end = after.find('"').unwrap_or(after.len());
                let next = names.len();
                let name = names.entry(after[..end].to_owned()).or_insert(next);
                out.push_str(&format!("b{name}"));
                rest = &after[end..];
            }
            out.push_str(rest);
            out
        })
        .collect()
}

fn sequential(format: RdfFormat, document: &str) -> Vec<String> {
    canonical(
        RdfParser::from_format(format)
            .recovering()
            .for_slice(document.as_bytes())
            .take(BOUND)
            .map(text)
            .collect(),
    )
}

/// The chunks' items in order, or why the split was refused.
fn chunked(format: RdfFormat, document: &str, parts: usize) -> Result<Vec<String>, String> {
    let chunks = RdfParser::from_format(format)
        .recovering()
        .split_slice_for_parallel_parsing(document.as_bytes(), parts)
        .map_err(|error| format!("the split was refused: {error}"))?;
    Ok(canonical(
        chunks
            .into_iter()
            .flat_map(|chunk| chunk.take(BOUND).map(text))
            .collect(),
    ))
}

/// Chunked equals sequential, with every cut the skimmer takes, and with two and three
/// parts.
fn agrees(format: RdfFormat, document: &str) -> Result<(), String> {
    let want = sequential(format, document);
    for parts in [document.len().max(1), 2, 3] {
        let got = chunked(format, document, parts)?;
        same(got, want.clone()).map_err(|why| format!("{parts} parts: {why}"))?;
    }
    Ok(())
}

/// How many chunks the skimmer cuts `document` into when it may cut anywhere.
fn chunks(format: RdfFormat, document: &str) -> Result<usize, String> {
    RdfParser::from_format(format)
        .recovering()
        .split_slice_for_parallel_parsing(document.as_bytes(), document.len())
        .map(|chunks| chunks.len())
        .map_err(|error| format!("the split was refused: {error}"))
}

const AFTER: &str = " <urn:after> <urn:p> <urn:o> .";

/// The witnesses of where a cut fell inside a failed statement, or a header made a load
/// fail (design note 2026-11-08-split-recovery, W1–W4, W6), and the parser's recovery
/// rows they come from (P2-A1b). Guards held before.
#[test]
fn a_chunked_recovering_parse_is_the_sequential_one() {
    let mut table = Table::default();
    let rows: Vec<(bool, RdfFormat, String)> = vec![
        // W1: a closer of another kind closes nothing (the parser's R2-11 and A1B-R3-F1).
        (
            false,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> [ <urn:q> . ) . <urn:u> <urn:v> <urn:w> . ] .{AFTER}"),
        ),
        // The nested form: the scalar depth's two `]` keep it inside, so no wrong cut (a guard).
        (
            true,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> [ <urn:q> . [ ) . <urn:u> <urn:v> <urn:w> . ] ] .{AFTER}"),
        ),
        (
            false,
            RdfFormat::TriG,
            format!(
                "<urn:g> {{ <urn:s> <urn:p> [ <urn:q> . ) . <urn:u> <urn:v> <urn:w> . ] .{AFTER} }}"
            ),
        ),
        // W2: a lone `>` or `|` is a byte the lexer can't read, not a closer.
        (
            false,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> [ <urn:q> > . <urn:u> <urn:v> <urn:w> . ] .{AFTER}"),
        ),
        (
            true,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> [ <urn:q> | . <urn:u> <urn:v> <urn:w> . ] .{AFTER}"),
        ),
        // W3: `\` outside a name escapes nothing: the `(` after it opens.
        (
            false,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> <urn:o> \\( . <urn:u> <urn:v> <urn:w> . ) .{AFTER}"),
        ),
        // W4: in Turtle a `}` ends no block: the failing `{` stays open to its `}`, and the
        // statement goes on to its `.`.
        (
            false,
            RdfFormat::Turtle,
            format!(
                "<urn:s> <urn:p> {{ <urn:q> . <urn:u> <urn:v> <urn:w> . }} <urn:x> <urn:y> <urn:z> .{AFTER}"
            ),
        ),
        // W6: a directive that fails is quarantined like any statement; the chunks after it
        // are read with what the sequential parse has in force there.
        (
            false,
            RdfFormat::Turtle,
            format!("@prefix ex: <a b> .\n<urn:a> <urn:b> <urn:c> .{AFTER}"),
        ),
        (
            false,
            RdfFormat::Turtle,
            format!("PREFIX ex: <a b>\n<urn:a> <urn:b> <urn:c> .{AFTER}"),
        ),
        (
            false,
            RdfFormat::Turtle,
            format!("@base <a b> .\n<urn:a> <urn:b> <urn:c> .{AFTER}"),
        ),
        (
            false,
            RdfFormat::Turtle,
            format!("@version 12 .\n<urn:a> <urn:b> <urn:c> .{AFTER}"),
        ),
        // The parser's rows: a failing brace counts like a skipped one (R4-1, R4-3).
        (
            true,
            RdfFormat::TriG,
            format!(
                "<urn:g> {{ <urn:s> <urn:p> }} <http://e/a> <http://e/b> <http://e/c> .{AFTER}"
            ),
        ),
        // A VERSION whose short string has a raw line break: recovery skips the string as a
        // malformed token and the directive ends at its `.`, so no cut after the string
        // (sol-loop r1 S2).
        (
            false,
            RdfFormat::Turtle,
            format!("VERSION 12 \"a\nb\"\n<urn:s> <urn:p> <urn:o> .{AFTER}"),
        ),
        // An escaped line break is part of a valid short string: that VERSION ends with it
        // (sol-loop r2 S5).
        (
            false,
            RdfFormat::Turtle,
            format!("VERSION \"a\\\nb\"\n<urn:s> <urn:p> \"o\" .{AFTER}"),
        ),
        // A failed VERSION that ends with its string leaves no string behind: an `@` after it
        // starts a directive, as after one that parses (sol-loop r3 S6).
        (
            false,
            RdfFormat::Turtle,
            "VERSION 12 \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\\nb\"\n@prefix : <urn:> .\n:s :p :o ."
                .to_owned(),
        ),
        // A byte order mark before the first directive: the parser reads past it, so the
        // directive is in force in every chunk (sol-loop r1 S4; before this package too).
        (
            false,
            RdfFormat::Turtle,
            format!("\u{FEFF}@prefix ex: <http://e/> .\nex:a ex:b ex:c .\nex:d ex:e ex:f .{AFTER}"),
        ),
        // Valid names with escapes (`\(`, `\)`, `\.`) are names: guards.
        (
            true,
            RdfFormat::Turtle,
            format!(
                "@prefix ex: <http://e/> .\nex:Foo_\\(bar\\) ex:p ex:o\\. .\nex:a ex:p ex:b .{AFTER}"
            ),
        ),
    ];
    for (guard, format, document) in rows {
        table.row(guard, (format, &document), agrees(format, &document));
    }
    table.done();
}

/// The same for every document of the parser's recovery sweep (a delimiter at or before
/// every token of statements using every grammar routine that opens one): the invariant,
/// each cut where the sequential parse starts a statement.
#[test]
fn every_cut_of_the_recovery_sweep_is_a_statement_start() {
    let mut table = Table::default();
    for (format, document) in crate::recovery_inputs::sweep() {
        table.row(false, (format, &document), agrees(format, &document));
    }
    table.done();
}

/// Where recovery ends a failed statement, the skimmer cuts again (no parallelism lost for
/// the rest of the file): a TriG `}` ends its block, open brackets or not (the parser's
/// R4-4), and a stray closer opens nothing. Valid documents keep their cuts (guards): one
/// at each statement's end, the last chunk the final line break.
#[test]
fn the_skimmer_cuts_where_recovery_resumes() {
    let mut table = Table::default();
    let three = "<urn:a> <urn:b> <urn:c> .\n<urn:d> <urn:e> <urn:f> .\n<urn:h> <urn:i> <urn:j> .\n";
    let rows: Vec<(bool, RdfFormat, String, usize)> = vec![
        (
            false,
            RdfFormat::TriG,
            format!("<urn:g> {{ <urn:s> <urn:p> [ <urn:q> <urn:o> }}\n{three}"),
            5,
        ),
        (
            false,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> 1, ] .\n{three}"),
            5,
        ),
        (
            true,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> 1 .\n{three}"),
            5,
        ),
        (
            true,
            RdfFormat::TriG,
            format!("<urn:g> {{ <urn:s> <urn:p> [ <urn:q> <urn:o> ] }}\n{three}"),
            5,
        ),
        // A byte order mark before a TriG block's head: still a block (sol-loop r1 S4).
        (
            false,
            RdfFormat::TriG,
            "\u{FEFF}<urn:g> { <urn:s> <urn:p> <urn:o> . }\n<urn:h> { <urn:a> <urn:b> <urn:c> . }\n"
                .to_owned(),
            3,
        ),
    ];
    for (guard, format, document, want) in rows {
        let outcome = chunks(format, &document).and_then(|got| same(got, want));
        table.row(guard, (format, &document), outcome);
    }
    // Every seed of the sweep, valid, then one statement: a cut after each.
    for (format, seed) in crate::recovery_inputs::SEEDS {
        let document = format!("{seed}{AFTER}\n");
        let outcome = chunks(format, &document).and_then(|got| same(got, 3));
        table.row(true, (format, &document), outcome);
    }
    table.done();
}
