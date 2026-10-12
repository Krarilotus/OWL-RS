//! The work of a commit that splits an `owl:sameAs` class (B3, ADR-0011): the fast suite's
//! `eq-giant-split` data in memory (a class of `size` members, two chains joined by one
//! bridge, each member with a value of its own, beside `groups` classes of 10), materialised
//! with every copy, then the bridge deleted through the delta executor. Prints the update's
//! phases and counts beside a full rematerialisation, and checks the result against it.
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example split_work -- [--size N] [--groups N]
//!     [--plan dred|recompute] [--repeat N]
//! ```

use std::collections::HashSet;
use std::time::Instant;

use nrese_reasoner::batch::{self, Schema};
use nrese_reasoner::delta::policy::{Plan, Policy};
use nrese_reasoner::delta::{MemoryBase, Rules, program, update_with};
use nrese_reasoner::ir::{OWL, Triple, Vocabulary};
use nrese_reasoner::lists::ListVocabulary;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut size, mut groups, mut policy) = (500u64, 5000u64, Policy::default());
    let mut repeat = 1usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next();
        let number = value.as_deref().and_then(|n| n.parse().ok());
        match arg.as_str() {
            "--size" => size = number.unwrap_or(size),
            "--groups" => groups = number.unwrap_or(groups),
            "--repeat" => repeat = number.map_or(repeat, |n| n as usize),
            "--plan" => {
                policy = match value.as_deref() {
                    Some("dred") => Policy::forcing(Plan::Dred),
                    Some("recompute") => Policy::forcing(Plan::Recompute),
                    _ => Policy::default(),
                }
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary)?;
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let same_as = vocabulary.iri(&format!("{OWL}sameAs"));
    let val = vocabulary.iri("http://example.com/val");
    let mut asserted: Vec<Triple> = Vec::new();
    let half = size / 2;
    let member = |v: &mut LocalVocabulary, j: u64| v.iri(&format!("http://example.com/g{j}"));
    for j in 0..size {
        let x = member(&mut vocabulary, j);
        let value = vocabulary.literal(&format!("g{j}"), "http://www.w3.org/2001/XMLSchema#string");
        asserted.push([x, val, value]);
        if j + 1 < size && j + 1 != half {
            let next = member(&mut vocabulary, j + 1);
            asserted.push([x, same_as, next]);
        }
    }
    let bridge = [
        member(&mut vocabulary, half - 1),
        same_as,
        member(&mut vocabulary, half),
    ];
    asserted.push(bridge);
    for g in 0..groups {
        for j in 0..10 {
            let x = vocabulary.iri(&format!("http://example.com/s{g}_{j}"));
            let value = vocabulary.literal(
                &format!("s{g}_{j}"),
                "http://www.w3.org/2001/XMLSchema#string",
            );
            asserted.push([x, val, value]);
            if j < 9 {
                let next = vocabulary.iri(&format!("http://example.com/s{g}_{}", j + 1));
                asserted.push([x, same_as, next]);
            }
        }
    }
    asserted.sort_unstable();
    asserted.dedup();
    let clock = Instant::now();
    let closure = batch::materialise(&asserted, &rules, Some(&lists), &schema);
    println!(
        "materialised {} asserted, {} derived in {:.3} s",
        asserted.len(),
        closure.derived.len(),
        clock.elapsed().as_secs_f64()
    );
    let compiled = Rules {
        rules: &rules,
        lists: Some(&lists),
        schema: &schema,
    };
    let base_before = MemoryBase::new(&asserted, &closure.derived);
    let cache = program(&base_before, compiled);
    drop(base_before);
    let after: Vec<Triple> = asserted.iter().copied().filter(|&t| t != bridge).collect();
    let stack: Vec<Triple> = closure.derived.clone();
    let base = MemoryBase::new(&after, &stack);
    // The update's time is the median of `repeat` runs.
    let mut times = Vec::new();
    for _ in 1..repeat {
        let clock = Instant::now();
        let _ = update_with(
            &base,
            &[],
            &[bridge],
            compiled,
            Some(&cache),
            nrese_reasoner::eval::NEVER,
            policy,
        );
        times.push(clock.elapsed().as_secs_f64());
    }
    let clock = Instant::now();
    let result = update_with(
        &base,
        &[],
        &[bridge],
        compiled,
        Some(&cache),
        nrese_reasoner::eval::NEVER,
        policy,
    )
    .map_err(|_| "interrupted")?;
    times.push(clock.elapsed().as_secs_f64());
    times.sort_by(f64::total_cmp);
    let took = times[times.len() / 2];
    let names = ["program", "overdelete", "rederive", "insert", "consistency"];
    let phases: Vec<String> = names
        .iter()
        .zip(result.phases)
        .map(|(name, d)| format!("{name} {:.3}", d.as_secs_f64()))
        .collect();
    println!(
        "split: {took:.3} s | {} | rounds {} | removed {} inserted {}",
        phases.join(" "),
        result.rounds,
        result.remove.len(),
        result.insert.len()
    );
    println!("policy: {:?}", result.policy);
    let clock = Instant::now();
    let fresh = batch::materialise(&after, &rules, Some(&lists), &schema);
    println!(
        "rematerialised: {} derived in {:.3} s",
        fresh.derived.len(),
        clock.elapsed().as_secs_f64()
    );
    let removal: HashSet<Triple> = result.remove.iter().copied().collect();
    let mut maintained: Vec<Triple> = stack
        .into_iter()
        .filter(|t| !removal.contains(t))
        .chain(result.insert)
        .filter(|t| after.binary_search(t).is_err())
        .collect();
    maintained.sort_unstable();
    maintained.dedup();
    let mut expected: Vec<Triple> = fresh
        .derived
        .into_iter()
        .filter(|t| after.binary_search(t).is_err())
        .collect();
    expected.sort_unstable();
    expected.dedup();
    println!("equal to rematerialisation: {}", maintained == expected);
    Ok(())
}
