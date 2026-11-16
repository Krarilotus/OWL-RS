//! Turtle and TriG recovery keeps offsets, never the text it skips: a statement whose
//! bracket never closes near the top of an 8 MiB document is skipped to its end within a
//! bound far below the document. Measured (`nrese_exec::measure`, every thread counts), so
//! this file is a test binary of its own.

use nrese_exec::measure;
use nrese_rdf_io::{RdfFormat, RdfParser};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

#[test]
fn an_unclosed_bracket_is_skipped_to_the_end_within_a_bound() {
    if !measure::alone() {
        return;
    }
    let mut document = b"<urn:s> <urn:p> [ <urn:q> \"open\" .\n".to_vec();
    for k in 0.. {
        if document.len() >= 8 << 20 {
            break;
        }
        document.extend_from_slice(format!("<urn:s{k}> <urn:p> \"v{k}\" .\n").as_bytes());
    }
    for format in [RdfFormat::Turtle, RdfFormat::TriG] {
        for recovering in [false, true] {
            let parser = match recovering {
                true => RdfParser::from_format(format).recovering(),
                false => RdfParser::from_format(format),
            };
            let (results, peak) =
                measure::measure(|| parser.for_reader(document.as_slice()).collect::<Vec<_>>());
            let row = format!("{format:?} (recovering {recovering})");
            let [Err(error)] = results.as_slice() else {
                panic!("{row}: one error and nothing else, got {results:?}");
            };
            assert!(error.ends_document(), "{row}: {error}");
            let cut = error.to_string().contains("ends inside a statement");
            assert_eq!(
                cut, recovering,
                "{row}: only recovery reads to the end: {error}"
            );
            assert!(peak < 1 << 20, "{row}: {peak} bytes held");
        }
    }
}
