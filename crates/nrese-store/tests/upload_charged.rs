//! Guard of the quads an upload must keep (`nrese_store::parse_payload`, a repository's
//! configuration): every byte they own (slots, IRIs, literals, triple terms) is charged to
//! the request's envelope before it is allocated, and a document past the envelope is
//! refused as a memory error. Measured against the envelope (`nrese_exec::measure`), so
//! this file is a test binary of its own.

use nrese_exec::{Envelope, measure};
use nrese_store::GraphResultFormat;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// What reading may hold uncharged at once: the parser's buffers for one statement, which
/// are its own, not the quads kept.
const SCRATCH: usize = 64 << 10;

/// `lines` statements of 1,000-character IRIs, every fourth with a triple term as object
/// and a literal in it; about 3 KB kept per statement.
fn long_terms(lines: usize) -> Vec<u8> {
    let iri =
        |i: usize, what: &str| format!("<http://example.com/{}/{what}{i}>", "x".repeat(1_000));
    let mut document = String::new();
    for i in 0..lines {
        let object = match i % 4 {
            0 => format!(
                "<<( {} {} \"{}\"@en )>>",
                iri(i, "a"),
                iri(i, "b"),
                "y".repeat(1_000)
            ),
            _ => iri(i, "o"),
        };
        document.push_str(&format!("{} {} {object} .\n", iri(i, "s"), iri(i, "p")));
    }
    document.into_bytes()
}

#[test]
fn kept_quads_are_charged_before_they_are_allocated() {
    if !measure::alone() {
        return;
    }
    let document = long_terms(2_000);
    let envelope = Envelope::new(Some(256 << 20), None);
    let (quads, excess) = measure::measure_within(&envelope, || {
        nrese_store::parse_payload(GraphResultFormat::NTriples, None, &document, &envelope)
    });
    let quads = quads.expect("within the envelope");
    assert_eq!(quads.len(), 2_000);
    assert!(
        envelope.used() >= 6 << 20,
        "the quads' terms are charged: {} KiB",
        envelope.used() >> 10
    );
    assert!(
        excess <= SCRATCH,
        "{excess} bytes held past their charge, more than the parser's {SCRATCH}"
    );
    drop(quads);
    assert_eq!(envelope.used(), 0, "dropped, they hold nothing");
}

#[test]
fn quads_past_the_envelope_are_refused_before_they_are_copied() {
    if !measure::alone() {
        return;
    }
    let document = long_terms(2_000);
    let envelope = Envelope::new(Some(1 << 20), None);
    let (outcome, peak) = measure::measure(|| {
        nrese_store::parse_payload(GraphResultFormat::NTriples, None, &document, &envelope)
    });
    let error = outcome.expect_err("past the envelope");
    assert!(
        error.to_string().contains("memory"),
        "a memory refusal: {error}"
    );
    assert!(
        peak <= (1 << 20) + SCRATCH,
        "{} KiB held at the peak: past the envelope's 1 MiB",
        peak >> 10
    );
    assert_eq!(envelope.used(), 0);
}
