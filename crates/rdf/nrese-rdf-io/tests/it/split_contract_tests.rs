//! Parallel parsing gives what a sequential parse gives: a Turtle or TriG document split
//! into chunks (`RdfParser::split_slice_for_parallel_parsing`) and parsed recovering, the
//! chunks' items in order, equals the sequential recovering parse of the whole document
//! (`RdfParser::recovering`: a failed statement is skipped through its end, and nothing in
//! it is emitted; docs/design/quarantine.md, quarantine isolation). Bulk load splits every
//! Turtle and TriG file so. A cut is only right where the sequential parse starts a
//! statement; with `parts` the document's length, the skimmer takes every cut it would
//! ever take, so each one is checked (each cut alone: the crate's `parser::split_cuts`).
//! Items compare field by field (`oracle`): quads, and errors with their location and the
//! statement they skipped.

use std::sync::atomic::{AtomicUsize, Ordering};

use nrese_rdf_io::{RdfFormat, RdfParser};

use crate::oracle::{BOUND, items};
use crate::table::{Table, same};

fn sequential(format: RdfFormat, document: &str) -> Vec<String> {
    items(
        RdfParser::from_format(format)
            .recovering()
            .for_slice(document.as_bytes()),
    )
}

/// The chunks' items in order, or why the split was refused.
fn chunked(format: RdfFormat, document: &str, parts: usize) -> Result<Vec<String>, String> {
    let chunks = RdfParser::from_format(format)
        .recovering()
        .split_slice_for_parallel_parsing(document.as_bytes(), parts)
        .map_err(|error| format!("the split was refused: {error}"))?;
    Ok(items(
        chunks.into_iter().flat_map(|chunk| chunk.take(BOUND)),
    ))
}

/// The same through a file (`split_file_for_parallel_parsing`, which skims it in pieces).
fn chunked_file(format: RdfFormat, document: &str, parts: usize) -> Result<Vec<String>, String> {
    static FILES: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "nrese-rdf-io-split-contract-{}-{}.{}",
        std::process::id(),
        FILES.fetch_add(1, Ordering::Relaxed),
        format.file_extension()
    ));
    std::fs::write(&path, document).map_err(|error| error.to_string())?;
    let chunks = RdfParser::from_format(format)
        .recovering()
        .split_file_for_parallel_parsing(&path, parts);
    let got = chunks
        .map(|chunks| items(chunks.into_iter().flat_map(|chunk| chunk.take(BOUND))))
        .map_err(|error| format!("the file's split was refused: {error}"));
    std::fs::remove_file(&path).map_err(|error| error.to_string())?;
    got
}

/// Chunked equals sequential, with every cut the skimmer takes, and with two and three
/// parts; from a slice and from a file.
fn agrees(format: RdfFormat, document: &str) -> Result<(), String> {
    let want = sequential(format, document);
    for parts in [document.len().max(1), 2, 3] {
        let got = chunked(format, document, parts)?;
        same(got, want.clone()).map_err(|why| format!("{parts} parts: {why}"))?;
        let got = chunked_file(format, document, parts)?;
        same(got, want.clone()).map_err(|why| format!("a file in {parts} parts: {why}"))?;
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
        // A form feed is no whitespace to the lexer (Turtle's WS is space, tab, CR, LF): the
        // statement starting there fails, the `@prefix` with it, and no chunk has `ex:` in
        // force (package review r1, R1-F1).
        (
            false,
            RdfFormat::Turtle,
            format!("\x0c@prefix ex: <urn:> .\nex:s ex:p ex:o .{AFTER}"),
        ),
        // A name the lexer ends where the skimmer's run went on (`×` is no name character):
        // `PREFIX` is a keyword there, the directive fails and ends after its IRI, and the
        // `@prefix` after it is in force in the later chunks.
        (
            false,
            RdfFormat::Turtle,
            format!("PREFIX\u{D7} <urn:a> @prefix ex: <urn:> .\nex:s ex:p ex:o .{AFTER}"),
        ),
        // A cut in a line after a character of two bytes: errors later in that line have the
        // same column, in characters (R1-F3).
        (
            false,
            RdfFormat::Turtle,
            format!("<urn:s> <urn:p> \"\u{E9}\" . <urn:s> <urn:p> ] .{AFTER}"),
        ),
        (
            false,
            RdfFormat::TriG,
            format!("<urn:g> {{ <urn:s> <urn:p> \"\u{E9}\" . }} <urn:s> <urn:p> ] .{AFTER}"),
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

/// The outcomes of R1-F1's and R1-F3's witnesses, stated, beside their agreement above:
/// - a form feed before a `@prefix` is a byte the lexer can't read (Turtle's WS: space,
///   tab, CR, LF; RDF 1.1 Turtle §6.5), so that statement fails with its directive, and
///   `ex:s ex:p ex:o` fails on its undeclared prefix: no quad, chunked or not;
/// - the `]` of `<urn:s> <urn:p> "é" . <urn:s> <urn:p> ] .` is its 39th character (column
///   38, from 0; `TextPosition` counts characters), the 40th byte: the error is there,
///   chunked or not.
#[test]
fn a_split_keeps_what_recovery_drops_out_and_its_error_columns() {
    let mut table = Table::default();
    let quads = |items: &[String]| {
        items
            .iter()
            .filter(|item| !item.starts_with("error"))
            .count()
    };
    let document = "\x0c@prefix ex: <urn:> .\nex:s ex:p ex:o .\n";
    let want = sequential(RdfFormat::Turtle, document);
    table.row(false, ("sequential", document), same(quads(&want), 0));
    for parts in [document.len(), 2] {
        let got = chunked(RdfFormat::Turtle, document, parts).map(|got| quads(&got));
        table.row(false, (parts, document), got.and_then(|got| same(got, 0)));
    }
    let document = "<urn:s> <urn:p> \"\u{E9}\" . <urn:s> <urn:p> ] .";
    let column = |items: Result<Vec<String>, String>| {
        items.map(|items| {
            items
                .iter()
                .find(|item| item.starts_with("error"))
                .map(|error| {
                    error.contains(" at TextPosition { line: 0, column: 38, offset: 39 }..")
                })
        })
    };
    let want = Ok(Some(true));
    table.row(
        false,
        ("sequential", document),
        same(
            column(Ok(sequential(RdfFormat::Turtle, document))),
            want.clone(),
        ),
    );
    for parts in [document.len(), 2] {
        let got = column(chunked(RdfFormat::Turtle, document, parts));
        table.row(false, (parts, document), same(got, want.clone()));
        let got = column(chunked_file(RdfFormat::Turtle, document, parts));
        table.row(false, ("file", parts, document), same(got, want.clone()));
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
