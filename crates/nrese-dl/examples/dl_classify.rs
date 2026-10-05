//! Classification and realisation with the DL driver (`nrese_dl::classify`) of N-Triples or
//! RDF/XML files, for the DL lab.
//!
//! ```text
//! cargo run --release -p nrese-dl --example dl_classify -- [options] input.nt...
//!   --tax FILE            the canonical taxonomy (`benches/reasoning/dl/canonical.py`'s format)
//!   --real FILE           realise too, and write the canonical realisation
//!   --threads N           workers (default 1)
//!   --timeout SECS        a deadline for the whole run
//!   --test-timeout SECS   a budget per hypertableau test
//!   --repeat N            run N times; times are the median of the runs
//!   --no-context-core --no-lower-bound --no-model-pruning --no-skip-seen --no-tbox-only
//!                         switch an optimisation off (the taxonomy must not change)
//! ```
//!
//! Prints `incomplete: <why>` lines where the result isn't complete, then
//! `profile read=… owl=… <driver profile>` (times in ms, medians over `--repeat`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use nrese_dl::classify::{self, Options};
use nrese_owl::{Statement, Term, TermKind, Terms};
use nrese_rdf::{NamedNode, Term as RdfTerm, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};

/// The terms of the triples, by id.
#[derive(Default)]
struct Table {
    terms: Vec<RdfTerm>,
    ids: HashMap<RdfTerm, u64>,
}

impl Table {
    fn id(&mut self, term: RdfTerm) -> u64 {
        if let Some(&id) = self.ids.get(&term) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(term.clone());
        self.ids.insert(term, id);
        id
    }

    /// An IRI's text (a blank node as `_:id`).
    fn name(&self, id: u64) -> String {
        match &self.terms[id as usize] {
            RdfTerm::NamedNode(n) => n.as_str().to_owned(),
            other => other.to_string(),
        }
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match &self.terms[term as usize] {
            RdfTerm::NamedNode(_) => TermKind::Iri,
            RdfTerm::BlankNode(_) => TermKind::Blank,
            _ => TermKind::Literal,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids
            .get(&RdfTerm::NamedNode(NamedNode::new_unchecked(iri)))
            .copied()
    }

    fn datatype(&self, term: Term) -> Option<String> {
        match self.terms.get(term as usize)? {
            RdfTerm::Literal(l) => Some(l.datatype().as_str().to_owned()),
            _ => None,
        }
    }

    fn language(&self, term: Term) -> Option<String> {
        match self.terms.get(term as usize)? {
            RdfTerm::Literal(l) => l.language().map(str::to_owned),
            _ => None,
        }
    }

    fn iri_text(&self, term: Term) -> Option<String> {
        match self.terms.get(term as usize)? {
            RdfTerm::NamedNode(n) => Some(n.as_str().to_owned()),
            _ => None,
        }
    }
}

/// The triples of an N-Triples (`.nt`) or RDF/XML file.
fn load(
    path: &str,
    table: &mut Table,
    out: &mut Vec<[u64; 3]>,
) -> Result<(), Box<dyn std::error::Error>> {
    let format = if path.ends_with(".nt") {
        RdfFormat::NTriples
    } else {
        RdfFormat::RdfXml
    };
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    for quad in RdfParser::from_format(format).for_reader(file) {
        let t = Triple::from(quad?);
        out.push([
            table.id(t.subject.into()),
            table.id(t.predicate.into()),
            table.id(t.object),
        ]);
    }
    Ok(())
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort_unstable();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

struct Args {
    tax: Option<String>,
    real: Option<String>,
    options: Options,
    repeat: usize,
    inputs: Vec<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        tax: None,
        real: None,
        options: Options::default(),
        repeat: 1,
        inputs: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut number = |what: &str| -> Result<u64, String> {
            it.next()
                .and_then(|n| n.parse().ok())
                .ok_or(format!("{what} N"))
        };
        match arg.as_str() {
            "--threads" => a.options.threads = number("--threads")?.max(1) as usize,
            "--repeat" => a.repeat = number("--repeat")?.max(1) as usize,
            "--timeout" => a.options.timeout = Some(Duration::from_secs(number("--timeout")?)),
            "--test-timeout" => {
                a.options.tableau.timeout = Some(Duration::from_secs(number("--test-timeout")?));
            }
            "--tax" => a.tax = it.next(),
            "--real" => a.real = it.next(),
            "--no-context-core" => a.options.context_core = false,
            "--no-lower-bound" => a.options.horn_lower_bound = false,
            "--no-model-pruning" => a.options.model_pruning = false,
            "--no-skip-seen" => a.options.skip_seen = false,
            "--no-tbox-only" => a.options.tbox_only = false,
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            _ => a.inputs.push(arg),
        }
    }
    Ok(a)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = args()?;
    let started = Instant::now();
    let (mut table, mut triples) = (Table::default(), Vec::new());
    for path in &a.inputs {
        load(path, &mut table, &mut triples)?;
    }
    let read = started.elapsed();
    let statements: Vec<Statement> = triples
        .iter()
        .map(|&triple| Statement { triple, graph: 0 })
        .collect();
    let mut runs = Vec::new();
    let mut last = None;
    for _ in 0..a.repeat {
        let started = Instant::now();
        let ontology = nrese_owl::read(&statements, &table);
        let owl = started.elapsed();
        if a.real.is_some() {
            let r = classify::realise(&ontology, &a.options);
            runs.push((owl, r.profile.clone()));
            last = Some((r.taxonomy.clone(), Some(r)));
        } else {
            let t = classify::classify(&ontology, &a.options);
            runs.push((owl, t.profile.clone()));
            last = Some((t, None));
        }
    }
    let (taxonomy, realisation) = last.ok_or("no run")?;
    let name = |t: Term| table.name(t);
    if let Some(path) = &a.tax {
        std::fs::write(path, taxonomy.classification.canonical(&name))?;
    }
    if let (Some(path), Some(r)) = (&a.real, &realisation) {
        std::fs::write(path, r.canonical(&name))?;
    }
    let incomplete = match &realisation {
        Some(r) => &r.incomplete,
        None => &taxonomy.incomplete,
    };
    for why in incomplete {
        println!("incomplete: {why}");
    }
    let c = &taxonomy.classification;
    eprintln!(
        "{} triples; {} classes, {} subsumptions, {} unsatisfiable, consistent {}",
        triples.len(),
        c.classes.len(),
        c.subsumptions.len(),
        c.unsatisfiable.len(),
        c.consistent
    );
    let mut profile = runs.last().map(|(_, p)| p.clone()).unwrap_or_default();
    profile.total = median(runs.iter().map(|(owl, p)| *owl + p.total).collect());
    eprintln!(
        "profile read={} owl={} {}",
        ms(read),
        ms(median(runs.iter().map(|(owl, _)| *owl).collect())),
        profile.line()
    );
    Ok(())
}
