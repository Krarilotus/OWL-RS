//! The rule work of a materialisation, in counts: bindings per round and per rule,
//! membership probes, rounds, the working set's bytes, and (with `--equality`) the outer
//! passes of equality by representatives. Counts don't depend on the machine or the
//! thread count, so they decide before timings do (the investigation of 6 October 2026,
//! §5.1 and §5.2). A fingerprint of the closure compares it with another build's.
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example rule_work -- \
//!     [--ruleset owl2-rl] [--naive] [--runs N] [--threads N] [--by-rule] \
//!     [--equality] [--cascade DEPTH] input.nt...
//! ```
//!
//! - `--naive` runs the reference evaluator too and checks that the closures agree.
//! - `--runs` repeats the batch materialisation (times per run, counts once).
//! - `--equality` materialises with equality by representatives
//!   (`representatives::materialise_until`) instead of the batch executor alone.
//! - `--cascade DEPTH` adds a functional property and `2 * DEPTH` facts over the input's
//!   typed individuals whose `sameAs` consequences merge classes one after another:
//!   depth d needs d merges, each found only after the previous one.

use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::time::Instant;

use nrese_reasoner::batch::{self, Materialisation, Schema};
use nrese_reasoner::ir::{OWL, RDF, Vocabulary};
use nrese_reasoner::lists::ListVocabulary;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

type Triple = [u64; 3];

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

/// An order-independent fingerprint of `facts` over their texts, so builds that intern
/// differently still compare.
fn fingerprint(vocabulary: &LocalVocabulary, facts: &[Triple]) -> u64 {
    facts
        .iter()
        .map(|fact| {
            let mut hasher = std::hash::DefaultHasher::new();
            for &term in fact {
                vocabulary.text(term).hash(&mut hasher);
            }
            hasher.finish()
        })
        .fold(0u64, u64::wrapping_add)
}

fn grouped(facts: &[Triple]) -> Vec<(u64, Vec<(u64, u64)>)> {
    let mut sorted = facts.to_vec();
    sorted.sort_unstable_by_key(|&[s, p, o]| (p, o, s));
    sorted.dedup();
    let mut groups: Vec<(u64, Vec<(u64, u64)>)> = Vec::new();
    for [s, p, o] in sorted {
        match groups.last_mut() {
            Some((last, pairs)) if *last == p => pairs.push((o, s)),
            _ => groups.push((p, vec![(o, s)])),
        }
    }
    groups
}

/// The facts of a `sameAs` cascade of `depth` over typed individuals of `facts`.
fn cascade(vocabulary: &mut LocalVocabulary, facts: &[Triple], depth: usize) -> Vec<Triple> {
    let rdf_type = vocabulary.iri(&format!("{RDF}type"));
    let mut individuals: Vec<u64> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &[s, p, _] in facts {
        if p == rdf_type && seen.insert(s) {
            individuals.push(s);
            if individuals.len() > 2 * depth {
                break;
            }
        }
    }
    assert!(individuals.len() > 2 * depth, "too few typed individuals");
    let f = vocabulary.iri("http://example.com/cascade#functional");
    let functional = vocabulary.iri(&format!("{OWL}FunctionalProperty"));
    let mut out = vec![[f, rdf_type, functional]];
    // x0 f a1, x0 f b1 gives a1 = b1; then a_k f a_{k+1} and b_k f b_{k+1} give
    // a_{k+1} = b_{k+1} once a_k = b_k.
    let a = |k: usize| individuals[2 * k - 1];
    let b = |k: usize| individuals[2 * k];
    out.push([individuals[0], f, a(1)]);
    out.push([individuals[0], f, b(1)]);
    for k in 1..depth {
        out.push([a(k), f, a(k + 1)]);
        out.push([b(k), f, b(k + 1)]);
    }
    out
}

fn report(result: &Materialisation, by_rule: bool) {
    let counters = &result.counters;
    let bindings: u64 = counters.bindings.iter().sum();
    let probes: u64 = counters.probes.iter().sum();
    let bytes = counters.store_bytes.last().map_or(0, |b| b.total());
    println!(
        "counts: rounds {} | ground rules {} | bindings {bindings} {:?} | probes {probes} {:?} | old checks {} | closure pairs {:?} | member scans {} | merged pairs {} | derived {} | store bytes {bytes}",
        result.rounds,
        result.ground_rules,
        counters.bindings,
        counters.probes,
        counters.old_checks,
        counters.closure_pairs,
        counters.member_scans,
        counters.merged_pairs,
        result.derived.len()
    );
    if by_rule {
        let mut rules: Vec<(&String, &u64)> = counters.bindings_by_rule.iter().collect();
        rules.sort_by_key(|&(name, count)| (std::cmp::Reverse(*count), name.clone()));
        for (name, count) in rules {
            println!("  {name:<12} {count:>12}");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut ruleset = Ruleset::Owl2Rl;
    let (mut naive, mut runs, mut threads, mut by_rule) = (false, 1, 0, false);
    let (mut equality, mut depth, mut inputs) = (false, 0, Vec::new());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ruleset" => {
                let name = args.next().unwrap_or_default();
                ruleset = Ruleset::from_name(&name).ok_or(format!("unknown ruleset {name}"))?;
            }
            "--naive" => naive = true,
            "--by-rule" => by_rule = true,
            "--equality" => equality = true,
            "--runs" => runs = args.next().ok_or("--runs N")?.parse()?,
            "--threads" => threads = args.next().ok_or("--threads N")?.parse()?,
            "--cascade" => depth = args.next().ok_or("--cascade DEPTH")?.parse()?,
            _ => inputs.push(arg),
        }
    }
    if threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }
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
    if depth > 0 {
        let extra = cascade(&mut vocabulary, &facts, depth);
        facts.extend(extra);
    }
    facts.sort_unstable();
    facts.dedup();
    println!(
        "asserted {} | load {:.2} s",
        facts.len(),
        started.elapsed().as_secs_f64()
    );
    if equality {
        for run in 0..runs {
            let started = Instant::now();
            let closure = nrese_reasoner::representatives::materialise_until(
                &facts,
                &rules,
                lists.as_ref(),
                &schema,
                nrese_reasoner::eval::NEVER,
            )
            .map_err(|_| "interrupted")?;
            let time = started.elapsed();
            println!(
                "run {run}: equality {:.3} s | passes {} | merges {} | rounds {} | facts {} | classes {} | fingerprint {:016x}",
                time.as_secs_f64(),
                closure.passes,
                closure.merges,
                closure.rounds,
                closure.facts.len(),
                closure.classes.classes().count(),
                fingerprint(&vocabulary, &closure.facts)
            );
        }
        return Ok(());
    }
    let mut first = None;
    for run in 0..runs {
        let started = Instant::now();
        let result = batch::materialise_grouped(grouped(&facts), &rules, lists.as_ref(), &schema);
        let time = started.elapsed();
        let p = &result.phases;
        println!(
            "run {run}: closure {:.3} s | grounding {:.3} joins {:.3} modules {:.3} merge {:.3} consistency {:.3} | derived {} | violations {} | fingerprint {:016x}",
            time.as_secs_f64(),
            p.grounding.as_secs_f64(),
            p.joins.as_secs_f64(),
            p.modules.as_secs_f64(),
            p.merge.as_secs_f64(),
            p.consistency.as_secs_f64(),
            result.derived.len(),
            result.violations.len(),
            fingerprint(&vocabulary, &result.derived)
        );
        if first.is_none() {
            report(&result, by_rule);
            first = Some(result);
        }
    }
    if naive && let Some(batch) = first {
        let started = Instant::now();
        let closure = nrese_reasoner::naive::materialise(&facts, &rules, lists.as_ref());
        let mut derived: Vec<Triple> = closure.derived.into_iter().collect();
        derived.sort_unstable();
        let mut violations = closure.violations;
        violations.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
        let same = derived == batch.derived && violations == batch.violations;
        println!(
            "naive {:.2} s | derived {} | violations {} | {}",
            started.elapsed().as_secs_f64(),
            derived.len(),
            violations.len(),
            if same {
                "same closure"
            } else {
                "CLOSURES DIFFER"
            }
        );
        if !same {
            return Err("the batch closure differs from the naive one".into());
        }
    }
    Ok(())
}
