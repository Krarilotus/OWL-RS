//! Reading one ontology from RDF/XML and from the functional syntax, for the reader's speed
//! gate (the functional-syntax reader at least as fast as RDF/XML).
//!
//! ```text
//! cargo run --release -p nrese-owl --example ofn_speed -- [--runs N] NAME.ofn NAME.nt ...
//! ```
//!
//! Per pair: the N-Triples are written as RDF/XML first (not timed); then, from memory and
//! into a fresh table of terms each time, the RDF/XML parsed and read by the reverse
//! mapping, and the functional syntax read. Prints the medians of `--runs` runs (3), the
//! sizes and the axioms each reading gives.

use std::collections::HashMap;
use std::time::Instant;

use nrese_owl::{Intern, Ontology, Statement, Term, TermKind, Terms, read, read_functional};
use nrese_rdf::{BlankNode, Literal, NamedNode, Term as RdfTerm, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser, RdfSerializer};

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
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => Some(l.datatype().as_str().to_owned()),
            _ => None,
        }
    }

    fn language(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => l.language().map(str::to_owned),
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

fn from_rdf_xml(text: &str) -> Ontology {
    let mut table = Table::default();
    let statements: Vec<Statement> = RdfParser::from_format(RdfFormat::RdfXml)
        .for_slice(text.as_bytes())
        .map(|quad| {
            let t = Triple::from(quad.expect("the RDF/XML written parses"));
            Statement {
                triple: [
                    table.id(t.subject.into()),
                    table.id(t.predicate.into()),
                    table.id(t.object),
                ],
                graph: 0,
            }
        })
        .collect();
    read(&statements, &table)
}

fn from_functional(text: &str) -> Ontology {
    let mut table = Table::default();
    read_functional(text, &mut table).0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut runs = 3usize;
    if args.first().map(String::as_str) == Some("--runs") {
        runs = args[1].parse()?;
        args.drain(..2);
    }
    println!("ontology\tofn_MB\trdfxml_MB\tofn_ms\trdfxml_ms\tratio\tofn_axioms\trdf_axioms");
    for pair in args.chunks(2) {
        let [ofn, nt] = pair else {
            return Err("pairs of NAME.ofn NAME.nt".into());
        };
        let functional = std::fs::read_to_string(ofn)?;
        let ntriples = std::fs::read(nt)?;
        let mut xml = Vec::new();
        {
            let mut out = RdfSerializer::from_format(RdfFormat::RdfXml).for_writer(&mut xml);
            for quad in RdfParser::from_format(RdfFormat::NTriples).for_slice(&ntriples) {
                out.serialize_triple(&Triple::from(quad?))?;
            }
            out.finish()?;
        }
        drop(ntriples);
        let xml = String::from_utf8(xml)?;
        let (mut times_f, mut times_x) = (Vec::new(), Vec::new());
        let (mut axioms_f, mut axioms_x) = (0, 0);
        for _ in 0..runs {
            let started = Instant::now();
            let o = from_functional(&functional);
            times_f.push(started.elapsed().as_secs_f64() * 1000.0);
            axioms_f = o.axioms.len();
            drop(o);
            let started = Instant::now();
            let o = from_rdf_xml(&xml);
            times_x.push(started.elapsed().as_secs_f64() * 1000.0);
            axioms_x = o.axioms.len();
        }
        let (f, x) = (median(times_f), median(times_x));
        let name = ofn.rsplit(['/', '\\']).next().unwrap_or(ofn);
        println!(
            "{name}\t{:.1}\t{:.1}\t{f:.0}\t{x:.0}\t{:.2}\t{axioms_f}\t{axioms_x}",
            functional.len() as f64 / 1e6,
            xml.len() as f64 / 1e6,
            x / f
        );
    }
    Ok(())
}
