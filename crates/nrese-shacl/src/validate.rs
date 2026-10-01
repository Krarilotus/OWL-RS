//! The validator (design §5): focus nodes from targets, value nodes from paths, results
//! from constraint components.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use nrese_engine::{TermId, TermKind};
use nrese_rdf::Term;
use nrese_sparql::ReadView;
use nrese_sparql::value::{Value, compare, lang_matches};

use crate::datatype::well_formed;
use crate::graph::{GraphView, Selection};
use crate::model::{
    Bound, Component, Constraint, Logical, NodeKind, Path, Shape, ShapeRef, Shapes, Target,
};
use crate::path;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// A validation result over term ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RawResult {
    pub(crate) focus: TermId,
    pub(crate) path: Option<Path>,
    pub(crate) value: Option<TermId>,
    pub(crate) shape: ShapeRef,
    pub(crate) component: Component,
}

/// Validates the data in `data` against every targeted shape.
pub(crate) fn validate_raw<V: ReadView>(
    view: &V,
    shapes: &Shapes,
    data: Selection,
) -> Vec<RawResult> {
    let validator = Validator::new(view, shapes, data);
    let mut results = Vec::new();
    for shape in shapes.targeted() {
        for focus in validator.focus_nodes(&shapes.shapes[shape]) {
            validator.validate_node(shape, focus, &mut results, &mut Vec::new());
        }
    }
    results
}

struct Validator<'a, V: ReadView> {
    graph: GraphView<'a, V>,
    shapes: &'a Shapes,
    rdf_type: Option<TermId>,
    /// Each class the shapes name, with its subclasses (itself included).
    classes: HashMap<TermId, BTreeSet<TermId>>,
}

/// The shapes and nodes being validated further up: recursion stops there.
type Stack = Vec<(ShapeRef, TermId)>;

fn is_literal(id: TermId) -> bool {
    !matches!(
        id.kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
}

impl<'a, V: ReadView> Validator<'a, V> {
    fn new(view: &'a V, shapes: &'a Shapes, data: Selection) -> Self {
        let graph = GraphView::new(view, data);
        let mut validator = Self {
            rdf_type: graph.iri(RDF_TYPE),
            graph,
            shapes,
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
        validator
    }

    fn focus_nodes(&self, shape: &Shape) -> Vec<TermId> {
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
        !is_literal(node)
            && self
                .graph
                .objects(node, rdf_type)
                .iter()
                .any(|class| subclasses.contains(class))
    }

    /// Whether `node` conforms to `shape`: validating it produces no result.
    fn conforms(&self, shape: ShapeRef, node: TermId, stack: &mut Stack) -> bool {
        let mut results = Vec::new();
        self.validate_node(shape, node, &mut results, stack);
        results.is_empty()
    }

    fn validate_node(
        &self,
        shape_ref: ShapeRef,
        focus: TermId,
        results: &mut Vec<RawResult>,
        stack: &mut Stack,
    ) {
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
        if !is_literal(node) {
            return false;
        }
        match self.graph.decode(node) {
            Some(Term::Literal(literal)) => {
                let actual = if literal.language().is_some() {
                    RDF_LANG_STRING
                } else {
                    literal.datatype().as_str()
                };
                actual == datatype && well_formed(datatype, literal.value())
            }
            _ => false,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn check(
        &self,
        shape_ref: ShapeRef,
        focus: TermId,
        values: &[TermId],
        constraint: &Constraint,
        results: &mut Vec<RawResult>,
        stack: &mut Stack,
    ) {
        let shape = &self.shapes.shapes[shape_ref];
        let result = |component: Component, value: Option<TermId>| RawResult {
            focus,
            path: shape.path.clone(),
            value,
            shape: shape_ref,
            component,
        };
        // One result per value node that fails `ok`.
        let mut each = |component: Component, ok: &mut dyn FnMut(TermId) -> bool| {
            for &value in values {
                if !ok(value) {
                    results.push(result(component, Some(value)));
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
                let literal = is_literal(v);
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
                    results.push(result(Component::MinCount, None));
                }
            }
            Constraint::MaxCount(max) => {
                if values.len() as u64 > *max {
                    results.push(result(Component::MaxCount, None));
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
                    results.push(result(Component::UniqueLang, None));
                }
            }
            Constraint::Equals(property) => {
                let other = self.graph.objects(focus, *property);
                each(Component::Equals, &mut |v| other.binary_search(&v).is_ok());
                for &value in &other {
                    if !values.contains(&value) {
                        results.push(result(Component::Equals, Some(value)));
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
                            results.push(result(component, Some(value)));
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
                    results.push(result(Component::QualifiedMinCount, None));
                }
                if max.is_some_and(|max| count > max) {
                    results.push(result(Component::QualifiedMaxCount, None));
                }
            }
            Constraint::Closed(allowed) => {
                for &value in values {
                    for (predicate, object) in self.graph.edges(value) {
                        if !allowed.contains(&predicate) {
                            results.push(RawResult {
                                focus,
                                path: Some(Path::Predicate(predicate)),
                                value: Some(object),
                                shape: shape_ref,
                                component: Component::Closed,
                            });
                        }
                    }
                }
            }
            Constraint::HasValue(expected) => {
                if !values.contains(expected) {
                    results.push(result(Component::HasValue, None));
                }
            }
            Constraint::In(allowed) => each(Component::In, &mut |v| allowed.contains(&v)),
        }
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
