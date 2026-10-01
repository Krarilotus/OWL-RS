//! Reads a shapes graph into [`Shapes`] (design §4).
//!
//! Shapes are found from their targets and their `sh:NodeShape` / `sh:PropertyShape`
//! types, and from there through every shape-valued parameter. An ill-formed shape is an
//! error naming the shape and the parameter: nothing is validated with half a shape.

use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;

use nrese_engine::{TermId, TermKind};
use nrese_rdf::Term;
use nrese_sparql::ReadView;
use nrese_sparql::value::{Value, compile_regex};

use crate::graph::{GraphView, Selection};
use crate::model::{
    Bound, Constraint, Logical, NodeKind, Path, SH, Severity, Shape, ShapeRef, Shapes, Target,
};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_CLASS: &str = "http://www.w3.org/2000/01/rdf-schema#Class";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";

/// The deepest path nesting accepted; a deeper (or cyclic) path is ill-formed.
const MAX_PATH_DEPTH: usize = 64;

/// Builds a path from its one inner path (`sh:inversePath` and the like).
type UnaryPath = fn(Box<Path>) -> Path;
/// Builds a property pair constraint from the other property.
type PropertyPair = fn(TermId) -> Constraint;

/// Why a shapes graph can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeError {
    /// The shape, as it is written in the shapes graph.
    pub shape: String,
    pub message: String,
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "shape {}: {}", self.shape, self.message)
    }
}

impl std::error::Error for ShapeError {}

/// Compiles the shapes in `shapes_graph`.
pub fn compile<V: ReadView>(view: &V, shapes_graph: Selection) -> Result<Shapes, Vec<ShapeError>> {
    let graph = GraphView::new(view, shapes_graph);
    let mut compiler = Compiler {
        rdf_type: graph.iri(&format!("{RDF}type")),
        rdf_first: graph.iri(&format!("{RDF}first")),
        rdf_rest: graph.iri(&format!("{RDF}rest")),
        rdf_nil: graph.iri(&format!("{RDF}nil")),
        graph,
        shapes: Vec::new(),
        index: HashMap::new(),
        errors: Vec::new(),
    };
    let mut roots: BTreeSet<TermId> = BTreeSet::new();
    for target in [
        "targetNode",
        "targetClass",
        "targetSubjectsOf",
        "targetObjectsOf",
    ] {
        if let Some(predicate) = compiler.sh(target) {
            roots.extend(compiler.graph.subjects_of(predicate));
        }
    }
    for class in ["NodeShape", "PropertyShape"] {
        roots.extend(compiler.instances(&format!("{SH}{class}")));
    }
    for root in roots {
        compiler.shape(root);
    }
    if compiler.errors.is_empty() {
        Ok(Shapes {
            shapes: compiler.shapes,
            index: compiler.index,
        })
    } else {
        Err(compiler.errors)
    }
}

struct Compiler<'a, V: ReadView> {
    graph: GraphView<'a, V>,
    rdf_type: Option<TermId>,
    rdf_first: Option<TermId>,
    rdf_rest: Option<TermId>,
    rdf_nil: Option<TermId>,
    shapes: Vec<Shape>,
    index: HashMap<TermId, ShapeRef>,
    errors: Vec<ShapeError>,
}

impl<V: ReadView> Compiler<'_, V> {
    fn sh(&self, local: &str) -> Option<TermId> {
        self.graph.iri(&format!("{SH}{local}"))
    }

    /// The values of the SHACL parameter `local` at `node`.
    fn values(&self, node: TermId, local: &str) -> Vec<TermId> {
        self.sh(local)
            .map(|predicate| self.graph.objects(node, predicate))
            .unwrap_or_default()
    }

    /// The nodes typed `class` in the shapes graph.
    fn instances(&self, class: &str) -> Vec<TermId> {
        match (self.rdf_type, self.graph.iri(class)) {
            (Some(rdf_type), Some(class)) => self.graph.subjects(rdf_type, class),
            _ => Vec::new(),
        }
    }

    fn is_a(&self, node: TermId, class: &str) -> bool {
        match (self.rdf_type, self.graph.iri(class)) {
            (Some(rdf_type), Some(class)) => self.graph.contains(node, rdf_type, class),
            _ => false,
        }
    }

    fn text(&self, id: TermId) -> String {
        self.graph
            .decode(id)
            .map_or_else(|| format!("#{}", id.raw()), |term| term.to_string())
    }

    fn error(&mut self, shape: TermId, message: impl Into<String>) {
        self.errors.push(ShapeError {
            shape: self.text(shape),
            message: message.into(),
        });
    }

    /// The single value of `local`, if any; more than one is an error.
    fn one(&mut self, node: TermId, local: &str) -> Option<TermId> {
        let values = self.values(node, local);
        if values.len() > 1 {
            self.error(node, format!("more than one sh:{local}"));
        }
        values.first().copied()
    }

    fn literal(&self, id: TermId) -> Option<nrese_rdf::Literal> {
        match self.graph.decode(id)? {
            Term::Literal(literal) => Some(literal),
            _ => None,
        }
    }

    /// A non-negative integer parameter.
    fn count(&mut self, node: TermId, id: TermId, local: &str) -> Option<u64> {
        let count = self
            .literal(id)
            .and_then(|literal| u64::from_str(literal.value()).ok());
        if count.is_none() {
            self.error(node, format!("sh:{local} isn't a non-negative integer"));
        }
        count
    }

    /// Whether a boolean parameter is switched on. Only the literal `true` does it: the
    /// specification names that value, and `"1"^^xsd:boolean` is a different term.
    fn is_true(&self, id: TermId) -> bool {
        self.literal(id).is_some_and(|literal| {
            literal.value() == "true" && literal.datatype() == nrese_rdf::vocab::xsd::BOOLEAN
        })
    }

    /// The members of the RDF list at `head`.
    fn list(&self, head: TermId) -> Result<Vec<TermId>, String> {
        let (Some(first), Some(rest)) = (self.rdf_first, self.rdf_rest) else {
            return if Some(head) == self.rdf_nil {
                Ok(Vec::new())
            } else {
                Err("isn't a list".to_owned())
            };
        };
        let mut members = Vec::new();
        let mut seen = BTreeSet::new();
        let mut node = head;
        while Some(node) != self.rdf_nil {
            if !seen.insert(node) {
                return Err("is a cyclic list".to_owned());
            }
            let (firsts, rests) = (
                self.graph.objects(node, first),
                self.graph.objects(node, rest),
            );
            let (&[member], &[next]) = (firsts.as_slice(), rests.as_slice()) else {
                return Err("isn't a well-formed list".to_owned());
            };
            members.push(member);
            node = next;
        }
        Ok(members)
    }

    fn path(&self, node: TermId, depth: usize) -> Result<Path, String> {
        if depth > MAX_PATH_DEPTH {
            return Err("sh:path is nested too deeply or cyclic".to_owned());
        }
        match node.kind() {
            TermKind::Iri => return Ok(Path::Predicate(node)),
            TermKind::BlankNode => {}
            _ => return Err("sh:path isn't an IRI or a path".to_owned()),
        }
        let members = |head: TermId| -> Result<Vec<Path>, String> {
            let members = self
                .list(head)
                .map_err(|problem| format!("sh:path {problem}"))?;
            if members.len() < 2 {
                return Err("a sequence or alternative path needs two members".to_owned());
            }
            members
                .into_iter()
                .map(|member| self.path(member, depth + 1))
                .collect()
        };
        // A node that is a list is a sequence path, whatever else it also says (the W3C
        // suite's `path-strange` tests pin this reading).
        if self
            .rdf_first
            .is_some_and(|first| !self.graph.objects(node, first).is_empty())
        {
            return Ok(Path::Sequence(members(node)?));
        }
        let unary: [(&str, UnaryPath); 4] = [
            ("inversePath", Path::Inverse),
            ("zeroOrMorePath", Path::ZeroOrMore),
            ("oneOrMorePath", Path::OneOrMore),
            ("zeroOrOnePath", Path::ZeroOrOne),
        ];
        for (local, make) in unary {
            if let [inner] = self.values(node, local).as_slice() {
                return Ok(make(Box::new(self.path(*inner, depth + 1)?)));
            }
        }
        if let [alternatives] = self.values(node, "alternativePath").as_slice() {
            return Ok(Path::Alternative(members(*alternatives)?));
        }
        Err("sh:path isn't an IRI or a path".to_owned())
    }

    /// The shapes in the list that is the value of a logical parameter.
    fn shape_list(&mut self, node: TermId, head: TermId, local: &str) -> Vec<ShapeRef> {
        match self.list(head) {
            Ok(members) => members
                .into_iter()
                .map(|member| self.shape(member))
                .collect(),
            Err(problem) => {
                self.error(node, format!("sh:{local} {problem}"));
                Vec::new()
            }
        }
    }

    /// Compiles `node` as a shape (once), and the shapes it refers to.
    fn shape(&mut self, node: TermId) -> ShapeRef {
        if let Some(&shape) = self.index.get(&node) {
            return shape;
        }
        let shape = self.shapes.len();
        self.index.insert(node, shape);
        // The slot exists before the shape's references are followed: shapes may refer
        // to each other, and to themselves.
        self.shapes.push(Shape {
            node,
            path: None,
            targets: Vec::new(),
            constraints: Vec::new(),
            severity: Severity::Violation,
            messages: Vec::new(),
            deactivated: false,
        });

        let path = self.one(node, "path").and_then(|path| {
            self.path(path, 0)
                .map_err(|problem| self.error(node, problem))
                .ok()
        });
        let targets = self.targets(node);
        let severity = match self.one(node, "severity") {
            None => Severity::Violation,
            Some(severity) if Some(severity) == self.sh("Violation") => Severity::Violation,
            Some(severity) if Some(severity) == self.sh("Warning") => Severity::Warning,
            Some(severity) if Some(severity) == self.sh("Info") => Severity::Info,
            Some(severity) => Severity::Other(severity),
        };
        let deactivated = self
            .one(node, "deactivated")
            .is_some_and(|value| self.is_true(value));
        let messages = self.values(node, "message");
        let constraints = self.constraints(node);
        self.shapes[shape] = Shape {
            node,
            path,
            targets,
            constraints,
            severity,
            messages,
            deactivated,
        };
        shape
    }

    fn targets(&mut self, node: TermId) -> Vec<Target> {
        let mut targets = Vec::new();
        targets.extend(
            self.values(node, "targetNode")
                .into_iter()
                .map(Target::Node),
        );
        for class in self.values(node, "targetClass") {
            if class.kind() == TermKind::Iri {
                targets.push(Target::Class(class));
            } else {
                self.error(node, "sh:targetClass isn't an IRI");
            }
        }
        targets.extend(
            self.values(node, "targetSubjectsOf")
                .into_iter()
                .map(Target::SubjectsOf),
        );
        targets.extend(
            self.values(node, "targetObjectsOf")
                .into_iter()
                .map(Target::ObjectsOf),
        );
        // Implicit class target: a shape that is also a class targets its instances.
        let is_shape = self.is_a(node, &format!("{SH}NodeShape"))
            || self.is_a(node, &format!("{SH}PropertyShape"));
        if is_shape && (self.is_a(node, RDFS_CLASS) || self.is_a(node, OWL_CLASS)) {
            targets.push(Target::Class(node));
        }
        targets
    }

    fn constraints(&mut self, node: TermId) -> Vec<Constraint> {
        let mut out = Vec::new();

        // Value type.
        for class in self.values(node, "class") {
            if class.kind() == TermKind::Iri {
                out.push(Constraint::Class(class));
            } else {
                self.error(node, "sh:class isn't an IRI");
            }
        }
        for datatype in self.values(node, "datatype") {
            match self.graph.decode(datatype) {
                Some(Term::NamedNode(datatype)) => {
                    out.push(Constraint::Datatype(datatype.into_string()));
                }
                _ => self.error(node, "sh:datatype isn't an IRI"),
            }
        }
        for kind in self.values(node, "nodeKind") {
            let kinds = [
                ("IRI", NodeKind::Iri),
                ("BlankNode", NodeKind::BlankNode),
                ("Literal", NodeKind::Literal),
                ("BlankNodeOrIRI", NodeKind::BlankNodeOrIri),
                ("BlankNodeOrLiteral", NodeKind::BlankNodeOrLiteral),
                ("IRIOrLiteral", NodeKind::IriOrLiteral),
            ];
            match kinds.iter().find(|(local, _)| self.sh(local) == Some(kind)) {
                Some(&(_, kind)) => out.push(Constraint::NodeKind(kind)),
                None => self.error(node, "sh:nodeKind isn't one of the six node kinds"),
            }
        }

        // Cardinality.
        for value in self.values(node, "minCount") {
            out.extend(
                self.count(node, value, "minCount")
                    .map(Constraint::MinCount),
            );
        }
        for value in self.values(node, "maxCount") {
            out.extend(
                self.count(node, value, "maxCount")
                    .map(Constraint::MaxCount),
            );
        }

        // Value range.
        for (local, bound) in [
            ("minExclusive", Bound::MinExclusive),
            ("minInclusive", Bound::MinInclusive),
            ("maxExclusive", Bound::MaxExclusive),
            ("maxInclusive", Bound::MaxInclusive),
        ] {
            for value in self.values(node, local) {
                match self.graph.decode(value) {
                    Some(term @ Term::Literal(_)) => {
                        out.push(Constraint::Range(bound, Value::of(&term)));
                    }
                    _ => self.error(node, format!("sh:{local} isn't a literal")),
                }
            }
        }

        // Strings.
        for value in self.values(node, "minLength") {
            out.extend(
                self.count(node, value, "minLength")
                    .map(Constraint::MinLength),
            );
        }
        for value in self.values(node, "maxLength") {
            out.extend(
                self.count(node, value, "maxLength")
                    .map(Constraint::MaxLength),
            );
        }
        let flags = self
            .one(node, "flags")
            .and_then(|flags| self.literal(flags))
            .map(|flags| flags.value().to_owned())
            .unwrap_or_default();
        for pattern in self.values(node, "pattern") {
            let regex = self
                .literal(pattern)
                .and_then(|pattern| compile_regex(pattern.value(), &flags));
            match regex {
                Some(regex) => out.push(Constraint::Pattern(regex)),
                None => self.error(node, "sh:pattern isn't a valid regular expression"),
            }
        }
        for languages in self.values(node, "languageIn") {
            let ranges: Option<Vec<String>> = self.list(languages).ok().and_then(|members| {
                members
                    .into_iter()
                    .map(|member| self.literal(member).map(|l| l.value().to_owned()))
                    .collect()
            });
            match ranges {
                Some(ranges) => out.push(Constraint::LanguageIn(ranges)),
                None => self.error(node, "sh:languageIn isn't a list of strings"),
            }
        }
        if self
            .one(node, "uniqueLang")
            .is_some_and(|value| self.is_true(value))
        {
            out.push(Constraint::UniqueLang);
        }

        // Property pairs.
        let pairs: [(&str, PropertyPair); 4] = [
            ("equals", Constraint::Equals),
            ("disjoint", Constraint::Disjoint),
            ("lessThan", Constraint::LessThan),
            ("lessThanOrEquals", Constraint::LessThanOrEquals),
        ];
        for (local, make) in pairs {
            for property in self.values(node, local) {
                if property.kind() == TermKind::Iri {
                    out.push(make(property));
                } else {
                    self.error(node, format!("sh:{local} isn't an IRI"));
                }
            }
        }

        // Logic.
        for shape in self.values(node, "not") {
            out.push(Constraint::Not(self.shape(shape)));
        }
        for (local, logical) in [
            ("and", Logical::And),
            ("or", Logical::Or),
            ("xone", Logical::Xone),
        ] {
            for head in self.values(node, local) {
                let shapes = self.shape_list(node, head, local);
                out.push(Constraint::Logical(logical, shapes));
            }
        }

        // Shapes.
        for shape in self.values(node, "node") {
            out.push(Constraint::Node(self.shape(shape)));
        }
        for shape in self.values(node, "property") {
            out.push(Constraint::Property(self.shape(shape)));
        }
        self.qualified(node, &mut out);

        // Others.
        if self
            .one(node, "closed")
            .is_some_and(|value| self.is_true(value))
        {
            out.push(Constraint::Closed(self.allowed_properties(node)));
        }
        out.extend(
            self.values(node, "hasValue")
                .into_iter()
                .map(Constraint::HasValue),
        );
        for head in self.values(node, "in") {
            match self.list(head) {
                Ok(members) => out.push(Constraint::In(members.into_iter().collect())),
                Err(problem) => self.error(node, format!("sh:in {problem}")),
            }
        }
        out
    }

    fn qualified(&mut self, node: TermId, out: &mut Vec<Constraint>) {
        let qualified = self.values(node, "qualifiedValueShape");
        if qualified.is_empty() {
            return;
        }
        let min = self
            .one(node, "qualifiedMinCount")
            .and_then(|value| self.count(node, value, "qualifiedMinCount"));
        let max = self
            .one(node, "qualifiedMaxCount")
            .and_then(|value| self.count(node, value, "qualifiedMaxCount"));
        let disjoint = self
            .one(node, "qualifiedValueShapesDisjoint")
            .is_some_and(|value| self.is_true(value));
        // Sibling shapes: the qualified value shapes of the other property shapes of this
        // shape's parents.
        let mut siblings: BTreeSet<TermId> = BTreeSet::new();
        if disjoint && let Some(property) = self.sh("property") {
            for parent in self.graph.subjects(property, node) {
                for child in self.graph.objects(parent, property) {
                    siblings.extend(self.values(child, "qualifiedValueShape"));
                }
            }
        }
        for shape in qualified {
            let siblings = siblings
                .iter()
                .filter(|&&sibling| sibling != shape)
                .map(|&sibling| self.shape(sibling))
                .collect();
            out.push(Constraint::Qualified {
                shape: self.shape(shape),
                min,
                max,
                siblings,
            });
        }
    }

    /// The predicates a closed shape allows: its property shapes' predicate paths and
    /// `sh:ignoredProperties`.
    fn allowed_properties(&mut self, node: TermId) -> BTreeSet<TermId> {
        let mut allowed = BTreeSet::new();
        for property in self.values(node, "property") {
            allowed.extend(
                self.values(property, "path")
                    .into_iter()
                    .filter(|path| path.kind() == TermKind::Iri),
            );
        }
        for ignored in self.values(node, "ignoredProperties") {
            match self.list(ignored) {
                Ok(members) => allowed.extend(members),
                Err(problem) => self.error(node, format!("sh:ignoredProperties {problem}")),
            }
        }
        allowed
    }
}
