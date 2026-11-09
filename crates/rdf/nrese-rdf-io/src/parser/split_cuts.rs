//! Each cut the skimmer offers, alone (design note 2026-11-08-split-recovery §4): a Turtle
//! or TriG document in two chunks, cut there, parsed recovering, gives what the sequential
//! recovering parse gives, field by field (`oracle`). The integration tests check every cut
//! together and two or three parts through the public API (`split_contract_tests`); a cut
//! alone needs the skim's cut list, which is this crate's.

use super::*;
use crate::oracle::{BOUND, items};

/// The first cut of `document`'s skim whose two chunks don't give the sequential parse's
/// items, and where they differ.
fn each_cut_agrees(format: RdfFormat, document: &str) -> Result<(), String> {
    let parser = RdfParser::from_format(format).recovering();
    let bytes = document.as_bytes();
    let want = items(parser.clone().for_slice(bytes));
    let mut skimmer = Skimmer::new(bytes.len() as u64, bytes.len(), format == RdfFormat::TriG);
    skimmer.feed(bytes);
    let skim = skimmer.finish();
    for &cut in &skim.cuts {
        let chunks = parser
            .turtle_chunks(bytes, &skim, &[cut])
            .map_err(|error| format!("cut {cut:?} refused: {error}"))?;
        let got = items(chunks.into_iter().flat_map(|chunk| chunk.take(BOUND)));
        if got != want {
            let at = got.iter().zip(&want).take_while(|(a, b)| a == b).count();
            return Err(format!(
                "cut {cut:?}: item {at} is {:?}, sequential {:?} ({} items, sequential {})",
                got.get(at),
                want.get(at),
                got.len(),
                want.len()
            ));
        }
    }
    Ok(())
}

/// Every document of the recovery sweep, and the package review's witnesses (R1-F1: a form
/// feed, and a name the lexer ends where a byte-wise run goes on; R1-F3: a column after a
/// character of two bytes).
#[test]
fn each_cut_alone_is_where_the_sequential_parse_starts_a_statement() {
    let witnesses = [
        (
            RdfFormat::Turtle,
            "\x0c@prefix ex: <urn:> .\nex:s ex:p ex:o .\n",
        ),
        (
            RdfFormat::Turtle,
            "PREFIX\u{D7} <urn:a> @prefix ex: <urn:> .\nex:s ex:p ex:o .\n",
        ),
        (
            RdfFormat::Turtle,
            "<urn:s> <urn:p> \"\u{E9}\" . <urn:s> <urn:p> ] .\n",
        ),
        (
            RdfFormat::Turtle,
            "<urn:s> <urn:p> [ <urn:q> . ) . <urn:u> <urn:v> <urn:w> . ] . <urn:a> <urn:b> <urn:c> .\n",
        ),
    ];
    let documents = witnesses
        .into_iter()
        .map(|(format, document)| (format, document.to_owned()))
        .chain(crate::recovery_inputs::sweep());
    let failed: Vec<String> = documents
        .filter_map(|(format, document)| {
            each_cut_agrees(format, &document)
                .err()
                .map(|why| format!("{format:?} {document:?}: {why}"))
        })
        .collect();
    assert!(
        failed.is_empty(),
        "{} document(s):\n{}",
        failed.len(),
        failed.join("\n")
    );
}
