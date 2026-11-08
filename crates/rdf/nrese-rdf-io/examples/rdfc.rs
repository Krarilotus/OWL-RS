//! Writes an N-Triples or N-Quads file's RDFC-1.0 canonical N-Quads (W3C, 21 May 2024) to
//! standard output: the document whose SHA-256 pins a dataset whose blank node labels its
//! maker picks afresh each time (benches/datasets.toml `rdfc_sha256`: Jena's riot labels
//! univ-bench.nt's blank nodes at random, so its bytes differ on every make, 7 Nov 2026).
//!
//! ```text
//! cargo run --release -p nrese-rdf-io --example rdfc -- univ-bench.nt | sha256sum
//! ```

use std::io::Write;
use std::path::PathBuf;

use nrese_rdf::Quad;
use nrese_rdf::rdfc::Rdfc10;
use nrese_rdf_io::{RdfFormat, RdfParser};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: rdfc FILE.nt|FILE.nq")?,
    );
    match path.extension().and_then(|e| e.to_str()) {
        Some("nt" | "nq") => {}
        other => return Err(format!("rdfc: .nt or .nq, not {other:?}").into()),
    }
    let input = std::fs::read(&path)?;
    // An N-Triples line is an N-Quads line in the default graph: one parser reads both.
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&input)
        .collect::<Result<_, _>>()?;
    let canonical = Rdfc10::new().canonicalize(quads.iter().map(Quad::as_ref))?;
    std::io::stdout()
        .lock()
        .write_all(canonical.to_nquads().as_bytes())?;
    Ok(())
}
