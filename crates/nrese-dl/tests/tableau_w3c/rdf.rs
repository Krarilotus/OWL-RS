//! Reading the test cases' RDF/XML into the structural model (as nrese-owl's W3C test
//! does).

use std::collections::HashMap;

use nrese_owl::{Ontology, Statement, Term, TermKind, Terms, read};
use nrese_rdf::{NamedNode, Term as RdfTerm, Triple};
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

/// Terms by id, shared by a test's premise and conclusion.
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

    pub fn name(&self, t: Term) -> String {
        match self.terms.get(t as usize) {
            Some(RdfTerm::BlankNode(_)) => format!("_:b{t}"),
            Some(other) => other.to_string(),
            None => format!("fresh{t}"),
        }
    }

    pub fn is_blank(&self, t: Term) -> bool {
        matches!(self.terms.get(t as usize), Some(RdfTerm::BlankNode(_)))
    }

    /// The ontology of an RDF/XML document; why not, if it has a fatal diagnostic.
    pub fn ontology(&mut self, text: &str) -> Result<Ontology, String> {
        let triples = parse_rdf_xml(text)?;
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
}
