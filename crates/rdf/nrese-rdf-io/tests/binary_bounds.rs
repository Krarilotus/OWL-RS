//! Binary RDF allocates what a document holds, never what its records declare: a long
//! declared string and a large declared value id cost no more than their bytes. Measured
//! (`nrese_exec::measure`, every thread counts), so this file is a test binary of its own.

use nrese_exec::measure;
use nrese_rdf_io::{RdfFormat, RdfParser};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// A version 2 header in UTF-8, then `records`.
fn document(records: &[u8]) -> Vec<u8> {
    let mut bytes = b"BRDF".to_vec();
    bytes.extend_from_slice(&2i32.to_be_bytes());
    bytes.push(5);
    bytes.extend_from_slice(b"UTF-8");
    bytes.extend_from_slice(records);
    bytes
}

fn varint(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[test]
fn declared_sizes_allocate_only_what_arrives() {
    if !measure::alone() {
        return;
    }
    // A plain literal declared 256 MiB long, with 16 bytes of it present.
    let mut long_string = vec![3, 0, 3];
    varint(1 << 28, &mut long_string);
    long_string.extend_from_slice(&[b'x'; 16]);
    // A value declared at id 2^20 (null), then the end: a valid, empty document.
    let mut high_id = vec![3];
    varint(1 << 20, &mut high_id);
    high_id.extend_from_slice(&[0, 127]);
    for (name, records, ok) in [
        ("long string", long_string, false),
        ("high id", high_id, true),
    ] {
        let bytes = document(&records);
        for reader in [false, true] {
            let parser = RdfParser::from_format(RdfFormat::BinaryRdf);
            let (outcome, peak) = measure::measure(|| {
                let quads: Result<Vec<_>, _> = match reader {
                    false => parser.for_slice(&bytes).collect(),
                    true => parser.for_reader(bytes.as_slice()).collect(),
                };
                quads.map(|quads| quads.len())
            });
            assert_eq!(outcome.is_ok(), ok, "{name} (reader {reader}): {outcome:?}");
            assert!(
                peak < 1 << 20,
                "{name} (reader {reader}): {peak} bytes held"
            );
        }
    }
}
