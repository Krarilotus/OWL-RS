//! The ABox islands of an ontology (`nrese_dl::islands`): counts, and with `--decide` the
//! consistency verdict island by island against the whole ABox's.
//!
//! ```text
//! cargo run --release -p nrese-dl --example islands -- [--decide] [--nominal-clauses] FILE...
//! ```
//!
//! The FILEs (N-Triples or RDF/XML) are read as one ontology (LUBM: `univ-bench.nt` and
//! the data). Prints the individuals, the islands, the largest five, the split role
//! assertions and the batches, or why the ABox is decided whole; with `--decide` both
//! verdicts and their times (indicative: measure inside `scripts/quiet-slot.sh`); with
//! `--nominal-clauses` the clauses with a nominal in the head (what keeps the ABox whole).

use std::collections::HashMap;
use std::time::Instant;

use nrese_dl::islands::{self, Split};
use nrese_dl::tableau::{self, Config};
use nrese_owl::{Intern, Statement, Term, TermKind, Terms, read};
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
            Some(tag) => Literal::new_language_tagged_literal_unchecked(lexical, tag),
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

fn parse(path: &str) -> Result<Vec<Triple>, Box<dyn std::error::Error>> {
    let format = match path.ends_with(".nt") {
        true => RdfFormat::NTriples,
        false => RdfFormat::RdfXml,
    };
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let triples: Result<Vec<Triple>, _> = RdfParser::from_format(format)
        .for_reader(file)
        .map(|q| q.map(Triple::from))
        .collect();
    Ok(triples?)
}

fn verdict(outcome: &tableau::Outcome) -> String {
    match &outcome.answer {
        tableau::Answer::Consistent => "consistent".into(),
        tableau::Answer::Inconsistent => "inconsistent".into(),
        tableau::Answer::Unsupported(why) => format!("unsupported ({why})"),
        tableau::Answer::GaveUp(why) => format!("gave up ({why})"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let decide = args.iter().any(|a| a == "--decide");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let mut table = Table::default();
    let mut statements = Vec::new();
    for path in &files {
        for t in parse(path)? {
            statements.push(Statement {
                triple: [
                    table.id(t.subject.into()),
                    table.id(t.predicate.into()),
                    table.id(t.object),
                ],
                graph: 0,
            });
        }
    }
    let ontology = read(&statements, &table);
    if args.iter().any(|a| a == "--nominal-clauses") {
        let normalised = nrese_owl::normalise(&ontology);
        for c in &normalised.clauses {
            if c.head
                .iter()
                .any(|h| matches!(h, nrese_owl::HeadAtom::Nominal(..)))
            {
                println!("{:?} -> {:?}", c.body, c.head);
            }
        }
    }
    let started = Instant::now();
    let split = islands::split(&ontology);
    let took = started.elapsed();
    match &split {
        Split::Whole(why) => println!("whole ABox: {why} (split decided in {took:.1?})"),
        Split::Islands(i) => {
            let individuals: usize = i.sizes.iter().sum();
            println!(
                "{individuals} individuals in {} islands, largest {:?}; {} role assertions split; {} batches (split in {took:.1?})",
                i.sizes.len(),
                &i.sizes[..i.sizes.len().min(5)],
                i.split_edges,
                i.batches.len()
            );
        }
    }
    if decide {
        let config = Config {
            timeout: Some(std::time::Duration::from_secs(600)),
            ..Config::default()
        };
        let at = Instant::now();
        let whole = tableau::consistency(&ontology, &config);
        println!("whole ABox: {} in {:.1?}", verdict(&whole), at.elapsed());
        let at = Instant::now();
        let parts = islands::by_islands(&ontology, &config);
        println!("islands: {} in {:.1?}", verdict(&parts), at.elapsed());
    }
    Ok(())
}
