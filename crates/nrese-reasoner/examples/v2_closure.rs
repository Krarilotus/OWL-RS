//! Reasoner v2 closure of N-Triples files, for checks against the reasoning benchmark's
//! oracle (`benches/reasoning/compare_inferred.py`).
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example v2_closure -- \
//!     [--ruleset owl2-rl|rdfs] --out derived.nt input.nt...
//! ```
//!
//! Uses the naive reference evaluator for now; the batch executor (R2) takes over once it
//! exists, with the naive one kept as its oracle.

use std::io::{BufRead, BufWriter, Write};
use std::time::Instant;

use nrese_reasoner::v2::lists::ListVocabulary;
use nrese_reasoner::v2::naive::materialise;
use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_reasoner::v2::testing::LocalVocabulary;

/// Splits an N-Triples line into its three terms (in N-Triples syntax).
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
    let (mut ruleset, mut out, mut inputs) = (Ruleset::Owl2Rl, None, Vec::new());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ruleset" => {
                ruleset = match args.next().as_deref() {
                    Some("rdfs") => Ruleset::Rdfs,
                    Some("owl2-rl") => Ruleset::Owl2Rl,
                    other => return Err(format!("unknown ruleset {other:?}").into()),
                }
            }
            "--out" => out = args.next(),
            _ => inputs.push(arg),
        }
    }
    let out = out.ok_or("--out is required")?;
    let mut vocabulary = LocalVocabulary::default();
    let rules = ruleset.rules(&mut vocabulary)?;
    let lists = ruleset
        .has_list_rules()
        .then(|| ListVocabulary::new(&mut vocabulary));
    let started = Instant::now();
    let mut facts = Vec::new();
    for path in &inputs {
        for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
            let line = line?;
            if let Some([s, p, o]) = split(&line) {
                facts.push([vocabulary.term(s), vocabulary.term(p), vocabulary.term(o)]);
            }
        }
    }
    let load = started.elapsed();
    let started = Instant::now();
    let closure = materialise(&facts, &rules, lists.as_ref());
    let reasoning = started.elapsed();
    let mut writer = BufWriter::new(std::fs::File::create(&out)?);
    let mut derived: Vec<_> = closure.derived.iter().collect();
    derived.sort_unstable();
    for [s, p, o] in derived {
        writeln!(
            writer,
            "{} {} {} .",
            vocabulary.text(*s),
            vocabulary.text(*p),
            vocabulary.text(*o)
        )?;
    }
    writer.flush()?;
    println!(
        "{}: asserted {} | load {:.2} s | closure {:.2} s in {} rounds | derived {} | violations {}",
        ruleset.name(),
        facts.len(),
        load.as_secs_f64(),
        reasoning.as_secs_f64(),
        closure.rounds,
        closure.derived.len(),
        closure.violations.len()
    );
    for diagnostic in closure.diagnostics.iter().take(5) {
        // Diagnostics name terms by id; show their text.
        let words: Vec<String> = diagnostic
            .split(' ')
            .map(|word| {
                let digits = word.trim_matches(|c: char| !c.is_ascii_digit());
                match digits.parse::<u64>() {
                    Ok(id) if !digits.is_empty() && word.len() - digits.len() <= 2 => {
                        word.replace(digits, vocabulary.text(id))
                    }
                    _ => word.to_owned(),
                }
            })
            .collect();
        println!("  diagnostic: {}", words.join(" "));
    }
    Ok(())
}
