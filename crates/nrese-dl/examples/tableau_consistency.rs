//! Consistency of RDF ontologies through the hypertableau, with per-phase times and the
//! search counters (docs/design/owl2-dl-performance.md §5), for the DL lab.
//!
//! ```text
//! cargo run --release -p nrese-dl --example tableau_consistency -- [--timeout SECS]
//!     [--no-semantic-branching] [--no-backjumping] [--ancestor-blocking] [--pairwise-always]
//!     [--disjunctions-first] [--no-disjunct-learning] [--full-blocking] [--expand-at-most N]
//!     [--exact-provenance] [--no-lazy-definitions] FILE...
//! ```
//!
//! `NRESE_CLAUSE_STATS=1` also prints, on standard error, the clauses per kind of axiom
//! they come from and the axioms with the most clauses.
//!
//! Each FILE is N-Triples (`.nt`, as the reference runner's `ntriples` task writes it),
//! OWL functional syntax (a file that starts with `Prefix(` or `Ontology(`, as the ORE
//! corpora have it) or RDF/XML. Prints one line per file (functional syntax: `parse_ms`
//! is reading the text, `read_ms` the functional-syntax reader):
//! `file<TAB>answer<TAB>parse_ms=… read_ms=… normalise_ms=… <telemetry><TAB>reason`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use nrese_dl::tableau::{Answer, Config, consistency_of};
use nrese_owl::{
    Intern, Ontology, Options, Statement, Term, TermKind, Terms, normalise_with, read,
    read_functional,
};
use nrese_rdf::{BlankNode, Literal, NamedNode, Term as RdfTerm, Triple};
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

/// The ontology of `path`, with the time to parse (or read the text) and to read it.
fn load(
    path: &str,
    table: &mut Table,
    started: Instant,
) -> Result<(Ontology, Duration, Duration), Box<dyn std::error::Error>> {
    let head = {
        use std::io::Read as _;
        let mut buf = [0u8; 256];
        let n = std::fs::File::open(path)?.read(&mut buf)?;
        String::from_utf8_lossy(&buf[..n]).trim_start().to_owned()
    };
    if head.starts_with("Prefix(") || head.starts_with("Ontology(") {
        let text = std::fs::read_to_string(path)?;
        let parsed = started.elapsed();
        let at = Instant::now();
        let (ontology, _) = read_functional(&text, table);
        return Ok((ontology, parsed, at.elapsed()));
    }
    let triples = parse(path)?;
    let parsed = started.elapsed();
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
    let ontology = read(&statements, table);
    Ok((ontology, parsed, at.elapsed()))
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
    let mut exact_provenance = false;
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
            "--exact-provenance" => exact_provenance = true,
            "--no-lazy-definitions" => config.lazy_definitions = false,
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
        let mut table = Table::default();
        let (ontology, parsed, read_time) = match load(&path, &mut table, started) {
            Ok(r) => r,
            Err(e) => {
                println!("{path}\tparse-error\t\t{e}");
                continue;
            }
        };
        let at = Instant::now();
        let normalised = normalise_with(
            &ontology,
            Options {
                expand_at_most_up_to: config.expand_at_most_up_to,
                lazy_definitions: config.lazy_definitions && !config.keep_model,
                exact_provenance,
            },
        );
        let normalise_time = at.elapsed();
        if std::env::var_os("NRESE_CLAUSE_STATS").is_some() {
            clause_stats(&ontology, &normalised);
        }
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

/// Clauses per kind of the axiom they come from (first source), and body and head sizes.
fn clause_stats(ontology: &Ontology, normalised: &nrese_owl::Normalised) {
    let mut by_kind: HashMap<String, (usize, usize, usize)> = HashMap::new();
    let mut axioms: HashMap<String, usize> = HashMap::new();
    for a in &ontology.axioms {
        let kind = format!("{a:?}").split('(').next().unwrap_or("").to_owned();
        *axioms.entry(kind).or_default() += 1;
    }
    for c in &normalised.clauses {
        let kind = c
            .sources
            .first()
            .and_then(|s| s.first())
            .map_or("none".to_owned(), |&i| {
                format!("{:?}", ontology.axioms[i])
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .to_owned()
            });
        let e = by_kind.entry(kind).or_default();
        e.0 += 1;
        e.1 += c.body.len();
        e.2 += c.head.len();
    }
    let mut rows: Vec<_> = by_kind.into_iter().collect();
    rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.0));
    for (kind, (n, body, head)) in rows {
        eprintln!(
            "{kind}: {n} clauses from {} axioms, body {:.1}, head {:.1}",
            axioms.get(&kind).copied().unwrap_or(0),
            body as f64 / n as f64,
            head as f64 / n as f64
        );
    }
    eprintln!("fresh names: {}", normalised.fresh.len());
    let mut shown = 0;
    for c in &normalised.clauses {
        let from = c.sources.first().and_then(|s| s.first()).copied();
        if from.is_some_and(|i| matches!(ontology.axioms[i], nrese_owl::Axiom::DisjointClasses(_)))
            && shown < 12
        {
            eprintln!("  {:?} -> {:?} from {:?}", c.body, c.head, c.sources);
            shown += 1;
        }
    }
    // Clauses per source set size, and per first source axiom.
    let mut per_axiom: HashMap<usize, usize> = HashMap::new();
    for c in &normalised.clauses {
        if let Some(&i) = c.sources.first().and_then(|s| s.first()) {
            *per_axiom.entry(i).or_default() += 1;
        }
    }
    let mut top: Vec<_> = per_axiom.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (i, n) in top.iter().take(5) {
        eprintln!("axiom {i}: {n} clauses: {:?}", ontology.axioms[*i]);
    }
}
