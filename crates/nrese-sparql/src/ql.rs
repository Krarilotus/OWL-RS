//! OWL 2 QL answers through existentials (docs/design/ql-rewriting.md): queries rewritten
//! with the tree witnesses of the QL part of the snapshot's schema, over the closure the
//! store materialised.
//!
//! [`QlRewriting`] is the store's switch and its cache of the compiled schema; the
//! rewriting of a query's basic graph patterns is `native/ql.rs`.

use std::sync::{Arc, Mutex};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_owl::ql::Tbox;
use nrese_rdf::{NamedNodeRef, Term};

pub use nrese_owl::ql::{Closure, Limits};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// The schema properties read, in every graph.
const SCHEMA: &[(&str, &str)] = &[
    (RDFS, "subClassOf"),
    (OWL, "equivalentClass"),
    (RDFS, "subPropertyOf"),
    (OWL, "equivalentProperty"),
    (OWL, "inverseOf"),
    (RDFS, "domain"),
    (RDFS, "range"),
    (OWL, "onProperty"),
    (OWL, "someValuesFrom"),
    (OWL, "allValuesFrom"),
    (OWL, "hasValue"),
    (OWL, "minCardinality"),
    (OWL, "minQualifiedCardinality"),
    (OWL, "cardinality"),
    (OWL, "qualifiedCardinality"),
    (OWL, "onClass"),
    (OWL, "onDataRange"),
    (OWL, "intersectionOf"),
    (OWL, "unionOf"),
    (OWL, "disjointUnionOf"),
    // What the rewriting doesn't follow, for the completeness it reports (design §7).
    (OWL, "propertyChainAxiom"),
    (OWL, "hasKey"),
    (OWL, "oneOf"),
    (OWL, "complementOf"),
    (OWL, "hasSelf"),
    (OWL, "maxCardinality"),
    (OWL, "maxQualifiedCardinality"),
];

/// The classes whose members' `rdf:type` statements are read: what the mapping needs to
/// know about the schema's terms.
const DECLARATIONS: &[(&str, &str)] = &[
    (OWL, "Restriction"),
    (OWL, "Class"),
    (RDFS, "Class"),
    (RDFS, "Datatype"),
    (OWL, "ObjectProperty"),
    (OWL, "DatatypeProperty"),
    (OWL, "AnnotationProperty"),
    (OWL, "SymmetricProperty"),
    (OWL, "ReflexiveProperty"),
    (OWL, "TransitiveProperty"),
    (OWL, "FunctionalProperty"),
    (OWL, "InverseFunctionalProperty"),
];

/// The list-valued properties whose lists are read.
const LISTS: &[&str] = &[
    "intersectionOf",
    "unionOf",
    "disjointUnionOf",
    "propertyChainAxiom",
    "hasKey",
    "oneOf",
];

/// What the QL rewriting did to one query (EXPLAIN, and with every answer).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QlReport {
    /// Basic graph patterns rewritten.
    pub patterns: usize,
    /// Their tree witnesses.
    pub witnesses: usize,
    /// Branches of their unions.
    pub branches: usize,
    /// Triple patterns of their rewritings, alternatives counted.
    pub atoms: usize,
    /// The bounds reached ([`Limits`]): those patterns ran as written.
    pub limits: Vec<&'static str>,
    pub completeness: Completeness,
}

/// Whether the answers through anonymous individuals are complete (docs/design/
/// ql-rewriting.md §5): `Complete`, or `SoundOnly` with the reasons some may be missing.
/// Answers are never changed for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Completeness {
    #[default]
    Complete,
    SoundOnly(Vec<String>),
}

impl Completeness {
    /// `complete` or `sound-only`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::SoundOnly(_) => "sound-only",
        }
    }

    pub fn reasons(&self) -> &[String] {
        match self {
            Self::Complete => &[],
            Self::SoundOnly(reasons) => reasons,
        }
    }

    /// Adds a reason (once).
    pub fn add(&mut self, reason: String) {
        match self {
            Self::Complete => *self = Self::SoundOnly(vec![reason]),
            Self::SoundOnly(reasons) => {
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
            }
        }
    }
}

/// A store's QL rewriting: what its closure applies, the bounds, and the schema compiled
/// for the last snapshot read.
#[derive(Debug)]
pub struct QlRewriting {
    closure: Closure,
    limits: Limits,
    cached: Mutex<Option<Cached>>,
}

#[derive(Debug)]
struct Cached {
    /// The snapshot it was read from: revision and sizes of the two stacks.
    key: (u64, u64, u64),
    /// A hash of the schema statements: a snapshot with the same schema reuses the TBox.
    schema: u64,
    tbox: Arc<Tbox>,
}

impl QlRewriting {
    /// Rewriting over a closure that applies what `closure` says, with the default bounds.
    pub fn new(closure: Closure) -> Self {
        Self {
            closure,
            limits: Limits::default(),
            cached: Mutex::new(None),
        }
    }

    /// The same with other bounds (tests make them small).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// What the closure it rewrites over applies.
    pub fn closure(&self) -> Closure {
        self.closure
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The QL part of `snapshot`'s schema, compiled. Read once per snapshot, and compiled
    /// only when the schema statements changed: data commits reuse it.
    pub(crate) fn tbox(&self, snapshot: &Snapshot) -> Arc<Tbox> {
        let key = (
            snapshot.revision(),
            snapshot.len_in(ReadModel::Asserted),
            snapshot.len_in(ReadModel::Inferred),
        );
        let mut cached = self.cached.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = cached.as_ref().filter(|c| c.key == key) {
            return Arc::clone(&c.tbox);
        }
        let statements = schema_statements(snapshot);
        let schema = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            statements.hash(&mut hasher);
            hasher.finish()
        };
        if let Some(c) = cached.as_mut().filter(|c| c.schema == schema) {
            c.key = key;
            return Arc::clone(&c.tbox);
        }
        let tbox = Arc::new(compile(snapshot, &statements, self.closure));
        *cached = Some(Cached {
            key,
            schema,
            tbox: Arc::clone(&tbox),
        });
        tbox
    }
}

fn iri(snapshot: &Snapshot, namespace: &str, local: &str) -> Option<TermId> {
    snapshot.lookup(NamedNodeRef::new_unchecked(&format!("{namespace}{local}")).into())
}

/// The asserted statements of the schema, in every graph, sorted: the schema properties',
/// the declarations', and the cells of the lists they name.
fn schema_statements(snapshot: &Snapshot) -> Vec<nrese_owl::Statement> {
    let mut out = Vec::new();
    let scan = |pattern: QuadPattern, out: &mut Vec<nrese_owl::Statement>| {
        out.extend(
            snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &pattern)
                .map(|q| nrese_owl::Statement {
                    triple: [q.subject.raw(), q.predicate.raw(), q.object.raw()],
                    graph: q.graph.raw(),
                }),
        );
    };
    let pattern = |predicate: TermId, object: Option<TermId>| QuadPattern {
        subject: None,
        predicate: Some(predicate),
        object,
        graph: GraphSelector::Any,
    };
    for &(namespace, local) in SCHEMA {
        if let Some(p) = iri(snapshot, namespace, local) {
            scan(pattern(p, None), &mut out);
        }
    }
    if let Some(rdf_type) = iri(snapshot, RDF, "type") {
        for &(namespace, local) in DECLARATIONS {
            if let Some(class) = iri(snapshot, namespace, local) {
                scan(pattern(rdf_type, Some(class)), &mut out);
            }
        }
    }
    // The lists: from each head, `rdf:first` and `rdf:rest` along the cells.
    let (first, rest) = (iri(snapshot, RDF, "first"), iri(snapshot, RDF, "rest"));
    if let (Some(first), Some(rest)) = (first, rest) {
        let heads: Vec<u64> = LISTS
            .iter()
            .filter_map(|local| iri(snapshot, OWL, local))
            .flat_map(|p| {
                out.iter()
                    .filter(move |s| s.triple[1] == p.raw())
                    .map(|s| s.triple[2])
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        for head in heads {
            let mut cell = head;
            while TermId::from_raw(cell).kind() == nrese_engine::TermKind::BlankNode
                && seen.insert(cell)
            {
                let mut next = None;
                for predicate in [first, rest] {
                    let before = out.len();
                    scan(
                        QuadPattern {
                            subject: Some(TermId::from_raw(cell)),
                            ..pattern(predicate, None)
                        },
                        &mut out,
                    );
                    if predicate == rest {
                        next = out[before..].first().map(|s| s.triple[2]);
                    }
                }
                match next {
                    Some(n) => cell = n,
                    None => break,
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The snapshot's terms, as the mapping reads them.
struct SnapshotTerms<'a>(&'a Snapshot);

impl nrese_owl::Terms for SnapshotTerms<'_> {
    fn kind(&self, term: nrese_owl::Term) -> nrese_owl::TermKind {
        match TermId::from_raw(term).kind() {
            nrese_engine::TermKind::Iri => nrese_owl::TermKind::Iri,
            nrese_engine::TermKind::BlankNode => nrese_owl::TermKind::Blank,
            _ => nrese_owl::TermKind::Literal,
        }
    }

    fn lexical(&self, term: nrese_owl::Term) -> Option<String> {
        match self.0.decode(TermId::from_raw(term))? {
            Term::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<nrese_owl::Term> {
        self.0
            .lookup(NamedNodeRef::new_unchecked(iri).into())
            .map(TermId::raw)
    }

    fn datatype(&self, term: nrese_owl::Term) -> Option<String> {
        match self.0.decode(TermId::from_raw(term))? {
            Term::Literal(l) => Some(l.datatype().as_str().to_owned()),
            _ => None,
        }
    }

    fn iri_text(&self, term: nrese_owl::Term) -> Option<String> {
        match self.0.decode(TermId::from_raw(term))? {
            Term::NamedNode(n) => Some(n.into_string()),
            _ => None,
        }
    }
}

fn compile(snapshot: &Snapshot, statements: &[nrese_owl::Statement], closure: Closure) -> Tbox {
    let terms = SnapshotTerms(snapshot);
    let ontology = nrese_owl::read(statements, &terms);
    let thing = iri(snapshot, OWL, "Thing").map(TermId::raw);
    Tbox::compile(&ontology, closure, thing)
}
