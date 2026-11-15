//! OWL 2 EL classification of N-Triples files (`classify`), for the ORE workload and
//! checks against ELK.
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example classify -- --out hierarchy.tsv [--threads N] input.nt...
//! ```
//!
//! `--threads N` saturates on N workers (`classify_parallel`); the default, 1, runs the
//! sequential saturation. `--repeat N` classifies N times in the process (for profilers
//! and warm timings); the output and profile are the last run's. `--within` runs the
//! bounded entry (`classify_parallel_within`) under a stop that never fires and the
//! process's memory limit, as the store runs it, and prints how often it polled the stop
//! (`polls N`, the last run's) before the profile.
//!
//! Writes one `sub<TAB>super` line per subsumption between named classes (IRIs without
//! brackets, sorted), `sub<TAB>owl:Nothing` for unsatisfiable classes,
//! `owl:Thing<TAB>super` for classes equivalent to `owl:Thing`, and prints the time and
//! what was skipped, then, as its last line, the profile (`nrese_reasoner::classify::Profile`)
//! as `name=value` pairs: the phases in milliseconds (reading and writing included) and the
//! counters, which the DL lab records (`benches/reasoning/dl/nrese.py`). An omitted class
//! or property axiom returns an error before writing a taxonomy, so callers cannot record
//! a partial one as complete; assertions about individuals are not read
//! (`nrese_reasoner::classify`).

use std::collections::BTreeMap;
use std::io::{BufRead, BufWriter, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use nrese_reasoner::classify::{
    Classification, Profile, classify_profiled, classify_profiled_within,
};
use nrese_reasoner::eval::OperationStop;
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
    let (mut out, mut inputs, mut threads, mut repeat) = (None, Vec::new(), 1, 1usize);
    let mut within = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next(),
            "--threads" => {
                threads = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or("--threads N")?
            }
            "--repeat" => {
                repeat = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or("--repeat N")?
            }
            "--within" => within = true,
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
    let names = vocabulary.clone();
    let named = |id: u64| names.text(id).starts_with('<');
    // The stop's caller is asked once per poll that finds it unlatched: its count is the polls.
    let polls = AtomicU64::new(0);
    let caller = || {
        polls.fetch_add(1, Relaxed);
        false
    };
    let run = |vocabulary: &mut LocalVocabulary| -> Result<(Classification, Profile), String> {
        polls.store(0, Relaxed);
        if within {
            let stop = OperationStop::new(&caller);
            classify_profiled_within(&triples, vocabulary, &named, threads, &stop)
                .map_err(|stopped| stopped.to_string())
        } else {
            Ok(classify_profiled(&triples, vocabulary, &named, threads))
        }
    };
    for _ in 1..repeat {
        run(&mut vocabulary.clone())?;
    }
    let started = Instant::now();
    let (result, profile) = run(&mut vocabulary)?;
    let elapsed = started.elapsed();
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, kind) in &result.skipped {
        *skipped.entry(kind).or_default() += 1;
    }
    if !skipped.is_empty() {
        return Err(format!("EL classification omitted input: {skipped:?}").into());
    }
    let started = Instant::now();
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
    let write = started.elapsed();
    eprintln!(
        "{} triples read in {:.3} s; classified in {:.3} s: {} subsumptions, {} unsatisfiable; skipped {:?}",
        triples.len(),
        load.as_secs_f64(),
        elapsed.as_secs_f64(),
        result.subsumptions.len(),
        result.unsatisfiable.len(),
        skipped
    );
    if within {
        eprintln!("polls {}", polls.load(Relaxed));
    }
    let ms = |d: std::time::Duration| format!("{:.1}", d.as_secs_f64() * 1000.0);
    eprintln!(
        "profile read={} normalise={} prepare={} saturate={} assemble={} write={} concepts={} contexts={} subsumers={} links={} conclusions={} duplicates={} threads={}",
        ms(load),
        ms(profile.normalise),
        ms(profile.prepare),
        ms(profile.saturate),
        ms(profile.assemble),
        ms(write),
        profile.concepts,
        profile.contexts,
        profile.subsumers,
        profile.links,
        profile.conclusions,
        profile.duplicates,
        profile.threads
    );
    Ok(())
}
