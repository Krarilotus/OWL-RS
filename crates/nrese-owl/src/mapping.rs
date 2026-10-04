//! The reverse OWL 2 RDF mapping (W3C *OWL 2 Mapping to RDF Graphs*, §3): the structural
//! model of an ontology from its triples, every axiom with the triples (and graphs) it
//! was read from, and a diagnostic for everything that isn't well-formed OWL 2 DL, never
//! a silent drop.
//!
//! The source's terms are read by id; only literals that carry numbers the mapping needs
//! (cardinalities, `owl:hasSelf`) are decoded ([`Terms`]).
//!
//! Properties are object or data properties by their declarations; an undeclared one by
//! its use (a literal object makes it a data property), with a diagnostic. Blank nodes
//! that build expressions are read once; one used by two expressions is reported.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::diagnostics::Diagnostic;
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, EntityKind, ExprId, Interner, ObjProp, RangeId,
    Term, canonical,
};
use crate::vocab::Vocabulary;

/// What a term of the source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermKind {
    Iri,
    Blank,
    Literal,
}

/// The source's terms, as far as the mapping needs them.
pub trait Terms {
    fn kind(&self, term: Term) -> TermKind;
    /// A literal's lexical form.
    fn lexical(&self, term: Term) -> Option<String>;
    /// The id of an IRI, if the source has it.
    fn iri(&self, iri: &str) -> Option<Term>;
}

/// A statement of the source: a triple and its graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Statement {
    pub triple: [Term; 3],
    pub graph: Term,
}

/// Where an axiom was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub graph: Term,
    /// The triples of its RDF form: its own and those of the expressions it was the first
    /// to read.
    pub triples: Vec<[Term; 3]>,
}

/// An ontology read from RDF.
#[derive(Debug, Clone, Default)]
pub struct Ontology {
    pub classes: Interner<ClassExpr>,
    pub ranges: Interner<DataRange>,
    /// Every axiom once, sorted.
    pub axioms: Vec<Axiom>,
    /// Per axiom (parallel to `axioms`): where it was read from (once per graph and form).
    pub sources: Vec<Vec<Source>>,
    pub diagnostics: Vec<Diagnostic>,
    /// Annotation statements and the ontology header, read past.
    pub annotations: usize,
    /// The source's ids of the properties with a fixed meaning.
    pub builtin: BuiltinProperties,
}

/// The properties OWL 2 gives a fixed meaning, by their ids in the source (`None` where
/// the source doesn't have the IRI): the normalisation gives them their semantics or
/// reports what uses them as unsupported, never reads them as ordinary properties.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuiltinProperties {
    /// `owl:topObjectProperty`: every pair of individuals.
    pub top_object: Option<Term>,
    /// `owl:bottomObjectProperty`: no pair.
    pub bottom_object: Option<Term>,
    /// `owl:topDataProperty`: every individual with every data value.
    pub top_data: Option<Term>,
    /// `owl:bottomDataProperty`: no pair.
    pub bottom_data: Option<Term>,
}

impl BuiltinProperties {
    /// Their ids in a source, by its vocabulary.
    pub fn of(v: &Vocabulary) -> Self {
        Self {
            top_object: v.owl_top_object_property,
            bottom_object: v.owl_bottom_object_property,
            top_data: v.owl_top_data_property,
            bottom_data: v.owl_bottom_data_property,
        }
    }
}

impl Ontology {
    pub fn class(&self, id: ExprId) -> &ClassExpr {
        self.classes.get(id.0)
    }

    pub fn range(&self, id: RangeId) -> &DataRange {
        self.ranges.get(id.0)
    }

    /// The axioms of a kind, by a test.
    pub fn axioms_where(&self, test: impl Fn(&Axiom) -> bool) -> impl Iterator<Item = &Axiom> {
        self.axioms.iter().filter(move |a| test(a))
    }
}

/// Reads the ontology of `statements` (any graphs, the union of them).
pub fn read(statements: &[Statement], terms: &dyn Terms) -> Ontology {
    let vocabulary = Vocabulary::new(&|iri| terms.iri(iri));
    let mut reader = Reader::new(statements, terms, vocabulary);
    reader.declarations();
    reader.axioms();
    reader.finish()
}

struct Reader<'a> {
    v: Vocabulary,
    terms: &'a dyn Terms,
    statements: &'a [Statement],
    /// Per subject: (predicate, object, graph).
    by_subject: HashMap<Term, Vec<(Term, Term, Term)>>,
    declared: BTreeMap<Term, Vec<EntityKind>>,
    /// Properties used with literal objects.
    used_with_literals: HashSet<Term>,
    used_with_nodes: HashSet<Term>,
    class_memo: HashMap<Term, Option<ExprId>>,
    range_memo: HashMap<Term, Option<RangeId>>,
    /// Blank nodes read as operands, and how often.
    uses: HashMap<Term, u32>,
    /// Triples that belong to a structure (an expression, a list, an n-ary axiom).
    structural: HashSet<[Term; 3]>,
    /// The structural triples read for the axiom being read.
    collected: Vec<[Term; 3]>,
    graph: Term,
    classes: Interner<ClassExpr>,
    ranges: Interner<DataRange>,
    axioms: BTreeMap<Axiom, Vec<Source>>,
    diagnostics: Vec<Diagnostic>,
    reported: HashSet<(Term, &'static str)>,
    annotations: usize,
}

/// The classes whose instances are declared entities: OWL 2's, and the RDFS and OWL 1
/// synonyms the mapping reads as them (`rdfs:Class`, `owl:DataRange`).
fn declaration_kinds(v: &Vocabulary) -> [(Option<Term>, EntityKind); 8] {
    [
        (v.owl_class, EntityKind::Class),
        (v.rdfs_class, EntityKind::Class),
        (v.owl_object_property, EntityKind::ObjectProperty),
        (v.owl_datatype_property, EntityKind::DataProperty),
        (v.owl_annotation_property, EntityKind::AnnotationProperty),
        (v.rdfs_datatype, EntityKind::Datatype),
        (v.owl_data_range, EntityKind::Datatype),
        (v.owl_named_individual, EntityKind::NamedIndividual),
    ]
}

/// `Some(id) == Some(x)` without matching an absent term.
fn is(id: Term, term: Option<Term>) -> bool {
    term == Some(id)
}

impl<'a> Reader<'a> {
    fn new(statements: &'a [Statement], terms: &'a dyn Terms, v: Vocabulary) -> Self {
        let mut by_subject: HashMap<Term, Vec<(Term, Term, Term)>> = HashMap::new();
        let mut used_with_literals = HashSet::new();
        let mut used_with_nodes = HashSet::new();
        for statement in statements {
            let [s, p, o] = statement.triple;
            by_subject
                .entry(s)
                .or_default()
                .push((p, o, statement.graph));
            match terms.kind(o) {
                TermKind::Literal => used_with_literals.insert(p),
                _ => used_with_nodes.insert(p),
            };
        }
        Self {
            v,
            terms,
            statements,
            by_subject,
            declared: BTreeMap::new(),
            used_with_literals,
            used_with_nodes,
            class_memo: HashMap::new(),
            range_memo: HashMap::new(),
            uses: HashMap::new(),
            structural: HashSet::new(),
            collected: Vec::new(),
            graph: 0,
            classes: Interner::default(),
            ranges: Interner::default(),
            axioms: BTreeMap::new(),
            diagnostics: Vec::new(),
            reported: HashSet::new(),
            annotations: 0,
        }
    }

    fn report(&mut self, diagnostic: Diagnostic) {
        let key = diagnostic.key();
        if self.reported.insert(key) {
            self.diagnostics.push(diagnostic);
        }
    }

    fn kind(&self, term: Term) -> TermKind {
        self.terms.kind(term)
    }

    /// The objects of `subject`'s `predicate` statements.
    fn objects(&self, subject: Term, predicate: Option<Term>) -> Vec<Term> {
        let Some(predicate) = predicate else {
            return Vec::new();
        };
        self.by_subject
            .get(&subject)
            .into_iter()
            .flatten()
            .filter(|&&(p, _, _)| p == predicate)
            .map(|&(_, o, _)| o)
            .collect()
    }

    /// The single object of `subject`'s `predicate`, collected as structural.
    fn one(&mut self, subject: Term, predicate: Option<Term>) -> Option<Term> {
        let objects = self.objects(subject, predicate);
        let &object = objects.first()?;
        if objects.len() > 1 {
            self.report(Diagnostic::Malformed {
                node: subject,
                what: "a structure property given twice",
            });
        }
        self.collect([subject, predicate?, object]);
        Some(object)
    }

    fn has_type(&self, subject: Term, class: Option<Term>) -> bool {
        class.is_some_and(|class| self.objects(subject, self.v.rdf_type).contains(&class))
    }

    fn collect(&mut self, triple: [Term; 3]) {
        self.structural.insert(triple);
        self.collected.push(triple);
    }

    fn declared_as(&self, term: Term, kind: EntityKind) -> bool {
        self.declared
            .get(&term)
            .is_some_and(|kinds| kinds.contains(&kind))
    }

    // Declarations ---------------------------------------------------------------------

    fn declarations(&mut self) {
        let v = self.v;
        let kinds = declaration_kinds(&v);
        // Characteristics other than functional make an object property.
        let object_only = [
            v.owl_inverse_functional,
            v.owl_reflexive,
            v.owl_irreflexive,
            v.owl_symmetric,
            v.owl_asymmetric,
            v.owl_transitive,
        ];
        let statements = self.statements;
        for statement in statements {
            let [s, p, o] = statement.triple;
            if !is(p, v.rdf_type) || self.kind(s) != TermKind::Iri {
                continue;
            }
            for &(class, kind) in &kinds {
                if is(o, class) {
                    let entry = self.declared.entry(s).or_default();
                    if !entry.contains(&kind) {
                        entry.push(kind);
                    }
                }
            }
            if object_only.iter().any(|&c| is(o, c)) {
                let entry = self.declared.entry(s).or_default();
                if !entry.contains(&EntityKind::ObjectProperty) {
                    entry.push(EntityKind::ObjectProperty);
                }
            }
        }
        // The built-in properties are declared by OWL 2 itself.
        for (property, kind) in [
            (v.owl_top_object_property, EntityKind::ObjectProperty),
            (v.owl_bottom_object_property, EntityKind::ObjectProperty),
            (v.owl_top_data_property, EntityKind::DataProperty),
            (v.owl_bottom_data_property, EntityKind::DataProperty),
        ] {
            if let Some(property) = property {
                let entry = self.declared.entry(property).or_default();
                if !entry.contains(&kind) {
                    entry.push(kind);
                }
            }
        }
        for (&term, kinds) in &self.declared {
            if kinds.contains(&EntityKind::ObjectProperty)
                && kinds.contains(&EntityKind::DataProperty)
            {
                self.diagnostics
                    .push(Diagnostic::AmbiguousProperty { property: term });
            }
        }
    }

    /// Whether `property` is a data property: declared so, or undeclared and used with
    /// literals.
    fn is_data_property(&mut self, property: Term) -> bool {
        if self.declared_as(property, EntityKind::DataProperty) {
            return !self.declared_as(property, EntityKind::ObjectProperty);
        }
        if self.declared_as(property, EntityKind::ObjectProperty)
            || self.kind(property) != TermKind::Iri
        {
            return false;
        }
        self.report(Diagnostic::UndeclaredProperty { property });
        self.used_with_literals.contains(&property) && !self.used_with_nodes.contains(&property)
    }

    // Lists ----------------------------------------------------------------------------

    /// The members of the list starting at `head`.
    fn list(&mut self, head: Term) -> Option<Vec<Term>> {
        let mut members = Vec::new();
        let mut seen = HashSet::new();
        let mut cell = head;
        loop {
            if is(cell, self.v.nil) {
                return Some(members);
            }
            if self.kind(cell) != TermKind::Blank || !seen.insert(cell) {
                self.report(Diagnostic::BrokenList { head });
                return None;
            }
            let (Some(first), Some(rest)) =
                (self.one(cell, self.v.first), self.one(cell, self.v.rest))
            else {
                self.report(Diagnostic::BrokenList { head });
                return None;
            };
            members.push(first);
            cell = rest;
        }
    }

    // Expressions ----------------------------------------------------------------------

    fn used(&mut self, blank: Term) {
        let uses = self.uses.entry(blank).or_default();
        *uses += 1;
        if *uses == 2 {
            self.report(Diagnostic::SharedBlankNode { node: blank });
        }
    }

    fn intern_class(&mut self, expr: ClassExpr) -> ExprId {
        ExprId(self.classes.intern(expr))
    }

    fn intern_range(&mut self, range: DataRange) -> RangeId {
        RangeId(self.ranges.intern(range))
    }

    /// The object property expression `term` stands for.
    fn object_property(&mut self, term: Term) -> Option<ObjProp> {
        match self.kind(term) {
            TermKind::Iri => Some(ObjProp::Named(term)),
            TermKind::Blank => {
                let Some(inverse) = self.one(term, self.v.owl_inverse_of) else {
                    self.report(Diagnostic::Malformed {
                        node: term,
                        what: "a blank node where a property belongs, without owl:inverseOf",
                    });
                    return None;
                };
                if self.kind(inverse) != TermKind::Iri {
                    self.report(Diagnostic::Malformed {
                        node: term,
                        what: "an inverse of an inverse",
                    });
                    return None;
                }
                Some(ObjProp::Inverse(inverse))
            }
            TermKind::Literal => {
                self.report(Diagnostic::Malformed {
                    node: term,
                    what: "a literal where a property belongs",
                });
                None
            }
        }
    }

    /// The class expression `term` stands for.
    fn class(&mut self, term: Term) -> Option<ExprId> {
        match self.kind(term) {
            TermKind::Iri => Some(if is(term, self.v.owl_thing) {
                self.intern_class(ClassExpr::Thing)
            } else if is(term, self.v.owl_nothing) {
                self.intern_class(ClassExpr::Nothing)
            } else {
                self.intern_class(ClassExpr::Class(term))
            }),
            TermKind::Literal => {
                self.report(Diagnostic::Malformed {
                    node: term,
                    what: "a literal where a class expression belongs",
                });
                None
            }
            TermKind::Blank => {
                self.used(term);
                if let Some(&known) = self.class_memo.get(&term) {
                    return known;
                }
                // Guards against a cycle through this node.
                self.class_memo.insert(term, None);
                let found = self.blank_class(term);
                if found.is_none() {
                    // Whatever made it unreadable may not have been reported; the axiom
                    // it belongs to is left out, never silently.
                    self.report(Diagnostic::Malformed {
                        node: term,
                        what: "a class expression that can't be read",
                    });
                }
                self.class_memo.insert(term, found);
                found
            }
        }
    }

    fn classes(&mut self, terms: &[Term]) -> Option<Vec<ExprId>> {
        terms.iter().map(|&t| self.class(t)).collect()
    }

    fn blank_class(&mut self, node: Term) -> Option<ExprId> {
        let v = self.v;
        for class_type in [v.owl_class, v.rdfs_class, v.owl_restriction] {
            if self.has_type(node, class_type) {
                self.collect([node, v.rdf_type?, class_type?]);
            }
        }
        for operator in [
            v.owl_intersection_of,
            v.owl_union_of,
            v.owl_complement_of,
            v.owl_one_of,
        ] {
            if let Some(operand) = self.one(node, operator) {
                return self.set_operator(operator?, operand);
            }
        }
        if !self.objects(node, v.owl_on_properties).is_empty() {
            self.report(Diagnostic::Unsupported {
                node,
                what: "n-ary data restrictions (owl:onProperties)",
            });
            return None;
        }
        let Some(property) = self.one(node, v.owl_on_property) else {
            self.report(Diagnostic::Malformed {
                node,
                what: "a class expression without an operator",
            });
            return None;
        };
        self.restriction(node, property)
    }

    /// The expression of a set operator (`owl:intersectionOf`, `owl:unionOf`,
    /// `owl:complementOf`, `owl:oneOf`) applied to `operand`.
    fn set_operator(&mut self, operator: Term, operand: Term) -> Option<ExprId> {
        let v = self.v;
        if is(operator, v.owl_complement_of) {
            let operand = self.class(operand)?;
            return Some(self.intern_class(ClassExpr::Not(operand)));
        }
        let members = self.list(operand)?;
        if is(operator, v.owl_one_of) {
            return Some(self.intern_class(ClassExpr::OneOf(canonical(members))));
        }
        let operands = canonical(self.classes(&members)?);
        Some(self.intern_class(if is(operator, v.owl_union_of) {
            ClassExpr::Or(operands)
        } else {
            ClassExpr::And(operands)
        }))
    }

    /// A restriction on `property`.
    fn restriction(&mut self, node: Term, property: Term) -> Option<ExprId> {
        let v = self.v;
        let data = self.kind(property) == TermKind::Iri && self.is_data_property(property);
        let number = |reader: &mut Self, predicate: Option<Term>| -> Option<Option<u32>> {
            let literal = reader.one(node, predicate)?;
            let parsed = reader
                .terms
                .lexical(literal)
                .and_then(|text| text.trim().parse::<u32>().ok());
            if parsed.is_none() {
                reader.report(Diagnostic::Malformed {
                    node,
                    what: "a cardinality that isn't a non-negative integer",
                });
            }
            Some(parsed)
        };
        if data {
            // `rdfs:Literal`, whether or not the source has a term for its IRI (a
            // cardinality over a data property was dropped where it hadn't).
            let literal_range = |reader: &mut Self| -> Option<RangeId> {
                Some(reader.intern_range(DataRange::Literal))
            };
            if let Some(filler) = self.one(node, v.owl_some_values_from) {
                let range = self.range(filler)?;
                return Some(self.intern_class(ClassExpr::DataSome(property, range)));
            }
            if let Some(filler) = self.one(node, v.owl_all_values_from) {
                let range = self.range(filler)?;
                return Some(self.intern_class(ClassExpr::DataAll(property, range)));
            }
            if let Some(value) = self.one(node, v.owl_has_value) {
                return Some(self.intern_class(ClassExpr::DataHasValue(property, value)));
            }
            for (predicate, qualified, make) in [
                (
                    v.owl_min_cardinality,
                    v.owl_min_qualified,
                    ClassExpr::DataMin as fn(u32, Term, RangeId) -> ClassExpr,
                ),
                (
                    v.owl_max_cardinality,
                    v.owl_max_qualified,
                    ClassExpr::DataMax,
                ),
                (v.owl_cardinality, v.owl_qualified, ClassExpr::DataExact),
            ] {
                if let Some(n) = number(self, predicate) {
                    let range = literal_range(self)?;
                    return Some(self.intern_class(make(n?, property, range)));
                }
                if let Some(n) = number(self, qualified) {
                    let filler = self.one(node, v.owl_on_data_range)?;
                    let range = self.range(filler)?;
                    return Some(self.intern_class(make(n?, property, range)));
                }
            }
        } else {
            let property = self.object_property(property)?;
            if let Some(filler) = self.one(node, v.owl_some_values_from) {
                let class = self.class(filler)?;
                return Some(self.intern_class(ClassExpr::Some(property, class)));
            }
            if let Some(filler) = self.one(node, v.owl_all_values_from) {
                let class = self.class(filler)?;
                return Some(self.intern_class(ClassExpr::All(property, class)));
            }
            if let Some(value) = self.one(node, v.owl_has_value) {
                return Some(self.intern_class(ClassExpr::HasValue(property, value)));
            }
            if let Some(flag) = self.one(node, v.owl_has_self) {
                let yes = self
                    .terms
                    .lexical(flag)
                    .is_some_and(|t| t == "true" || t == "1");
                if !yes {
                    self.report(Diagnostic::Malformed {
                        node,
                        what: "owl:hasSelf other than true",
                    });
                    return None;
                }
                return Some(self.intern_class(ClassExpr::HasSelf(property)));
            }
            for (predicate, qualified, make) in [
                (
                    v.owl_min_cardinality,
                    v.owl_min_qualified,
                    ClassExpr::Min as fn(u32, ObjProp, ExprId) -> ClassExpr,
                ),
                (v.owl_max_cardinality, v.owl_max_qualified, ClassExpr::Max),
                (v.owl_cardinality, v.owl_qualified, ClassExpr::Exact),
            ] {
                if let Some(n) = number(self, predicate) {
                    let thing = self.intern_class(ClassExpr::Thing);
                    return Some(self.intern_class(make(n?, property, thing)));
                }
                if let Some(n) = number(self, qualified) {
                    let filler = self.one(node, v.owl_on_class)?;
                    let class = self.class(filler)?;
                    return Some(self.intern_class(make(n?, property, class)));
                }
            }
        }
        self.report(Diagnostic::Malformed {
            node,
            what: "a restriction without a restriction property",
        });
        None
    }

    /// The data range `term` stands for.
    fn range(&mut self, term: Term) -> Option<RangeId> {
        match self.kind(term) {
            TermKind::Iri if is(term, self.v.rdfs_literal) => {
                Some(self.intern_range(DataRange::Literal))
            }
            TermKind::Iri => Some(self.intern_range(DataRange::Datatype(term))),
            TermKind::Literal => {
                self.report(Diagnostic::Malformed {
                    node: term,
                    what: "a literal where a data range belongs",
                });
                None
            }
            TermKind::Blank => {
                self.used(term);
                if let Some(&known) = self.range_memo.get(&term) {
                    return known;
                }
                self.range_memo.insert(term, None);
                let found = self.blank_range(term);
                if found.is_none() {
                    self.report(Diagnostic::Malformed {
                        node: term,
                        what: "a data range that can't be read",
                    });
                }
                self.range_memo.insert(term, found);
                found
            }
        }
    }

    fn blank_range(&mut self, node: Term) -> Option<RangeId> {
        let v = self.v;
        for range_type in [v.rdfs_datatype, v.owl_data_range] {
            if self.has_type(node, range_type) {
                self.collect([node, v.rdf_type?, range_type?]);
            }
        }
        let ranges = |reader: &mut Self, list: Term| -> Option<Vec<RangeId>> {
            let members = reader.list(list)?;
            members.iter().map(|&m| reader.range(m)).collect()
        };
        if let Some(list) = self.one(node, v.owl_intersection_of) {
            let operands = ranges(self, list)?;
            return Some(self.intern_range(DataRange::And(canonical(operands))));
        }
        if let Some(list) = self.one(node, v.owl_union_of) {
            let operands = ranges(self, list)?;
            return Some(self.intern_range(DataRange::Or(canonical(operands))));
        }
        if let Some(operand) = self.one(node, v.owl_datatype_complement_of) {
            let operand = self.range(operand)?;
            return Some(self.intern_range(DataRange::Not(operand)));
        }
        if let Some(list) = self.one(node, v.owl_one_of) {
            let literals = self.list(list)?;
            return Some(self.intern_range(DataRange::OneOf(canonical(literals))));
        }
        if let Some(datatype) = self.one(node, v.owl_on_datatype) {
            let list = self.one(node, v.owl_with_restrictions)?;
            let mut facets = Vec::new();
            for restriction in self.list(list)? {
                let pairs = self
                    .by_subject
                    .get(&restriction)
                    .cloned()
                    .unwrap_or_default();
                let [(facet, value, _)] = pairs[..] else {
                    self.report(Diagnostic::Malformed {
                        node: restriction,
                        what: "a facet restriction with other than one facet",
                    });
                    return None;
                };
                self.collect([restriction, facet, value]);
                facets.push((facet, value));
            }
            return Some(self.intern_range(DataRange::Restriction(datatype, canonical(facets))));
        }
        self.report(Diagnostic::Malformed {
            node,
            what: "a data range without an operator",
        });
        None
    }

    // Axioms ---------------------------------------------------------------------------

    fn add(&mut self, axiom: Axiom, triple: [Term; 3]) {
        let mut triples = vec![triple];
        triples.append(&mut self.collected);
        let source = Source {
            graph: self.graph,
            triples,
        };
        let sources = self.axioms.entry(axiom).or_default();
        if !sources.contains(&source) {
            sources.push(source);
        }
    }

    fn axioms(&mut self) {
        let statements = self.statements;
        // n-ary axioms on blank nodes first, so that their structure is known when the
        // statements are walked.
        for statement in statements {
            let [s, p, o] = statement.triple;
            if is(p, self.v.rdf_type) && self.kind(s) == TermKind::Blank {
                self.graph = statement.graph;
                self.nary(s, o, statement.triple);
            }
        }
        // Then: statements on IRIs; axioms on blank nodes (a complex left-hand side);
        // the rest on blank nodes (anonymous individuals), once every structure has
        // taken its statements.
        let v = self.v;
        let axiom_predicates = [
            v.rdfs_sub_class_of,
            v.owl_equivalent_class,
            v.owl_disjoint_with,
            v.rdfs_sub_property_of,
            v.owl_property_chain_axiom,
            v.owl_equivalent_property,
            v.owl_property_disjoint_with,
            v.rdfs_domain,
            v.rdfs_range,
            v.owl_has_key,
        ];
        for pass in 0..3 {
            for statement in statements {
                let triple = statement.triple;
                let blank = self.kind(triple[0]) == TermKind::Blank;
                let axiom = axiom_predicates.contains(&Some(triple[1]));
                let now = match pass {
                    0 => !blank,
                    1 => blank && axiom,
                    _ => blank && !axiom,
                };
                if !now || self.structural.contains(&triple) {
                    continue;
                }
                self.graph = statement.graph;
                self.collected.clear();
                self.statement(triple);
                self.collected.clear();
            }
        }
        // Structure statements on blank nodes that no axiom took.
        for statement in statements {
            let [s, p, _] = statement.triple;
            if self.kind(s) == TermKind::Blank
                && !self.structural.contains(&statement.triple)
                && (self.is_vocabulary(p) || is(p, self.v.owl_inverse_of))
                && !is(p, self.v.owl_imports)
            {
                self.report(Diagnostic::Unused { node: s });
            }
        }
    }

    /// An n-ary axiom whose node `node` has type `class`.
    fn nary(&mut self, node: Term, class: Term, triple: [Term; 3]) {
        let v = self.v;
        self.collected.clear();
        if is(class, v.owl_all_disjoint_classes) {
            self.collect(triple);
            let Some(list) = self.one(node, v.owl_members) else {
                return;
            };
            if let Some(members) = self.list(list)
                && let Some(classes) = self.classes(&members)
            {
                self.add(Axiom::DisjointClasses(canonical(classes)), triple);
            }
        } else if is(class, v.owl_all_disjoint_properties) {
            self.collect(triple);
            let Some(list) = self.one(node, v.owl_members) else {
                return;
            };
            if let Some(members) = self.list(list) {
                if members.iter().all(|&p| self.kind(p) == TermKind::Iri)
                    && members.iter().all(|&p| self.is_data_property(p))
                {
                    self.add(Axiom::DisjointDataProperties(canonical(members)), triple);
                } else if let Some(properties) = members
                    .iter()
                    .map(|&p| self.object_property(p))
                    .collect::<Option<Vec<_>>>()
                {
                    self.add(
                        Axiom::DisjointObjectProperties(canonical(properties)),
                        triple,
                    );
                }
            }
        } else if is(class, v.owl_all_different) {
            self.collect(triple);
            let list = self
                .one(node, v.owl_members)
                .or_else(|| self.one(node, v.owl_distinct_members));
            if let Some(list) = list
                && let Some(members) = self.list(list)
            {
                self.add(Axiom::DifferentIndividuals(canonical(members)), triple);
            }
        } else if is(class, v.owl_negative_property_assertion) {
            self.collect(triple);
            let (Some(source), Some(property)) = (
                self.one(node, v.owl_source_individual),
                self.one(node, v.owl_assertion_property),
            ) else {
                self.report(Diagnostic::Malformed {
                    node,
                    what: "a negative property assertion without its parts",
                });
                return;
            };
            if let Some(value) = self.one(node, v.owl_target_value) {
                self.add(
                    Axiom::NegativeDataPropertyAssertion(property, source, value),
                    triple,
                );
            } else if let Some(target) = self.one(node, v.owl_target_individual) {
                let axiom = match self.object_property(property) {
                    Some(ObjProp::Named(p)) => {
                        Axiom::NegativeObjectPropertyAssertion(p, source, target)
                    }
                    Some(ObjProp::Inverse(p)) => {
                        Axiom::NegativeObjectPropertyAssertion(p, target, source)
                    }
                    None => return,
                };
                self.add(axiom, triple);
            }
        } else if is(class, v.owl_axiom) || is(class, v.owl_annotation) {
            // An annotated axiom's reification (the axiom itself is stated as well), or an
            // annotation's annotation: no logical meaning.
            for (p, o, _) in self.by_subject.get(&node).cloned().unwrap_or_default() {
                self.structural.insert([node, p, o]);
                self.annotations += 1;
            }
        }
    }

    /// The axiom (or declaration, or annotation) one statement makes.
    fn statement(&mut self, triple: [Term; 3]) {
        let [s, p, o] = triple;
        let v = self.v;
        if is(p, v.rdf_type) {
            self.typing(s, o, triple);
        } else if is(p, v.rdfs_sub_class_of) {
            if let (Some(sub), Some(sup)) = (self.class(s), self.class(o)) {
                self.add(Axiom::SubClassOf(sub, sup), triple);
            }
        } else if is(p, v.owl_equivalent_class) {
            if self.declared_as(s, EntityKind::Datatype) || self.has_type(o, v.rdfs_datatype) {
                if let Some(range) = self.range(o) {
                    self.add(Axiom::DatatypeDefinition(s, range), triple);
                }
            } else if let (Some(a), Some(b)) = (self.class(s), self.class(o)) {
                self.add(Axiom::EquivalentClasses(canonical(vec![a, b])), triple);
            }
        } else if is(p, v.owl_disjoint_with) {
            if let (Some(a), Some(b)) = (self.class(s), self.class(o)) {
                self.add(Axiom::DisjointClasses(canonical(vec![a, b])), triple);
            }
        } else if is(p, v.owl_disjoint_union_of) {
            if let Some(members) = self.list(o)
                && let Some(classes) = self.classes(&members)
            {
                self.add(Axiom::DisjointUnion(s, canonical(classes)), triple);
            }
        } else if is(p, v.rdfs_sub_property_of) {
            if self.declared_as(s, EntityKind::AnnotationProperty) {
                self.annotations += 1;
            } else if self.kind(s) == TermKind::Iri && self.is_data_property(s) {
                self.add(Axiom::SubDataPropertyOf(s, o), triple);
            } else if let (Some(sub), Some(sup)) =
                (self.object_property(s), self.object_property(o))
            {
                self.add(Axiom::SubObjectPropertyOf(vec![sub], sup), triple);
            }
        } else if is(p, v.owl_property_chain_axiom) {
            let chain = self.list(o).and_then(|members| {
                members
                    .iter()
                    .map(|&m| self.object_property(m))
                    .collect::<Option<Vec<_>>>()
            });
            if let (Some(chain), Some(sup)) = (chain, self.object_property(s)) {
                self.add(Axiom::SubObjectPropertyOf(chain, sup), triple);
            }
        } else if is(p, v.owl_equivalent_property) || is(p, v.owl_property_disjoint_with) {
            let equivalent = is(p, v.owl_equivalent_property);
            if self.kind(s) == TermKind::Iri && self.is_data_property(s) {
                let pair = canonical(vec![s, o]);
                self.add(
                    if equivalent {
                        Axiom::EquivalentDataProperties(pair)
                    } else {
                        Axiom::DisjointDataProperties(pair)
                    },
                    triple,
                );
            } else if let (Some(a), Some(b)) = (self.object_property(s), self.object_property(o)) {
                let pair = canonical(vec![a, b]);
                self.add(
                    if equivalent {
                        Axiom::EquivalentObjectProperties(pair)
                    } else {
                        Axiom::DisjointObjectProperties(pair)
                    },
                    triple,
                );
            }
        } else if is(p, v.owl_inverse_of) {
            if self.kind(s) == TermKind::Iri
                && let (Some(a), Some(b)) = (self.object_property(s), self.object_property(o))
            {
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                self.add(Axiom::InverseObjectProperties(a, b), triple);
            }
        } else if is(p, v.rdfs_domain) || is(p, v.rdfs_range) {
            let domain = is(p, v.rdfs_domain);
            if self.declared_as(s, EntityKind::AnnotationProperty) {
                self.annotations += 1;
            } else if self.kind(s) == TermKind::Iri && self.is_data_property(s) {
                if domain {
                    if let Some(class) = self.class(o) {
                        self.add(Axiom::DataPropertyDomain(s, class), triple);
                    }
                } else if let Some(range) = self.range(o) {
                    self.add(Axiom::DataPropertyRange(s, range), triple);
                }
            } else if let (Some(property), Some(class)) = (self.object_property(s), self.class(o)) {
                self.add(
                    if domain {
                        Axiom::ObjectPropertyDomain(property, class)
                    } else {
                        Axiom::ObjectPropertyRange(property, class)
                    },
                    triple,
                );
            }
        } else if is(p, v.owl_has_key) {
            if let (Some(class), Some(members)) = (self.class(s), self.list(o)) {
                let (mut objects, mut datas) = (Vec::new(), Vec::new());
                for member in members {
                    if self.kind(member) == TermKind::Iri && self.is_data_property(member) {
                        datas.push(member);
                    } else if let Some(property) = self.object_property(member) {
                        objects.push(property);
                    }
                }
                self.add(
                    Axiom::HasKey(class, canonical(objects), canonical(datas)),
                    triple,
                );
            }
        } else if is(p, v.owl_same_as) {
            self.add(Axiom::SameIndividual(canonical(vec![s, o])), triple);
        } else if is(p, v.owl_different_from) {
            self.add(Axiom::DifferentIndividuals(canonical(vec![s, o])), triple);
        } else if self.kind(s) == TermKind::Iri
            && [
                v.owl_intersection_of,
                v.owl_union_of,
                v.owl_complement_of,
                v.owl_one_of,
            ]
            .contains(&Some(p))
            && !self.declared_as(s, EntityKind::Datatype)
        {
            // OWL 1's class axioms on a named class: the class is equivalent to the
            // expression (read so by the OWL 2 mapping, for backward compatibility).
            if let (Some(named), Some(expr)) = (self.class(s), self.set_operator(p, o)) {
                self.add(
                    Axiom::EquivalentClasses(canonical(vec![named, expr])),
                    triple,
                );
            }
        } else if self.is_vocabulary(p) || is(p, v.owl_inverse_of) {
            // OWL vocabulary outside the forms above. On a blank node it is structure an
            // axiom may still read (judged after all of them); on an IRI it isn't OWL.
            if is(p, v.owl_imports) {
                self.annotations += 1;
            } else if self.kind(s) != TermKind::Blank {
                self.report(Diagnostic::NotOwl { triple });
            }
        } else if self.declared_as(p, EntityKind::AnnotationProperty)
            || self.is_builtin_annotation(p)
        {
            self.annotations += 1;
        } else if self.kind(o) == TermKind::Literal || self.is_data_property(p) {
            self.add(Axiom::DataPropertyAssertion(p, s, o), triple);
        } else {
            self.add(Axiom::ObjectPropertyAssertion(p, s, o), triple);
        }
    }

    /// An `rdf:type` statement: a declaration, a characteristic, or a class assertion.
    fn typing(&mut self, s: Term, o: Term, triple: [Term; 3]) {
        let v = self.v;
        let declarations = declaration_kinds(&v);
        if let Some(&(_, kind)) = declarations.iter().find(|(c, _)| is(o, *c)) {
            if self.kind(s) == TermKind::Iri {
                self.add(Axiom::Declaration(kind, s), triple);
            }
            return;
        }
        let characteristics = [
            (v.owl_inverse_functional, Characteristic::InverseFunctional),
            (v.owl_reflexive, Characteristic::Reflexive),
            (v.owl_irreflexive, Characteristic::Irreflexive),
            (v.owl_symmetric, Characteristic::Symmetric),
            (v.owl_asymmetric, Characteristic::Asymmetric),
            (v.owl_transitive, Characteristic::Transitive),
        ];
        if is(o, v.owl_functional) {
            if self.kind(s) == TermKind::Iri && self.is_data_property(s) {
                self.add(Axiom::FunctionalDataProperty(s), triple);
            } else if let Some(property) = self.object_property(s) {
                self.add(
                    Axiom::ObjectCharacteristic(Characteristic::Functional, property),
                    triple,
                );
            }
            return;
        }
        if let Some(&(_, characteristic)) = characteristics.iter().find(|(c, _)| is(o, *c)) {
            if let Some(property) = self.object_property(s) {
                self.add(
                    Axiom::ObjectCharacteristic(characteristic, property),
                    triple,
                );
            }
            return;
        }
        let header = [
            v.owl_ontology,
            v.owl_restriction,
            v.owl_all_disjoint_classes,
            v.owl_all_disjoint_properties,
            v.owl_all_different,
            v.owl_negative_property_assertion,
            v.owl_axiom,
            v.owl_annotation,
            v.owl_deprecated_class,
            v.owl_deprecated_property,
            v.owl_ontology_property,
            // The typing of properties and list cells in RDF: nothing under the direct
            // semantics (a property is typed by its declaration or its use).
            v.rdf_property,
            v.rdf_list,
        ];
        if header.iter().any(|&c| is(o, c)) {
            self.annotations += 1;
            return;
        }
        if let Some(class) = self.class(o) {
            self.add(Axiom::ClassAssertion(class, s), triple);
        }
    }

    fn is_vocabulary(&self, predicate: Term) -> bool {
        let v = self.v;
        [
            v.first,
            v.rest,
            v.owl_intersection_of,
            v.owl_union_of,
            v.owl_complement_of,
            v.owl_one_of,
            v.owl_on_property,
            v.owl_on_properties,
            v.owl_some_values_from,
            v.owl_all_values_from,
            v.owl_has_value,
            v.owl_has_self,
            v.owl_min_cardinality,
            v.owl_max_cardinality,
            v.owl_cardinality,
            v.owl_min_qualified,
            v.owl_max_qualified,
            v.owl_qualified,
            v.owl_on_class,
            v.owl_on_data_range,
            v.owl_datatype_complement_of,
            v.owl_on_datatype,
            v.owl_with_restrictions,
            v.owl_members,
            v.owl_distinct_members,
            v.owl_source_individual,
            v.owl_assertion_property,
            v.owl_target_individual,
            v.owl_target_value,
            v.owl_annotated_source,
            v.owl_annotated_property,
            v.owl_annotated_target,
            v.owl_imports,
        ]
        .contains(&Some(predicate))
    }

    /// `rdfs:label` and the other annotation properties OWL 2 builds in.
    fn is_builtin_annotation(&self, predicate: Term) -> bool {
        const BUILT_IN: [&str; 9] = [
            "http://www.w3.org/2000/01/rdf-schema#label",
            "http://www.w3.org/2000/01/rdf-schema#comment",
            "http://www.w3.org/2000/01/rdf-schema#seeAlso",
            "http://www.w3.org/2000/01/rdf-schema#isDefinedBy",
            "http://www.w3.org/2002/07/owl#deprecated",
            "http://www.w3.org/2002/07/owl#versionInfo",
            "http://www.w3.org/2002/07/owl#priorVersion",
            "http://www.w3.org/2002/07/owl#backwardCompatibleWith",
            "http://www.w3.org/2002/07/owl#incompatibleWith",
        ];
        BUILT_IN
            .iter()
            .any(|iri| self.terms.iri(iri) == Some(predicate))
            || self.terms.iri("http://www.w3.org/2002/07/owl#versionIRI") == Some(predicate)
    }

    fn finish(mut self) -> Ontology {
        let mut axioms = Vec::with_capacity(self.axioms.len());
        let mut sources = Vec::with_capacity(self.axioms.len());
        for (axiom, from) in std::mem::take(&mut self.axioms) {
            axioms.push(axiom);
            sources.push(from);
        }
        let mut ontology = Ontology {
            classes: self.classes,
            ranges: self.ranges,
            axioms,
            sources,
            diagnostics: self.diagnostics,
            annotations: self.annotations,
            builtin: BuiltinProperties::of(&self.v),
        };
        crate::diagnostics::check_global_restrictions(&mut ontology);
        ontology
    }
}
