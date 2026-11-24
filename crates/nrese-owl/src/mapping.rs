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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::diagnostics::{Diagnostic, Treatment, Treatments};
use crate::model::{
    Axiom, Characteristic, ClassExpr, DataRange, DataTerms, Disjoint, EntityKind, ExprId, Interner,
    Literal, ObjProp, RangeId, Term, canonical,
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

    /// A literal's datatype IRI (`rdf:langString` for a language-tagged one), or `None`
    /// where the source doesn't know it (every RDF literal has one). The datatype theory
    /// needs it for the literal's value; the profile check admits to OWL 2 RL a literal whose
    /// datatype isn't known (`crate::profile`), so each source says which it is.
    fn datatype(&self, term: Term) -> Option<String>;

    /// A literal's language tag, if it has one and the source gives it.
    fn language(&self, _term: Term) -> Option<String> {
        None
    }

    /// The text of an IRI, `None` for any other term: datatypes and facets beyond those the
    /// vocabulary knows, and the reserved vocabulary's names, which the reader tells by it.
    /// Required, with no default: a source that gave none would pass every reserved name.
    fn iri_text(&self, term: Term) -> Option<String>;
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
    /// The annotation properties (declared or built in) given a domain, a range, a
    /// super-property, or a sub-property of another kind (no OWL 2 DL axiom), read past
    /// among [`Self::annotations`]: OWL 2 DL gives these axioms no meaning, while the OWL 2
    /// RL rules over the triples (prp-dom, prp-rng, prp-spo1) read them. Sorted, each once.
    pub annotation_schema: Vec<Term>,
    /// The properties used as annotation properties: those of the statements read past as
    /// annotations (*Mapping to RDF* Table 10; in the functional syntax, annotation
    /// assertions and the annotations of axioms and the ontology), and the properties of
    /// the annotation axioms read past. Not the header's `owl:imports` and `owl:versionIRI`
    /// (Table 4), nor SHACL content. Where a logical axiom or an assertion also uses one, it is
    /// a name used as two kinds (`crate::typing`).
    pub annotation_uses: BTreeSet<Term>,
    /// The properties the annotation axioms read past name (Table 17): a domain's or range's
    /// property, a sub-property axiom's two. Among [`Self::annotation_uses`]; each is named an
    /// annotation property by an axiom (`crate::typing`).
    pub annotation_axioms: BTreeSet<Term>,
    /// The properties of the SHACL vocabulary ([`shacl_properties`]) whose statements were read
    /// as SHACL content, which has no OWL meaning (read past as an annotation is).
    pub shacl: BTreeSet<Term>,
    /// The source's ids of the properties with a fixed meaning.
    pub builtin: BuiltinProperties,
    /// The literals, datatypes and facets of the data axioms, as the source gives them.
    pub data: DataTerms,
    /// The individuals that are blank nodes (anonymous: keys don't apply to them).
    pub anonymous: BTreeSet<Term>,
    /// DL-safe rules (`crate::rules`; not OWL 2), each with a [`Diagnostic::Rule`].
    pub rules: Vec<crate::rules::Rule>,
    /// The first restriction whose max cardinality (qualified or not; the only ones the RL
    /// rules read) isn't written as the RDF mapping writes it,
    /// `"n"^^xsd:nonNegativeInteger` with `n` canonical (`1`, not `01`; Turtle's `1` is an
    /// xsd:integer): read here by its value, but OWL 2 RL's rules (cls-maxc1/2,
    /// cls-maxqc1-4) name the mapping's literal, so they don't read it.
    pub noncanonical_cardinality: Option<Term>,
    /// The first empty `owl:intersectionOf` or `owl:unionOf` list, `owl:Thing` or
    /// `owl:Nothing` (OWL 2 Mapping to RDF, Tables 15 and 18), which OWL 2 RL's rules don't
    /// read: they build no rule from it, so no clash from an instance of the empty union, and
    /// where a rule names `owl:Thing` they see a blank node.
    pub empty_list: Option<Term>,
    /// The first data property with a maximum written with a data range (`owl:onDataRange`,
    /// `rdfs:Literal` too), which OWL 2 RL's rules don't read: its range can't tell it from
    /// an unqualified maximum.
    pub qualified_data_max: Option<Term>,
    /// The first statement OWL 2 RL's rules don't read, by its subject, that the reader read
    /// as axioms they would read: a disjoint union whose operands read as one (no rule reads
    /// `owl:disjointUnionOf`), read as an equivalence and an empty operand. Every later
    /// rewrite of that kind sets it too.
    pub rules_unread: Option<Term>,
    /// The first list cell a statement outside its list names; the OWL 2 RL rules may equate
    /// it and read sequences the graph doesn't write.
    pub list_cell_outside: Option<Term>,
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

    /// The named individuals: declared, asserted about, or in a nominal (blank nodes, the
    /// anonymous individuals, left out, and so is a reserved name the reader left out,
    /// [`Diagnostic::Reserved`]: the nominal or `hasValue` of an axiom left out stays among the
    /// class expressions). Sorted. O(axioms + class expressions).
    pub fn named_individuals(&self) -> Vec<Term> {
        let mut out: Vec<Term> = Vec::new();
        for a in &self.axioms {
            if let Axiom::Declaration(crate::model::EntityKind::NamedIndividual, t) = a {
                out.push(*t);
            }
            out.extend(individuals(a));
        }
        out.extend(in_expressions(self));
        let reserved: HashSet<Term> = (self.diagnostics.iter())
            .filter_map(|d| match *d {
                Diagnostic::Reserved { term } => Some(term),
                _ => None,
            })
            .collect();
        out.retain(|t| !self.anonymous.contains(t) && !reserved.contains(t));
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The axioms of a kind, by a test.
    pub fn axioms_where(&self, test: impl Fn(&Axiom) -> bool) -> impl Iterator<Item = &Axiom> {
        self.axioms.iter().filter(move |a| test(a))
    }
}

/// Reads the ontology of `statements` (any graphs, the union of them).
pub fn read(statements: &[Statement], terms: &dyn Terms) -> Ontology {
    read_rdf(statements, &[], terms, true)
}

/// [`read`], its SHACL rule ([`Ontology::shacl`]) over `beside` and `statements` as one source:
/// a conclusion's read beside its premise's statements, which are read for nothing else.
pub fn read_beside(statements: &[Statement], beside: &[Statement], terms: &dyn Terms) -> Ontology {
    read_rdf(statements, beside, terms, false)
}

fn read_rdf(
    statements: &[Statement],
    beside: &[Statement],
    terms: &dyn Terms,
    membership: bool,
) -> Ontology {
    let vocabulary = Vocabulary::new(&|iri| terms.iri(iri));
    let mut reader = Reader::new(statements, terms, vocabulary);
    reader.declarations();
    reader.shacl(beside);
    reader.axioms();
    reader.finish(membership)
}

/// What makes a blank node a class expression to [`read`], by the source's ids: its types,
/// or a statement with one of its properties. Such a node in a class's place is read, or
/// reported when it can't be (malformed, unsupported).
///
/// `Reader::blank_class` reads exactly these, and a caller choosing input (the store's QL
/// reader, by memberships) finds class expressions by the same definition.
/// Both take it apart field by field, so a marker added here doesn't compile
/// until each of them handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassExpressionMarkers {
    /// `owl:Class`, `rdfs:Class`, `owl:Restriction`.
    pub types: [Option<Term>; 3],
    /// `owl:intersectionOf`, `owl:unionOf`, `owl:complementOf`, `owl:oneOf`.
    pub set_operators: [Option<Term>; 4],
    /// `owl:onProperties`: an n-ary data restriction, reported as unsupported.
    pub on_properties: Option<Term>,
    /// `owl:onProperty`: a restriction.
    pub on_property: Option<Term>,
}

impl ClassExpressionMarkers {
    pub fn of(v: &Vocabulary) -> Self {
        Self {
            types: [v.owl_class, v.rdfs_class, v.owl_restriction],
            set_operators: [
                v.owl_intersection_of,
                v.owl_union_of,
                v.owl_complement_of,
                v.owl_one_of,
            ],
            on_properties: v.owl_on_properties,
            on_property: v.owl_on_property,
        }
    }

    /// [`Self::of`] the source's vocabulary, looking up only these terms, by their
    /// compile-time IRIs: a caller needing nothing else of the vocabulary looks nothing else
    /// up. The QL reader does this once per fresh read, where a whole [`Vocabulary::new`]
    /// cost 0.6 % of a compile (count i24c-attrib2-f6ec07c9).
    pub fn lookup(id_of: &dyn Fn(&str) -> Option<Term>) -> Self {
        Self {
            types: [
                id_of(const { Vocabulary::iri("owl_class") }),
                id_of(const { Vocabulary::iri("rdfs_class") }),
                id_of(const { Vocabulary::iri("owl_restriction") }),
            ],
            set_operators: [
                id_of(const { Vocabulary::iri("owl_intersection_of") }),
                id_of(const { Vocabulary::iri("owl_union_of") }),
                id_of(const { Vocabulary::iri("owl_complement_of") }),
                id_of(const { Vocabulary::iri("owl_one_of") }),
            ],
            on_properties: id_of(const { Vocabulary::iri("owl_on_properties") }),
            on_property: id_of(const { Vocabulary::iri("owl_on_property") }),
        }
    }

    /// The properties, those the source has.
    pub fn properties(&self) -> Vec<Term> {
        let Self {
            types: _,
            set_operators,
            on_properties,
            on_property,
        } = *self;
        set_operators
            .into_iter()
            .chain([on_properties, on_property])
            .flatten()
            .collect()
    }
}

/// What every reader adds to the axioms it read: the literals' parts, the anonymous
/// individuals, and the diagnostics of OWL 2 DL's global restrictions.
pub(crate) fn complete(ontology: &mut Ontology, terms: &dyn Terms, membership: bool) {
    crate::diagnostics::leave_out_reserved(ontology, terms);
    crate::diagnostics::report_reserved_in_expressions(ontology, terms);
    ontology.data = data_terms(ontology, terms);
    // Wherever a blank node stands for an individual, it is an anonymous one.
    ontology.anonymous = (ontology.axioms.iter())
        .flat_map(individuals)
        .chain(in_expressions(ontology))
        .filter(|&t| terms.kind(t) == TermKind::Blank)
        .collect();
    crate::diagnostics::check_global_restrictions(ontology, membership);
    // An unused structure that is also an individual: an equality the rules derive may give
    // it a second structure's statements (`Reader::reads_more`).
    for d in &mut ontology.diagnostics {
        if let Diagnostic::Unused {
            node, reads_more, ..
        } = d
        {
            *reads_more |= ontology.anonymous.contains(node);
        }
    }
    let (two_kinds, findings) = crate::typing::check(ontology, membership.then_some(terms));
    ontology.diagnostics.extend(findings);
    let two_kinds = two_kinds.into_iter();
    ontology
        .diagnostics
        .extend(two_kinds.map(|t| Diagnostic::TwoKinds {
            term: t.term,
            kinds: t.kinds,
            by_axiom: t.by_axiom,
            treatment: Treatment::Counted,
        }));
    ontology.treat(&Treatments::default());
}

/// The individuals class expressions name: in nominals and `hasValue`.
pub(crate) fn in_expressions(ontology: &Ontology) -> impl Iterator<Item = Term> + '_ {
    (0..ontology.classes.len() as u32).flat_map(|id| match ontology.classes.get(id) {
        ClassExpr::OneOf(v) => v.clone(),
        ClassExpr::HasValue(_, t) => vec![*t],
        _ => Vec::new(),
    })
}

/// The individuals an assertion is about.
fn individuals(axiom: &Axiom) -> Vec<Term> {
    match axiom {
        Axiom::ClassAssertion(_, a)
        | Axiom::DataPropertyAssertion(_, a, _)
        | Axiom::NegativeDataPropertyAssertion(_, a, _) => vec![*a],
        Axiom::ObjectPropertyAssertion(_, a, b)
        | Axiom::NegativeObjectPropertyAssertion(_, a, b) => vec![*a, *b],
        Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => xs.clone(),
        _ => Vec::new(),
    }
}

/// The literals and datatype and facet IRIs the ontology's data axioms and ranges use,
/// with their parts as `terms` gives them.
fn data_terms(o: &Ontology, terms: &dyn Terms) -> DataTerms {
    let mut literals: BTreeSet<Term> = BTreeSet::new();
    let mut iris: BTreeSet<Term> = BTreeSet::new();
    for id in 0..o.ranges.len() as u32 {
        match o.ranges.get(id) {
            DataRange::Datatype(t) => {
                iris.insert(*t);
            }
            DataRange::OneOf(xs) => literals.extend(xs),
            DataRange::Restriction(t, facets) => {
                iris.insert(*t);
                for &(f, v) in facets {
                    iris.insert(f);
                    literals.insert(v);
                }
            }
            _ => {}
        }
    }
    for id in 0..o.classes.len() as u32 {
        if let ClassExpr::DataHasValue(_, v) = o.classes.get(id) {
            literals.insert(*v);
        }
    }
    for a in &o.axioms {
        match a {
            Axiom::DataPropertyAssertion(_, _, v)
            | Axiom::NegativeDataPropertyAssertion(_, _, v) => {
                literals.insert(*v);
            }
            Axiom::DatatypeDefinition(t, _) => {
                iris.insert(*t);
            }
            _ => {}
        }
    }
    // The rules' literals and built-ins.
    for atom in o.rules.iter().flat_map(|r| r.body.iter().chain(&r.head)) {
        use crate::rules::{Atom, DArg};
        let mut args: Vec<DArg> = Vec::new();
        match atom {
            Atom::Range(_, v) | Atom::Data(_, _, v) => args.push(*v),
            Atom::BuiltIn(b, xs) => {
                iris.insert(*b);
                args.extend(xs);
            }
            _ => {}
        }
        for a in args {
            if let DArg::Literal(l) = a {
                literals.insert(l);
            }
        }
    }
    // IRIs: as the source writes them, else those of the datatype map it has ids for.
    let mut known: HashMap<Term, String> = HashMap::new();
    for iri in crate::vocab::data_iris() {
        if let Some(t) = terms.iri(&iri) {
            known.insert(t, iri);
        }
    }
    let mut out = DataTerms::default();
    for t in literals {
        if terms.kind(t) != TermKind::Literal {
            continue;
        }
        if let Some(lexical) = terms.lexical(t) {
            out.literals.insert(
                t,
                Literal {
                    lexical,
                    datatype: terms.datatype(t),
                    language: terms.language(t),
                },
            );
        }
    }
    for t in iris {
        if let Some(iri) = terms.iri_text(t).or_else(|| known.get(&t).cloned()) {
            out.iris.insert(t, iri);
        }
    }
    out
}

/// The facets of the OWL 2 datatype map (*Structural Specification* §4): what a facet
/// restriction's one statement may say.
pub const FACETS: [&str; 9] = [
    "http://www.w3.org/2001/XMLSchema#minInclusive",
    "http://www.w3.org/2001/XMLSchema#maxInclusive",
    "http://www.w3.org/2001/XMLSchema#minExclusive",
    "http://www.w3.org/2001/XMLSchema#maxExclusive",
    "http://www.w3.org/2001/XMLSchema#length",
    "http://www.w3.org/2001/XMLSchema#minLength",
    "http://www.w3.org/2001/XMLSchema#maxLength",
    "http://www.w3.org/2001/XMLSchema#pattern",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#langRange",
];

/// A class expression whose RDF form has been read, with class operands still to resolve.
enum ClassRead {
    Known(ExprId),
    Ready(ClassExpr),
    Unary(Term, ClassConstructor),
    Set(Vec<Term>, bool),
}

enum ClassConstructor {
    Not,
    Some(ObjProp),
    All(ObjProp),
    Cardinality(Option<u32>, ObjProp, fn(u32, ObjProp, ExprId) -> ClassExpr),
}

impl ClassConstructor {
    fn apply(self, operand: ExprId) -> Option<ClassExpr> {
        Some(match self {
            Self::Not => ClassExpr::Not(operand),
            Self::Some(property) => ClassExpr::Some(property, operand),
            Self::All(property) => ClassExpr::All(property, operand),
            Self::Cardinality(n, property, make) => make(n?, property, operand),
        })
    }
}

enum ClassFrame {
    Unary(ClassConstructor),
    Set {
        remaining: std::vec::IntoIter<Term>,
        operands: Vec<ExprId>,
        union: bool,
    },
}

/// A data range whose RDF form has been read, with range operands still to resolve.
enum RangeRead {
    Known(RangeId),
    Ready(DataRange),
    Not(Term),
    Set(Vec<Term>, bool),
}

enum RangeFrame {
    Not,
    Set {
        remaining: std::vec::IntoIter<Term>,
        operands: Vec<RangeId>,
        union: bool,
    },
}

struct Reader<'a> {
    v: Vocabulary,
    /// [`DECLARATION_KINDS`] by the source's ids.
    declaration_kinds: [(Option<Term>, EntityKind); 9],
    /// [`FACETS`] by the source's ids.
    facets: [Option<Term>; 9],
    /// [`BUILTIN_ANNOTATIONS`] and `owl:versionIRI` by the source's ids, looked up once per
    /// read: [`Self::is_builtin_annotation`] meets them on every assertion.
    builtin_annotations: ([Option<Term>; 9], Option<Term>),
    annotation_properties: AnnotationProperties,
    terms: &'a dyn Terms,
    statements: &'a [Statement],
    /// Per subject: (predicate, object, graph).
    by_subject: HashMap<Term, Vec<(Term, Term, Term)>>,
    declared: BTreeMap<Term, Vec<EntityKind>>,
    /// Properties used with literal objects.
    used_with_literals: HashSet<Term>,
    used_with_nodes: HashSet<Term>,
    qualified_data_max: Option<Term>,
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
    reported: HashSet<([Term; 3], &'static str)>,
    /// The reserved-name walk over a repeated disjointness's axioms, kept across them.
    walk: crate::diagnostics::Walk,
    annotations: usize,
    noncanonical_cardinality: Option<Term>,
    empty_list: Option<Term>,
    rules_unread: Option<Term>,
    annotation_schema: Vec<Term>,
    annotation_uses: BTreeSet<Term>,
    annotation_axioms: BTreeSet<Term>,
    /// The properties declared object or data properties (`owl:ObjectProperty`,
    /// `owl:DatatypeProperty`) or by OWL 2 itself (the four built-in ones), not typed so by a
    /// characteristic: what makes a statement about the ontology no annotation (Table 10).
    logical: HashSet<Term>,
    shacl: BTreeSet<Term>,
    /// The cells of the lists read to `rdf:nil` ([`Self::cells_outside_their_lists`]).
    cells: HashSet<Term>,
    list_cell_outside: Option<Term>,
}

/// The classes whose instances are declared entities, by IRI: OWL 2's, and the RDFS and
/// OWL 1 synonyms the mapping reads as them (`rdfs:Class`, `owl:DataRange`, and
/// `owl:OntologyProperty` by Table 6 of §3.1.2). [`read`]
/// declares by this table, and a reader of RDF elsewhere that tells what a name is from its
/// declarations reads by it too, so that both read a declaration as the same kind.
pub const DECLARATION_KINDS: [(&str, EntityKind); 9] = [
    (Vocabulary::iri("owl_class"), EntityKind::Class),
    (Vocabulary::iri("rdfs_class"), EntityKind::Class),
    (
        Vocabulary::iri("owl_object_property"),
        EntityKind::ObjectProperty,
    ),
    (
        Vocabulary::iri("owl_datatype_property"),
        EntityKind::DataProperty,
    ),
    (
        Vocabulary::iri("owl_annotation_property"),
        EntityKind::AnnotationProperty,
    ),
    (
        Vocabulary::iri("owl_ontology_property"),
        EntityKind::AnnotationProperty,
    ),
    (Vocabulary::iri("rdfs_datatype"), EntityKind::Datatype),
    (Vocabulary::iri("owl_data_range"), EntityKind::Datatype),
    (
        Vocabulary::iri("owl_named_individual"),
        EntityKind::NamedIndividual,
    ),
];

/// The annotation properties OWL 2 builds in (Structural Specification §5.5): the reader
/// reads a statement of one as an annotation, which gives its terms no kind.
pub const BUILTIN_ANNOTATIONS: [&str; 9] = [
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

/// SHACL's namespace.
const SHACL: &str = "http://www.w3.org/ns/shacl#";

/// The properties of the SHACL vocabulary, by local name in [`SHACL`]'s namespace: a statement
/// of one is SHACL content, with no OWL meaning ([`Ontology::shacl`]). From W3C, *Shapes
/// Constraint Language (SHACL)*, 2017: §2 (targets, severity, messages, deactivation, property
/// shapes and their paths, the non-validating characteristics), §1.5 and §3 (shapes graphs,
/// entailment, the validation report), §4 (the core constraint components' parameters), §5 and
/// §6 (SPARQL-based constraints and constraint components). A term of the namespace not listed
/// is read as any other term, by its use: conservative, as it keeps the kind OWL 2 gives it.
#[rustfmt::skip]
const SHACL_PROPERTIES: [&str; 78] = [
    // §2
    "targetNode", "targetClass", "targetSubjectsOf", "targetObjectsOf",
    "property", "path", "severity", "message", "deactivated",
    "inversePath", "alternativePath", "zeroOrMorePath", "oneOrMorePath", "zeroOrOnePath",
    "name", "description", "order", "group", "defaultValue",
    // §1.5 and §3
    "shapesGraph", "suggestedShapesGraph", "entailment",
    "conforms", "result", "focusNode", "resultPath", "value", "sourceShape",
    "sourceConstraintComponent", "sourceConstraint", "detail", "resultMessage", "resultSeverity",
    // §4
    "class", "datatype", "nodeKind", "minCount", "maxCount",
    "minExclusive", "minInclusive", "maxExclusive", "maxInclusive",
    "minLength", "maxLength", "pattern", "flags", "languageIn", "uniqueLang",
    "equals", "disjoint", "lessThan", "lessThanOrEquals",
    "not", "and", "or", "xone", "node",
    "qualifiedValueShape", "qualifiedMinCount", "qualifiedMaxCount", "qualifiedValueShapesDisjoint",
    "closed", "ignoredProperties", "hasValue", "in",
    // §5 and §6
    "sparql", "select", "ask", "prefixes", "declare", "prefix", "namespace",
    "parameter", "optional", "labelTemplate", "validator", "nodeValidator", "propertyValidator",
];

/// [`SHACL_PROPERTIES`]' IRIs.
pub fn shacl_properties() -> impl Iterator<Item = String> {
    SHACL_PROPERTIES.iter().map(|name| format!("{SHACL}{name}"))
}

/// Whether `iri` is one of [`shacl_properties`].
pub fn is_shacl_property(iri: &str) -> bool {
    iri.strip_prefix(SHACL)
        .is_some_and(|name| SHACL_PROPERTIES.contains(&name))
}

/// Whether `iri` may name an annotation property (Structural Specification §5.5): of the
/// reserved vocabulary only the nine built-in ones, [`BUILTIN_ANNOTATIONS`].
pub fn may_name_annotation(iri: &str) -> bool {
    BUILTIN_ANNOTATIONS.contains(&iri) || !crate::diagnostics::reserved_vocabulary(iri)
}

/// The annotation properties as [`read`] reads them (§5.8), for every reader of the RDF: the
/// built-in ones, and each IRI declared one by [`DECLARATION_KINDS`] (no blank node). Holds
/// the declarations' `rdf:type` and class, and the properties.
#[derive(Debug, Clone)]
pub struct AnnotationProperties(Vec<[Option<Term>; 2]>, HashSet<Term>);

impl AnnotationProperties {
    /// The built-in ones, by the source's ids (`iri`).
    pub fn new(iri: &mut dyn FnMut(&str) -> Option<Term>) -> Self {
        let ty = iri(Vocabulary::iri("rdf_type"));
        let kinds = DECLARATION_KINDS.iter();
        let annotation = kinds.filter(|d| d.1 == EntityKind::AnnotationProperty);
        let declaring = annotation.map(|d| [ty, iri(d.0)]).collect();
        let properties = BUILTIN_ANNOTATIONS.iter().filter_map(|b| iri(b)).collect();
        Self(declaring, properties)
    }

    /// Reads a statement, whose subject is an IRI where `named`.
    pub fn read(&mut self, [s, p, o]: [Term; 3], named: bool) {
        if named && self.declares([s, p, o]) {
            self.1.insert(s);
        }
    }

    /// Whether a statement declares its subject an annotation property, in either spelling.
    pub fn declares(&self, [_, p, o]: [Term; 3]) -> bool {
        self.0.contains(&[Some(p), Some(o)])
    }

    pub fn contains(&self, term: Term) -> bool {
        self.1.contains(&term)
    }

    /// The `(predicate, object)` of a declaration, in either spelling, where both are known.
    pub fn declaring(&self) -> impl Iterator<Item = [Term; 2]> + '_ {
        self.0.iter().filter_map(|&[p, o]| Some([p?, o?]))
    }

    /// The annotation properties read so far, the built-in ones included.
    pub fn properties(&self) -> impl Iterator<Item = Term> + '_ {
        self.1.iter().copied()
    }
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
            declaration_kinds: DECLARATION_KINDS.map(|(iri, kind)| (terms.iri(iri), kind)),
            facets: FACETS.map(|iri| terms.iri(iri)),
            builtin_annotations: (
                BUILTIN_ANNOTATIONS.map(|iri| terms.iri(iri)),
                terms.iri("http://www.w3.org/2002/07/owl#versionIRI"),
            ),
            annotation_properties: AnnotationProperties::new(&mut |iri| terms.iri(iri)),
            terms,
            statements,
            by_subject,
            declared: BTreeMap::new(),
            used_with_literals,
            used_with_nodes,
            qualified_data_max: None,
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
            walk: crate::diagnostics::Walk::new(0),
            annotations: 0,
            noncanonical_cardinality: None,
            empty_list: None,
            rules_unread: None,
            annotation_schema: Vec::new(),
            annotation_uses: BTreeSet::new(),
            annotation_axioms: BTreeSet::new(),
            logical: HashSet::new(),
            shacl: BTreeSet::new(),
            cells: HashSet::new(),
            list_cell_outside: None,
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

    /// The objects of `subject`'s `predicate` statements, each once: the ontology is the
    /// union of the graphs, where a statement in two graphs is one.
    fn objects(&self, subject: Term, predicate: Option<Term>) -> Vec<Term> {
        let Some(predicate) = predicate else {
            return Vec::new();
        };
        let mut objects = Vec::new();
        for &(p, o, _) in self.by_subject.get(&subject).into_iter().flatten() {
            if p == predicate && !objects.contains(&o) {
                objects.push(o);
            }
        }
        objects
    }

    /// The object of `subject`'s `predicate` where it has exactly one, not collected: evidence
    /// for how to read, never a reading.
    fn single(&self, subject: Term, predicate: Option<Term>) -> Option<Term> {
        match self.objects(subject, predicate)[..] {
            [object] => Some(object),
            _ => None,
        }
    }

    /// The single object of `subject`'s `predicate`, collected as structural; none where it
    /// has two ([`Self::twice`]).
    fn one(&mut self, subject: Term, predicate: Option<Term>) -> Option<Term> {
        let objects = self.objects(subject, predicate);
        if objects.len() > 1 {
            self.twice(subject);
            return None;
        }
        let &object = objects.first()?;
        self.collect([subject, predicate?, object]);
        Some(object)
    }

    /// Whether `node` gives a structure property ([`Self::is_vocabulary`], `owl:inverseOf`)
    /// two objects: then it is no structure, since which one a reading took would follow the
    /// order the statements come in. Reported, and every statement of such a property taken.
    fn twice(&mut self, node: Term) -> bool {
        let given_twice = self.given_twice(node);
        for &triple in &given_twice {
            self.collect(triple);
        }
        if !given_twice.is_empty() {
            self.report(Diagnostic::Malformed {
                node,
                what: "a structure property given twice",
            });
        }
        !given_twice.is_empty()
    }

    /// The statements of `node` whose structure property has a second object there: what
    /// [`Self::twice`] reports, without reporting or taking anything.
    fn given_twice(&self, node: Term) -> Vec<[Term; 3]> {
        let Some(pairs) = self.by_subject.get(&node) else {
            return Vec::new();
        };
        let structure = |p| {
            (self.is_vocabulary(p) || is(p, self.v.owl_inverse_of)) && !is(p, self.v.owl_imports)
        };
        (pairs.iter())
            .filter(|&&(p, o, _)| structure(p) && pairs.iter().any(|&(q, x, _)| q == p && x != o))
            .map(|&(p, o, _)| [node, p, o])
            .collect()
    }

    /// Of `forms`, those `node` gives: what [`Self::forms`] counts, without reporting or
    /// taking anything.
    fn given_forms(&self, node: Term, forms: &[Option<Term>]) -> Vec<Term> {
        (forms.iter().flatten().copied())
            .filter(|&p| !self.objects(node, Some(p)).is_empty())
            .collect()
    }

    /// Whether the OWL 2 RL rules may read the blank node `node`, a structure no axiom read
    /// ([`Diagnostic::Unused`]), as more than what one structure says: it has two forms (a
    /// structure property given twice, two of [`Self::expression_forms`], two of
    /// [`Self::restriction_fillers`]), or a negative property assertion's three statements,
    /// which prp-npa1 and prp-npa2 read untyped. That it is also an individual, which an
    /// equality can give a second structure's statements, is added once the axioms are
    /// complete ([`Self::finish`]). Pure: [`Self::twice`] and [`Self::forms`] would report the
    /// node malformed.
    fn reads_more(&self, node: Term) -> bool {
        let v = self.v;
        let has = |p| !self.objects(node, p).is_empty();
        let two = |forms: &[Option<Term>]| self.given_forms(node, forms).len() > 1;
        let negative = has(v.owl_source_individual)
            && has(v.owl_assertion_property)
            && (has(v.owl_target_individual) || has(v.owl_target_value));
        !self.given_twice(node).is_empty()
            || two(&self.expression_forms())
            || two(&self.restriction_fillers())
            || negative
    }

    /// The properties that each give a restriction its one filler or cardinality (*Mapping to
    /// RDF* Table 13).
    fn restriction_fillers(&self) -> [Option<Term>; 10] {
        let v = self.v;
        [
            v.owl_some_values_from,
            v.owl_all_values_from,
            v.owl_has_value,
            v.owl_has_self,
            v.owl_min_cardinality,
            v.owl_min_qualified,
            v.owl_max_cardinality,
            v.owl_max_qualified,
            v.owl_cardinality,
            v.owl_qualified,
        ]
    }

    /// The properties that each make a blank node one class expression or one data range
    /// (*Mapping to RDF* Tables 12 and 13), the forms [`Self::forms`] takes for both: a node
    /// has one form, whichever it is read as.
    fn expression_forms(&self) -> [Option<Term>; 8] {
        let v = self.v;
        let ClassExpressionMarkers {
            types: _,
            set_operators: [intersection, union, complement, one_of],
            on_properties,
            on_property,
        } = ClassExpressionMarkers::of(&v);
        // A data range's `owl:intersectionOf`, `owl:unionOf` and `owl:oneOf` are a class
        // expression's too.
        [
            intersection,
            union,
            complement,
            one_of,
            on_properties,
            on_property,
            v.owl_datatype_complement_of,
            v.owl_on_datatype,
        ]
    }

    /// Whether the blank node `node` gives more than one of `forms`, the properties that each
    /// make it one structure (*Mapping to RDF* Tables 12 and 13: a node has one form): then it
    /// is no structure, as with a structure property given twice ([`Self::twice`]). Reported,
    /// and every statement of those properties taken.
    fn forms(&mut self, node: Term, forms: &[Option<Term>]) -> bool {
        let given = self.given_forms(node, forms);
        if given.len() < 2 {
            return false;
        }
        for p in given {
            for o in self.objects(node, Some(p)) {
                self.collect([node, p, o]);
            }
        }
        self.report(Diagnostic::Malformed {
            node,
            what: "a node with more than one form",
        });
        true
    }

    /// `term` as an individual (*Mapping to RDF* Tables 13 and 16: `*:x` or `_:x`); a literal
    /// there is malformed. Every place an individual is read takes it here.
    fn individual(&mut self, term: Term) -> Option<Term> {
        if self.kind(term) == TermKind::Literal {
            self.report(Diagnostic::Malformed {
                node: term,
                what: "a literal where an individual belongs",
            });
            return None;
        }
        Some(term)
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
        let kinds = self.declaration_kinds;
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
            // Either spelling of an annotation property's declaration (Table 6).
            let annotation = kinds.contains(&(Some(o), EntityKind::AnnotationProperty));
            if annotation && self.reserved_annotation(s) {
                continue;
            }
            self.annotation_properties.read(statement.triple, true);
            for &(class, kind) in &kinds {
                if is(o, class) {
                    let entry = self.declared.entry(s).or_default();
                    if !entry.contains(&kind) {
                        entry.push(kind);
                    }
                    if matches!(kind, EntityKind::ObjectProperty | EntityKind::DataProperty) {
                        self.logical.insert(s);
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
                self.logical.insert(property);
            }
        }
    }

    /// The terms of the SHACL content ([`Ontology::shacl`]): none where a statement that is
    /// neither SHACL content nor an annotation names one, which gives it an OWL meaning, so
    /// the source reads as any other. The rule is over `beside` and the source as one: the
    /// properties either uses as content, and either's annotation declarations (of an IRI, as
    /// [`Self::declarations`] reads them).
    fn shacl(&mut self, beside: &[Statement]) {
        let (terms, statements) = (self.terms, self.statements);
        let vocabulary: HashSet<Term> = shacl_properties().filter_map(|i| terms.iri(&i)).collect();
        let content = |[_, p, _]: [Term; 3]| vocabulary.contains(&p).then_some(p);
        let all = || statements.iter().chain(beside);
        let found: BTreeSet<Term> = all().filter_map(|s| content(s.triple)).collect();
        let mut declared = AnnotationProperties::new(&mut |i| terms.iri(i));
        for s in beside {
            let [subject, ..] = s.triple;
            let declares = self.kind(subject) == TermKind::Iri && declared.declares(s.triple);
            declared.read(s.triple, declares && !self.reserved_annotation(subject));
        }
        let named = !found.is_empty()
            && all().any(|s| {
                let [subject, p, o] = s.triple;
                content(s.triple).is_none()
                    && !self.declared_as(p, EntityKind::AnnotationProperty)
                    && !declared.contains(p)
                    && !self.is_builtin_annotation(p)
                    && (found.contains(&subject) || found.contains(&o))
            });
        if !named {
            self.shacl = statements
                .iter()
                .filter_map(|s| content(s.triple))
                .collect();
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

    /// The members of the list starting at `head`. Its cells are checked once every statement
    /// is read ([`Self::cells_outside_their_lists`]).
    fn list(&mut self, head: Term) -> Option<Vec<Term>> {
        let mut members = Vec::new();
        let mut seen = HashSet::new();
        let mut cell = head;
        loop {
            if is(cell, self.v.nil) {
                self.cells.extend(seen);
                return Some(members);
            }
            if self.kind(cell) != TermKind::Blank || !seen.insert(cell) {
                self.report(Diagnostic::BrokenList { head });
                return None;
            }
            if self.twice(cell) {
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
                if self.twice(term) {
                    return None;
                }
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
        let (node, input) = self.start_class(term);
        self.read_class(node, input)
    }

    fn start_class(&mut self, term: Term) -> (Option<Term>, Option<ClassRead>) {
        match self.kind(term) {
            TermKind::Iri => {
                let expr = if is(term, self.v.owl_thing) {
                    ClassExpr::Thing
                } else if is(term, self.v.owl_nothing) {
                    ClassExpr::Nothing
                } else {
                    ClassExpr::Class(term)
                };
                (None, Some(ClassRead::Ready(expr)))
            }
            TermKind::Literal => {
                self.report(Diagnostic::Malformed {
                    node: term,
                    what: "a literal where a class expression belongs",
                });
                (None, None)
            }
            TermKind::Blank => {
                self.used(term);
                if let Some(&known) = self.class_memo.get(&term) {
                    return (None, known.map(ClassRead::Known));
                }
                // Guards against a cycle through this node.
                self.class_memo.insert(term, None);
                (Some(term), self.blank_class(term))
            }
        }
    }

    fn read_class(
        &mut self,
        mut node: Option<Term>,
        mut input: Option<ClassRead>,
    ) -> Option<ExprId> {
        let mut stack = Vec::new();
        'read: loop {
            let mut found = match input {
                Some(ClassRead::Known(id)) => Some(id),
                Some(ClassRead::Ready(expr)) => Some(self.intern_class(expr)),
                Some(ClassRead::Unary(operand, make)) => {
                    stack.push((node, ClassFrame::Unary(make)));
                    (node, input) = self.start_class(operand);
                    continue;
                }
                Some(ClassRead::Set(members, union)) => {
                    let mut remaining = members.into_iter();
                    if let Some(first) = remaining.next() {
                        let operands = Vec::with_capacity(remaining.len() + 1);
                        let frame = ClassFrame::Set {
                            remaining,
                            operands,
                            union,
                        };
                        stack.push((node, frame));
                        (node, input) = self.start_class(first);
                        continue;
                    }
                    Some(self.intern_class(Self::class_set(Vec::new(), union)))
                }
                None => None,
            };
            loop {
                if let Some(term) = node {
                    self.finish_class(term, found);
                }
                let Some((parent, frame)) = stack.pop() else {
                    return found;
                };
                node = parent;
                found = match (frame, found) {
                    (ClassFrame::Unary(make), Some(id)) => {
                        make.apply(id).map(|expr| self.intern_class(expr))
                    }
                    (
                        ClassFrame::Set {
                            mut remaining,
                            mut operands,
                            union,
                        },
                        Some(id),
                    ) => {
                        operands.push(id);
                        if let Some(next) = remaining.next() {
                            let frame = ClassFrame::Set {
                                remaining,
                                operands,
                                union,
                            };
                            stack.push((node, frame));
                            (node, input) = self.start_class(next);
                            continue 'read;
                        }
                        Some(self.intern_class(Self::class_set(operands, union)))
                    }
                    (_, None) => None,
                };
            }
        }
    }

    fn finish_class(&mut self, term: Term, found: Option<ExprId>) {
        if found.is_none() {
            // Whatever made it unreadable may not have been reported; the axiom
            // it belongs to is left out, never silently.
            self.report(Diagnostic::Malformed {
                node: term,
                what: "a class expression that can't be read",
            });
        }
        self.class_memo.insert(term, found);
    }

    fn classes(&mut self, terms: &[Term]) -> Option<Vec<ExprId>> {
        terms.iter().map(|&t| self.class(t)).collect()
    }

    /// A blank class expression, by [`ClassExpressionMarkers`].
    fn blank_class(&mut self, node: Term) -> Option<ClassRead> {
        let v = self.v;
        let ClassExpressionMarkers {
            types,
            set_operators,
            on_properties,
            on_property,
        } = ClassExpressionMarkers::of(&v);
        for class_type in types {
            if self.has_type(node, class_type) {
                self.collect([node, v.rdf_type?, class_type?]);
            }
        }
        if self.twice(node) || self.forms(node, &self.expression_forms()) {
            return None;
        }
        for operator in set_operators {
            if let Some(operand) = self.one(node, operator) {
                return self.set_operator(operator?, operand);
            }
        }
        if !self.objects(node, on_properties).is_empty() {
            self.report(Diagnostic::Unsupported {
                node,
                what: "n-ary data restrictions (owl:onProperties)",
            });
            return None;
        }
        let Some(property) = self.one(node, on_property) else {
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
    fn set_operator(&mut self, operator: Term, operand: Term) -> Option<ClassRead> {
        let v = self.v;
        if is(operator, v.owl_complement_of) {
            return Some(ClassRead::Unary(operand, ClassConstructor::Not));
        }
        let members = self.list(operand)?;
        if is(operator, v.owl_one_of) {
            let individuals = (members.iter())
                .map(|&m| self.individual(m))
                .collect::<Option<Vec<_>>>()?;
            return Some(ClassRead::Ready(ClassExpr::OneOf(canonical(individuals))));
        }
        let union = is(operator, v.owl_union_of);
        if members.is_empty() {
            // An empty intersection is `owl:Thing`, an empty union `owl:Nothing` (OWL 2
            // Mapping to RDF, Tables 15 and 18), which the rules don't read
            // ([`Ontology::empty_list`]). The other short lists keep their form, which no
            // OWL 2 RL check takes.
            self.empty_list.get_or_insert(operand);
        }
        Some(ClassRead::Set(members, union))
    }

    fn class_set(operands: Vec<ExprId>, union: bool) -> ClassExpr {
        if union {
            ClassExpr::Or(canonical(operands))
        } else if operands.is_empty() {
            ClassExpr::Thing
        } else {
            ClassExpr::And(canonical(operands))
        }
    }

    /// Whether the restriction `node` says its undeclared property is a data property, where
    /// no literal use does (a schema read alone): its value a literal, or its filler a data
    /// range ([`Self::is_data_range`]).
    fn says_data(&mut self, node: Term) -> bool {
        let first = |reader: &Self, p| reader.single(node, p);
        if let Some(value) = first(self, self.v.owl_has_value) {
            return self.kind(value) == TermKind::Literal;
        }
        let filler = first(self, self.v.owl_some_values_from);
        let Some(filler) = filler.or_else(|| first(self, self.v.owl_all_values_from)) else {
            return false;
        };
        self.is_data_range(filler)
    }

    /// Whether `term` is a data range by its own statements: a datatype (declared, or of
    /// OWL 2's datatype map, whose names no class may take), or a blank node typed
    /// `rdfs:Datatype` (`owl:DataRange` in OWL 1), restricting a datatype, a datatype's
    /// complement, an enumeration of literals, or an intersection or union of data ranges.
    /// A mix with classes, an empty list or a cycle is no evidence: read as before. Each
    /// operator node's answer is kept, `false` while on the path (a shared one is read once).
    /// The operands are decided from the left with a stack of its own (the nesting depth is
    /// no call depth), the first that isn't a data range deciding.
    fn is_data_range(&mut self, term: Term) -> bool {
        let mut seen = HashMap::new();
        // The open intersections and unions, each with its operands still to decide.
        let mut open: Vec<(Term, std::vec::IntoIter<Term>)> = Vec::new();
        let mut next = term;
        loop {
            let data = match self.data_range_operands(next, &mut seen) {
                Ok(data) => data,
                Err(operands) => {
                    open.push((next, operands.into_iter()));
                    true
                }
            };
            loop {
                let Some((operator, operands)) = open.last_mut() else {
                    return data;
                };
                if data && let Some(operand) = operands.next() {
                    next = operand;
                    break;
                }
                seen.insert(*operator, data);
                open.pop();
            }
        }
    }

    /// [`Self::is_data_range`] of `term` where its own statements decide it, else the
    /// operands of the intersection or union it is (at least one), all of which must be.
    fn data_range_operands(
        &mut self,
        term: Term,
        seen: &mut HashMap<Term, bool>,
    ) -> Result<bool, Vec<Term>> {
        let v = self.v;
        match self.kind(term) {
            TermKind::Iri => Ok(self.declared_as(term, EntityKind::Datatype)
                || crate::vocab::DATA_IRIS
                    .iter()
                    .any(|&iri| self.terms.iri(iri) == Some(term))),
            TermKind::Blank => {
                let first = |reader: &Self, p| reader.single(term, p);
                if self.has_type(term, v.rdfs_datatype)
                    || self.has_type(term, v.owl_data_range)
                    || first(self, v.owl_on_datatype).is_some()
                    || first(self, v.owl_datatype_complement_of).is_some()
                {
                    return Ok(true);
                }
                if let Some(list) = first(self, v.owl_one_of) {
                    let members = self.list(list).unwrap_or_default();
                    return Ok(!members.is_empty()
                        && members.iter().all(|&m| self.kind(m) == TermKind::Literal));
                }
                if let Some(&data) = seen.get(&term) {
                    return Ok(data);
                }
                seen.insert(term, false);
                let operands =
                    first(self, v.owl_intersection_of).or_else(|| first(self, v.owl_union_of));
                let members = operands
                    .and_then(|list| self.list(list))
                    .unwrap_or_default();
                match members.is_empty() {
                    true => Ok(false),
                    false => Err(members),
                }
            }
            _ => Ok(false),
        }
    }

    /// A restriction on `property`.
    fn restriction(&mut self, node: Term, property: Term) -> Option<ClassRead> {
        let v = self.v;
        if self.forms(node, &self.restriction_fillers()) {
            return None;
        }
        let data = self.kind(property) == TermKind::Iri
            && (self.is_data_property(property)
                || (!self.declared_as(property, EntityKind::ObjectProperty)
                    && self.says_data(node)));
        let number = |reader: &mut Self, predicate: Option<Term>| -> Option<Option<u32>> {
            let literal = reader.one(node, predicate)?;
            let text = reader.terms.lexical(literal);
            let parsed = text.as_deref().and_then(|t| t.trim().parse::<u32>().ok());
            if parsed.is_none() {
                reader.report(Diagnostic::Malformed {
                    node,
                    what: "a cardinality that isn't a non-negative integer",
                });
            }
            // The source may not give the datatype: then the lexical form alone decides.
            let datatype = reader.terms.datatype(literal);
            let mapped = parsed.is_some_and(|n| text == Some(n.to_string()))
                && datatype
                    .is_none_or(|d| d == "http://www.w3.org/2001/XMLSchema#nonNegativeInteger");
            // The rules read only a max cardinality: an exact or min one's spelling matters
            // to none.
            let max = [reader.v.owl_max_cardinality, reader.v.owl_max_qualified];
            if max.contains(&predicate) && !mapped {
                reader.noncanonical_cardinality.get_or_insert(node);
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
                return Some(ClassRead::Ready(ClassExpr::DataSome(property, range)));
            }
            if let Some(filler) = self.one(node, v.owl_all_values_from) {
                let range = self.range(filler)?;
                return Some(ClassRead::Ready(ClassExpr::DataAll(property, range)));
            }
            if let Some(value) = self.one(node, v.owl_has_value) {
                return Some(ClassRead::Ready(ClassExpr::DataHasValue(property, value)));
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
                    return Some(ClassRead::Ready(make(n?, property, range)));
                }
                if let Some(n) = number(self, qualified) {
                    let filler = self.one(node, v.owl_on_data_range)?;
                    let range = self.range(filler)?;
                    if qualified == v.owl_max_qualified {
                        self.qualified_data_max.get_or_insert(property);
                    }
                    return Some(ClassRead::Ready(make(n?, property, range)));
                }
            }
        } else {
            let property = self.object_property(property)?;
            if let Some(filler) = self.one(node, v.owl_some_values_from) {
                return Some(ClassRead::Unary(filler, ClassConstructor::Some(property)));
            }
            if let Some(filler) = self.one(node, v.owl_all_values_from) {
                return Some(ClassRead::Unary(filler, ClassConstructor::All(property)));
            }
            if let Some(value) = self.one(node, v.owl_has_value) {
                let value = self.individual(value)?;
                return Some(ClassRead::Ready(ClassExpr::HasValue(property, value)));
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
                return Some(ClassRead::Ready(ClassExpr::HasSelf(property)));
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
                    return Some(ClassRead::Ready(make(n?, property, thing)));
                }
                if let Some(n) = number(self, qualified) {
                    let filler = self.one(node, v.owl_on_class)?;
                    return Some(ClassRead::Unary(
                        filler,
                        ClassConstructor::Cardinality(n, property, make),
                    ));
                }
            }
        }
        self.report(Diagnostic::Malformed {
            node,
            what: "a restriction without a restriction property",
        });
        None
    }

    /// The data range `term` stands for, read as [`Self::class`] reads a class expression:
    /// with a stack of its own, so that the nesting depth is no call depth.
    fn range(&mut self, term: Term) -> Option<RangeId> {
        let (mut node, mut input) = self.start_range(term);
        let mut stack = Vec::new();
        'read: loop {
            let mut found = match input {
                Some(RangeRead::Known(id)) => Some(id),
                Some(RangeRead::Ready(range)) => Some(self.intern_range(range)),
                Some(RangeRead::Not(operand)) => {
                    stack.push((node, RangeFrame::Not));
                    (node, input) = self.start_range(operand);
                    continue;
                }
                Some(RangeRead::Set(members, union)) => {
                    let mut remaining = members.into_iter();
                    if let Some(first) = remaining.next() {
                        let operands = Vec::with_capacity(remaining.len() + 1);
                        let frame = RangeFrame::Set {
                            remaining,
                            operands,
                            union,
                        };
                        stack.push((node, frame));
                        (node, input) = self.start_range(first);
                        continue;
                    }
                    Some(self.intern_range(Self::range_set(Vec::new(), union)))
                }
                None => None,
            };
            loop {
                if let Some(term) = node {
                    self.finish_range(term, found);
                }
                let Some((parent, frame)) = stack.pop() else {
                    return found;
                };
                node = parent;
                found = match (frame, found) {
                    (RangeFrame::Not, Some(id)) => Some(self.intern_range(DataRange::Not(id))),
                    (
                        RangeFrame::Set {
                            mut remaining,
                            mut operands,
                            union,
                        },
                        Some(id),
                    ) => {
                        operands.push(id);
                        if let Some(next) = remaining.next() {
                            let frame = RangeFrame::Set {
                                remaining,
                                operands,
                                union,
                            };
                            stack.push((node, frame));
                            (node, input) = self.start_range(next);
                            continue 'read;
                        }
                        Some(self.intern_range(Self::range_set(operands, union)))
                    }
                    (_, None) => None,
                };
            }
        }
    }

    fn start_range(&mut self, term: Term) -> (Option<Term>, Option<RangeRead>) {
        match self.kind(term) {
            TermKind::Iri if is(term, self.v.rdfs_literal) => {
                (None, Some(RangeRead::Ready(DataRange::Literal)))
            }
            TermKind::Iri => (None, Some(RangeRead::Ready(DataRange::Datatype(term)))),
            TermKind::Literal => {
                self.report(Diagnostic::Malformed {
                    node: term,
                    what: "a literal where a data range belongs",
                });
                (None, None)
            }
            TermKind::Blank => {
                self.used(term);
                if let Some(&known) = self.range_memo.get(&term) {
                    return (None, known.map(RangeRead::Known));
                }
                // Guards against a cycle through this node.
                self.range_memo.insert(term, None);
                (Some(term), self.blank_range(term))
            }
        }
    }

    fn finish_range(&mut self, term: Term, found: Option<RangeId>) {
        if found.is_none() {
            self.report(Diagnostic::Malformed {
                node: term,
                what: "a data range that can't be read",
            });
        }
        self.range_memo.insert(term, found);
    }

    fn range_set(operands: Vec<RangeId>, union: bool) -> DataRange {
        match union {
            true => DataRange::Or(canonical(operands)),
            false => DataRange::And(canonical(operands)),
        }
    }

    /// A blank data range, with its range operands still to read.
    fn blank_range(&mut self, node: Term) -> Option<RangeRead> {
        let v = self.v;
        for range_type in [v.rdfs_datatype, v.owl_data_range] {
            if self.has_type(node, range_type) {
                self.collect([node, v.rdf_type?, range_type?]);
            }
        }
        if self.twice(node) || self.forms(node, &self.expression_forms()) {
            return None;
        }
        for (operator, union) in [(v.owl_intersection_of, false), (v.owl_union_of, true)] {
            if let Some(list) = self.one(node, operator) {
                let members = self.list(list)?;
                if members.len() == 1 {
                    self.report(Diagnostic::Malformed {
                        node,
                        what: "an intersection or union needs at least two data ranges",
                    });
                    return None;
                }
                return Some(RangeRead::Set(members, union));
            }
        }
        if let Some(operand) = self.one(node, v.owl_datatype_complement_of) {
            return Some(RangeRead::Not(operand));
        }
        if let Some(list) = self.one(node, v.owl_one_of) {
            let literals = self.list(list)?;
            return Some(RangeRead::Ready(DataRange::OneOf(canonical(literals))));
        }
        if let Some(datatype) = self.one(node, v.owl_on_datatype) {
            let list = self.one(node, v.owl_with_restrictions)?;
            let mut facets = Vec::new();
            for restriction in self.list(list)? {
                // Blank (Mapping to RDF, Table 12): a named node's statements are its own.
                if self.kind(restriction) != TermKind::Blank {
                    self.report(Diagnostic::Malformed {
                        node: restriction,
                        what: "a facet restriction not on a blank node",
                    });
                    return None;
                }
                let mut pairs = self
                    .by_subject
                    .get(&restriction)
                    .cloned()
                    .unwrap_or_default();
                // Each statement once, in whichever graphs (`objects`).
                pairs.sort_unstable();
                pairs.dedup_by_key(|&mut (p, o, _)| (p, o));
                let [(facet, value, _)] = pairs[..] else {
                    self.report(Diagnostic::Malformed {
                        node: restriction,
                        what: "a facet restriction with other than one facet",
                    });
                    return None;
                };
                if !self.facets.contains(&Some(facet)) {
                    self.report(Diagnostic::Malformed {
                        node: restriction,
                        what: "a facet restriction without a facet",
                    });
                    return None;
                }
                self.collect([restriction, facet, value]);
                facets.push((facet, value));
            }
            return Some(RangeRead::Ready(DataRange::Restriction(
                datatype,
                canonical(facets),
            )));
        }
        self.report(Diagnostic::Malformed {
            node,
            what: "a data range without an operator",
        });
        None
    }

    // Axioms ---------------------------------------------------------------------------

    /// A disjointness's axioms (`Disjoint::axioms`): a repeated operand reported, and then,
    /// where they name a reserved name in its place, all of them left out with it
    /// (`diagnostics::reserved_in`). Whether one was.
    fn disjoint(&mut self, d: Disjoint, triple: [Term; 3]) -> bool {
        let (axioms, repeated) = d.axioms(&mut self.classes, &mut self.ranges);
        if repeated {
            self.report(Diagnostic::RepeatedOperand);
            let reserved =
                crate::diagnostics::reserved_in(&self.classes, &axioms, self.terms, &mut self.walk);
            if !reserved.is_empty() {
                for term in reserved {
                    self.report(Diagnostic::Reserved { term });
                }
                self.collected.clear();
                return repeated;
            }
        }
        // Each axiom comes from the whole form: each gets its list's statements.
        let collected = std::mem::take(&mut self.collected);
        for a in axioms {
            self.collected.clone_from(&collected);
            self.add(a, triple);
        }
        repeated
    }

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
        // Then: statements on IRIs; axioms on blank nodes (a complex left-hand side); the
        // blank nodes' other statements that read a structure (an anonymous individual's
        // typing, a disjoint union); the rest on blank nodes, once every structure has
        // taken its statements.
        let v = self.v;
        let reads_structure = [v.rdf_type, v.owl_disjoint_union_of];
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
        for pass in 0..4 {
            for statement in statements {
                let triple = statement.triple;
                let blank = self.kind(triple[0]) == TermKind::Blank;
                let axiom = axiom_predicates.contains(&Some(triple[1]));
                let reads = reads_structure.contains(&Some(triple[1]));
                let now = match pass {
                    0 => !blank,
                    1 => blank && axiom,
                    2 => blank && reads,
                    _ => blank && !axiom && !reads,
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
        // Structure statements on blank nodes that no axiom took: each node once, in the order
        // its first such statement comes.
        let (mut unused, mut seen) = (Vec::new(), HashSet::new());
        for statement in statements {
            let [s, p, _] = statement.triple;
            if self.kind(s) == TermKind::Blank
                && !self.structural.contains(&statement.triple)
                && (self.is_vocabulary(p) || is(p, self.v.owl_inverse_of))
                && !is(p, self.v.owl_imports)
                && seen.insert(s)
            {
                unused.push(s);
            }
        }
        for node in unused {
            let reads_more = self.reads_more(node);
            self.report(Diagnostic::Unused {
                node,
                reads_more,
                treatment: Treatment::Counted,
            });
        }
    }

    /// An n-ary axiom whose node `node` has type `class`.
    fn nary(&mut self, node: Term, class: Term, triple: [Term; 3]) {
        let v = self.v;
        self.collected.clear();
        let without = |what| Diagnostic::Malformed { node, what };
        let forms = [
            v.owl_all_disjoint_classes,
            v.owl_all_disjoint_properties,
            v.owl_all_different,
            v.owl_negative_property_assertion,
        ];
        if forms.contains(&Some(class)) && self.twice(node) {
            return self.collect(triple);
        }
        if is(class, v.owl_all_disjoint_classes) {
            self.collect(triple);
            let Some(list) = self.one(node, v.owl_members) else {
                return self.report(without("an n-ary axiom without its members"));
            };
            if let Some(members) = self.list(list)
                && let Some(classes) = self.classes(&members)
            {
                self.disjoint(Disjoint::Classes(classes), triple);
            }
        } else if is(class, v.owl_all_disjoint_properties) {
            self.collect(triple);
            let Some(list) = self.one(node, v.owl_members) else {
                return self.report(without("an n-ary axiom without its members"));
            };
            if let Some(members) = self.list(list) {
                if members.iter().all(|&p| self.kind(p) == TermKind::Iri)
                    && members.iter().all(|&p| self.is_data_property(p))
                {
                    self.disjoint(Disjoint::Data(members), triple);
                } else if let Some(properties) = members
                    .iter()
                    .map(|&p| self.object_property(p))
                    .collect::<Option<Vec<_>>>()
                {
                    self.disjoint(Disjoint::Objects(properties), triple);
                }
            }
        } else if is(class, v.owl_all_different) {
            self.collect(triple);
            let list = self
                .one(node, v.owl_members)
                .or_else(|| self.one(node, v.owl_distinct_members));
            let Some(list) = list else {
                return self.report(without("an n-ary axiom without its members"));
            };
            if let Some(members) = self.list(list)
                && let Some(individuals) = (members.iter())
                    .map(|&m| self.individual(m))
                    .collect::<Option<Vec<_>>>()
            {
                self.disjoint(Disjoint::Individuals(individuals), triple);
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
            let Some(source) = self.individual(source) else {
                return;
            };
            if let Some(value) = self.one(node, v.owl_target_value) {
                self.add(
                    Axiom::NegativeDataPropertyAssertion(property, source, value),
                    triple,
                );
            } else if let Some(target) = self.one(node, v.owl_target_individual) {
                let Some(target) = self.individual(target) else {
                    return;
                };
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
            } else {
                self.report(without("a negative property assertion without its target"));
            }
        } else if is(class, v.owl_axiom) || is(class, v.owl_annotation) {
            // An annotated axiom's reification, or an annotation's annotation: no logical
            // meaning. It annotates its main triple, which the graph states (Table 17); one
            // whose main triple is absent annotates nothing.
            let main = [
                self.single(node, v.owl_annotated_source),
                self.single(node, v.owl_annotated_property),
                self.single(node, v.owl_annotated_target),
            ];
            let stated = match main {
                [Some(s), Some(p), Some(o)] => (self.by_subject.get(&s))
                    .is_some_and(|pairs| pairs.iter().any(|&(q, x, _)| q == p && x == o)),
                _ => false,
            };
            if !stated {
                self.report(Diagnostic::Malformed {
                    node,
                    what: "a reified axiom or annotation whose main triple is absent",
                });
            }
            for (p, o, _) in self.by_subject.get(&node).cloned().unwrap_or_default() {
                self.structural.insert([node, p, o]);
                self.annotations += 1;
                // Its annotations' properties; its typing and roles are vocabulary.
                if !is(p, v.rdf_type) && !self.is_vocabulary(p) {
                    self.annotation_uses.insert(p);
                }
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
                self.disjoint(Disjoint::Classes(vec![a, b]), triple);
            }
        } else if is(p, v.owl_disjoint_union_of) {
            // The subject is a class (Table 16, `CE(*:x)`): an IRI, `owl:Thing` and
            // `owl:Nothing` read as such; a blank node, a class expression or not, is none.
            let class = if self.kind(s) == TermKind::Iri {
                self.class(s)
            } else {
                self.report(Diagnostic::Malformed {
                    node: s,
                    what: "a disjoint union of no named class",
                });
                None
            };
            if let Some(members) = self.list(o)
                && let Some(classes) = self.classes(&members)
                && let Some(class) = class
            {
                // Two members at least (Table 16, `n ≥ 2`): of none the class would be read
                // empty, of one equal to it.
                if members.len() < 2 {
                    self.report(Diagnostic::Malformed {
                        node: s,
                        what: "a disjoint union of fewer than two classes",
                    });
                } else if self.disjoint(Disjoint::Union(class, classes), triple) {
                    // A repeat, or one distinct operand left: read as axioms the rules would
                    // read ([`Ontology::rules_unread`]).
                    self.rules_unread.get_or_insert(s);
                }
            }
        } else if is(p, v.rdfs_sub_property_of) {
            let subject = self.annotation_property(s);
            if subject || self.annotation_property(o) {
                self.annotations += 1;
                self.annotation_schema.push(if subject { s } else { o });
                self.annotation_uses.extend([s, o]);
                self.annotation_axioms.extend([s, o]);
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
            if chain.as_ref().is_some_and(Vec::is_empty) {
                self.report(Diagnostic::Malformed {
                    node: s,
                    what: "a property chain without a property",
                });
            } else if let (Some(chain), Some(sup)) = (chain, self.object_property(s)) {
                self.add(Axiom::SubObjectPropertyOf(chain, sup), triple);
            }
        } else if is(p, v.owl_equivalent_property) || is(p, v.owl_property_disjoint_with) {
            let equivalent = is(p, v.owl_equivalent_property);
            if self.kind(s) == TermKind::Iri && self.is_data_property(s) {
                if equivalent {
                    self.add(
                        Axiom::EquivalentDataProperties(canonical(vec![s, o])),
                        triple,
                    );
                } else {
                    self.disjoint(Disjoint::Data(vec![s, o]), triple);
                }
            } else if let (Some(a), Some(b)) = (self.object_property(s), self.object_property(o)) {
                if equivalent {
                    self.add(
                        Axiom::EquivalentObjectProperties(canonical(vec![a, b])),
                        triple,
                    );
                } else {
                    self.disjoint(Disjoint::Objects(vec![a, b]), triple);
                }
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
            if self.annotation_property(s) {
                self.annotations += 1;
                self.annotation_schema.push(s);
                self.annotation_uses.insert(s);
                self.annotation_axioms.insert(s);
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
                // A key has a property (*Structural Specification* §9.5). The empty key the
                // universal-property rewriting makes of one on the top property comes later.
                if members.is_empty() {
                    self.report(Diagnostic::Malformed {
                        node: s,
                        what: "a key without a property",
                    });
                    return;
                }
                let (mut objects, mut datas) = (Vec::new(), Vec::new());
                for member in members {
                    if self.kind(member) == TermKind::Iri && self.is_data_property(member) {
                        datas.push(member);
                    } else if let Some(property) = self.object_property(member) {
                        objects.push(property);
                    } else {
                        // On its other members it would be a stronger key than stated.
                        return self.report(Diagnostic::Malformed {
                            node: s,
                            what: "a key with a member that can't be read",
                        });
                    }
                }
                self.add(
                    Axiom::HasKey(class, canonical(objects), canonical(datas)),
                    triple,
                );
            }
        } else if is(p, v.owl_same_as) {
            if let Some(o) = self.individual(o) {
                self.add(Axiom::SameIndividual(canonical(vec![s, o])), triple);
            }
        } else if is(p, v.owl_different_from) {
            if let Some(o) = self.individual(o) {
                self.disjoint(Disjoint::Individuals(vec![s, o]), triple);
            }
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
            let named = self.class(s);
            let input = self.set_operator(p, o);
            if let (Some(named), Some(expr)) = (named, self.read_class(None, input)) {
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
            || self.shacl.contains(&p)
            || (self.has_type(s, v.owl_ontology) && !self.logical.contains(&p))
        {
            // An annotation, SHACL content, or a statement about the ontology: its header's
            // annotations (an anonymous ontology's too: its node is a blank node). A header
            // statement of a declared object or data property is no annotation (*Mapping to
            // RDF* Table 10 reads one only where the property is an annotation property) and is
            // read as any other below; one of an undeclared property is, as the W3C test cases
            // read it (FS2RDF-ontology-annotation-annotation-ar), and so is one of a property
            // only a characteristic types, which declares nothing.
            self.annotations += 1;
            if !self.shacl.contains(&p) && !is(p, self.builtin_annotations.1) {
                self.annotation_uses.insert(p);
            }
        } else if self.kind(o) == TermKind::Literal
            && self.declared_as(p, EntityKind::ObjectProperty)
            && !self.declared_as(p, EntityKind::DataProperty)
        {
            // An object property's assertion (Table 16, `*:x` or `_:x`): a literal there is
            // malformed. A property declared both ways is read as a data property, as before.
            self.individual(o);
        } else if self.kind(o) == TermKind::Literal {
            self.add(Axiom::DataPropertyAssertion(p, s, o), triple);
        } else if self.is_data_property(p) {
            // A data property's assertion has a literal (Table 16): an IRI or a blank node there
            // is malformed, and the statement left out.
            self.report(Diagnostic::Malformed {
                node: o,
                what: "an IRI where a data value belongs",
            });
        } else {
            self.add(Axiom::ObjectPropertyAssertion(p, s, o), triple);
        }
    }

    /// An `rdf:type` statement: a declaration, a characteristic, or a class assertion.
    fn typing(&mut self, s: Term, o: Term, triple: [Term; 3]) {
        let v = self.v;
        if let Some(&(_, kind)) = self.declaration_kinds.iter().find(|(c, _)| is(o, *c)) {
            if self.declared_as(s, kind) {
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

    /// Declared an annotation property, or one OWL 2 builds in (declared implicitly: §5.8).
    fn annotation_property(&self, term: Term) -> bool {
        self.annotation_properties.contains(term)
    }

    /// Whether `term` is of the reserved vocabulary (`rdf:`, `rdfs:`, `xsd:`, `owl:`) and not
    /// one of the nine annotation properties OWL 2 builds in: no declaration makes it one
    /// (§5.5), `rdf:type`, `owl:sameAs`, `owl:Thing` and the top and bottom properties alike.
    fn reserved_annotation(&self, term: Term) -> bool {
        self.terms
            .iri_text(term)
            .is_some_and(|i| !may_name_annotation(&i))
    }

    /// `rdfs:label` and the other annotation properties OWL 2 builds in.
    fn is_builtin_annotation(&self, predicate: Term) -> bool {
        let (builtin, version_iri) = &self.builtin_annotations;
        builtin.contains(&Some(predicate)) || *version_iri == Some(predicate)
    }

    /// Reports each cell of [`Self::cells`] that a statement outside its list names (*Mapping
    /// to RDF* §3: a sequence's blank nodes occur only in that sequence), once. Allowed: as
    /// subject, its `rdf:first`, `rdf:rest` and `rdf:type rdf:List`; as object, the one
    /// statement that points to it (its axiom's, or its predecessor's `rdf:rest`) and, where
    /// that is its axiom's, the `owl:annotatedTarget` of that axiom's reification
    /// ([`Self::reified_axiom`]). Read over every statement, so the order they come in changes
    /// nothing; not at all where no list was read.
    fn cells_outside_their_lists(&mut self) {
        if self.cells.is_empty() {
            return;
        }
        let v = self.v;
        let mut outside = BTreeSet::new();
        let mut owners: HashMap<Term, [Term; 3]> = HashMap::new();
        let mut targets = Vec::new();
        for statement in self.statements {
            let [s, p, o] = statement.triple;
            let typed_list = is(p, v.rdf_type) && is(o, v.rdf_list);
            if self.cells.contains(&s) && !is(p, v.first) && !is(p, v.rest) && !typed_list {
                outside.insert(s);
            }
            if !self.cells.contains(&o) {
                continue;
            }
            if is(p, v.owl_annotated_target) {
                targets.push((s, o));
            } else if *owners.entry(o).or_insert(statement.triple) != statement.triple {
                outside.insert(o);
            }
        }
        // Each reification read once, however many targets it gives.
        let mut mains: HashMap<Term, Option<[Term; 2]>> = HashMap::new();
        for (reification, cell) in targets {
            let main =
                *(mains.entry(reification)).or_insert_with(|| self.reified_axiom(reification));
            let axiom = owners.get(&cell).filter(|&&[_, p, _]| !is(p, v.rest));
            if main.is_none() || axiom.map(|&[s, p, _]| [s, p]) != main {
                outside.insert(cell);
            }
        }
        for node in outside {
            self.list_cell_outside.get_or_insert(node);
            let what = "a list's cell used outside its list";
            self.report(Diagnostic::Malformed { node, what });
        }
    }

    /// The source and property of the main triple `node` reifies, where it is an annotated
    /// axiom's reification (*Mapping to RDF* §2.3.1, Table 17): a blank node typed `owl:Axiom`
    /// with one `owl:annotatedSource`, one `owl:annotatedProperty` and one
    /// `owl:annotatedTarget`. Its statements read once, none past a role's second value.
    fn reified_axiom(&self, node: Term) -> Option<[Term; 2]> {
        let v = self.v;
        if self.kind(node) != TermKind::Blank {
            return None;
        }
        let roles = [
            v.owl_annotated_source,
            v.owl_annotated_property,
            v.owl_annotated_target,
        ];
        let (mut typed, mut parts) = (false, [None; 3]);
        for &(p, o, _) in self.by_subject.get(&node).into_iter().flatten() {
            typed |= is(p, v.rdf_type) && is(o, v.owl_axiom);
            if let Some(i) = roles.iter().position(|&r| is(p, r)) {
                // A second value: no reification of one main triple.
                if parts[i].replace(o).is_some_and(|before| before != o) {
                    return None;
                }
            }
        }
        match parts {
            [Some(s), Some(p), Some(_)] if typed => Some([s, p]),
            _ => None,
        }
    }

    fn finish(mut self, membership: bool) -> Ontology {
        self.cells_outside_their_lists();
        let mut axioms = Vec::with_capacity(self.axioms.len());
        let mut sources = Vec::with_capacity(self.axioms.len());
        for (axiom, from) in std::mem::take(&mut self.axioms) {
            axioms.push(axiom);
            sources.push(from);
        }
        self.annotation_schema.sort_unstable();
        self.annotation_schema.dedup();
        let mut ontology = Ontology {
            classes: self.classes,
            ranges: self.ranges,
            axioms,
            sources,
            diagnostics: self.diagnostics,
            annotations: self.annotations,
            annotation_schema: self.annotation_schema,
            annotation_uses: self.annotation_uses,
            annotation_axioms: self.annotation_axioms,
            shacl: self.shacl,
            builtin: BuiltinProperties::of(&self.v),
            data: DataTerms::default(),
            anonymous: BTreeSet::new(),
            rules: Vec::new(),
            noncanonical_cardinality: self.noncanonical_cardinality,
            empty_list: self.empty_list,
            qualified_data_max: self.qualified_data_max,
            rules_unread: self.rules_unread,
            list_cell_outside: self.list_cell_outside,
        };
        complete(&mut ontology, self.terms, membership);
        ontology
    }
}
