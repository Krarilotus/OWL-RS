//! Consistency of RDF ontologies through the hypertableau, with per-phase times and the
//! search counters (docs/design/owl2-dl-performance.md §5), for the DL lab.
//!
//! ```text
//! cargo run --release -p nrese-dl --example tableau_consistency -- [--timeout SECS]
//!     [--no-semantic-branching] [--no-backjumping] [--ancestor-blocking] [--pairwise-always]
//!     [--disjunctions-first] [--no-disjunct-learning] [--full-blocking] [--expand-at-most N]
//!     FILE...
//! ```
//!
//! Each FILE is N-Triples (`.nt`, as the reference runner's `ntriples` task writes it) or
//! RDF/XML (`.rdf`, `.owl`). Prints one line per file:
//! `file<TAB>answer<TAB>parse_ms=… read_ms=… normalise_ms=… <telemetry><TAB>reason`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use nrese_dl::tableau::{Answer, Config, consistency_of};
use nrese_owl::{Options, Statement, Term, TermKind, Terms, normalise_with, read};
use nrese_rdf::{NamedNode, Term as RdfTerm, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};

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

fn parse(path: &str) -> Result<Vec<Triple>, Box<dyn std::error::Error>> {
    let format = if path.ends_with(".nt") {
        RdfFormat::NTriples
    } else {
        RdfFormat::RdfXml
    };
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let triples: Result<Vec<Triple>, _> = RdfParser::from_format(format)
        .for_reader(file)
        .map(|q| q.map(Triple::from))
        .collect();
    Ok(triples?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut timeout, mut files) = (300u64, Vec::new());
    let mut config = Config::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--timeout" => timeout = args.next().ok_or("--timeout SECS")?.parse()?,
            // The switches, for the lab: each flips the default.
            "--no-semantic-branching" => config.semantic_branching = false,
            "--no-backjumping" => config.backjumping = false,
            "--ancestor-blocking" => config.anywhere_blocking = false,
            "--pairwise-always" => config.single_blocking = false,
            "--disjunctions-first" => config.disjunctions_first = true,
            "--no-disjunct-learning" => config.disjunct_learning = false,
            "--full-blocking" => config.incremental_blocking = false,
            "--expand-at-most" => {
                config.expand_at_most_up_to = args.next().ok_or("--expand-at-most N")?.parse()?;
            }
            _ => files.push(arg),
        }
    }
    let config = Config {
        timeout: Some(Duration::from_secs(timeout)),
        ..config
    };
    for path in files {
        let started = Instant::now();
        let triples = match parse(&path) {
            Ok(t) => t,
            Err(e) => {
                println!("{path}\tparse-error\t\t{e}");
                continue;
            }
        };
        let parsed = started.elapsed();
        let mut table = Table::default();
        let statements: Vec<Statement> = triples
            .into_iter()
            .map(|t| Statement {
                triple: [
                    table.id(t.subject.into()),
                    table.id(t.predicate.into()),
                    table.id(t.object),
                ],
                graph: 0,
            })
            .collect();
        let at = Instant::now();
        let ontology = read(&statements, &table);
        let read_time = at.elapsed();
        let at = Instant::now();
        let normalised = normalise_with(
            &ontology,
            Options {
                expand_at_most_up_to: config.expand_at_most_up_to,
                lazy_definitions: config.lazy_definitions && !config.keep_model,
            },
        );
        let normalise_time = at.elapsed();
        let out = consistency_of(&ontology, &normalised, &config);
        let reason = match &out.answer {
            Answer::Unsupported(why) | Answer::GaveUp(why) => why.clone(),
            _ => String::new(),
        };
        println!(
            "{path}\t{}\tparse_ms={:.3} read_ms={:.3} normalise_ms={:.3} {} axioms={} clauses={} whole_ms={:.3}\t{reason}",
            out.answer.class(),
            parsed.as_secs_f64() * 1000.0,
            read_time.as_secs_f64() * 1000.0,
            normalise_time.as_secs_f64() * 1000.0,
            out.telemetry,
            ontology.axioms.len(),
            normalised.clauses.len(),
            started.elapsed().as_secs_f64() * 1000.0,
        );
    }
    Ok(())
}
