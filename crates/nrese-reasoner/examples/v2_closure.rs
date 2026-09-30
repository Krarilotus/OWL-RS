//! Reasoner v2 closure of N-Triples files, for checks against the reasoning benchmark's
//! oracle (`benches/reasoning/compare_inferred.py`).
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example v2_closure -- \
//!     [--ruleset owl2-rl|rdfs] [--executor batch|naive] --out derived.nt input.nt...
//! ```
//!
//! The batch executor is the default; `naive` runs the reference evaluator (the oracle).

use std::io::{BufRead, BufWriter, Write};
use std::time::Instant;

use nrese_reasoner::v2::batch::{self, Schema};
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
    let mut naive = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ruleset" => {
                ruleset = match args.next().as_deref() {
                    Some(name) if Ruleset::from_name(name).is_some() => {
                        Ruleset::from_name(name).expect("checked")
                    }
                    other => return Err(format!("unknown ruleset {other:?}").into()),
                }
            }
            "--executor" => {
                naive = match args.next().as_deref() {
                    Some("naive") => true,
                    Some("batch") => false,
                    other => return Err(format!("unknown executor {other:?}").into()),
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
    let schema = Schema::owl(&mut vocabulary);
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
    // Count each asserted triple once, as the other systems do.
    facts.sort_unstable();
    facts.dedup();
    let load = started.elapsed();
    let started = Instant::now();
    let (derived, violations, diagnostics, rounds) = if naive {
        let closure = materialise(&facts, &rules, lists.as_ref());
        let mut derived: Vec<_> = closure.derived.into_iter().collect();
        derived.sort_unstable();
        (
            derived,
            closure.violations.len(),
            closure.diagnostics,
            closure.rounds,
        )
    } else {
        let result = batch::materialise(&facts, &rules, lists.as_ref(), &schema);
        eprintln!(
            "ground rules {} | transitive {} | {:?}",
            result.ground_rules, result.transitive, result.phases
        );
        (
            result.derived,
            result.violations.len(),
            result.diagnostics,
            result.rounds,
        )
    };
    let reasoning = started.elapsed();
    let mut writer = BufWriter::new(std::fs::File::create(&out)?);
    for [s, p, o] in &derived {
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
        "{} {}: asserted {} | load {:.2} s | closure {:.2} s in {} rounds | derived {} | violations {}",
        ruleset.name(),
        if naive { "naive" } else { "batch" },
        facts.len(),
        load.as_secs_f64(),
        reasoning.as_secs_f64(),
        rounds,
        derived.len(),
        violations
    );
    for diagnostic in diagnostics.iter().take(5) {
        let text = diagnostic.describe(&|id| vocabulary.text(id).to_owned());
        println!("  diagnostic: {text}");
    }
    Ok(())
}
