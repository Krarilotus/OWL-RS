//! OWL 2 EL classification of N-Triples files (`classify`), for the ORE workload and
//! checks against ELK.
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example classify -- --out hierarchy.tsv [--threads N] input.nt...
//! ```
//!
//! `--threads N` saturates on N workers (`classify_parallel`); the default, 1, runs the
//! sequential saturation.
//!
//! Writes one `sub<TAB>super` line per subsumption between named classes (IRIs without
//! brackets, sorted), `sub<TAB>owl:Nothing` for unsatisfiable classes,
//! `owl:Thing<TAB>super` for classes equivalent to `owl:Thing`, and prints the time and
//! what was skipped.

use std::collections::BTreeMap;
use std::io::{BufRead, BufWriter, Write};
use std::time::Instant;

use nrese_reasoner::classify::classify_parallel;
use nrese_reasoner::vocabulary::LocalVocabulary;

fn split(line: &str) -> Option<[&str; 3]> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (subject, rest) = line.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (predicate, rest) = rest.split_once(char::is_whitespace)?;
    let object = rest.trim().strip_suffix('.')?.trim_end();
    Some([subject, predicate, object])
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut out, mut inputs, mut threads) = (None, Vec::new(), 1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next(),
            "--threads" => {
                threads = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or("--threads N")?
            }
            _ => inputs.push(arg),
        }
    }
    let out = out.ok_or("--out is required")?;
    let mut vocabulary = LocalVocabulary::default();
    let started = Instant::now();
    let mut triples = Vec::new();
    for path in &inputs {
        for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
            let line = line?;
            if let Some([s, p, o]) = split(&line) {
                triples.push([vocabulary.term(s), vocabulary.term(p), vocabulary.term(o)]);
            }
        }
    }
    let load = started.elapsed();
    let started = Instant::now();
    let names = vocabulary.clone();
    let result = classify_parallel(
        &triples,
        &mut vocabulary,
        &|id| names.text(id).starts_with('<'),
        threads,
    );
    let elapsed = started.elapsed();
    let text = |id: u64| vocabulary.text(id).trim_matches(['<', '>']).to_owned();
    let mut file = BufWriter::new(std::fs::File::create(&out)?);
    for &(sub, sup) in &result.subsumptions {
        writeln!(file, "{}\t{}", text(sub), text(sup))?;
    }
    for &class in &result.unsatisfiable {
        writeln!(file, "{}\towl:Nothing", text(class))?;
    }
    for &class in &result.top {
        writeln!(file, "owl:Thing\t{}", text(class))?;
    }
    file.flush()?;
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, kind) in &result.skipped {
        *skipped.entry(kind).or_default() += 1;
    }
    eprintln!(
        "{} triples read in {:.3} s; classified in {:.3} s: {} subsumptions, {} unsatisfiable; skipped {:?}",
        triples.len(),
        load.as_secs_f64(),
        elapsed.as_secs_f64(),
        result.subsumptions.len(),
        result.unsatisfiable.len(),
        skipped
    );
    Ok(())
}
