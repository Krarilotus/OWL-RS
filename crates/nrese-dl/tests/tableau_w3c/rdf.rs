//! Reading the test cases' RDF/XML (as nrese-owl's W3C test does) and functional-syntax
//! documents (`nrese_owl::read_functional`) into the structural model, over one table of
//! terms.

use std::collections::HashMap;

use nrese_owl::{Intern, Ontology, Statement, Term, TermKind, Terms, read, read_functional};
use nrese_rdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term as RdfTerm, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};

/// RDF/XML with its entity declarations in double quotes (the test cases write
/// `<!ENTITY owl 'http://…'>`, which the parser doesn't take).
fn quoted_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!ENTITY") {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let end = from.find('>').map_or(from.len(), |e| e + 1);
        let declaration = &from[..end];
        if declaration.contains('"') {
            out.push_str(declaration);
        } else {
            out.push_str(&declaration.replace('\'', "\""));
        }
        rest = &from[end..];
    }
    out.push_str(rest);
    out
}

pub fn parse_rdf_xml(text: &str) -> Result<Vec<Triple>, String> {
    RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri("http://www.w3.org/2007/OWL/test-base/")
        .map_err(|e| e.to_string())?
        .for_reader(quoted_entities(text).as_bytes())
        .map(|quad| quad.map(Triple::from).map_err(|e| e.to_string()))
        .collect()
}

/// `triples` with their blank nodes renamed apart from other documents' (`tag`).
fn relabelled(triples: Vec<Triple>, tag: usize) -> Vec<Triple> {
    let rename = |b: &BlankNode| BlankNode::new_unchecked(format!("import{tag}x{}", b.as_str()));
    triples
        .into_iter()
        .map(|mut t| {
            if let NamedOrBlankNode::BlankNode(b) = &t.subject {
                t.subject = NamedOrBlankNode::BlankNode(rename(b));
            }
            if let RdfTerm::BlankNode(b) = &t.object {
                t.object = RdfTerm::BlankNode(rename(b));
            }
            t
        })
        .collect()
}

/// Terms by id, shared by a test's premise and conclusion.
#[derive(Default)]
pub struct Table {
    terms: Vec<RdfTerm>,
    ids: HashMap<RdfTerm, u64>,
    /// Functional-syntax documents read: their anonymous individuals are their own.
    documents: u32,
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

    pub fn name(&self, t: Term) -> String {
        match self.terms.get(t as usize) {
            Some(RdfTerm::BlankNode(_)) => format!("_:b{t}"),
            Some(other) => other.to_string(),
            None => format!("fresh{t}"),
        }
    }

    /// The ontology of a functional-syntax document; why not, if it imports another (none
    /// of the suite's does) or the reader has a fatal diagnostic.
    pub fn functional(&mut self, text: &str) -> Result<Ontology, String> {
        self.documents += 1;
        let (o, header) = read_functional(text, self);
        if let Some(import) = header.imports.first() {
            return Err(format!(
                "imports {import} (not read from the functional syntax)"
            ));
        }
        if let Some(d) = o.diagnostics.iter().find(|d| d.is_fatal()) {
            return Err(format!("reader: {d:?}"));
        }
        Ok(o)
    }

    pub fn is_blank(&self, t: Term) -> bool {
        matches!(self.terms.get(t as usize), Some(RdfTerm::BlankNode(_)))
    }

    /// The ontology of an RDF/XML document with its imports closure, the imported
    /// documents by ontology IRI (the test case's `test:importedOntology`); why not, if an
    /// import isn't given or the reader has a fatal diagnostic.
    pub fn ontology(
        &mut self,
        text: &str,
        imports: &HashMap<String, String>,
    ) -> Result<Ontology, String> {
        const IMPORTS: &str = "http://www.w3.org/2002/07/owl#imports";
        let mut triples = parse_rdf_xml(text)?;
        let mut done: Vec<String> = Vec::new();
        let mut at = 0;
        while at < triples.len() {
            let t = &triples[at];
            at += 1;
            if t.predicate.as_str() != IMPORTS {
                continue;
            }
            let RdfTerm::NamedNode(iri) = &t.object else {
                return Err("an owl:imports of a non-IRI".into());
            };
            let iri = iri.as_str().to_owned();
            if done.contains(&iri) {
                continue;
            }
            let Some(document) = imports.get(&iri) else {
                return Err(format!("imports {iri}, which the test case doesn't give"));
            };
            done.push(iri);
            triples.extend(relabelled(parse_rdf_xml(document)?, done.len()));
        }
        let statements: Vec<Statement> = triples
            .into_iter()
            .map(|t| Statement {
                triple: [
                    self.id(t.subject.into()),
                    self.id(t.predicate.into()),
                    self.id(t.object),
                ],
                graph: 0,
            })
            .collect();
        let o = read(&statements, self);
        if let Some(d) = o.diagnostics.iter().find(|d| d.is_fatal()) {
            return Err(format!("reader: {d:?}"));
        }
        Ok(o)
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

    /// Per document the table read (a premise's `_:x` isn't its conclusion's).
    fn blank_id(&mut self, label: &str, _document: u32) -> Term {
        let label = format!("fs{}x{label}", self.documents);
        self.id(BlankNode::new_unchecked(label).into())
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
