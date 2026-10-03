//! Support graph sets over a materialised closure: the time and the sets per fact, with
//! the input's statements spread over N graphs by subject.
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example graph_sets -- [--graphs N] [--cap K] input.nt...
//! ```

use std::io::BufRead;
use std::time::Instant;

use nrese_reasoner::v2::batch::{self, Schema};
use nrese_reasoner::v2::delta::{MemoryBase, Rules, program};
use nrese_reasoner::v2::ir::Triple;
use nrese_reasoner::v2::lists::ListVocabulary;
use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_reasoner::v2::vocabulary::LocalVocabulary;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut graphs, mut cap, mut inputs) = (8u32, 4usize, Vec::new());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--graphs" => graphs = args.next().and_then(|n| n.parse().ok()).unwrap_or(8),
            "--cap" => cap = args.next().and_then(|n| n.parse().ok()).unwrap_or(4),
            _ => inputs.push(arg),
        }
    }
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary)?;
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let mut asserted: Vec<Triple> = Vec::new();
    for path in &inputs {
        for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
            let line = line?;
            let line = line.trim();
            let Some((s, rest)) = line.split_once(' ') else {
                continue;
            };
            let Some((p, o)) = rest.trim_start().split_once(' ') else {
                continue;
            };
            let Some(o) = o.trim().strip_suffix('.') else {
                continue;
            };
            asserted.push([
                vocabulary.term(s),
                vocabulary.term(p),
                vocabulary.term(o.trim_end()),
            ]);
        }
    }
    asserted.sort_unstable();
    asserted.dedup();
    let started = Instant::now();
    let inferred = batch::materialise(&asserted, &rules, Some(&lists), &schema).derived;
    let materialised = started.elapsed();
    let compiled = Rules {
        rules: &rules,
        lists: Some(&lists),
        schema: &schema,
    };
    let ground = program(&MemoryBase::new(&asserted, &inferred), compiled);
    let mut all = asserted.clone();
    all.extend(&inferred);
    all.sort_unstable();
    let graph_of = |fact: Triple| -> Vec<u32> {
        if asserted.binary_search(&fact).is_ok() {
            vec![(fact[0] % u64::from(graphs)) as u32]
        } else {
            Vec::new()
        }
    };
    let started = Instant::now();
    let sets = nrese_reasoner::v2::graph_sets::support_sets(&all, &ground, &graph_of, cap);
    let elapsed = started.elapsed();
    let per_fact: usize = inferred
        .iter()
        .map(|f| sets.get(f).map_or(0, Vec::len))
        .sum();
    let without = inferred.iter().filter(|f| !sets.contains_key(*f)).count();
    println!(
        "{} asserted, {} inferred; materialisation {:.0} ms; support sets {:.0} ms ({:.2} per inferred fact, {} without)",
        asserted.len(),
        inferred.len(),
        materialised.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0,
        per_fact as f64 / inferred.len().max(1) as f64,
        without
    );
    Ok(())
}
