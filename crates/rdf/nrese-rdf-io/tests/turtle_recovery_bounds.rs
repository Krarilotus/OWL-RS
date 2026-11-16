//! Turtle and TriG recovery holds a bounded window, never the text it skips, near the top of
//! a large document:
//! - a statement whose bracket never closes is skipped to the end, holding offsets only: the
//!   document ends inside it;
//! - a short string broken by a line break, with no closing quote after it, is searched for
//!   one across at most `MAX_STATEMENT_TEXT` bytes; past them the error ends the document.
//!
//! Without recovery the first error stops at once. Measured (`nrese_exec::measure`, every
//! thread counts), so this file is a test binary of its own.

use nrese_exec::measure;
use nrese_rdf_io::{RdfFormat, RdfParser};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// `first`, then `statement(k)` for k = 0, 1, … up to `size` bytes.
fn document(first: &str, statement: impl Fn(usize) -> String, size: usize) -> Vec<u8> {
    let mut document = first.as_bytes().to_vec();
    for k in 0.. {
        if document.len() >= size {
            return document;
        }
        document.extend_from_slice(statement(k).as_bytes());
    }
    unreachable!()
}

#[test]
fn recovery_near_the_top_holds_a_bounded_window() {
    if !measure::alone() {
        return;
    }
    let bracket = document(
        "<urn:s> <urn:p> [ <urn:q> \"open\" .\n",
        |k| format!("<urn:s{k}> <urn:p> \"v{k}\" .\n"),
        8 << 20,
    );
    // No quote after the stray one: the search for its closing quote finds none.
    let quote = document(
        "<urn:s> <urn:p> \"stray .\n",
        |k| format!("<urn:s{k}> <urn:p> <urn:o{k}> .\n"),
        16 << 20,
    );
    // The document, whether recovery reaches its end, and the most held: offsets only, or
    // the 1 MiB window in a buffer that doubles (2 MiB, 3 while it reallocates).
    for (name, document, to_the_end, bound) in [
        ("unclosed bracket", bracket, true, 1 << 20),
        ("stray quote", quote, false, 4 << 20),
    ] {
        for format in [RdfFormat::Turtle, RdfFormat::TriG] {
            for recovering in [false, true] {
                let parser = match recovering {
                    true => RdfParser::from_format(format).recovering(),
                    false => RdfParser::from_format(format),
                };
                let (results, peak) =
                    measure::measure(|| parser.for_reader(document.as_slice()).collect::<Vec<_>>());
                let row = format!("{name}, {format:?} (recovering {recovering})");
                let [Err(error)] = results.as_slice() else {
                    let n = results.len();
                    panic!("{row}: one error and nothing else, got {n} results");
                };
                let message = error.to_string();
                assert!(error.ends_document(), "{row}: {message}");
                assert!(message.starts_with("line 1, "), "{row}: {message}");
                let cut = message.contains("ends inside a statement");
                assert_eq!(cut, to_the_end && recovering, "{row}: {message}");
                assert!(peak < bound, "{row}: {peak} bytes held");
            }
        }
    }
}
