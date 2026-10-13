//! Reading an ontology for the DL examples: N-Triples, RDF/XML or functional-syntax files
//! (`.ofn`, or any text starting with `Prefix(` or `Ontology(`, as the ORE corpus's `.owl`
//! files do; imports not followed), into one ontology over one term table.

// Each example uses its part of it.
#![allow(dead_code)]

use std::collections::HashMap;

use nrese_owl::{FunctionalReader, Intern, Statement, Term, TermKind, Terms};
use nrese_rdf::{BlankNode, Literal, NamedNode, Term as RdfTerm, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};

/// The terms of the triples, by id.
#[derive(Default)]
pub struct Table {
    terms: Vec<RdfTerm>,
    ids: HashMap<RdfTerm, u64>,
}

impl Table {
    pub fn id(&mut self, term: RdfTerm) -> u64 {
        if let Some(&id) = self.ids.get(&term) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(term.clone());
        self.ids.insert(term, id);
        id
    }

    /// A term as the functional syntax writes it: `<iri>`, a literal, `_:label`.
    pub fn functional(&self, id: u64) -> String {
        match &self.terms[id as usize] {
            RdfTerm::NamedNode(n) => format!("<{}>", n.as_str()),
            other => other.to_string(),
        }
    }

    /// An IRI's text (a blank node as `_:id`).
    pub fn name(&self, id: u64) -> String {
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
pub fn functional(path: &str, text: Option<&str>) -> bool {
    path.ends_with(".ofn")
        || text.is_some_and(|t| {
            let t = t.trim_start();
            t.starts_with("Prefix(") || t.starts_with("Ontology(")
        })
}

/// The triples of an N-Triples (`.nt`) or RDF/XML file.
pub fn load(
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

/// The files as one ontology: functional-syntax documents read into the model directly,
/// the rest as triples.
pub fn ontology(
    paths: &[String],
    table: &mut Table,
) -> Result<nrese_owl::Ontology, Box<dyn std::error::Error>> {
    let (mut documents, mut triples) = (Vec::new(), Vec::new());
    for path in paths {
        let head = if path.ends_with(".nt") || path.ends_with(".rdf") {
            None
        } else {
            Some(std::fs::read_to_string(path)?)
        };
        match head {
            Some(text) if functional(path, Some(&text)) => documents.push(text),
            _ => load(path, table, &mut triples)?,
        }
    }
    if documents.is_empty() {
        let statements: Vec<Statement> = triples
            .iter()
            .map(|&triple| Statement { triple, graph: 0 })
            .collect();
        return Ok(nrese_owl::read(&statements, &*table));
    }
    let mut reader = FunctionalReader::new(table);
    for d in &documents {
        reader.read(d);
    }
    Ok(reader.finish())
}
