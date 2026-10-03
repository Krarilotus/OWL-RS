//! A term table for the tests: terms by their N-Triples form, usable by `nrese-owl`'s
//! reader and writer and by the EL classifier; ontologies built in the structural model.

use std::collections::HashMap;

use nrese_dl::context::{self, Classification, Options};
use nrese_owl::fuzz::Name;
use nrese_owl::{
    Axiom, ClassExpr, EntityKind, ExprId, Make, ObjProp, Ontology, Statement, Term, TermKind,
    Terms, Vocabulary, read, write,
};

pub const EX: &str = "http://example.org/t#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Terms by id and back.
#[derive(Debug, Default, Clone)]
pub struct Table {
    terms: Vec<String>,
    ids: HashMap<String, u64>,
    blanks: u64,
}

impl Table {
    pub fn term(&mut self, text: &str) -> u64 {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(text.to_owned());
        self.ids.insert(text.to_owned(), id);
        id
    }

    pub fn iri_id(&mut self, iri: &str) -> u64 {
        self.term(&format!("<{iri}>"))
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn text(&self, id: u64) -> &str {
        &self.terms[id as usize]
    }

    /// An IRI without brackets.
    pub fn name(&self, id: u64) -> String {
        self.text(id).trim_matches(['<', '>']).to_owned()
    }

    pub fn intern(&mut self, name: &Name) -> Term {
        match name {
            Name::Iri(iri) => self.iri_id(iri),
            Name::Integer(n) => self.term(&format!("\"{n}\"^^<{XSD}integer>")),
        }
    }

    pub fn vocabulary(&mut self) -> Vocabulary {
        for (_, iri) in Vocabulary::iris() {
            self.iri_id(&iri);
        }
        Vocabulary::new(&|iri| self.ids.get(&format!("<{iri}>")).copied())
    }

    /// `o` as triples (the forward mapping).
    pub fn triples(&mut self, o: &Ontology) -> Vec<[u64; 3]> {
        let vocabulary = self.vocabulary();
        write(o, &vocabulary, self)
    }

    /// `o` written and read back (an ontology as the store would give it).
    pub fn round_trip(&mut self, o: &Ontology) -> Ontology {
        let statements: Vec<Statement> = self
            .triples(o)
            .into_iter()
            .map(|triple| Statement { triple, graph: 0 })
            .collect();
        read(&statements, self)
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match self.text(term).as_bytes().first() {
            Some(b'<') => TermKind::Iri,
            Some(b'_') => TermKind::Blank,
            _ => TermKind::Literal,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        let text = self.text(term).strip_prefix('"')?;
        Some(text[..text.rfind('"')?].to_owned())
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids.get(&format!("<{iri}>")).copied()
    }
}

impl Make for Table {
    fn blank(&mut self) -> Term {
        self.blanks += 1;
        let text = format!("_:w{}", self.blanks);
        self.term(&text)
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        self.term(&format!("\"{lexical}\"^^<{datatype}>"))
    }
}

impl nrese_reasoner::ir::Vocabulary for Table {
    fn iri(&mut self, iri: &str) -> u64 {
        self.iri_id(iri)
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        self.term(&format!("\"{lexical}\"^^<{datatype}>"))
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        self.term(&format!("\"{lexical}\"@{language}"))
    }
}

/// An ontology built axiom by axiom over `ex:` names.
pub struct Build<'t> {
    pub table: &'t mut Table,
    pub o: Ontology,
}

impl<'t> Build<'t> {
    pub fn new(table: &'t mut Table) -> Self {
        Self {
            table,
            o: Ontology::default(),
        }
    }

    pub fn t(&mut self, local: &str) -> Term {
        self.table.iri_id(&format!("{EX}{local}"))
    }

    pub fn e(&mut self, expr: ClassExpr) -> ExprId {
        ExprId(self.o.classes.intern(expr))
    }

    pub fn c(&mut self, local: &str) -> ExprId {
        let t = self.t(local);
        self.o.axioms.push(Axiom::Declaration(EntityKind::Class, t));
        self.e(ClassExpr::Class(t))
    }

    pub fn r(&mut self, local: &str) -> ObjProp {
        let t = self.t(local);
        self.o
            .axioms
            .push(Axiom::Declaration(EntityKind::ObjectProperty, t));
        ObjProp::Named(t)
    }

    pub fn some(&mut self, r: ObjProp, e: ExprId) -> ExprId {
        self.e(ClassExpr::Some(r, e))
    }

    pub fn all(&mut self, r: ObjProp, e: ExprId) -> ExprId {
        self.e(ClassExpr::All(r, e))
    }

    pub fn and(&mut self, xs: &[ExprId]) -> ExprId {
        let mut v = xs.to_vec();
        v.sort_unstable();
        v.dedup();
        self.e(ClassExpr::And(v))
    }

    pub fn or(&mut self, xs: &[ExprId]) -> ExprId {
        let mut v = xs.to_vec();
        v.sort_unstable();
        v.dedup();
        self.e(ClassExpr::Or(v))
    }

    pub fn not(&mut self, e: ExprId) -> ExprId {
        self.e(ClassExpr::Not(e))
    }

    pub fn sub(&mut self, a: ExprId, b: ExprId) {
        self.o.axioms.push(Axiom::SubClassOf(a, b));
    }

    pub fn axiom(&mut self, a: Axiom) {
        self.o.axioms.push(a);
    }

    pub fn done(mut self) -> Ontology {
        self.o.axioms.sort();
        self.o.axioms.dedup();
        self.o.sources = vec![Vec::new(); self.o.axioms.len()];
        self.o
    }
}

/// Classifies with the defaults, `threads` workers.
pub fn classify(o: &Ontology, threads: usize) -> Result<Classification, context::Unsupported> {
    let options = Options {
        threads,
        ..Options::default()
    };
    context::classify(o, &options).map(|(c, _)| c)
}

/// Whether `sub ⊑ sup` is in the classification (by local names).
pub fn holds(table: &mut Table, c: &Classification, sub: &str, sup: &str) -> bool {
    let (a, b) = (
        table.iri_id(&format!("{EX}{sub}")),
        table.iri_id(&format!("{EX}{sup}")),
    );
    c.subsumptions.binary_search(&(a, b)).is_ok()
}

pub fn unsat(table: &mut Table, c: &Classification, class: &str) -> bool {
    let a = table.iri_id(&format!("{EX}{class}"));
    c.unsatisfiable.binary_search(&a).is_ok()
}
