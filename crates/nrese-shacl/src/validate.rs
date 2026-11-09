//! The validator (design §5): focus nodes from targets, value nodes from paths, results
//! from constraint components.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::ControlFlow;
use std::sync::{Arc, OnceLock};

use nrese_engine::{TermId, TermKind};
use nrese_exec::{Budget, Reservation};
use nrese_rdf::{BlankNode, NamedNode, NamedOrBlankNode, Term, Triple};
use nrese_sparql::value::{Value, compare, lang_matches};
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};

use crate::datatype::well_formed;
use crate::graph::{GraphView, Selection};
use crate::model::{
    Bound, Component, Constraint, Logical, NodeKind, Path, Shape, ShapeRef, Shapes, SparqlCheck,
    Target,
};
use crate::{path, sparql};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// A term a result names: one of the dictionary, or one a SHACL-SPARQL query computed
/// (`CONCAT(…) AS ?value`, `BNODE()`), owned here and never interned, so a validation leaves
/// the dictionary as it found it.
///
/// Which one is decided against a *frontier* view ([`Validator::with_frontier`]): the view
/// validated, or for a comparison of two states the later one, for both. The dictionary only
/// grows, so what the earlier view knows the later knows by the same id, and every id a
/// result holds otherwise (a focus or value node, a shape's path) is the later dictionary's
/// too: one RDF term has one form in both states, whatever made it. Blank nodes and triple
/// terms holding them are the exception ([`Validator::result_term`]). Equality and order are
/// the comparison's: a computed term by its [`Computed::key`], so fresh blank nodes compare
/// up to their names; a stored and a computed term are never equal.
#[derive(Debug, Clone)]
pub(crate) enum ResultTerm {
    Stored(TermId),
    Computed(Box<Computed>),
}

/// A term a query computed: as the report shows it, and as it is compared.
#[derive(Debug, Clone)]
pub(crate) struct Computed {
    /// The term as the query made it: its blank nodes keep the query's names, so one node
    /// of a query is one node of the report, however many results name it.
    pub(crate) term: Term,
    /// The term as it is compared, where that differs from `term`: its fresh blank nodes
    /// named by their place in it and its stored ones by id ([`Validator::triple_key`]).
    key: Option<Term>,
}

impl Computed {
    fn key(&self) -> &Term {
        self.key.as_ref().unwrap_or(&self.term)
    }

    /// The heap it holds.
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + crate::report::term_bytes(&self.term)
            + self.key.as_ref().map_or(0, crate::report::term_bytes)
    }
}

impl ResultTerm {
    fn order(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Stored(a), Self::Stored(b)) => a.cmp(b),
            (Self::Stored(_), Self::Computed(_)) => Ordering::Less,
            (Self::Computed(_), Self::Stored(_)) => Ordering::Greater,
            (Self::Computed(a), Self::Computed(b)) => a.key().cmp(b.key()),
        }
    }
}

impl PartialEq for ResultTerm {
    fn eq(&self, other: &Self) -> bool {
        self.order(other) == Ordering::Equal
    }
}

impl Eq for ResultTerm {}

impl PartialOrd for ResultTerm {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ResultTerm {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order(other)
    }
}

impl std::hash::Hash for ResultTerm {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Stored(id) => (0_u8, id).hash(state),
            Self::Computed(computed) => (1_u8, computed.key()).hash(state),
        }
    }
}

/// A result's path: the shape's (or a closed shape's predicate, or a stored IRI a query
/// returned as `?path`), or an IRI a query computed, owned as [`Computed`] terms are.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ResultPath {
    Path(Path),
    Computed(Box<NamedNode>),
}

/// A validation result over term ids, with the terms SHACL-SPARQL computed owned.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RawResult {
    pub(crate) focus: TermId,
    pub(crate) path: Option<ResultPath>,
    pub(crate) value: Option<ResultTerm>,
    pub(crate) shape: ShapeRef,
    pub(crate) component: Component,
    /// `sh:sourceConstraint` (SHACL-SPARQL).
    pub(crate) source: Option<TermId>,
    /// The result's messages, where they aren't the shape's (SHACL-SPARQL).
    pub(crate) messages: Option<Vec<TermId>>,
}

impl RawResult {
    /// What keeping this result can hold, at most: its slot in the vector of its focus node
    /// and in the collected one (each up to twice its items while it grows), and its heap,
    /// the terms a query computed (owned here) included.
    pub(crate) fn retained_bytes(&self) -> usize {
        4 * std::mem::size_of::<Self>()
            + match &self.path {
                None => 0,
                Some(ResultPath::Path(path)) => path_bytes(path),
                Some(ResultPath::Computed(predicate)) => {
                    std::mem::size_of::<NamedNode>() + predicate.as_str().len()
                }
            }
            + match &self.value {
                Some(ResultTerm::Computed(computed)) => computed.bytes(),
                _ => 0,
            }
            + self.messages.as_ref().map_or(0, |messages| {
                messages.capacity() * std::mem::size_of::<TermId>()
            })
    }
}

/// The heap a path holds.
fn path_bytes(path: &Path) -> usize {
    match path {
        Path::Predicate(_) => 0,
        Path::Inverse(inner)
        | Path::ZeroOrMore(inner)
        | Path::OneOrMore(inner)
        | Path::ZeroOrOne(inner) => std::mem::size_of::<Path>() + path_bytes(inner),
        Path::Sequence(parts) | Path::Alternative(parts) => {
            parts.capacity() * std::mem::size_of::<Path>()
                + parts.iter().map(path_bytes).sum::<usize>()
        }
    }
}

/// A validation's failures with their charge, which goes where they go: the report takes
/// both, nothing is charged twice. The fields drop in this order: the strings are freed,
/// then their charge returned.
pub(crate) struct Failures {
    pub(crate) list: Vec<String>,
    pub(crate) held: Reservation,
}

/// What a validation keeps ([`Validator::into_failures`]): its results, its failures with
/// their charge, and the results' charge (free the results, then drop it).
pub(crate) type Kept = (Vec<RawResult>, Failures, Reservation);

/// The results one task keeps ([`Validator::validate_all`]), with their charge: the task's
/// own, so the tasks of a validation charge without sharing a lock. The fields drop in this
/// order: the results are freed, then their charge returned.
pub(crate) struct TaskResults {
    list: Vec<RawResult>,
    held: Reservation,
}

impl TaskResults {
    fn new(budget: &Arc<Budget>) -> Self {
        Self {
            list: Vec::new(),
            held: Reservation::new(Arc::clone(budget)),
        }
    }

    /// Drops the results from `start` on, then returns their charge.
    fn truncate(&mut self, start: usize) {
        let dropped: usize = self.list[start..]
            .iter()
            .map(RawResult::retained_bytes)
            .sum();
        self.list.truncate(start);
        self.held.release(dropped);
    }
}

/// Validates the data in `data` against every targeted shape: the results, and the
/// failures (SPARQL-based constraints that couldn't be evaluated), with their charge to
/// `resources`' envelope. Focus nodes are validated in parallel; the results keep the
/// sequential order. A stop of `resources` is the outcome instead
/// ([`Validator::into_failures`]). `compares`: the results will be compared with another
/// state's ([`Validator::comparing`]).
pub(crate) fn validate_raw<V: ReadView + Sync>(
    view: &V,
    shapes: &Shapes,
    data: Selection,
    resources: &Resources,
    compares: bool,
) -> Result<Kept, QueryEvaluationError> {
    resources.check()?;
    let mut validator = Validator::new(view, shapes, data, resources);
    if compares {
        validator = validator.comparing();
    }
    let results = validate_every_focus(&validator, shapes);
    let (failures, held) = validator.into_failures()?;
    Ok((results, failures, held))
}

/// [`validate_raw`]'s results alone, with their charge, for a validation whose failures are
/// thrown away (the state before a change, compared with the state after): its failures
/// are neither kept nor charged, so they can't refuse it. Its terms are told stored or
/// computed against `frontier`, the state after's dictionary ([`ResultTerm`]).
pub(crate) fn validate_raw_results<'a, V: ReadView + Sync>(
    view: &'a V,
    shapes: &'a Shapes,
    data: Selection,
    resources: &'a Resources,
    frontier: &'a Frontier<'a>,
) -> Result<(Vec<RawResult>, Reservation), QueryEvaluationError> {
    resources.check()?;
    let validator = Validator::discarding_failures(view, shapes, data, resources)
        .with_frontier(frontier)
        .comparing();
    let results = validate_every_focus(&validator, shapes);
    let (_, held) = validator.into_failures()?;
    Ok((results, held))
}

/// Every targeted shape against each of its focus nodes; after a stop, nothing (freed
/// before the validator's charge goes).
fn validate_every_focus<V: ReadView + Sync>(
    validator: &Validator<'_, V>,
    shapes: &Shapes,
) -> Vec<RawResult> {
    let work: Vec<(ShapeRef, TermId)> = shapes
        .targeted()
        .flat_map(|shape| {
            validator
                .focus_nodes(&shapes.shapes[shape])
                .into_iter()
                .map(move |focus| (shape, focus))
        })
        .collect();
    let results = validator.validate_all(&work);
    if validator.has_stopped() {
        return Vec::new();
    }
    results
}

/// Charges `bytes` to `held`, or records the refusal as the validation's stop.
fn charge_to(
    held: &std::sync::Mutex<Reservation>,
    stop: &OnceLock<QueryEvaluationError>,
    bytes: usize,
) -> bool {
    let charged = held
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .charge(bytes);
    match charged {
        Ok(()) => true,
        Err(refused) => {
            let _ = stop.set(QueryEvaluationError::MemoryLimit(refused));
            false
        }
    }
}

/// Whether validating `data` against `shapes` checks anything: an active targeted shape
/// that can fail ([`can_fail`]) and has at least one focus node. A conforming report with
/// nothing applicable is a vacuous pass.
pub(crate) fn applicable<V: ReadView + Sync>(view: &V, shapes: &Shapes, data: Selection) -> bool {
    // Only targets are read: no constraint runs, so nothing that resources stop.
    let resources = Resources::default();
    let validator = Validator::discarding_failures(view, shapes, data, &resources);
    let fails = can_fail(shapes);
    shapes
        .targeted()
        .any(|shape| fails[shape] && !validator.focus_nodes(&shapes.shapes[shape]).is_empty())
}

/// Which shapes can produce a result for some node: an active shape with a constraint that
/// can fail. A shape that can't is satisfied by every node (an empty or deactivated shape
/// conforms), which decides the logical components: `sh:not` of any shape can fail (a node
/// that conforms fails it), `sh:or` only if every member can, `sh:xone` of one member as
/// that member.
///
/// The least fixpoint over the shapes: every shape starts as one that can't fail, and a
/// shape is checked again when one it names turns out to fail. The rules only ever turn a
/// shape to "can fail", so each turns at most once; a shape reached again through itself
/// counts as conforming meanwhile, as validation takes a node being validated against a
/// shape to conform. The answer doesn't depend on the order shapes are visited in.
fn can_fail(shapes: &Shapes) -> Vec<bool> {
    let count = shapes.shapes.len();
    // The shapes that name each shape in a constraint whose answer depends on it.
    let mut named_by: Vec<Vec<ShapeRef>> = vec![Vec::new(); count];
    for (shape, compiled) in shapes.shapes.iter().enumerate() {
        for constraint in &compiled.constraints {
            let nested: &[ShapeRef] = match constraint {
                Constraint::Node(nested) | Constraint::Property(nested) => {
                    std::slice::from_ref(nested)
                }
                Constraint::Logical(_, nested) => nested,
                _ => &[],
            };
            for &nested in nested {
                named_by[nested].push(shape);
            }
        }
    }
    let mut fails = vec![false; count];
    let mut work: Vec<ShapeRef> = (0..count).collect();
    while let Some(shape) = work.pop() {
        let compiled = &shapes.shapes[shape];
        if fails[shape] || compiled.deactivated {
            continue;
        }
        if compiled
            .constraints
            .iter()
            .any(|constraint| constraint_can_fail(constraint, &fails))
        {
            fails[shape] = true;
            work.extend_from_slice(&named_by[shape]);
        }
    }
    fails
}

/// Whether `constraint` can fail, given which shapes can (`fails`).
fn constraint_can_fail(constraint: &Constraint, fails: &[bool]) -> bool {
    match constraint {
        // A node that conforms to the nested shape fails `sh:not`; one always does.
        Constraint::Not(_) => true,
        Constraint::Node(nested) | Constraint::Property(nested) => fails[*nested],
        Constraint::Logical(Logical::And, nested) => nested.iter().any(|&nested| fails[nested]),
        // Fails where no member conforms: never if one always does (`sh:or ()`: always).
        Constraint::Logical(Logical::Or, nested) => nested.iter().all(|&nested| fails[nested]),
        // Fails unless exactly one member conforms: never only for one member that always
        // does.
        Constraint::Logical(Logical::Xone, nested) => match nested.as_slice() {
            [only] => fails[*only],
            _ => true,
        },
        // A count of conforming values: below a positive minimum (no values) or above a
        // maximum (every value conforming) whatever the nested shape.
        Constraint::Qualified { min, max, .. } => min.is_some_and(|min| min > 0) || max.is_some(),
        _ => true,
    }
}

pub(crate) struct Validator<'a, V: ReadView> {
    graph: GraphView<'a, V>,
    shapes: &'a Shapes,
    /// What the validation runs on, its SHACL-SPARQL queries included.
    resources: &'a Resources,
    /// The first stop met: the cancellation fired, or the envelope refused a query's
    /// charge. From then on work returns without results, and the stop is the validation's
    /// outcome, whatever the work returned.
    stop: OnceLock<QueryEvaluationError>,
    failures: std::sync::Mutex<Vec<String>>,
    /// The charge, to the request's envelope, for the failures kept: made before each is
    /// kept; it goes with them ([`Failures`]). Declared after them: freed, then returned.
    held_failures: std::sync::Mutex<Reservation>,
    /// The charge for the results kept: made before each is kept, by its task
    /// ([`TaskResults`]), and taken over here when the task ends; held until they are dropped.
    held_results: std::sync::Mutex<Reservation>,
    /// The budget the results are charged to (`held_results`' and the tasks').
    results_budget: Arc<Budget>,
    /// Whether failures are kept: not where they are thrown away ([`validate_raw_results`]),
    /// so they are neither made nor charged there.
    keep_failures: bool,
    /// The view whose dictionary decides whether a term a query bound is stored or computed
    /// ([`ResultTerm`]); `None`: this validator's own.
    frontier: Option<&'a Frontier<'a>>,
    /// Whether its results are compared with another state's ([`Validator::comparing`]): only
    /// then do blank nodes a query bound need their comparison form.
    compares: bool,
    rdf_type: Option<TermId>,
    /// Each class the shapes name, with its subclasses (itself included).
    classes: HashMap<TermId, BTreeSet<TermId>>,
}

/// The shapes and nodes being validated further up: recursion stops there.
type Stack = Vec<(ShapeRef, TermId)>;

/// A frontier view's lookup ([`ResultTerm`]): the id of a term it knows.
pub(crate) type Frontier<'a> = dyn Fn(&Term) -> Option<TermId> + Sync + 'a;

/// The comparison form of a stored blank node inside a computed term: an IRI no data can
/// hold (its salt is drawn once per process), naming the node by id, so it is never taken
/// for a fresh blank node nor renamed with them.
fn stored_blank(id: TermId) -> NamedNode {
    static SALT: OnceLock<u64> = OnceLock::new();
    let salt = SALT.get_or_init(|| {
        std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), 0_u8)
    });
    NamedNode::new_unchecked(format!("urn:nrese-shacl:{salt:016x}:blank:{}", id.raw()))
}

/// The comparison form of a lone fresh blank node: every fresh node is the same up to its
/// name. (A computed term's other keys are triple terms or hold no blank node.)
fn fresh_blank() -> Term {
    BlankNode::new_unchecked("fresh").into()
}

/// Whether `term` is a blank node or a triple term holding one, at any depth.
fn holds_blank_node(term: &Term) -> bool {
    match term {
        Term::BlankNode(_) => true,
        Term::Triple(triple) => {
            matches!(triple.subject, NamedOrBlankNode::BlankNode(_))
                || holds_blank_node(&triple.object)
        }
        _ => false,
    }
}

impl<'a, V: ReadView> Validator<'a, V> {
    pub(crate) fn new(
        view: &'a V,
        shapes: &'a Shapes,
        data: Selection,
        resources: &'a Resources,
    ) -> Self {
        Self::build(view, shapes, data, resources, true)
    }

    /// A validator whose failures are thrown away: none is made or charged.
    pub(crate) fn discarding_failures(
        view: &'a V,
        shapes: &'a Shapes,
        data: Selection,
        resources: &'a Resources,
    ) -> Self {
        Self::build(view, shapes, data, resources, false)
    }

    fn build(
        view: &'a V,
        shapes: &'a Shapes,
        data: Selection,
        resources: &'a Resources,
        keep_failures: bool,
    ) -> Self {
        let graph = GraphView::new(view, data);
        let results_budget = Arc::new(resources.memory.part());
        let mut validator = Self {
            rdf_type: graph.iri(RDF_TYPE),
            graph,
            shapes,
            resources,
            stop: OnceLock::new(),
            failures: std::sync::Mutex::default(),
            held_failures: std::sync::Mutex::new(Reservation::new(Arc::new(
                resources.memory.part(),
            ))),
            held_results: std::sync::Mutex::new(Reservation::new(Arc::clone(&results_budget))),
            results_budget,
            keep_failures,
            frontier: None,
            compares: false,
            classes: HashMap::new(),
        };
        let sub_class_of = graph.iri(RDFS_SUB_CLASS_OF);
        for shape in &shapes.shapes {
            let named = shape
                .targets
                .iter()
                .filter_map(|target| match target {
                    Target::Class(class) => Some(*class),
                    _ => None,
                })
                .chain(shape.constraints.iter().filter_map(|c| match c {
                    Constraint::Class(class) => Some(*class),
                    _ => None,
                }));
            for class in named {
                validator
                    .classes
                    .entry(class)
                    .or_insert_with(|| subclasses(&graph, sub_class_of, class));
            }
        }
        // The shapes graph's `sh:entailment` values are failures of every validation (SHACL
        // §1.5: no entailment regime is supported), the first ones, charged as the others are.
        if validator.keep_failures {
            for &regime in &shapes.entailments {
                let regime = graph
                    .decode(regime)
                    .map_or_else(String::new, |term| term.to_string());
                let failure = format!(
                    "the shapes graph asks for the entailment regime {regime} (sh:entailment), \
                     which this processor doesn't support"
                );
                if charge_to(
                    &validator.held_failures,
                    &validator.stop,
                    2 * std::mem::size_of::<String>() + failure.len(),
                ) {
                    validator
                        .failures
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(failure);
                }
            }
        }
        validator
    }

    /// The validator, deciding stored and computed terms against `frontier`'s dictionary: the
    /// later state's, where this one (the earlier) is compared with it ([`ResultTerm`]).
    pub(crate) fn with_frontier(mut self, frontier: &'a Frontier<'a>) -> Self {
        self.frontier = Some(frontier);
        self
    }

    /// The validator, its results to be compared with another state's ([`validate_changes`],
    /// [`validate_shapes_change`]): blank nodes a query bound get their comparison form
    /// ([`Validator::result_term`]). Without it (a report alone) they are kept as the query
    /// made them, which is what the report shows either way.
    ///
    /// [`validate_changes`]: crate::validate_changes
    /// [`validate_shapes_change`]: crate::validate_shapes_change
    pub(crate) fn comparing(mut self) -> Self {
        self.compares = true;
        self
    }

    /// The id of `term` in the frontier's dictionary.
    fn known(&self, term: &Term) -> Option<TermId> {
        match self.frontier {
            Some(frontier) => frontier(term),
            None => self.graph.lookup(term),
        }
    }

    /// A term a query bound, as a result holds it: its id where the frontier knows it; else
    /// owned, with its comparison form. A blank node is stored where this view knows it (a node
    /// of this state's data, compared by id), else fresh (one the query made): the frontier
    /// isn't asked, since a later state may store a node under a fresh node's name. A triple
    /// term holding a fresh blank node can't be stored (the dictionary knows a triple term only
    /// with its parts): computed, compared by [`Validator::triple_key`]. One holding only this
    /// state's blank nodes goes through the frontier as any other term. Where nothing compares
    /// the results ([`Validator::comparing`]), a blank node or a triple term holding one is the
    /// query's term as it is: no lookup, no comparison form (the report shows the same term).
    fn result_term(&self, term: Term) -> ResultTerm {
        if !self.compares && holds_blank_node(&term) {
            return ResultTerm::Computed(Box::new(Computed { term, key: None }));
        }
        if let Term::BlankNode(_) = term {
            return match self.graph.lookup(&term) {
                Some(id) => ResultTerm::Stored(id),
                None => ResultTerm::Computed(Box::new(Computed {
                    term,
                    key: Some(fresh_blank()),
                })),
            };
        }
        let key = match &term {
            Term::Triple(_) if holds_blank_node(&term) => {
                let mut fresh = Vec::new();
                let key = self.triple_key(&term, &mut fresh);
                if !fresh.is_empty() {
                    return ResultTerm::Computed(Box::new(Computed {
                        term,
                        key: Some(key),
                    }));
                }
                Some(key)
            }
            _ => None,
        };
        match self.known(&term) {
            Some(id) => ResultTerm::Stored(id),
            None => ResultTerm::Computed(Box::new(Computed { term, key })),
        }
    }

    /// The comparison form of a triple term holding blank nodes: each this view knows named by
    /// its [`stored_blank`] IRI, each fresh one (gathered in `fresh`) by its place among them in
    /// order of first appearance, subject before object, depth first. A triple term is an
    /// ordered tree, so a renaming of fresh blank nodes that maps one term onto another maps
    /// each place to the same place: two terms are the same up to their fresh nodes' names
    /// exactly when their forms are equal. One walk over the term.
    fn triple_key(&self, term: &Term, fresh: &mut Vec<BlankNode>) -> Term {
        let mut blank = |node: &BlankNode| -> Term {
            match self.graph.lookup(&Term::BlankNode(node.clone())) {
                Some(id) => stored_blank(id).into(),
                None => {
                    let place = match fresh.iter().position(|seen| seen == node) {
                        Some(place) => place,
                        None => {
                            fresh.push(node.clone());
                            fresh.len() - 1
                        }
                    };
                    BlankNode::new_unchecked(format!("f{place}")).into()
                }
            }
        };
        match term {
            Term::BlankNode(node) => blank(node),
            Term::Triple(triple) => {
                let subject = match &triple.subject {
                    NamedOrBlankNode::BlankNode(node) => match blank(node) {
                        Term::NamedNode(stored) => NamedOrBlankNode::NamedNode(stored),
                        Term::BlankNode(renamed) => NamedOrBlankNode::BlankNode(renamed),
                        _ => unreachable!("a blank node's form is an IRI or a blank node"),
                    },
                    NamedOrBlankNode::NamedNode(_) => triple.subject.clone(),
                };
                let object = self.triple_key(&triple.object, fresh);
                Term::Triple(Box::new(Triple::new(
                    subject,
                    triple.predicate.clone(),
                    object,
                )))
            }
            _ => term.clone(),
        }
    }

    /// An IRI a query bound as `?path`, as a result holds it (as [`Validator::result_term`]).
    fn result_path(&self, path: NamedNode) -> ResultPath {
        let path = Term::NamedNode(path);
        match self.known(&path) {
            Some(id) => ResultPath::Path(Path::Predicate(id)),
            None => match path {
                Term::NamedNode(path) => ResultPath::Computed(Box::new(path)),
                _ => unreachable!("made a named node above"),
            },
        }
    }

    /// The failures recorded while validating, with their charge, and the charge for the
    /// results kept; or the stop that ended it: then no result it produced may be used (drop
    /// them first: [`Validator::has_stopped`]).
    pub(crate) fn into_failures(self) -> Result<(Failures, Reservation), QueryEvaluationError> {
        if let Some(stop) = self.stop.into_inner() {
            return Err(stop);
        }
        let list = self
            .failures
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = self
            .held_failures
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let results = self
            .held_results
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok((Failures { list, held }, results))
    }

    /// Whether a stop was met: the results made are then dropped before the validator (and
    /// with it their charge) goes.
    pub(crate) fn has_stopped(&self) -> bool {
        self.stop.get().is_some()
    }

    /// Returns `bytes` charged for results that have been dropped.
    pub(crate) fn release(&self, bytes: usize) {
        self.held_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release(bytes);
    }

    /// Keeps `result` in `results`, charged first to the task's own reservation (no lock); a
    /// refusal is the validation's stop.
    fn keep(&self, results: &mut TaskResults, result: RawResult) {
        match results.held.charge(result.retained_bytes()) {
            Ok(()) => results.list.push(result),
            Err(refused) => {
                let _ = self.stop.set(QueryEvaluationError::MemoryLimit(refused));
            }
        }
    }

    /// A task's results, its charge taken over by the validation's (one lock a task): the
    /// charge is never returned in between, so the results stay charged throughout.
    fn adopt(&self, task: TaskResults) -> Vec<RawResult> {
        let TaskResults { list, held } = task;
        let bytes = held.transfer();
        let mut shared = self
            .held_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let empty = Reservation::new(Arc::clone(&self.results_budget));
        let total = std::mem::replace(&mut *shared, empty).transfer() + bytes;
        *shared = Reservation::adopt(Arc::clone(&self.results_budget), total);
        list
    }

    /// Whether the validation has stopped: a stop was met, or the cancellation fires now
    /// (recorded as the stop).
    pub(crate) fn stopped(&self) -> bool {
        if self.stop.get().is_some() {
            return true;
        }
        match self.resources.check() {
            Ok(()) => false,
            Err(stop) => {
                let _ = self.stop.set(stop);
                true
            }
        }
    }

    /// The results of validating each `(shape, focus node)` of `work`, in that order: in
    /// parallel, a few dozen nodes per task, each task charging its own reservation, taken
    /// over by the validation's when it ends ([`Validator::adopt`]).
    pub(crate) fn validate_all(&self, work: &[(ShapeRef, TermId)]) -> Vec<RawResult>
    where
        V: Sync,
    {
        use rayon::prelude::*;
        work.par_iter()
            .with_min_len(32)
            .fold(
                || TaskResults::new(&self.results_budget),
                |mut task, &(shape, focus)| {
                    self.validate_node(shape, focus, &mut task, &mut Vec::new());
                    task
                },
            )
            .map(|task| self.adopt(task))
            .flat_map_iter(|list| list)
            .collect()
    }

    pub(crate) fn focus_nodes(&self, shape: &Shape) -> Vec<TermId> {
        let mut nodes: Vec<TermId> = Vec::new();
        for target in &shape.targets {
            match *target {
                Target::Node(node) => nodes.push(node),
                Target::Class(class) => {
                    if let Some(rdf_type) = self.rdf_type {
                        for &subclass in &self.classes[&class] {
                            nodes.extend(self.graph.subjects(rdf_type, subclass));
                        }
                    }
                }
                Target::SubjectsOf(predicate) => nodes.extend(self.graph.subjects_of(predicate)),
                Target::ObjectsOf(predicate) => nodes.extend(self.graph.objects_of(predicate)),
            }
        }
        nodes.sort_unstable();
        nodes.dedup();
        nodes
    }

    /// Whether `node` is a SHACL instance of `class`: typed by it or by a subclass.
    fn is_instance(&self, node: TermId, class: TermId) -> bool {
        let Some(rdf_type) = self.rdf_type else {
            return false;
        };
        let subclasses = &self.classes[&class];
        !node.kind().is_literal()
            && self
                .graph
                .objects(node, rdf_type)
                .iter()
                .any(|class| subclasses.contains(class))
    }

    /// Whether `node` conforms to `shape`: validating it produces no result.
    fn conforms(&self, shape: ShapeRef, node: TermId, stack: &mut Stack) -> bool {
        let mut results = TaskResults::new(&self.results_budget);
        self.validate_node(shape, node, &mut results, stack);
        // Only their existence counts: dropped, they are freed, then their charge returned.
        results.list.is_empty()
    }

    pub(crate) fn validate_node(
        &self,
        shape_ref: ShapeRef,
        focus: TermId,
        results: &mut TaskResults,
        stack: &mut Stack,
    ) {
        // Every validation of a node against a shape, nested ones included, looks at the
        // stop first: the work after one is bounded by one call's.
        if self.stopped() {
            return;
        }
        let shape = &self.shapes.shapes[shape_ref];
        // Recursive shapes are undefined in SHACL: a node already being validated against
        // the shape is taken to conform.
        if shape.deactivated || stack.contains(&(shape_ref, focus)) {
            return;
        }
        stack.push((shape_ref, focus));
        let values = match &shape.path {
            None => vec![focus],
            Some(path) => path::values(&self.graph, path, focus),
        };
        for constraint in &shape.constraints {
            self.check(shape_ref, focus, &values, constraint, results, stack);
        }
        stack.pop();
    }

    /// The string `sh:minLength`, `sh:maxLength` and `sh:pattern` look at: an IRI or a
    /// literal's lexical form; blank nodes have none.
    fn string(&self, node: TermId) -> Option<String> {
        match self.graph.decode(node)? {
            Term::NamedNode(node) => Some(node.into_string()),
            Term::Literal(literal) => Some(literal.value().to_owned()),
            Term::BlankNode(_) | Term::Triple(_) => None,
        }
    }

    fn value(&self, node: TermId) -> Option<Value> {
        self.graph.decode(node).map(|term| Value::of(&term))
    }

    /// The language tag of `node`, lower-cased, if it is a language-tagged literal.
    fn language(&self, node: TermId) -> Option<String> {
        if node.kind() != TermKind::LangString {
            return None;
        }
        match self.graph.decode(node)? {
            Term::Literal(literal) => literal.language().map(str::to_ascii_lowercase),
            _ => None,
        }
    }

    fn has_datatype(&self, node: TermId, datatype: &str) -> bool {
        if !node.kind().is_literal() {
            return false;
        }
        match self.graph.decode(node) {
            Some(Term::Literal(literal)) => {
                let actual = if literal.language().is_some() {
                    RDF_LANG_STRING
                } else {
                    literal.datatype().as_str()
                };
                actual == datatype && well_formed(datatype, literal.value(), literal.language())
            }
            _ => false,
        }
    }

    #[expect(clippy::too_many_lines)]
    fn check(
        &self,
        shape_ref: ShapeRef,
        focus: TermId,
        values: &[TermId],
        constraint: &Constraint,
        results: &mut TaskResults,
        stack: &mut Stack,
    ) {
        let shape = &self.shapes.shapes[shape_ref];
        let result = |component: Component, value: Option<TermId>| RawResult {
            focus,
            path: shape.path.clone().map(ResultPath::Path),
            value: value.map(ResultTerm::Stored),
            shape: shape_ref,
            component,
            source: None,
            messages: None,
        };
        // One result per value node that fails `ok`.
        let mut each = |component: Component, ok: &mut dyn FnMut(TermId) -> bool| {
            for &value in values {
                if !ok(value) {
                    self.keep(results, result(component, Some(value)));
                }
            }
        };
        match constraint {
            Constraint::Class(class) => {
                each(Component::Class, &mut |v| self.is_instance(v, *class));
            }
            Constraint::Datatype(datatype) => {
                each(Component::Datatype, &mut |v| self.has_datatype(v, datatype));
            }
            Constraint::NodeKind(kind) => each(Component::NodeKind, &mut |v| {
                let (iri, blank) = (v.kind() == TermKind::Iri, v.kind() == TermKind::BlankNode);
                let literal = v.kind().is_literal();
                match kind {
                    NodeKind::Iri => iri,
                    NodeKind::BlankNode => blank,
                    NodeKind::Literal => literal,
                    NodeKind::BlankNodeOrIri => blank || iri,
                    NodeKind::BlankNodeOrLiteral => blank || literal,
                    NodeKind::IriOrLiteral => iri || literal,
                }
            }),
            Constraint::MinCount(min) => {
                if (values.len() as u64) < *min {
                    self.keep(results, result(Component::MinCount, None));
                }
            }
            Constraint::MaxCount(max) => {
                if values.len() as u64 > *max {
                    self.keep(results, result(Component::MaxCount, None));
                }
            }
            Constraint::Range(bound, limit) => {
                let (component, accepted): (Component, &[Ordering]) = match bound {
                    Bound::MinExclusive => (Component::MinExclusive, &[Ordering::Greater]),
                    Bound::MinInclusive => (
                        Component::MinInclusive,
                        &[Ordering::Greater, Ordering::Equal],
                    ),
                    Bound::MaxExclusive => (Component::MaxExclusive, &[Ordering::Less]),
                    Bound::MaxInclusive => {
                        (Component::MaxInclusive, &[Ordering::Less, Ordering::Equal])
                    }
                };
                // A value that can't be compared with the bound violates it.
                each(component, &mut |v| {
                    self.value(v)
                        .and_then(|value| compare(&value, limit))
                        .is_some_and(|order| accepted.contains(&order))
                });
            }
            Constraint::MinLength(min) => each(Component::MinLength, &mut |v| {
                self.string(v)
                    .is_some_and(|s| s.chars().count() as u64 >= *min)
            }),
            Constraint::MaxLength(max) => each(Component::MaxLength, &mut |v| {
                self.string(v)
                    .is_some_and(|s| s.chars().count() as u64 <= *max)
            }),
            Constraint::Pattern(regex) => each(Component::Pattern, &mut |v| {
                self.string(v).is_some_and(|s| regex.is_match(&s))
            }),
            Constraint::LanguageIn(ranges) => each(Component::LanguageIn, &mut |v| {
                self.language(v)
                    .is_some_and(|tag| ranges.iter().any(|range| lang_matches(&tag, range)))
            }),
            Constraint::UniqueLang => {
                let mut tags: BTreeMap<String, usize> = BTreeMap::new();
                for &value in values {
                    if let Some(tag) = self.language(value) {
                        *tags.entry(tag).or_default() += 1;
                    }
                }
                for _ in tags.values().filter(|&&count| count > 1) {
                    self.keep(results, result(Component::UniqueLang, None));
                }
            }
            Constraint::Equals(property) => {
                let other = self.graph.objects(focus, *property);
                each(Component::Equals, &mut |v| other.binary_search(&v).is_ok());
                for &value in &other {
                    if !values.contains(&value) {
                        self.keep(results, result(Component::Equals, Some(value)));
                    }
                }
            }
            Constraint::Disjoint(property) => {
                let other = self.graph.objects(focus, *property);
                each(Component::Disjoint, &mut |v| {
                    other.binary_search(&v).is_err()
                });
            }
            Constraint::LessThan(property) | Constraint::LessThanOrEquals(property) => {
                let strict = matches!(constraint, Constraint::LessThan(_));
                let component = if strict {
                    Component::LessThan
                } else {
                    Component::LessThanOrEquals
                };
                let others: Vec<Option<Value>> = self
                    .graph
                    .objects(focus, *property)
                    .into_iter()
                    .map(|other| self.value(other))
                    .collect();
                for &value in values {
                    let left = self.value(value);
                    for right in &others {
                        let order = left
                            .as_ref()
                            .zip(right.as_ref())
                            .and_then(|(l, r)| compare(l, r));
                        let ok = matches!(order, Some(Ordering::Less))
                            || (!strict && matches!(order, Some(Ordering::Equal)));
                        if !ok {
                            self.keep(results, result(component, Some(value)));
                        }
                    }
                }
            }
            Constraint::Not(shape) => {
                each(Component::Not, &mut |v| !self.conforms(*shape, v, stack));
            }
            Constraint::Logical(logical, shapes) => {
                let component = match logical {
                    Logical::And => Component::And,
                    Logical::Or => Component::Or,
                    Logical::Xone => Component::Xone,
                };
                // `all` and `any` stop at the first shape that decides it.
                each(component, &mut |v| match logical {
                    Logical::And => shapes.iter().all(|&s| self.conforms(s, v, stack)),
                    Logical::Or => shapes.iter().any(|&s| self.conforms(s, v, stack)),
                    Logical::Xone => {
                        shapes
                            .iter()
                            .filter(|&&s| self.conforms(s, v, stack))
                            .count()
                            == 1
                    }
                });
            }
            Constraint::Node(shape) => {
                each(Component::Node, &mut |v| self.conforms(*shape, v, stack));
            }
            Constraint::Property(shape) => {
                // The nested property shape reports its own results.
                for &value in values {
                    self.validate_node(*shape, value, results, stack);
                }
            }
            Constraint::Qualified {
                shape,
                min,
                max,
                siblings,
            } => {
                let count = values
                    .iter()
                    .filter(|&&v| {
                        self.conforms(*shape, v, stack)
                            && !siblings.iter().any(|&s| self.conforms(s, v, stack))
                    })
                    .count() as u64;
                if min.is_some_and(|min| count < min) {
                    self.keep(results, result(Component::QualifiedMinCount, None));
                }
                if max.is_some_and(|max| count > max) {
                    self.keep(results, result(Component::QualifiedMaxCount, None));
                }
            }
            Constraint::Closed(allowed) => {
                for &value in values {
                    for (predicate, object) in self.graph.edges(value) {
                        if !allowed.contains(&predicate) {
                            self.keep(
                                results,
                                RawResult {
                                    focus,
                                    path: Some(ResultPath::Path(Path::Predicate(predicate))),
                                    value: Some(ResultTerm::Stored(object)),
                                    shape: shape_ref,
                                    component: Component::Closed,
                                    source: None,
                                    messages: None,
                                },
                            );
                        }
                    }
                }
            }
            Constraint::HasValue(expected) => {
                if !values.contains(expected) {
                    self.keep(results, result(Component::HasValue, None));
                }
            }
            Constraint::In(allowed) => each(Component::In, &mut |v| allowed.contains(&v)),
            Constraint::Sparql(check) => self.sparql(shape_ref, focus, values, check, results),
        }
    }

    /// A SHACL-SPARQL check for `focus` ([`sparql`]): its results, or a failure, or the
    /// validation's stop.
    fn sparql(
        &self,
        shape_ref: ShapeRef,
        focus: TermId,
        values: &[TermId],
        check: &SparqlCheck,
        results: &mut TaskResults,
    ) {
        let shape = &self.shapes.shapes[shape_ref];
        let (view, data) = self.graph.parts();
        let raw = |value: Option<ResultTerm>, path: Option<ResultPath>| RawResult {
            focus,
            path,
            value,
            shape: shape_ref,
            component: check.component,
            source: check.source,
            messages: (!check.messages.is_empty()).then(|| check.messages.clone()),
        };
        let failure = |problem: &str| {
            // Where failures are thrown away, none is made.
            if !self.keep_failures {
                return;
            }
            let at = self
                .graph
                .decode(focus)
                .map_or_else(String::new, |term| term.to_string());
            let failure = format!("a SPARQL constraint at focus node {at}: {problem}");
            // Charged first (twice its slot, for the vector's growth), to the failures' own
            // charge, which the report takes over with them.
            if charge_to(
                &self.held_failures,
                &self.stop,
                2 * std::mem::size_of::<String>() + failure.len(),
            ) {
                self.failures
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(failure);
            }
        };
        // Each combination of the parameters' values is a constraint of its own (SHACL §6.2),
        // run in turn: a stop between them ends the walk.
        check.each_combination(|parameters| {
            if self.stopped() {
                return ControlFlow::Break(());
            }
            let outcome = sparql::run(
                view,
                data,
                check,
                parameters,
                focus,
                values,
                shape.node,
                self.shapes.graph,
                self.resources,
            );
            match outcome {
                Err(sparql::Problem::Stopped(stop)) => {
                    let _ = self.stop.set(stop);
                    return ControlFlow::Break(());
                }
                Err(sparql::Problem::Failed(error)) => failure(&error),
                Ok(sparql::Outcome::Failed(failed)) => {
                    for value in failed {
                        self.keep(
                            results,
                            raw(
                                Some(ResultTerm::Stored(value)),
                                shape.path.clone().map(ResultPath::Path),
                            ),
                        );
                    }
                }
                Ok(sparql::Outcome::Solutions(solutions)) => {
                    // Each solution is decoded, made a result (by id) and dropped before the
                    // next: only the kept results, charged, stay. An evaluation error part way
                    // makes the combination a failure with no results, as when it was read whole.
                    let start = results.list.len();
                    for solution in solutions {
                        if self.stopped() {
                            return ControlFlow::Break(());
                        }
                        let solution = match solution.map_err(sparql::Problem::from) {
                            Ok(solution) => solution,
                            Err(sparql::Problem::Stopped(stop)) => {
                                let _ = self.stop.set(stop);
                                return ControlFlow::Break(());
                            }
                            Err(sparql::Problem::Failed(error)) => {
                                results.truncate(start);
                                failure(&error);
                                break;
                            }
                        };
                        // The solution's terms are moved into the result, not copied; a
                        // `?failure` row's are dropped unread.
                        let found = sparql::found(solution);
                        if found.failure {
                            failure("the query reported ?failure");
                            continue;
                        }
                        // The bound value and path are the result's (SHACL §5.3.2), stored or
                        // computed ([`ResultTerm`]).
                        let value = match found.value {
                            Some(term) => Some(self.result_term(term)),
                            // A node shape's value is its focus node.
                            None => shape.path.is_none().then_some(ResultTerm::Stored(focus)),
                        };
                        let path = found
                            .path
                            .map(|path| self.result_path(path))
                            .or_else(|| shape.path.clone().map(ResultPath::Path));
                        self.keep(results, raw(value, path));
                    }
                }
            }
            ControlFlow::Continue(())
        });
    }
}

/// `class` and everything below it through `rdfs:subClassOf`.
fn subclasses<V: ReadView>(
    graph: &GraphView<'_, V>,
    sub_class_of: Option<TermId>,
    class: TermId,
) -> BTreeSet<TermId> {
    let mut all = BTreeSet::from([class]);
    let Some(sub_class_of) = sub_class_of else {
        return all;
    };
    let mut frontier = vec![class];
    while let Some(class) = frontier.pop() {
        for subclass in graph.subjects(sub_class_of, class) {
            if all.insert(subclass) {
                frontier.push(subclass);
            }
        }
    }
    all
}
