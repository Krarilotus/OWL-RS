//! The grammar (OWL 2 Structural Specification §§3–11, its functional-style syntax):
//! recursive descent over the tokens, one axiom at a time; an axiom that can't be read is
//! a diagnostic, and reading resumes after its closing parenthesis.

use std::collections::HashMap;
use std::rc::Rc;

use super::lex::{Lexer, Tok, position};
use super::{Document, Intern, Model};
use crate::diagnostics::Diagnostic;
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, EntityKind, ExprId, ObjProp, RangeId, Term,
    canonical,
};
use crate::vocab::{OWL, RDF, RDFS, XSD};

/// Why an axiom can't be read: where, and what.
pub(super) struct Fail {
    at: usize,
    what: &'static str,
}

type R<T> = Result<T, Fail>;

/// What an IRI means where a class or a data range may stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Special {
    None,
    Thing,
    Nothing,
    Literal,
}

pub(super) struct Parser<'t, 'm> {
    lex: Lexer<'t>,
    peeked: Option<(Tok<'t>, usize)>,
    terms: &'m mut dyn Intern,
    model: &'m mut Model,
    document: u32,
    prefixes: HashMap<String, String>,
    /// Expanded prefixed names, by their token.
    expanded: HashMap<&'t str, Rc<str>>,
    /// Terms of IRIs, by full-IRI token and by prefixed-name token.
    full: HashMap<&'t str, (Term, Special)>,
    short: HashMap<&'t str, (Term, Special)>,
    blanks: HashMap<&'t str, Term>,
}

fn fail<T>(at: usize, what: &'static str) -> R<T> {
    Err(Fail { at, what })
}

/// The failure at an unexpected token: the lexer's reason where the token is malformed.
fn unexpected<T>(tok: &Tok<'_>, at: usize, what: &'static str) -> R<T> {
    match tok {
        Tok::Bad(why) => fail(at, why),
        _ => fail(at, what),
    }
}

fn special(iri: &str) -> Special {
    if let Some(local) = iri.strip_prefix(OWL) {
        match local {
            "Thing" => return Special::Thing,
            "Nothing" => return Special::Nothing,
            _ => {}
        }
    } else if iri.strip_prefix(RDFS) == Some("Literal") {
        return Special::Literal;
    }
    Special::None
}

/// Whether a name token is a prefixed name (or a blank node label), not a keyword.
fn is_prefixed(name: &str) -> bool {
    name.contains(':')
}

impl<'t, 'm> Parser<'t, 'm> {
    pub fn new(
        text: &'t str,
        terms: &'m mut dyn Intern,
        model: &'m mut Model,
        document: u32,
    ) -> Self {
        let prefixes = [("rdf", RDF), ("rdfs", RDFS), ("xsd", XSD), ("owl", OWL)]
            .into_iter()
            .map(|(p, iri)| (p.to_owned(), iri.to_owned()))
            .collect();
        Self {
            lex: Lexer::new(text),
            peeked: None,
            terms,
            model,
            document,
            prefixes,
            expanded: HashMap::new(),
            full: HashMap::new(),
            short: HashMap::new(),
            blanks: HashMap::new(),
        }
    }

    // Tokens ----------------------------------------------------------------------------

    fn peek(&mut self) -> &Tok<'t> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex.next());
        }
        &self.peeked.as_ref().expect("peeked").0
    }

    fn take(&mut self) -> (Tok<'t>, usize) {
        self.peeked.take().unwrap_or_else(|| self.lex.next())
    }

    /// The offset of the next token.
    fn here(&mut self) -> usize {
        self.peek();
        self.peeked.as_ref().map_or(0, |p| p.1)
    }

    fn open(&mut self) -> R<()> {
        match self.take() {
            (Tok::Open, _) => Ok(()),
            (tok, at) => unexpected(&tok, at, "'(' expected"),
        }
    }

    fn close(&mut self) -> R<()> {
        match self.take() {
            (Tok::Close, _) => Ok(()),
            (tok, at) => unexpected(&tok, at, "')' expected"),
        }
    }

    fn at_close(&mut self) -> bool {
        matches!(self.peek(), Tok::Close | Tok::End)
    }

    fn report(&mut self, at: usize, what: &'static str) {
        let (line, column) = position(self.lex.text(), at);
        self.model
            .diagnostics
            .push(Diagnostic::Syntax { line, column, what });
    }

    /// Skips what is left of a construct begun at parenthesis depth `depth` (to its
    /// closing parenthesis), and at least one token.
    fn recover(&mut self, depth: u32, from: usize) {
        self.peeked = None;
        loop {
            if self.lex.depth <= depth && self.here() > from {
                return;
            }
            if matches!(self.take().0, Tok::End) {
                return;
            }
        }
    }

    // IRIs and terms --------------------------------------------------------------------

    /// The IRI a prefixed name abbreviates.
    fn expand(&mut self, name: &'t str, at: usize) -> R<Rc<str>> {
        if let Some(iri) = self.expanded.get(name) {
            return Ok(iri.clone());
        }
        let (prefix, local) = name.split_once(':').unwrap_or(("", name));
        let Some(namespace) = self.prefixes.get(prefix) else {
            return fail(at, "a prefix no Prefix(…) declares");
        };
        let local = if local.contains('\\') {
            // PN_LOCAL's escapes: the character after each backslash.
            let mut out = String::with_capacity(local.len());
            let mut chars = local.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    out.extend(chars.next());
                } else {
                    out.push(c);
                }
            }
            out
        } else {
            local.to_owned()
        };
        let iri: Rc<str> = Rc::from(format!("{namespace}{local}"));
        self.expanded.insert(name, iri.clone());
        Ok(iri)
    }

    /// The text of the IRI at the next token (full or prefixed).
    fn iri_text(&mut self) -> R<Rc<str>> {
        match self.take() {
            (Tok::Iri(iri), _) => Ok(Rc::from(iri)),
            (Tok::Name(name), at) if is_prefixed(name) && !name.starts_with("_:") => {
                self.expand(name, at)
            }
            (tok, at) => unexpected(&tok, at, "an IRI expected"),
        }
    }

    /// The term of the IRI at the next token, and what it means where classes and data
    /// ranges stand.
    fn named(&mut self) -> R<(Term, Special)> {
        let (tok, at) = self.take();
        match tok {
            Tok::Iri(iri) => {
                if let Some(&known) = self.full.get(iri) {
                    return Ok(known);
                }
                let found = (self.terms.iri_id(iri), special(iri));
                self.full.insert(iri, found);
                Ok(found)
            }
            Tok::Name(name) if is_prefixed(name) && !name.starts_with("_:") => {
                if let Some(&known) = self.short.get(name) {
                    return Ok(known);
                }
                let iri = self.expand(name, at)?;
                let found = (self.terms.iri_id(&iri), special(&iri));
                self.short.insert(name, found);
                Ok(found)
            }
            tok => unexpected(&tok, at, "an IRI expected"),
        }
    }

    fn entity(&mut self) -> R<Term> {
        Ok(self.named()?.0)
    }

    /// An individual: an IRI or an anonymous individual `_:label`.
    fn individual(&mut self) -> R<Term> {
        if let Tok::Name(name) = *self.peek()
            && let Some(label) = name.strip_prefix("_:")
        {
            self.take();
            if let Some(&known) = self.blanks.get(name) {
                return Ok(known);
            }
            let id = self.terms.blank_id(label, self.document);
            self.blanks.insert(name, id);
            return Ok(id);
        }
        self.entity()
    }

    fn object_property(&mut self) -> R<ObjProp> {
        let p = if matches!(self.peek(), Tok::Name("ObjectInverseOf")) {
            self.take();
            self.open()?;
            let p = self.entity()?;
            self.close()?;
            ObjProp::Inverse(p)
        } else {
            ObjProp::Named(self.entity()?)
        };
        self.model.uses.entry(p.named()).or_default().object = true;
        Ok(p)
    }

    fn data_property(&mut self) -> R<Term> {
        let p = self.entity()?;
        self.model.uses.entry(p).or_default().data = true;
        Ok(p)
    }

    /// A literal: `"…"`, `"…"^^datatype`, `"…"@tag`.
    fn literal(&mut self) -> R<Term> {
        let (tok, at) = self.take();
        let Tok::Str(text) = tok else {
            return unexpected(&tok, at, "a literal expected");
        };
        match self.peek() {
            Tok::Carets => {
                self.take();
                let datatype = self.iri_text()?;
                if datatype.strip_prefix(RDF) == Some("PlainLiteral") {
                    // `"text@tag"^^rdf:PlainLiteral` is the language-tagged `"text"@tag`.
                    if let Some((plain, tag)) = text.rsplit_once('@') {
                        return Ok(if tag.is_empty() {
                            self.terms.literal_id(plain, &format!("{XSD}string"), None)
                        } else {
                            self.terms
                                .literal_id(plain, &format!("{RDF}langString"), Some(tag))
                        });
                    }
                }
                Ok(self.terms.literal_id(&text, &datatype, None))
            }
            Tok::Lang(tag) => {
                let tag = *tag;
                self.take();
                Ok(self
                    .terms
                    .literal_id(&text, &format!("{RDF}langString"), Some(tag)))
            }
            _ => Ok(self.terms.literal_id(&text, &format!("{XSD}string"), None)),
        }
    }

    fn number(&mut self) -> R<u32> {
        match self.take() {
            (Tok::Name(n), at) => n
                .parse::<u32>()
                .or_else(|_| fail(at, "a cardinality that isn't a non-negative integer")),
            (tok, at) => unexpected(&tok, at, "a cardinality expected"),
        }
    }

    // Annotations -----------------------------------------------------------------------

    /// `Annotation(…)*`, counted.
    fn annotations(&mut self) -> R<()> {
        while matches!(self.peek(), Tok::Name("Annotation")) {
            self.take();
            self.open()?;
            self.annotations()?;
            self.iri_text()?;
            self.annotation_value()?;
            self.close()?;
            self.model.annotations += 1;
        }
        Ok(())
    }

    /// An annotation's value or subject: an IRI, an anonymous individual or a literal
    /// (read, not interned: annotations aren't kept).
    fn annotation_value(&mut self) -> R<()> {
        match *self.peek() {
            Tok::Str(_) => {
                self.take();
                match self.peek() {
                    Tok::Carets => {
                        self.take();
                        self.iri_text()?;
                    }
                    Tok::Lang(_) => {
                        self.take();
                    }
                    _ => {}
                }
                Ok(())
            }
            Tok::Name(name) if name.starts_with("_:") => {
                self.take();
                Ok(())
            }
            _ => self.iri_text().map(|_| ()),
        }
    }

    // Expressions -----------------------------------------------------------------------

    fn e(&mut self, expr: ClassExpr) -> ExprId {
        ExprId(self.model.classes.intern(expr))
    }

    fn r(&mut self, range: DataRange) -> RangeId {
        RangeId(self.model.ranges.intern(range))
    }

    fn class(&mut self) -> R<ExprId> {
        let (keyword, at) = match *self.peek() {
            Tok::Name(name) if !is_prefixed(name) => (name, self.here()),
            _ => {
                let (term, special) = self.named()?;
                return Ok(match special {
                    Special::Thing => self.e(ClassExpr::Thing),
                    Special::Nothing => self.e(ClassExpr::Nothing),
                    _ => self.e(ClassExpr::Class(term)),
                });
            }
        };
        self.take();
        self.open()?;
        let expr = match keyword {
            "ObjectIntersectionOf" | "ObjectUnionOf" => {
                let xs = canonical(self.classes()?);
                if keyword == "ObjectUnionOf" {
                    ClassExpr::Or(xs)
                } else {
                    ClassExpr::And(xs)
                }
            }
            "ObjectComplementOf" => ClassExpr::Not(self.class()?),
            "ObjectOneOf" => {
                let mut xs = Vec::new();
                while !self.at_close() {
                    xs.push(self.individual()?);
                }
                ClassExpr::OneOf(canonical(xs))
            }
            "ObjectSomeValuesFrom" | "ObjectAllValuesFrom" => {
                let p = self.object_property()?;
                let c = self.class()?;
                if keyword == "ObjectSomeValuesFrom" {
                    ClassExpr::Some(p, c)
                } else {
                    ClassExpr::All(p, c)
                }
            }
            "ObjectHasValue" => {
                let p = self.object_property()?;
                ClassExpr::HasValue(p, self.individual()?)
            }
            "ObjectHasSelf" => ClassExpr::HasSelf(self.object_property()?),
            "ObjectMinCardinality" | "ObjectMaxCardinality" | "ObjectExactCardinality" => {
                let n = self.number()?;
                let p = self.object_property()?;
                let c = if self.at_close() {
                    self.e(ClassExpr::Thing)
                } else {
                    self.class()?
                };
                match keyword {
                    "ObjectMinCardinality" => ClassExpr::Min(n, p, c),
                    "ObjectMaxCardinality" => ClassExpr::Max(n, p, c),
                    _ => ClassExpr::Exact(n, p, c),
                }
            }
            "DataSomeValuesFrom" | "DataAllValuesFrom" => {
                let p = self.data_property()?;
                let r = self.range()?;
                if !self.at_close() {
                    return fail(
                        self.here(),
                        "a data restriction over several properties (OWL 2's datatypes are unary)",
                    );
                }
                if keyword == "DataSomeValuesFrom" {
                    ClassExpr::DataSome(p, r)
                } else {
                    ClassExpr::DataAll(p, r)
                }
            }
            "DataHasValue" => {
                let p = self.data_property()?;
                ClassExpr::DataHasValue(p, self.literal()?)
            }
            "DataMinCardinality" | "DataMaxCardinality" | "DataExactCardinality" => {
                let n = self.number()?;
                let p = self.data_property()?;
                let r = if self.at_close() {
                    self.r(DataRange::Literal)
                } else {
                    self.range()?
                };
                match keyword {
                    "DataMinCardinality" => ClassExpr::DataMin(n, p, r),
                    "DataMaxCardinality" => ClassExpr::DataMax(n, p, r),
                    _ => ClassExpr::DataExact(n, p, r),
                }
            }
            _ => return fail(at, "a class expression expected"),
        };
        self.close()?;
        Ok(self.e(expr))
    }

    /// Class expressions up to the closing parenthesis.
    fn classes(&mut self) -> R<Vec<ExprId>> {
        let mut xs = Vec::new();
        while !self.at_close() {
            xs.push(self.class()?);
        }
        Ok(xs)
    }

    fn range(&mut self) -> R<RangeId> {
        let (keyword, at) = match *self.peek() {
            Tok::Name(name) if !is_prefixed(name) => (name, self.here()),
            _ => {
                let (term, special) = self.named()?;
                return Ok(if special == Special::Literal {
                    self.r(DataRange::Literal)
                } else {
                    self.r(DataRange::Datatype(term))
                });
            }
        };
        self.take();
        self.open()?;
        let range = match keyword {
            "DataIntersectionOf" | "DataUnionOf" => {
                let mut xs = Vec::new();
                while !self.at_close() {
                    xs.push(self.range()?);
                }
                if keyword == "DataUnionOf" {
                    DataRange::Or(canonical(xs))
                } else {
                    DataRange::And(canonical(xs))
                }
            }
            "DataComplementOf" => DataRange::Not(self.range()?),
            "DataOneOf" => {
                let mut xs = Vec::new();
                while !self.at_close() {
                    xs.push(self.literal()?);
                }
                DataRange::OneOf(canonical(xs))
            }
            "DatatypeRestriction" => {
                let datatype = self.entity()?;
                let mut facets = Vec::new();
                while !self.at_close() {
                    let facet = self.entity()?;
                    facets.push((facet, self.literal()?));
                }
                if facets.is_empty() {
                    return fail(at, "a datatype restriction without a facet");
                }
                DataRange::Restriction(datatype, canonical(facets))
            }
            _ => return fail(at, "a data range expected"),
        };
        self.close()?;
        Ok(self.r(range))
    }

    fn object_properties(&mut self) -> R<Vec<ObjProp>> {
        let mut ps = Vec::new();
        while !self.at_close() {
            ps.push(self.object_property()?);
        }
        Ok(ps)
    }

    fn data_properties(&mut self) -> R<Vec<Term>> {
        let mut ps = Vec::new();
        while !self.at_close() {
            ps.push(self.data_property()?);
        }
        Ok(ps)
    }

    fn individuals(&mut self) -> R<Vec<Term>> {
        let mut xs = Vec::new();
        while !self.at_close() {
            xs.push(self.individual()?);
        }
        Ok(xs)
    }

    // Axioms ----------------------------------------------------------------------------

    fn add(&mut self, axiom: Axiom) {
        self.model.axioms.insert(axiom, ());
    }

    fn axiom(&mut self) -> R<()> {
        let (tok, at) = self.take();
        let Tok::Name(keyword) = tok else {
            return unexpected(&tok, at, "an axiom expected");
        };
        if is_prefixed(keyword) {
            return fail(at, "an axiom expected");
        }
        self.open()?;
        self.annotations()?;
        let characteristic = |k: &str| {
            Some(match k {
                "FunctionalObjectProperty" => Characteristic::Functional,
                "InverseFunctionalObjectProperty" => Characteristic::InverseFunctional,
                "ReflexiveObjectProperty" => Characteristic::Reflexive,
                "IrreflexiveObjectProperty" => Characteristic::Irreflexive,
                "SymmetricObjectProperty" => Characteristic::Symmetric,
                "AsymmetricObjectProperty" => Characteristic::Asymmetric,
                "TransitiveObjectProperty" => Characteristic::Transitive,
                _ => return None,
            })
        };
        let axiom = match keyword {
            "Declaration" => {
                let (kind, at) = match self.take() {
                    (Tok::Name(k), at) => (k, at),
                    (tok, at) => return unexpected(&tok, at, "an entity expected"),
                };
                let kind = match kind {
                    "Class" => EntityKind::Class,
                    "Datatype" => EntityKind::Datatype,
                    "ObjectProperty" => EntityKind::ObjectProperty,
                    "DataProperty" => EntityKind::DataProperty,
                    "AnnotationProperty" => EntityKind::AnnotationProperty,
                    "NamedIndividual" => EntityKind::NamedIndividual,
                    _ => return fail(at, "an entity expected"),
                };
                self.open()?;
                let t = self.entity()?;
                self.close()?;
                let uses = self.model.uses.entry(t).or_default();
                match kind {
                    EntityKind::ObjectProperty => uses.object = true,
                    EntityKind::DataProperty => uses.data = true,
                    EntityKind::AnnotationProperty => uses.annotation = true,
                    _ => {}
                }
                Some(Axiom::Declaration(kind, t))
            }
            "SubClassOf" => {
                let sub = self.class()?;
                Some(Axiom::SubClassOf(sub, self.class()?))
            }
            "EquivalentClasses" => Some(Axiom::EquivalentClasses(canonical(self.classes()?))),
            "DisjointClasses" => Some(Axiom::DisjointClasses(canonical(self.classes()?))),
            "DisjointUnion" => {
                let class = self.entity()?;
                Some(Axiom::DisjointUnion(class, canonical(self.classes()?)))
            }
            "SubObjectPropertyOf" => {
                let chain = if matches!(self.peek(), Tok::Name("ObjectPropertyChain")) {
                    self.take();
                    self.open()?;
                    let chain = self.object_properties()?;
                    self.close()?;
                    chain
                } else {
                    vec![self.object_property()?]
                };
                Some(Axiom::SubObjectPropertyOf(chain, self.object_property()?))
            }
            "EquivalentObjectProperties" => Some(Axiom::EquivalentObjectProperties(canonical(
                self.object_properties()?,
            ))),
            "DisjointObjectProperties" => Some(Axiom::DisjointObjectProperties(canonical(
                self.object_properties()?,
            ))),
            "InverseObjectProperties" => {
                let a = self.object_property()?;
                let b = self.object_property()?;
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                Some(Axiom::InverseObjectProperties(a, b))
            }
            "ObjectPropertyDomain" | "ObjectPropertyRange" => {
                let p = self.object_property()?;
                let c = self.class()?;
                Some(if keyword == "ObjectPropertyDomain" {
                    Axiom::ObjectPropertyDomain(p, c)
                } else {
                    Axiom::ObjectPropertyRange(p, c)
                })
            }
            k if characteristic(k).is_some() => {
                let kind = characteristic(k).expect("matched");
                Some(Axiom::ObjectCharacteristic(kind, self.object_property()?))
            }
            "SubDataPropertyOf" => {
                let sub = self.data_property()?;
                Some(Axiom::SubDataPropertyOf(sub, self.data_property()?))
            }
            "EquivalentDataProperties" => Some(Axiom::EquivalentDataProperties(canonical(
                self.data_properties()?,
            ))),
            "DisjointDataProperties" => Some(Axiom::DisjointDataProperties(canonical(
                self.data_properties()?,
            ))),
            "DataPropertyDomain" => {
                let p = self.data_property()?;
                Some(Axiom::DataPropertyDomain(p, self.class()?))
            }
            "DataPropertyRange" => {
                let p = self.data_property()?;
                Some(Axiom::DataPropertyRange(p, self.range()?))
            }
            "FunctionalDataProperty" => Some(Axiom::FunctionalDataProperty(self.data_property()?)),
            "DatatypeDefinition" => {
                let d = self.entity()?;
                Some(Axiom::DatatypeDefinition(d, self.range()?))
            }
            "HasKey" => {
                let class = self.class()?;
                self.open()?;
                let objects = self.object_properties()?;
                self.close()?;
                self.open()?;
                let data = self.data_properties()?;
                self.close()?;
                Some(Axiom::HasKey(class, canonical(objects), canonical(data)))
            }
            "SameIndividual" => Some(Axiom::SameIndividual(canonical(self.individuals()?))),
            "DifferentIndividuals" => {
                Some(Axiom::DifferentIndividuals(canonical(self.individuals()?)))
            }
            "ClassAssertion" => {
                let c = self.class()?;
                Some(Axiom::ClassAssertion(c, self.individual()?))
            }
            "ObjectPropertyAssertion" | "NegativeObjectPropertyAssertion" => {
                let p = self.object_property()?;
                let (a, b) = (self.individual()?, self.individual()?);
                // Over the named property: an inverse's assertion swapped.
                let (p, a, b) = match p {
                    ObjProp::Named(p) => (p, a, b),
                    ObjProp::Inverse(p) => (p, b, a),
                };
                Some(if keyword == "ObjectPropertyAssertion" {
                    Axiom::ObjectPropertyAssertion(p, a, b)
                } else {
                    Axiom::NegativeObjectPropertyAssertion(p, a, b)
                })
            }
            "DataPropertyAssertion" | "NegativeDataPropertyAssertion" => {
                let p = self.data_property()?;
                let a = self.individual()?;
                let v = self.literal()?;
                Some(if keyword == "DataPropertyAssertion" {
                    Axiom::DataPropertyAssertion(p, a, v)
                } else {
                    Axiom::NegativeDataPropertyAssertion(p, a, v)
                })
            }
            "AnnotationAssertion" => {
                self.iri_text()?;
                self.annotation_value()?;
                self.annotation_value()?;
                None
            }
            "SubAnnotationPropertyOf" | "AnnotationPropertyDomain" | "AnnotationPropertyRange" => {
                let property = self.entity()?;
                self.model.uses.entry(property).or_default().annotation = true;
                if keyword == "SubAnnotationPropertyOf" {
                    let sup = self.entity()?;
                    self.model.uses.entry(sup).or_default().annotation = true;
                } else {
                    self.iri_text()?;
                }
                None
            }
            "DLSafeRule" => return fail(at, "a DL-safe rule (SWRL, not OWL 2)"),
            "DescriptionGraph" => return fail(at, "a description graph (not OWL 2)"),
            _ => return fail(at, "an axiom expected"),
        };
        self.close()?;
        match axiom {
            Some(axiom) => self.add(axiom),
            None => self.model.annotations += 1,
        }
        Ok(())
    }

    // The document ----------------------------------------------------------------------

    fn prefix(&mut self) -> R<()> {
        self.take();
        self.open()?;
        let (name, at) = match self.take() {
            (Tok::Name(n), at) if n.ends_with(':') => (n, at),
            (tok, at) => return unexpected(&tok, at, "a prefix name ending in ':' expected"),
        };
        if !matches!(self.take().0, Tok::Equals) {
            return fail(at, "'=' expected after the prefix name");
        }
        let iri = match self.take() {
            (Tok::Iri(iri), _) => iri,
            (tok, at) => return unexpected(&tok, at, "a full IRI expected"),
        };
        self.close()?;
        self.prefixes
            .insert(name[..name.len() - 1].to_owned(), iri.to_owned());
        Ok(())
    }

    /// Reads the document: prefixes, then `Ontology(…)`.
    pub fn document(mut self) -> Document {
        let mut header = Document::default();
        loop {
            let at = self.here();
            match *self.peek() {
                Tok::Name("Prefix") => {
                    let depth = self.lex.depth;
                    if let Err(f) = self.prefix() {
                        self.report(f.at, f.what);
                        self.recover(depth, at);
                    }
                }
                Tok::Name("Ontology") => break,
                Tok::End => {
                    self.report(at, "no Ontology(…)");
                    return header;
                }
                _ => {
                    self.report(at, "Prefix(…) or Ontology(…) expected");
                    self.recover(self.lex.depth, at);
                }
            }
        }
        self.take();
        if let Err(f) = self.open() {
            self.report(f.at, f.what);
            return header;
        }
        let outer = self.lex.depth - 1;
        // The ontology IRI and version IRI.
        for slot in [&mut header.iri, &mut header.version] {
            let iri = match *self.peek() {
                Tok::Iri(_) => true,
                Tok::Name(n) => is_prefixed(n) && !n.starts_with("_:"),
                _ => false,
            };
            if !iri {
                break;
            }
            match self.iri_text() {
                Ok(text) => *slot = Some(text.to_string()),
                Err(f) => self.report(f.at, f.what),
            }
        }
        while matches!(self.peek(), Tok::Name("Import")) {
            let (depth, at) = (self.lex.depth, self.here());
            self.take();
            let import = self.open().and_then(|()| {
                let iri = self.iri_text()?;
                self.close()?;
                Ok(iri)
            });
            match import {
                Ok(iri) => header.imports.push(iri.to_string()),
                Err(f) => {
                    self.report(f.at, f.what);
                    self.recover(depth, at);
                }
            }
        }
        if let Err(f) = self.annotations() {
            self.report(f.at, f.what);
        }
        loop {
            let at = self.here();
            match self.peek() {
                Tok::Close => {
                    self.take();
                    break;
                }
                Tok::End => {
                    self.report(at, "the ontology's ')' is missing");
                    return header;
                }
                _ => {}
            }
            let depth = self.lex.depth;
            if let Err(f) = self.axiom() {
                self.report(f.at, f.what);
                self.recover(depth, at);
            }
            if self.lex.depth < outer {
                break;
            }
        }
        let at = self.here();
        if !matches!(self.peek(), Tok::End) {
            self.report(at, "text after the ontology");
        }
        header
    }
}
