//! The OWL 2 Functional-Style Syntax reader (W3C *OWL 2 Structural Specification*,
//! §§3–11): ontology documents straight into the structural model the RDF reader builds
//! ([`crate::read`]), over the same term ids, so that both readings of an ontology are one
//! model and the model's way back to triples ([`crate::write`]) serves both.
//!
//! - **Terms** are made by the caller ([`Intern`]): the IRIs, literals and anonymous
//!   individuals the text names get the ids the caller's store gives them, as the triples
//!   the RDF reader reads have; [`crate::Terms`] then answers for them.
//! - **The model's conventions are the RDF reader's:** n-ary operands sorted and without
//!   repeats, `owl:Thing`, `owl:Nothing` and `rdfs:Literal` by their meaning, an assertion
//!   of an inverse property stored swapped, `InverseObjectProperties` with its operands
//!   ordered, `"…"@tag` and `"…@tag"^^rdf:PlainLiteral` as language-tagged literals.
//! - **Annotations** (on the ontology, on axioms, annotation axioms) are read and counted,
//!   not kept: they have no logical meaning (as the RDF reader does).
//! - **Never silent:** whatever can't be read is a [`Diagnostic::Syntax`] with its line
//!   and column, and the axiom it is in is left out; reading goes on after it. What OWL 2
//!   DL forbids is diagnosed as for RDF: a property declared or used as two of object,
//!   data and annotation property, and the global restrictions.

mod lex;
mod parse;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::diagnostics::Diagnostic;
use crate::mapping::{BuiltinProperties, Ontology, Terms};
use crate::model::{Axiom, ClassExpr, DataRange, Interner, Term};
use crate::vocab::Vocabulary;

/// What the reader needs of the caller's terms: an id for each IRI, literal and anonymous
/// individual it reads (the same id for the same term), and [`Terms`]' answers about them
/// afterwards (as for triples the RDF reader reads).
pub trait Intern: Terms {
    fn iri_id(&mut self, iri: &str) -> Term;
    /// A literal: its lexical form, its datatype's IRI (`rdf:langString` for one with a
    /// language tag) and its tag.
    fn literal_id(&mut self, lexical: &str, datatype: &str, language: Option<&str>) -> Term;
    /// An anonymous individual `_:label` of document `document` (the reader's count, from
    /// 0): labels are local to their document, so the caller keeps documents apart.
    fn blank_id(&mut self, label: &str, document: u32) -> Term;
}

/// A document's header: its ontology IRI, version IRI, and the IRIs it imports (the caller
/// reads those documents into the same reader).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub iri: Option<String>,
    pub version: Option<String>,
    pub imports: Vec<String>,
}

/// What how a term is used says it is (for the typing diagnostics).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Uses {
    object: bool,
    data: bool,
    annotation: bool,
}

/// The model being read, over every document read so far.
#[derive(Default)]
pub(super) struct Model {
    pub classes: Interner<ClassExpr>,
    pub ranges: Interner<DataRange>,
    pub axioms: BTreeMap<Axiom, ()>,
    pub diagnostics: Vec<Diagnostic>,
    pub annotations: usize,
    pub uses: HashMap<Term, Uses>,
}

/// Reads functional-syntax documents into one ontology (a document and its imports).
pub struct FunctionalReader<'a> {
    terms: &'a mut dyn Intern,
    model: Model,
    documents: u32,
}

impl<'a> FunctionalReader<'a> {
    pub fn new(terms: &'a mut dyn Intern) -> Self {
        Self {
            terms,
            model: Model::default(),
            documents: 0,
        }
    }

    /// Reads one document's axioms into the ontology; its header.
    pub fn read(&mut self, text: &str) -> Document {
        let document = self.documents;
        self.documents += 1;
        parse::Parser::new(text, &mut *self.terms, &mut self.model, document).document()
    }

    /// The ontology of the documents read.
    pub fn finish(self) -> Ontology {
        let terms: &dyn Terms = &*self.terms;
        let mut model = self.model;
        let mut ambiguous: Vec<Term> = model
            .uses
            .iter()
            .filter(|(_, u)| (u.object && u.data) || (u.annotation && (u.object || u.data)))
            .map(|(&t, _)| t)
            .collect();
        ambiguous.sort_unstable();
        for property in ambiguous {
            model
                .diagnostics
                .push(Diagnostic::AmbiguousProperty { property });
        }
        let axioms: Vec<Axiom> = model.axioms.into_keys().collect();
        let mut ontology = Ontology {
            classes: model.classes,
            ranges: model.ranges,
            sources: vec![Vec::new(); axioms.len()],
            axioms,
            diagnostics: model.diagnostics,
            annotations: model.annotations,
            builtin: BuiltinProperties::of(&Vocabulary::new(&|iri| terms.iri(iri))),
            data: Default::default(),
            anonymous: BTreeSet::new(),
        };
        crate::mapping::complete(&mut ontology, terms);
        ontology
    }
}

/// The ontology of one functional-syntax document (without its imports) and its header.
pub fn read_functional(text: &str, terms: &mut dyn Intern) -> (Ontology, Document) {
    let mut reader = FunctionalReader::new(terms);
    let document = reader.read(text);
    (reader.finish(), document)
}
