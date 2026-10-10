//! Classification and realisation with the DL driver (`nrese_dl::classify`) of N-Triples,
//! RDF/XML or functional-syntax files (`.ofn`, or any text starting with `Prefix(` or
//! `Ontology(`, as the ORE corpus's `.owl` files do; imports not followed), for the DL lab.
//!
//! ```text
//! cargo run --release -p nrese-dl --example dl_classify -- [options] input.nt...
//!   --tax FILE            the canonical taxonomy (`benches/reasoning/dl/canonical.py`'s format)
//!   --real FILE           realise too, and write the canonical realisation
//!   --threads N           workers (default 1)
//!   --timeout SECS        a deadline for the whole run
//!   --test-timeout SECS   a budget per hypertableau test
//!   --lower-bound-timeout SECS  the Horn lower bound's budget
//!   --repeat N            run N times; times are the median of the runs
//!   --expand-at-most N    spell `≤ n` out as clauses up to this `n` (else at-most atoms)
//!   --cautious-lower-bound  the lower bound's context core with Strategy::Cautious
//!   --eager-lower-bound     ... with Strategy::Eager
//!   --equality            the context core takes functional properties (its Eq rule)
//!   --max-join N          the most conclusions one context-core join may make
//!   --max-join-steps N    the most steps one context-core Pred join may take
//!   --max-memory-mb N     the most memory the process may hold while the context core runs
//!   --task-memory-mib N   context saturation capacity budget; 0 disables accounting
//!   --clauses             print the DL-clauses with more than one head atom, then stop
//!   --no-context-core --no-inline --no-lower-bound --no-exact-shortcut --no-model-pruning
//!   --no-skip-seen --no-tbox-only --no-detached-probes --no-reuse-model
//!                         switch an optimisation off (the taxonomy must not change)
//! ```
//!
//! Prints `incomplete: <why>` lines where the result isn't complete, then
//! `profile read=… owl=… <driver profile>` (times in ms, medians over `--repeat`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use nrese_dl::classify::{self, Options};
use nrese_owl::{FunctionalReader, Intern, Statement, Term, TermKind, Terms};
use nrese_rdf::{BlankNode, Literal, NamedNode, Term as RdfTerm, Triple};
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

impl Intern for Table {
    fn iri_id(&mut self, iri: &str) -> Term {
        self.id(NamedNode::new_unchecked(iri).into())
    }

    fn literal_id(&mut self, lexical: &str, datatype: &str, language: Option<&str>) -> Term {
        let literal = match language {
            Some(tag) => Literal::new_language_tagged_literal(lexical, tag).unwrap_or_else(|_| {
                Literal::new_language_tagged_literal_unchecked(lexical, tag.to_lowercase())
            }),
            None => Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)),
        };
        self.id(literal.into())
    }

    fn blank_id(&mut self, label: &str, document: u32) -> Term {
        self.id(BlankNode::new_unchecked(format!("d{document}x{label}")).into())
    }
}

/// Whether a file is in the functional syntax: `.ofn`, or a text that starts with
/// `Prefix(` or `Ontology(` (the ORE corpus's `.owl` files).
fn functional(path: &str, text: Option<&str>) -> bool {
    path.ends_with(".ofn")
        || text.is_some_and(|t| {
            let t = t.trim_start();
            t.starts_with("Prefix(") || t.starts_with("Ontology(")
        })
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
    // Unchecked: the OWL API's conversions keep the relative IRIs its input had.
    for quad in RdfParser::from_format(format).unchecked().for_reader(file) {
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
    clauses: bool,
    inputs: Vec<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        tax: None,
        real: None,
        options: Options::default(),
        repeat: 1,
        clauses: false,
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
            "--lower-bound-timeout" => {
                a.options.lower_bound_timeout =
                    Duration::from_secs(number("--lower-bound-timeout")?);
            }
            "--test-timeout" => {
                a.options.tableau.timeout = Some(Duration::from_secs(number("--test-timeout")?));
            }
            "--tax" => a.tax = it.next(),
            "--real" => a.real = it.next(),
            "--no-context-core" => a.options.context_core = false,
            "--no-inline" => a.options.inline = false,
            "--no-lower-bound" => a.options.horn_lower_bound = false,
            "--no-exact-shortcut" => a.options.exact_lower_bound = false,
            "--no-model-pruning" => a.options.model_pruning = false,
            "--no-skip-seen" => a.options.skip_seen = false,
            "--no-tbox-only" => a.options.tbox_only = false,
            "--no-detached-probes" => a.options.detached_probes = false,
            "--no-reuse-model" => a.options.reuse_model = false,
            "--clauses" => a.clauses = true,
            "--cautious-lower-bound" => {
                a.options.lower_bound_strategy = nrese_dl::context::Strategy::Cautious;
            }
            "--equality" => a.options.equality = true,
            "--eager-lower-bound" => {
                a.options.lower_bound_strategy = nrese_dl::context::Strategy::Eager;
            }
            "--max-join" => a.options.max_join = number("--max-join")? as usize,
            "--max-join-steps" => {
                a.options.max_join_steps = number("--max-join-steps")? as usize;
            }
            "--max-memory-mb" => {
                a.options.max_memory = Some(number("--max-memory-mb")? << 20);
            }
            "--task-memory-mib" => {
                let mib = usize::try_from(number("--task-memory-mib")?)
                    .map_err(|_| "memory budget overflow")?;
                a.options.task_memory = if mib == 0 {
                    None
                } else {
                    Some(
                        mib.checked_mul(1024 * 1024)
                            .ok_or("memory budget overflow")?,
                    )
                };
            }
            "--expand-at-most" => {
                let n = number("--expand-at-most")? as u32;
                a.options.normalise.expand_at_most_up_to = n;
                a.options.tableau.expand_at_most_up_to = n;
            }
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
    // Functional-syntax documents are read into the model directly (their imports are
    // not followed); RDF files as triples.
    let mut documents: Vec<String> = Vec::new();
    for path in &a.inputs {
        let head = if path.ends_with(".nt") || path.ends_with(".rdf") {
            None
        } else {
            Some(std::fs::read_to_string(path)?)
        };
        match head {
            Some(text) if functional(path, Some(&text)) => documents.push(text),
            _ => load(path, &mut table, &mut triples)?,
        }
    }
    let read = started.elapsed();
    let statements: Vec<Statement> = triples
        .iter()
        .map(|&triple| Statement { triple, graph: 0 })
        .collect();
    let ontology_of = |table: &mut Table| -> nrese_owl::Ontology {
        if documents.is_empty() {
            return nrese_owl::read(&statements, &*table);
        }
        let mut reader = FunctionalReader::new(table);
        for d in &documents {
            reader.read(d);
        }
        reader.finish()
    };
    if a.clauses {
        let ontology = ontology_of(&mut table);
        let n = nrese_owl::normalise(&nrese_dl::tableau::prepared(&ontology));
        let wide: Vec<_> = n.clauses.iter().filter(|c| c.head.len() > 1).collect();
        println!(
            "{} clauses, {} with more than one head atom",
            n.clauses.len(),
            wide.len()
        );
        for c in wide.iter().take(10) {
            println!("{:?} -> {:?} (axioms {:?})", c.body, c.head, c.sources);
        }
        // Every clause mentioning the first wide clause's fresh names.
        let fresh: Vec<String> = wide
            .first()
            .map(|c| {
                format!("{:?}", c.head)
                    .split("Fresh(")
                    .skip(1)
                    .map(|t| format!("Fresh({}", &t[..=t.find(')').unwrap_or(0)]))
                    .collect()
            })
            .unwrap_or_default();
        for c in &n.clauses {
            let text = format!("{:?} -> {:?}", c.body, c.head);
            if fresh.iter().any(|f| text.contains(f.as_str())) {
                println!("  uses {fresh:?}: {text}");
            }
        }
        return Ok(());
    }
    let mut runs = Vec::new();
    let mut last = None;
    for _ in 0..a.repeat {
        let started = Instant::now();
        let ontology = ontology_of(&mut table);
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
