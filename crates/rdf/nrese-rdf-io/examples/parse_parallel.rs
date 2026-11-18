//! Parses a Turtle, TriG, N-Triples or N-Quads file the way the parallel loader does:
//! split into chunks, each parsed on its own thread. Prints the statements per chunk and
//! the first error, so that a file a parallel load rejects can be checked without a
//! store.
//!
//! ```text
//! cargo run --release -p nrese-rdf-io --example parse_parallel -- graph.ttl [parts]
//! ```

use std::path::PathBuf;
use std::time::Instant;

use nrese_rdf_io::{RdfFormat, RdfParser};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or("usage: parse_parallel FILE [parts]")?);
    let parts: usize = args.next().map_or(Ok(4), |n| n.parse())?;
    let format = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(RdfFormat::from_extension)
        .ok_or("unknown file extension")?;
    let started = Instant::now();
    let parsers = RdfParser::from_format(format).split_file_for_parallel_parsing(&path, parts)?;
    let chunks = parsers.len();
    let results: Vec<(usize, Option<String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = parsers
            .into_iter()
            .map(|parser| {
                scope.spawn(move || {
                    let mut count = 0;
                    for quad in parser {
                        match quad {
                            Ok(_) => count += 1,
                            Err(error) => return (count, Some(error.to_string())),
                        }
                    }
                    (count, None)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a parser thread panicked"))
            .collect()
    });
    let total: usize = results.iter().map(|(count, _)| count).sum();
    for (i, (count, error)) in results.iter().enumerate() {
        match error {
            Some(error) => println!("chunk {i}: {count} statements, then: {error}"),
            None => println!("chunk {i}: {count} statements"),
        }
    }
    println!(
        "{total} statements in {chunks} chunks, {:.1} s; {} chunk(s) failed",
        started.elapsed().as_secs_f64(),
        results.iter().filter(|(_, e)| e.is_some()).count()
    );
    Ok(())
}
