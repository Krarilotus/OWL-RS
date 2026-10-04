//! The bounds on an ontology with data: L (OWL 2 RL), U1, their gap per predicate, the
//! telemetry, and, given a reference reasoner's realisation, the check that U1 holds every
//! certain class assertion (package 3.2's gates).
//!
//! ```text
//! cargo run --release -p nrese-dl --example bounds_eval -- FILE... \
//!     [--real REALISATION --tax TAXONOMY] [--top N] [--over-lower]
//! ```
//!
//! - `FILE`: RDF documents (`.nt`, `.ttl`, `.rdf`/`.owl`), read as one ontology.
//! - `--real`, `--tax`: the reference runner's realisation (`a individual class` per
//!   direct type) and taxonomy (benches/reasoning/dl/README.md); the certain class
//!   assertions are their closure, and every one must be in U1.
//! - `--top N`: the N predicates with the largest gap (default 15).
//! - `--over-lower`: also time U1 over L's closure (the store's query path), with the
//!   tests' closure (its overhead included).
//! - `--n3 FILE`: write U1 as Notation3.
//! - `--axioms`: print the axioms as read, and the reader's diagnostics.
//! - `--repeat N`: time L's and U1's materialisations N times, interleaved, and report
//!   the medians (default 1).
//!
//! The times are the materialisations alone, by the rule reasoner's batch engine (U1's
//! equality by its equality module, which copies), from the same input; the checks use
//! the tests' closures (`support::upper`: equality by representatives).

#[path = "../tests/bounds/support.rs"]
#[allow(dead_code)]
mod support;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use nrese_dl::bounds::{Bounds, ProgramSize, Telemetry};
use nrese_rdf_io::RdfFormat;
use support::{Table, Triple};

fn format_of(path: &str) -> RdfFormat {
    match path.rsplit('.').next() {
        Some("nt") => RdfFormat::NTriples,
        Some("ttl") => RdfFormat::Turtle,
        _ => RdfFormat::RdfXml,
    }
}

/// The certain class assertions of a realisation and taxonomy: per individual, its
/// direct types, their equivalents and all their ancestors.
fn certain(table: &mut Table, real: &str, tax: &str) -> HashSet<Triple> {
    let rdf_type = table.named("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let mut members: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
    for line in tax.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        match parts[..] {
            ["=", rep, member] => members.entry(rep).or_default().push(member),
            ["<", sub, sup] => parents.entry(sub).or_default().push(sup),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    for line in real.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        let ["a", individual, direct] = parts[..] else {
            continue;
        };
        let a = table.named(individual);
        let mut stack = vec![direct];
        let mut seen = HashSet::new();
        while let Some(rep) = stack.pop() {
            if !seen.insert(rep) {
                continue;
            }
            for &m in members.get(rep).map_or(&[][..], Vec::as_slice) {
                out.insert([a, rdf_type, table.named(m)]);
            }
            out.insert([a, rdf_type, table.named(rep)]);
            stack.extend(parents.get(rep).map_or(&[][..], Vec::as_slice));
        }
    }
    out
}

/// The medians of `repeat` interleaved materialisations of L and of U1.
fn times(
    table: &mut Table,
    program: &nrese_dl::bounds::Program,
    input: &[Triple],
    repeat: usize,
) -> (Duration, Duration) {
    use nrese_reasoner::{batch, eval::Schema, lists::ListVocabulary, rulesets::Ruleset};
    let rl = Ruleset::Owl2Rl.rules(table).expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(table);
    let schema = Schema::owl(table);
    let u1 = support::reasoner_rules(program);
    let u1_input = support::upper_input(program, input);
    let (mut lower, mut upper) = (Vec::new(), Vec::new());
    for _ in 0..repeat {
        let clock = Instant::now();
        std::hint::black_box(batch::materialise(input, &rl, Some(&lists), &schema));
        lower.push(clock.elapsed());
        let clock = Instant::now();
        std::hint::black_box(batch::materialise(&u1_input, &u1, None, &schema));
        upper.push(clock.elapsed());
    }
    lower.sort_unstable();
    upper.sort_unstable();
    (lower[repeat / 2], upper[repeat / 2])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let top: usize = flag("--top").and_then(|n| n.parse().ok()).unwrap_or(15);
    let mut skip = false;
    let files: Vec<&String> = args
        .iter()
        .filter(|a| {
            let keep = !skip && !a.starts_with("--");
            skip = matches!(
                a.as_str(),
                "--real" | "--tax" | "--top" | "--n3" | "--repeat"
            );
            keep
        })
        .collect();
    let mut table = Table::default();
    let clock = Instant::now();
    let mut input = Vec::new();
    for file in &files {
        let bytes = std::fs::read(file).expect("the file reads");
        input.extend(table.parse(format_of(file), &bytes));
    }
    input.sort_unstable();
    input.dedup();
    eprintln!("parsed {} triples in {:.2?}", input.len(), clock.elapsed());
    let mut t = Telemetry {
        input: input.len(),
        ..Telemetry::default()
    };
    let clock = Instant::now();
    let (ontology, normalised) = support::read(&mut table, &input);
    t.normalise = clock.elapsed();
    let clock = Instant::now();
    let program = support::compile(&mut table, &ontology, &normalised);
    t.compile = clock.elapsed();
    t.program = ProgramSize::of(&program);
    eprintln!(
        "{} axioms, {} clauses, incomplete: {:?}",
        ontology.axioms.len(),
        normalised.clauses.len(),
        program.incomplete
    );
    if args.iter().any(|a| a == "--axioms") {
        for axiom in &ontology.axioms {
            eprintln!("  {}", ontology.functional(axiom, &|t| table.text(t)));
        }
        eprintln!("diagnostics: {:?}", ontology.diagnostics);
    }
    if let Some(path) = flag("--n3") {
        let text = program.to_n3(&|t| table.text(t));
        let text = text.unwrap_or_else(|e| format!("# {e}\n"));
        std::fs::write(path, text).expect("the N3 file writes");
    }
    let lower = support::lower(&mut table, &input);
    t.lower_facts = lower.facts.len();
    let upper = support::upper(&mut table, &program, &input);
    t.upper_facts = upper.facts.len();
    let repeat: usize = flag("--repeat").and_then(|n| n.parse().ok()).unwrap_or(1);
    (t.evaluate_lower, t.evaluate_upper) = times(&mut table, &program, &input, repeat.max(1));
    if args.iter().any(|a| a == "--over-lower") {
        let clock = Instant::now();
        let over = support::upper(&mut table, &program, &lower.facts);
        eprintln!(
            "U1 over L: {} facts in {:.2?}",
            over.facts.len(),
            clock.elapsed()
        );
    }
    let bounds = Bounds::new(
        &program,
        &table,
        lower.facts.iter().copied(),
        upper.facts.iter().copied(),
    );
    t.gap = bounds.report();
    println!("{t}");
    println!("L violations: {:?}", lower.violations);
    let name = |table: &Table, id: u64| table.text(id);
    println!("largest gaps (predicate: L, U1, gap, open):");
    for p in t.gap.predicates.iter().take(top) {
        println!(
            "  {}{}: {} {} {} {}",
            if p.class { "class " } else { "" },
            name(&table, p.predicate),
            p.lower,
            p.upper,
            p.gap,
            p.open
        );
    }
    let missing = bounds.lower_not_in_upper();
    println!("L not in U1: {}", missing.len());
    for m in missing.iter().take(10) {
        println!("  {}", m.map(|x| name(&table, x)).join(" "));
    }
    if let (Some(real), Some(tax)) = (flag("--real"), flag("--tax")) {
        let real = std::fs::read_to_string(real).expect("the realisation reads");
        let tax = std::fs::read_to_string(tax).expect("the taxonomy reads");
        let certain = certain(&mut table, &real, &tax);
        let upper: HashSet<Triple> = upper.facts.iter().copied().collect();
        let lower: HashSet<Triple> = lower.facts.iter().copied().collect();
        let not_in_upper: Vec<&Triple> = certain.iter().filter(|c| !upper.contains(*c)).collect();
        let in_lower = certain.iter().filter(|c| lower.contains(*c)).count();
        // Per class: L complete (L = certain), U1 tight (U1 = certain).
        let mut per: HashMap<u64, [usize; 3]> = HashMap::new();
        for c in &certain {
            per.entry(c[2]).or_default()[0] += 1;
        }
        for p in &t.gap.predicates {
            if p.class {
                let e = per.entry(p.predicate).or_default();
                e[1] = p.lower;
                e[2] = p.upper;
            }
        }
        let classes = per.len();
        let lower_complete = per.values().filter(|v| v[1] == v[0]).count();
        let upper_tight = per.values().filter(|v| v[2] == v[0]).count();
        let both = per
            .values()
            .filter(|v| v[1] == v[0] && v[2] == v[0])
            .count();
        println!(
            "reference: {} certain class assertions, {} in L, {} not in U1; class queries: {} \
             with L complete, {} with U1 tight, {} exact by bounds, of {}",
            certain.len(),
            in_lower,
            not_in_upper.len(),
            lower_complete,
            upper_tight,
            both,
            classes
        );
        for c in not_in_upper.iter().take(10) {
            println!("  missing {}", c.map(|x| name(&table, x)).join(" "));
        }
        let spurious: usize = per.values().map(|v| v[2].saturating_sub(v[0])).sum();
        println!("U1 beyond the certain class assertions: {spurious}");
    }
}
