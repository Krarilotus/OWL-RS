//! Axioms in the OWL 2 Functional-Style Syntax (W3C *OWL 2 Structural Specification*),
//! terms named by the caller: for diagnostics, explanations and tests (two readings of
//! one ontology compare equal by their text, whatever ids they interned), and whole
//! documents that [`crate::read_functional`] reads back.

use crate::mapping::Ontology;
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, EntityKind, ExprId, ObjProp, RangeId, Term,
};

impl Ontology {
    /// `axiom` in the functional syntax; `name` writes a term (an IRI as `<…>`, a
    /// literal as `"…"^^<…>`).
    pub fn functional(&self, axiom: &Axiom, name: &dyn Fn(Term) -> String) -> String {
        let f = Functional {
            ontology: self,
            name,
        };
        f.axiom(axiom)
    }

    /// A DL-safe rule in the functional syntax (the OWL API's `DLSafeRule`).
    pub fn functional_rule(
        &self,
        rule: &crate::rules::Rule,
        name: &dyn Fn(Term) -> String,
    ) -> String {
        let f = Functional {
            ontology: self,
            name,
        };
        f.rule(rule)
    }

    /// The ontology as a functional-syntax document: the standard prefixes, then
    /// `Ontology(<iri>` and an axiom per line. `name` must write terms as the syntax does
    /// ([`iri_text`], [`literal_text`], `_:label` for an anonymous individual).
    pub fn functional_document(&self, iri: Option<&str>, name: &dyn Fn(Term) -> String) -> String {
        let mut out = String::new();
        for (prefix, namespace) in [
            ("rdf", crate::vocab::RDF),
            ("rdfs", crate::vocab::RDFS),
            ("xsd", crate::vocab::XSD),
            ("owl", crate::vocab::OWL),
        ] {
            out.push_str(&format!("Prefix({prefix}:=<{namespace}>)\n"));
        }
        out.push_str("Ontology(");
        if let Some(iri) = iri {
            out.push_str(&iri_text(iri));
        }
        out.push('\n');
        for axiom in &self.axioms {
            out.push_str(&self.functional(axiom, name));
            out.push('\n');
        }
        for rule in &self.rules {
            out.push_str(&self.functional_rule(rule, name));
            out.push('\n');
        }
        out.push_str(")\n");
        out
    }
}

/// An IRI as the functional syntax writes it.
pub fn iri_text(iri: &str) -> String {
    format!("<{iri}>")
}

/// A literal as the functional syntax writes it: `"…"` (`xsd:string`), `"…"@tag`, or
/// `"…"^^<datatype>`, with `"` and `\` escaped.
pub fn literal_text(lexical: &str, datatype: Option<&str>, language: Option<&str>) -> String {
    let escaped = lexical.replace('\\', "\\\\").replace('"', "\\\"");
    match (language, datatype) {
        (Some(tag), _) => format!("\"{escaped}\"@{tag}"),
        (None, Some(d)) if d != format!("{}string", crate::vocab::XSD) => {
            format!("\"{escaped}\"^^<{d}>")
        }
        _ => format!("\"{escaped}\""),
    }
}

struct Functional<'a> {
    ontology: &'a Ontology,
    name: &'a dyn Fn(Term) -> String,
}

/// A class expression or a data range, as [`Functional::print`] prints it.
#[derive(Clone, Copy)]
enum Part {
    Class(ExprId),
    Range(RangeId),
}

/// A part's text ([`Functional::shape`]): whole; up to its one operand; or up to its set of
/// operands, which print in text order.
enum Shape {
    Text(String),
    Unary(String, Part),
    Set(&'static str, Vec<Part>),
}

impl Functional<'_> {
    fn n(&self, term: Term) -> String {
        (self.name)(term)
    }

    fn terms(&self, terms: &[Term]) -> String {
        terms
            .iter()
            .map(|&t| self.n(t))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn p(&self, property: ObjProp) -> String {
        match property {
            ObjProp::Named(p) => self.n(p),
            ObjProp::Inverse(p) => format!("ObjectInverseOf({})", self.n(p)),
        }
    }

    fn ps(&self, properties: &[ObjProp]) -> String {
        properties
            .iter()
            .map(|&p| self.p(p))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn c(&self, id: ExprId) -> String {
        self.print(Part::Class(id))
    }

    /// Operands in a stable order: by their text (ids differ between readings).
    fn cs(&self, xs: &[ExprId]) -> String {
        let mut parts: Vec<String> = xs.iter().map(|&x| self.c(x)).collect();
        parts.sort();
        parts.join(" ")
    }

    fn r(&self, id: RangeId) -> String {
        self.print(Part::Range(id))
    }

    /// `part` printed with a stack of its own (the nesting depth is no call depth): a single
    /// operand straight into its parent's text, each operand of a set into a text of its
    /// own, the set's in text order once all are printed ([`Self::cs`]).
    fn print(&self, part: Part) -> String {
        enum Task {
            Print(Part),
            Text(&'static str),
            Member(Part),
            Join(usize),
        }
        let mut texts = vec![String::new()];
        let mut tasks = vec![Task::Print(part)];
        while let Some(task) = tasks.pop() {
            let text = texts.last_mut().expect("a text to print into");
            match task {
                Task::Print(part) => match self.shape(part) {
                    Shape::Text(t) => text.push_str(&t),
                    Shape::Unary(before, operand) => {
                        text.push_str(&before);
                        tasks.extend([Task::Text(")"), Task::Print(operand)]);
                    }
                    Shape::Set(before, operands) => {
                        text.push_str(before);
                        tasks.extend([Task::Text(")"), Task::Join(operands.len())]);
                        tasks.extend(operands.into_iter().rev().map(Task::Member));
                    }
                },
                Task::Text(t) => text.push_str(t),
                Task::Member(part) => {
                    texts.push(String::new());
                    tasks.push(Task::Print(part));
                }
                Task::Join(n) => {
                    let mut members = texts.split_off(texts.len() - n);
                    members.sort();
                    let text = texts.last_mut().expect("the set's text");
                    text.push_str(&members.join(" "));
                }
            }
        }
        texts.pop().expect("the printed text")
    }

    /// How `part` prints: whole, or up to its class or range operands, each closed by `)`.
    fn shape(&self, part: Part) -> Shape {
        let (c, r) = (Part::Class, Part::Range);
        let id = match part {
            Part::Class(id) => id,
            Part::Range(id) => return self.range_shape(id),
        };
        match self.ontology.class(id) {
            ClassExpr::Class(t) => Shape::Text(self.n(*t)),
            ClassExpr::Thing => Shape::Text("owl:Thing".to_owned()),
            ClassExpr::Nothing => Shape::Text("owl:Nothing".to_owned()),
            ClassExpr::And(xs) => {
                Shape::Set("ObjectIntersectionOf(", xs.iter().map(|&x| c(x)).collect())
            }
            ClassExpr::Or(xs) => Shape::Set("ObjectUnionOf(", xs.iter().map(|&x| c(x)).collect()),
            ClassExpr::Not(x) => Shape::Unary("ObjectComplementOf(".to_owned(), c(*x)),
            ClassExpr::OneOf(xs) => Shape::Text(format!("ObjectOneOf({})", self.terms(xs))),
            ClassExpr::Some(p, x) => {
                Shape::Unary(format!("ObjectSomeValuesFrom({} ", self.p(*p)), c(*x))
            }
            ClassExpr::All(p, x) => {
                Shape::Unary(format!("ObjectAllValuesFrom({} ", self.p(*p)), c(*x))
            }
            ClassExpr::HasValue(p, a) => {
                Shape::Text(format!("ObjectHasValue({} {})", self.p(*p), self.n(*a)))
            }
            ClassExpr::HasSelf(p) => Shape::Text(format!("ObjectHasSelf({})", self.p(*p))),
            ClassExpr::Min(n, p, x) => {
                Shape::Unary(format!("ObjectMinCardinality({n} {} ", self.p(*p)), c(*x))
            }
            ClassExpr::Max(n, p, x) => {
                Shape::Unary(format!("ObjectMaxCardinality({n} {} ", self.p(*p)), c(*x))
            }
            ClassExpr::Exact(n, p, x) => {
                Shape::Unary(format!("ObjectExactCardinality({n} {} ", self.p(*p)), c(*x))
            }
            ClassExpr::DataSome(p, x) => {
                Shape::Unary(format!("DataSomeValuesFrom({} ", self.n(*p)), r(*x))
            }
            ClassExpr::DataAll(p, x) => {
                Shape::Unary(format!("DataAllValuesFrom({} ", self.n(*p)), r(*x))
            }
            ClassExpr::DataHasValue(p, v) => {
                Shape::Text(format!("DataHasValue({} {})", self.n(*p), self.n(*v)))
            }
            ClassExpr::DataMin(n, p, x) => {
                Shape::Unary(format!("DataMinCardinality({n} {} ", self.n(*p)), r(*x))
            }
            ClassExpr::DataMax(n, p, x) => {
                Shape::Unary(format!("DataMaxCardinality({n} {} ", self.n(*p)), r(*x))
            }
            ClassExpr::DataExact(n, p, x) => {
                Shape::Unary(format!("DataExactCardinality({n} {} ", self.n(*p)), r(*x))
            }
        }
    }

    fn range_shape(&self, id: RangeId) -> Shape {
        let r = Part::Range;
        match self.ontology.range(id) {
            DataRange::Literal => Shape::Text("rdfs:Literal".to_owned()),
            DataRange::Datatype(t) => Shape::Text(self.n(*t)),
            DataRange::And(xs) => {
                Shape::Set("DataIntersectionOf(", xs.iter().map(|&x| r(x)).collect())
            }
            DataRange::Or(xs) => Shape::Set("DataUnionOf(", xs.iter().map(|&x| r(x)).collect()),
            DataRange::Not(x) => Shape::Unary("DataComplementOf(".to_owned(), r(*x)),
            DataRange::OneOf(xs) => Shape::Text(format!("DataOneOf({})", self.terms(xs))),
            DataRange::Restriction(d, facets) => Shape::Text(format!(
                "DatatypeRestriction({} {})",
                self.n(*d),
                facets
                    .iter()
                    .map(|&(f, v)| format!("{} {}", self.n(f), self.n(v)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
        }
    }

    fn rule(&self, rule: &crate::rules::Rule) -> String {
        use crate::rules::{Atom, DArg, IArg};
        let var = |v: u32| format!("Variable({})", self.n(rule.variables[v as usize]));
        let i = |a: &IArg| match *a {
            IArg::Var(v) => var(v),
            IArg::Individual(t) => self.n(t),
        };
        let d = |a: &DArg| match *a {
            DArg::Var(v) => var(v),
            DArg::Literal(t) => self.n(t),
        };
        let atoms = |atoms: &[Atom]| -> String {
            atoms
                .iter()
                .map(|a| match a {
                    Atom::Class(c, x) => format!("ClassAtom({} {})", self.c(*c), i(x)),
                    Atom::Range(r, v) => format!("DataRangeAtom({} {})", self.r(*r), d(v)),
                    Atom::Object(p, x, y) => {
                        format!("ObjectPropertyAtom({} {} {})", self.p(*p), i(x), i(y))
                    }
                    Atom::Data(p, x, v) => {
                        format!("DataPropertyAtom({} {} {})", self.n(*p), i(x), d(v))
                    }
                    Atom::Same(x, y) => format!("SameIndividualAtom({} {})", i(x), i(y)),
                    Atom::Different(x, y) => {
                        format!("DifferentIndividualsAtom({} {})", i(x), i(y))
                    }
                    Atom::BuiltIn(b, xs) => format!(
                        "BuiltInAtom({} {})",
                        self.n(*b),
                        xs.iter().map(d).collect::<Vec<_>>().join(" ")
                    ),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "DLSafeRule(Body({}) Head({}))",
            atoms(&rule.body),
            atoms(&rule.head)
        )
    }

    fn axiom(&self, axiom: &Axiom) -> String {
        match axiom {
            Axiom::Declaration(kind, t) => {
                let kind = match kind {
                    EntityKind::Class => "Class",
                    EntityKind::ObjectProperty => "ObjectProperty",
                    EntityKind::DataProperty => "DataProperty",
                    EntityKind::AnnotationProperty => "AnnotationProperty",
                    EntityKind::Datatype => "Datatype",
                    EntityKind::NamedIndividual => "NamedIndividual",
                };
                format!("Declaration({kind}({}))", self.n(*t))
            }
            Axiom::SubClassOf(a, b) => format!("SubClassOf({} {})", self.c(*a), self.c(*b)),
            Axiom::EquivalentClasses(xs) => format!("EquivalentClasses({})", self.cs(xs)),
            Axiom::DisjointClasses(xs) => format!("DisjointClasses({})", self.cs(xs)),
            Axiom::DisjointUnion(c, xs) => format!("DisjointUnion({} {})", self.c(*c), self.cs(xs)),
            Axiom::SubObjectPropertyOf(chain, sup) => match chain[..] {
                [sub] => format!("SubObjectPropertyOf({} {})", self.p(sub), self.p(*sup)),
                _ => format!(
                    "SubObjectPropertyOf(ObjectPropertyChain({}) {})",
                    self.ps(chain),
                    self.p(*sup)
                ),
            },
            Axiom::EquivalentObjectProperties(ps) => {
                format!("EquivalentObjectProperties({})", self.ps(ps))
            }
            Axiom::DisjointObjectProperties(ps) => {
                format!("DisjointObjectProperties({})", self.ps(ps))
            }
            Axiom::InverseObjectProperties(a, b) => {
                format!("InverseObjectProperties({} {})", self.p(*a), self.p(*b))
            }
            Axiom::ObjectPropertyDomain(p, x) => {
                format!("ObjectPropertyDomain({} {})", self.p(*p), self.c(*x))
            }
            Axiom::ObjectPropertyRange(p, x) => {
                format!("ObjectPropertyRange({} {})", self.p(*p), self.c(*x))
            }
            Axiom::ObjectCharacteristic(kind, p) => {
                let kind = match kind {
                    Characteristic::Functional => "FunctionalObjectProperty",
                    Characteristic::InverseFunctional => "InverseFunctionalObjectProperty",
                    Characteristic::Reflexive => "ReflexiveObjectProperty",
                    Characteristic::Irreflexive => "IrreflexiveObjectProperty",
                    Characteristic::Symmetric => "SymmetricObjectProperty",
                    Characteristic::Asymmetric => "AsymmetricObjectProperty",
                    Characteristic::Transitive => "TransitiveObjectProperty",
                };
                format!("{kind}({})", self.p(*p))
            }
            Axiom::SubDataPropertyOf(a, b) => {
                format!("SubDataPropertyOf({} {})", self.n(*a), self.n(*b))
            }
            Axiom::EquivalentDataProperties(ps) => {
                format!("EquivalentDataProperties({})", self.terms(ps))
            }
            Axiom::DisjointDataProperties(ps) => {
                format!("DisjointDataProperties({})", self.terms(ps))
            }
            Axiom::DataPropertyDomain(p, x) => {
                format!("DataPropertyDomain({} {})", self.n(*p), self.c(*x))
            }
            Axiom::DataPropertyRange(p, r) => {
                format!("DataPropertyRange({} {})", self.n(*p), self.r(*r))
            }
            Axiom::FunctionalDataProperty(p) => format!("FunctionalDataProperty({})", self.n(*p)),
            Axiom::DatatypeDefinition(d, r) => {
                format!("DatatypeDefinition({} {})", self.n(*d), self.r(*r))
            }
            Axiom::HasKey(c, ops, dps) => format!(
                "HasKey({} ({}) ({}))",
                self.c(*c),
                self.ps(ops),
                self.terms(dps)
            ),
            Axiom::ClassAssertion(c, a) => format!("ClassAssertion({} {})", self.c(*c), self.n(*a)),
            Axiom::ObjectPropertyAssertion(p, a, b) => format!(
                "ObjectPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*b)
            ),
            Axiom::NegativeObjectPropertyAssertion(p, a, b) => format!(
                "NegativeObjectPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*b)
            ),
            Axiom::DataPropertyAssertion(p, a, v) => format!(
                "DataPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*v)
            ),
            Axiom::NegativeDataPropertyAssertion(p, a, v) => format!(
                "NegativeDataPropertyAssertion({} {} {})",
                self.n(*p),
                self.n(*a),
                self.n(*v)
            ),
            Axiom::SameIndividual(xs) => format!("SameIndividual({})", self.terms(xs)),
            Axiom::DifferentIndividuals(xs) => format!("DifferentIndividuals({})", self.terms(xs)),
        }
    }
}
