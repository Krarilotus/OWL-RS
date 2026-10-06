//! A query printed as standard SPARQL 1.1 that answers on a store without reasoning as
//! NRESE answers it with reasoning (docs/design/ql-rewriting.md §8; benches/PROTOCOL.md,
//! "stores without reasoning, by rewriting").
//!
//! What the printed query does that the closure and the QL rewriting did:
//! - the tree witnesses of the QL rewriting, as NRESE runs them;
//! - every class atom expanded by the QL part of the schema (subclasses, equivalences,
//!   domains and ranges, existentials), and by RL rules whose left side is complex
//!   (intersections, qualified existentials, `hasValue`, enumerations, unions), unfolded
//!   into conjunctive branches;
//! - every property atom expanded into a path over its sub-properties and inverses, with
//!   `+` for transitive properties and `/` for property chains.
//!
//! Each expanded atom is a `SELECT DISTINCT` of its own, so the printed query counts rows
//! as a match over the closure does. What it can't write exactly it reports, never
//! approximates: equality (`owl:sameAs` in the data, functional and inverse functional
//! properties, keys, maximum cardinalities), `allValuesFrom`, `hasValue` and `hasSelf` on
//! the right of an axiom, recursion beyond transitivity, variable predicates and classes,
//! the schema vocabulary itself, and whatever the QL rewriting reports `sound-only` for.

use std::collections::{HashMap, HashSet, VecDeque};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_owl::ql::{Basic, Closure, Limits, Role, Tbox};
use nrese_owl::{Axiom, Characteristic, ClassExpr, DataRange, ExprId, ObjProp, Ontology};
use nrese_rdf::{NamedNode, Term, Variable};
use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::{Expression, GraphPattern, PropertyPathExpression as PathExpr};
use nrese_sparql_syntax::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

use super::{OWL, RDF, RDFS, SnapshotTerms, iri, schema_statements};
use crate::results::QueryEvaluationError;
use crate::view::ReadView;

/// How a printed query writes what reasoning adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintForm {
    /// Property paths: `rdf:type/(rdfs:subClassOf|owl:equivalentClass|^owl:equivalentClass)*`
    /// over the schema's own statements (the target store holds them), enumerated classes
    /// the path doesn't reach, alternatives of sub-properties and inverses, `+` for
    /// transitive properties, `/` for chains. The headline form.
    Paths,
    /// Enumerated `VALUES` of the classes and properties of NRESE's computed hierarchy;
    /// paths only where a closure needs them (transitivity, chains).
    Values,
}

impl PrintForm {
    pub fn name(self) -> &'static str {
        match self {
            Self::Paths => "paths",
            Self::Values => "values",
        }
    }

    /// The form named `name` (`paths`, `values`).
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Paths, Self::Values]
            .into_iter()
            .find(|f| f.name() == name)
    }
}

/// A printed query, or why it can't be printed exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Printed {
    /// The query, answering exactly as NRESE does, with NRESE's completeness for it: where
    /// NRESE's answers are `sound-only` (an axiom beyond QL meets anonymous individuals),
    /// the printed query's are the same answers, and so `sound-only` too.
    Query {
        text: String,
        completeness: crate::Completeness,
    },
    NotExpressible(Vec<String>),
}

/// `query` printed in `form` for a store without reasoning that holds what `view` holds
/// (its schema statements included), answering as NRESE does under `owl2-rl` with the QL
/// rewriting on.
pub fn print<V: ReadView>(
    view: &V,
    query: &Query,
    form: PrintForm,
) -> Result<Printed, QueryEvaluationError> {
    let snapshot = view.evaluation_snapshot();
    let snapshot: &Snapshot = &snapshot;
    let statements = schema_statements(snapshot);
    let ontology = nrese_owl::read(&statements, &SnapshotTerms(snapshot));
    let thing = iri(snapshot, OWL, "Thing").map(TermId::raw);
    let tbox = Tbox::compile(&ontology, Closure { lists: true }, thing);
    let name = |t: u64| match snapshot.decode(TermId::from_raw(t)) {
        Some(term) => term.to_string(),
        None => format!("term {t}"),
    };
    let rules = Rules::of(&ontology, &name);
    let mut reasons: Vec<String> = Vec::new();
    if let Some(same_as) = iri(snapshot, OWL, "sameAs") {
        let pattern = QuadPattern {
            subject: None,
            predicate: Some(same_as),
            object: None,
            graph: GraphSelector::Any,
        };
        if snapshot.exists_in(ReadModel::Asserted, &pattern) {
            reasons.push("owl:sameAs between individuals: equality isn't expressible".to_owned());
        }
    }
    reasons.extend(rules.global.iter().cloned());

    let (pattern, needed, set) = match query {
        Query::Select { pattern, .. } => (pattern, None, false),
        Query::Ask { pattern, .. } => (pattern, Some(Vec::new()), true),
        Query::Construct {
            pattern, template, ..
        } => {
            let mut vars = Vec::new();
            GraphPattern::Bgp {
                patterns: template.clone(),
            }
            .on_in_scope_variable(|v| vars.push(v.clone()));
            (pattern, Some(vars), true)
        }
        Query::Describe { pattern, .. } => (pattern, None, true),
    };
    let (rewritten, report) =
        crate::native::ql::rewrite_query(pattern, &tbox, snapshot, &Limits::default(), needed, set);
    // NRESE's own status goes with the printed query: the same answers, so the same
    // completeness. What only the printer can't write is below.

    let mut printer = Printer {
        tbox: &tbox,
        ontology: &ontology,
        rules: &rules,
        snapshot,
        form,
        paths: SubclassPaths::of(&statements, snapshot),
        fresh: 0,
        reasons: Vec::new(),
    };
    let printed = printer.pattern(&rewritten);
    reasons.extend(printer.reasons);
    let mut seen = HashSet::new();
    reasons.retain(|r| seen.insert(r.clone()));
    if !reasons.is_empty() {
        return Ok(Printed::NotExpressible(reasons));
    }
    let query = match query.clone() {
        Query::Select {
            dataset, base_iri, ..
        } => Query::Select {
            dataset,
            pattern: printed,
            base_iri,
        },
        Query::Ask {
            dataset, base_iri, ..
        } => Query::Ask {
            dataset,
            pattern: printed,
            base_iri,
        },
        Query::Construct {
            template,
            dataset,
            base_iri,
            ..
        } => Query::Construct {
            template,
            dataset,
            pattern: printed,
            base_iri,
        },
        Query::Describe {
            dataset, base_iri, ..
        } => Query::Describe {
            dataset,
            pattern: printed,
            base_iri,
        },
    };
    Ok(Printed::Query {
        text: query.to_string(),
        completeness: report.completeness,
    })
}

/// What the RL closure derives beyond the QL part, as the printer reads it.
#[derive(Default)]
struct Rules {
    /// Per class, left sides of axioms beyond QL whose instances are its instances.
    bodies: HashMap<u64, Vec<ExprId>>,
    /// Classes and properties an axiom the printer can't write concludes, and why.
    blocked_classes: HashMap<u64, String>,
    blocked_roles: HashMap<u64, String>,
    /// What makes every query inexpressible (equality).
    global: Option<String>,
    transitive: HashSet<u64>,
    /// `links ⊑ sup`, two or more links.
    chains: Vec<(Vec<ObjProp>, ObjProp)>,
}

impl Rules {
    fn of(o: &Ontology, name: &dyn Fn(u64) -> String) -> Self {
        let mut r = Self::default();
        let equality =
            |what: String| format!("{what}: equality between individuals isn't expressible");
        for axiom in &o.axioms {
            match axiom {
                Axiom::SubClassOf(l, rt) => r.conclude(o, Some(*l), *rt, name),
                Axiom::EquivalentClasses(classes) => {
                    for &a in classes {
                        for &b in classes {
                            if a != b {
                                r.conclude(o, Some(a), b, name);
                            }
                        }
                    }
                }
                Axiom::DisjointUnion(class, parts) => {
                    for &part in parts {
                        if !ql_left(o, part) {
                            r.bodies.entry(*class).or_default().push(part);
                        }
                    }
                }
                Axiom::ObjectPropertyDomain(_, c)
                | Axiom::ObjectPropertyRange(_, c)
                | Axiom::DataPropertyDomain(_, c) => r.conclude(o, None, *c, name),
                Axiom::ClassAssertion(c, _) if !matches!(o.class(*c), ClassExpr::Class(_)) => {
                    let mut classes = Vec::new();
                    named_classes(o, *c, &mut classes);
                    for class in classes {
                        r.blocked_classes.entry(class).or_insert_with(|| {
                            format!(
                                "an individual is asserted to be in a class expression over {}",
                                name(class)
                            )
                        });
                    }
                    r.conclude(o, None, *c, name);
                }
                Axiom::SubObjectPropertyOf(chain, sup) if chain.len() > 1 => {
                    r.chains.push((chain.clone(), *sup));
                }
                Axiom::ObjectCharacteristic(Characteristic::Transitive, p) => {
                    r.transitive.insert(p.named());
                }
                Axiom::ObjectCharacteristic(
                    Characteristic::Functional | Characteristic::InverseFunctional,
                    p,
                ) => {
                    r.global.get_or_insert_with(|| {
                        equality(format!("{} is functional", name(p.named())))
                    });
                }
                Axiom::FunctionalDataProperty(p) => {
                    r.global
                        .get_or_insert_with(|| equality(format!("{} is functional", name(*p))));
                }
                Axiom::HasKey(..) => {
                    r.global.get_or_insert_with(|| equality("a key".to_owned()));
                }
                Axiom::SameIndividual(..) => {
                    r.global
                        .get_or_insert_with(|| equality("owl:sameAs".to_owned()));
                }
                Axiom::ObjectCharacteristic(Characteristic::Reflexive, p) => {
                    r.blocked_roles
                        .entry(p.named())
                        .or_insert_with(|| format!("{} is reflexive", name(p.named())));
                }
                _ => {}
            }
        }
        r
    }

    /// `left ⊑ right` (`left` `None`: a left side the QL part reads).
    fn conclude(
        &mut self,
        o: &Ontology,
        left: Option<ExprId>,
        right: ExprId,
        name: &dyn Fn(u64) -> String,
    ) {
        let body = left.filter(|&l| !ql_left(o, l));
        self.right(o, body, right, name);
    }

    fn right(
        &mut self,
        o: &Ontology,
        body: Option<ExprId>,
        e: ExprId,
        name: &dyn Fn(u64) -> String,
    ) {
        match o.class(e) {
            ClassExpr::Class(d) => {
                if let Some(b) = body {
                    self.bodies.entry(*d).or_default().push(b);
                }
            }
            ClassExpr::And(parts) => {
                for &p in parts {
                    self.right(o, body, p, name);
                }
            }
            ClassExpr::Exact(_, p, _) | ClassExpr::Max(_, p, _) => {
                let what = format!("a maximum cardinality on {}", name(p.named()));
                self.global.get_or_insert_with(|| {
                    format!("{what}: equality between individuals isn't expressible")
                });
            }
            ClassExpr::DataExact(_, p, _) | ClassExpr::DataMax(_, p, _) => {
                let what = format!("a maximum cardinality on {}", name(*p));
                self.global.get_or_insert_with(|| {
                    format!("{what}: equality between individuals isn't expressible")
                });
            }
            ClassExpr::All(p, f) => {
                let mut classes = Vec::new();
                named_classes(o, *f, &mut classes);
                for d in classes {
                    self.blocked_classes.entry(d).or_insert_with(|| {
                        format!(
                            "an allValuesFrom on {} concludes {}",
                            name(p.named()),
                            name(d)
                        )
                    });
                }
            }
            ClassExpr::DataAll(..) => {}
            ClassExpr::HasValue(p, _) | ClassExpr::HasSelf(p) => {
                self.blocked_roles.entry(p.named()).or_insert_with(|| {
                    format!(
                        "a hasValue or hasSelf on {} on the right of an axiom",
                        name(p.named())
                    )
                });
            }
            ClassExpr::DataHasValue(p, _) => {
                self.blocked_roles.entry(*p).or_insert_with(|| {
                    format!("a hasValue on {} on the right of an axiom", name(*p))
                });
            }
            // Existentials make anonymous individuals (the QL rewriting's); unions,
            // complements and enumerations on the right have no RL rule, so NRESE derives
            // nothing from them either.
            _ => {}
        }
    }
}

/// Whether the QL part reads a left side as it is (a class, `∃P`, `∃P.⊤`, a union of them).
fn ql_left(o: &Ontology, e: ExprId) -> bool {
    match o.class(e) {
        ClassExpr::Class(_) | ClassExpr::Thing => true,
        ClassExpr::Some(_, f) | ClassExpr::Min(1, _, f) => matches!(o.class(*f), ClassExpr::Thing),
        ClassExpr::DataSome(_, range) => matches!(o.range(*range), DataRange::Literal),
        ClassExpr::Or(parts) => parts.iter().all(|&p| ql_left(o, p)),
        _ => false,
    }
}

/// The named classes of an expression.
fn named_classes(o: &Ontology, e: ExprId, out: &mut Vec<u64>) {
    match o.class(e) {
        ClassExpr::Class(c) => out.push(*c),
        ClassExpr::And(parts) | ClassExpr::Or(parts) => {
            for &p in parts {
                named_classes(o, p, out);
            }
        }
        ClassExpr::Not(inner) => named_classes(o, *inner, out),
        ClassExpr::Some(_, f)
        | ClassExpr::All(_, f)
        | ClassExpr::Min(_, _, f)
        | ClassExpr::Max(_, _, f)
        | ClassExpr::Exact(_, _, f) => named_classes(o, *f, out),
        _ => {}
    }
}

/// Which terms reach a class along `rdfs:subClassOf`, `owl:equivalentClass` (either way)
/// and from an intersection to its members, in the schema's own statements: what
/// `rdf:type/(…)*` finds on a store that holds them (each step a subsumption the closure
/// draws too).
struct SubclassPaths {
    /// Per term, the terms one step below it.
    below: HashMap<u64, Vec<u64>>,
}

impl SubclassPaths {
    fn of(statements: &[nrese_owl::Statement], snapshot: &Snapshot) -> Self {
        let id = |namespace: &str, local: &str| iri(snapshot, namespace, local).map(TermId::raw);
        let (sub, equivalent, intersection) = (
            id(RDFS, "subClassOf"),
            id(OWL, "equivalentClass"),
            id(OWL, "intersectionOf"),
        );
        let (first, rest) = (id(RDF, "first"), id(RDF, "rest"));
        let mut cells: HashMap<u64, (Option<u64>, Option<u64>)> = HashMap::new();
        for s in statements {
            let [a, p, b] = s.triple;
            if Some(p) == first {
                cells.entry(a).or_default().0 = Some(b);
            } else if Some(p) == rest {
                cells.entry(a).or_default().1 = Some(b);
            }
        }
        let mut below: HashMap<u64, Vec<u64>> = HashMap::new();
        for s in statements {
            let [a, p, b] = s.triple;
            if Some(p) == sub {
                below.entry(b).or_default().push(a);
            } else if Some(p) == equivalent {
                below.entry(b).or_default().push(a);
                below.entry(a).or_default().push(b);
            } else if Some(p) == intersection {
                let mut seen = HashSet::new();
                let mut cell = Some(b);
                while let Some(c) = cell.filter(|&c| seen.insert(c)) {
                    let Some(&(member, next)) = cells.get(&c) else {
                        break;
                    };
                    if let Some(m) = member {
                        below.entry(m).or_default().push(a);
                    }
                    cell = next;
                }
            }
        }
        Self { below }
    }

    /// The terms with a path to `class`, `class` included.
    fn reaching(&self, class: u64) -> HashSet<u64> {
        let mut seen = HashSet::from([class]);
        let mut queue = VecDeque::from([class]);
        while let Some(t) = queue.pop_front() {
            for &u in self.below.get(&t).into_iter().flatten() {
                if seen.insert(u) {
                    queue.push_back(u);
                }
            }
        }
        seen
    }
}

struct Printer<'a> {
    tbox: &'a Tbox,
    ontology: &'a Ontology,
    rules: &'a Rules,
    snapshot: &'a Snapshot,
    form: PrintForm,
    paths: SubclassPaths,
    fresh: usize,
    reasons: Vec<String>,
}

/// A class's membership at a term, being expanded (recursion through them).
type Active = Vec<(u64, TermPattern)>;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

impl Printer<'_> {
    fn fresh(&mut self) -> Variable {
        self.fresh += 1;
        Variable::new_unchecked(format!("_p{}", self.fresh))
    }

    fn name(&self, t: u64) -> String {
        match self.snapshot.decode(TermId::from_raw(t)) {
            Some(term) => term.to_string(),
            None => format!("term {t}"),
        }
    }

    fn node(&self, t: u64) -> Option<NamedNode> {
        match self.snapshot.decode(TermId::from_raw(t))? {
            Term::NamedNode(n) => Some(n),
            _ => None,
        }
    }

    fn term(&self, t: u64) -> Option<TermPattern> {
        match self.snapshot.decode(TermId::from_raw(t))? {
            Term::NamedNode(n) => Some(TermPattern::NamedNode(n)),
            Term::Literal(l) => Some(TermPattern::Literal(l)),
            _ => None,
        }
    }

    fn id(&self, n: &NamedNode) -> Option<u64> {
        self.snapshot.lookup(n.as_ref().into()).map(TermId::raw)
    }

    // The walk ------------------------------------------------------------------------

    fn pattern(&mut self, pattern: &GraphPattern) -> GraphPattern {
        let b = |p: &mut Self, x: &GraphPattern| Box::new(p.pattern(x));
        match pattern {
            GraphPattern::Bgp { patterns } => self.bgp(patterns),
            GraphPattern::Path { path, .. } => {
                let mut names = Vec::new();
                path_names(path, &mut names);
                for n in names {
                    match n {
                        None => self.reasons.push(
                            "a negated property set in a path: it would read inferred properties"
                                .to_owned(),
                        ),
                        Some(n) => {
                            if let Some(id) = self.id(&n)
                                && self.role_alternatives(ObjProp::Named(id)).map(|a| a.len())
                                    != Ok(1)
                            {
                                self.reasons.push(format!(
                                    "a property path over {n}, which reasoning extends"
                                ));
                            }
                        }
                    }
                }
                pattern.clone()
            }
            GraphPattern::Values { .. } => pattern.clone(),
            // Named graphs and remote endpoints have no inferences.
            GraphPattern::Graph { .. } | GraphPattern::Service { .. } => pattern.clone(),
            GraphPattern::Join { left, right } => GraphPattern::Join {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::Lateral { left, right } => GraphPattern::Lateral {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::Union { left, right } => GraphPattern::Union {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::Minus { left, right } => GraphPattern::Minus {
                left: b(self, left),
                right: b(self, right),
            },
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => GraphPattern::LeftJoin {
                left: b(self, left),
                right: b(self, right),
                expression: expression.as_ref().map(|e| self.expression(e)),
            },
            GraphPattern::Filter { expr, inner } => GraphPattern::Filter {
                expr: self.expression(expr),
                inner: b(self, inner),
            },
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => GraphPattern::Extend {
                inner: b(self, inner),
                variable: variable.clone(),
                expression: self.expression(expression),
            },
            GraphPattern::OrderBy { inner, expression } => GraphPattern::OrderBy {
                inner: b(self, inner),
                expression: expression.clone(),
            },
            GraphPattern::Project { inner, variables } => GraphPattern::Project {
                inner: b(self, inner),
                variables: variables.clone(),
            },
            GraphPattern::Distinct { inner } => match &**inner {
                GraphPattern::Project { inner, variables } => {
                    distinct_projection(self.pattern(inner), variables.clone())
                }
                _ => GraphPattern::Distinct {
                    inner: b(self, inner),
                },
            },
            GraphPattern::Reduced { inner } => GraphPattern::Reduced {
                inner: b(self, inner),
            },
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => GraphPattern::Slice {
                inner: b(self, inner),
                start: *start,
                length: *length,
            },
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => GraphPattern::Group {
                inner: b(self, inner),
                variables: variables.clone(),
                aggregates: aggregates.clone(),
            },
        }
    }

    /// `EXISTS` patterns inside an expression, expanded.
    fn expression(&mut self, expression: &Expression) -> Expression {
        let e = |p: &mut Self, x: &Expression| Box::new(p.expression(x));
        let list = |p: &mut Self, l: &[Expression]| l.iter().map(|x| p.expression(x)).collect();
        match expression {
            Expression::Exists(p) => Expression::Exists(Box::new(self.pattern(p))),
            Expression::Or(a, b) => Expression::Or(e(self, a), e(self, b)),
            Expression::And(a, b) => Expression::And(e(self, a), e(self, b)),
            Expression::Equal(a, b) => Expression::Equal(e(self, a), e(self, b)),
            Expression::SameTerm(a, b) => Expression::SameTerm(e(self, a), e(self, b)),
            Expression::Greater(a, b) => Expression::Greater(e(self, a), e(self, b)),
            Expression::GreaterOrEqual(a, b) => Expression::GreaterOrEqual(e(self, a), e(self, b)),
            Expression::Less(a, b) => Expression::Less(e(self, a), e(self, b)),
            Expression::LessOrEqual(a, b) => Expression::LessOrEqual(e(self, a), e(self, b)),
            Expression::Add(a, b) => Expression::Add(e(self, a), e(self, b)),
            Expression::Subtract(a, b) => Expression::Subtract(e(self, a), e(self, b)),
            Expression::Multiply(a, b) => Expression::Multiply(e(self, a), e(self, b)),
            Expression::Divide(a, b) => Expression::Divide(e(self, a), e(self, b)),
            Expression::UnaryPlus(a) => Expression::UnaryPlus(e(self, a)),
            Expression::UnaryMinus(a) => Expression::UnaryMinus(e(self, a)),
            Expression::Not(a) => Expression::Not(e(self, a)),
            Expression::In(a, l) => Expression::In(e(self, a), list(self, l)),
            Expression::If(a, b, c) => Expression::If(e(self, a), e(self, b), e(self, c)),
            Expression::Coalesce(l) => Expression::Coalesce(list(self, l)),
            Expression::FunctionCall(f, l) => Expression::FunctionCall(f.clone(), list(self, l)),
            other => other.clone(),
        }
    }

    /// A basic graph pattern: each atom as what the closure holds of it.
    fn bgp(&mut self, patterns: &[TriplePattern]) -> GraphPattern {
        let mut plain: Vec<TriplePattern> = Vec::new();
        let mut expanded: Vec<GraphPattern> = Vec::new();
        for t in patterns {
            let t = TriplePattern {
                subject: variable_for_blank(&t.subject),
                predicate: t.predicate.clone(),
                object: variable_for_blank(&t.object),
            };
            match self.atom(&t) {
                Ok(Some(p)) => expanded.push(p),
                Ok(None) => plain.push(t),
                Err(reason) => {
                    self.reasons.push(reason);
                    plain.push(t);
                }
            }
        }
        let mut out = GraphPattern::Bgp { patterns: plain };
        for p in expanded {
            out = GraphPattern::Join {
                left: Box::new(out),
                right: Box::new(p),
            };
        }
        out
    }

    /// One atom expanded (a `SELECT DISTINCT` of what the closure holds of it); `None` if
    /// the closure adds nothing to it.
    fn atom(&mut self, t: &TriplePattern) -> Result<Option<GraphPattern>, String> {
        let NamedNodePattern::NamedNode(p) = &t.predicate else {
            return Err("a variable predicate: it would read inferred properties".to_owned());
        };
        if p.as_str() == RDF_TYPE {
            let class = match &t.object {
                TermPattern::NamedNode(c) => c,
                TermPattern::Literal(_) => return Ok(None),
                _ => return Err("a variable class: it would read inferred classes".to_owned()),
            };
            if vocabulary(class.as_str()) {
                return Err(format!("a class atom on {class}, of the schema vocabulary"));
            }
            let Some(id) = self.id(class) else {
                return Ok(None);
            };
            let mut active = Vec::new();
            let alternatives = self.class(id, &t.subject, &mut active)?;
            let union = alternatives.unwrap_or_else(nothing);
            return Ok(Some(distinct(union, &[&t.subject])));
        }
        if vocabulary(p.as_str()) {
            return Err(format!("{p}, of the schema vocabulary"));
        }
        let Some(id) = self.id(p) else {
            return Ok(None);
        };
        let alternatives = self.role_alternatives(ObjProp::Named(id))?;
        if alternatives == [PathExpr::NamedNode(p.clone())] {
            return Ok(None);
        }
        let pattern = self.role_pattern(&t.subject, alternatives, &t.object);
        Ok(Some(distinct(pattern, &[&t.subject, &t.object])))
    }

    // Properties ------------------------------------------------------------------------

    /// The paths whose edges are `role`'s in the closure: its sub-roles (inverses
    /// reversed), `(…)+` of transitive ones, chains as sequences.
    fn role_alternatives(&self, role: Role) -> Result<Vec<PathExpr>, String> {
        self.role_paths(role, &HashSet::new(), &mut Vec::new())
    }

    fn role_paths(
        &self,
        role: Role,
        plain: &HashSet<u64>,
        chains: &mut Vec<u64>,
    ) -> Result<Vec<PathExpr>, String> {
        let mut out: Vec<PathExpr> = Vec::new();
        let push = |p: PathExpr, out: &mut Vec<PathExpr>| {
            if !out.contains(&p) {
                out.push(p);
            }
        };
        let subs = self.tbox.sub_roles(role);
        for &sigma in &subs {
            if let Some(reason) = self.rules.blocked_roles.get(&sigma.named()) {
                return Err(reason.clone());
            }
            let Some(node) = self.node(sigma.named()) else {
                continue;
            };
            push(atomic(sigma, node), &mut out);
        }
        for &sigma in &subs {
            let t = sigma.named();
            if self.rules.transitive.contains(&t) && !plain.contains(&t) {
                let mut inner_plain = plain.clone();
                inner_plain.insert(t);
                let inner = self.role_paths(sigma, &inner_plain, chains)?;
                push(PathExpr::OneOrMore(Box::new(alternation(inner))), &mut out);
            }
            for (links, sup) in &self.rules.chains {
                let reversed = if *sup == sigma {
                    false
                } else if sup.inverse() == sigma {
                    true
                } else {
                    continue;
                };
                if chains.contains(&sup.named()) {
                    return Err(format!(
                        "{} is defined through itself (a recursive property chain)",
                        self.name(sup.named())
                    ));
                }
                chains.push(sup.named());
                let mut steps = Vec::new();
                for &link in links {
                    let link = if reversed { link.inverse() } else { link };
                    steps.push(alternation(self.role_paths(link, plain, chains)?));
                }
                chains.pop();
                if reversed {
                    steps.reverse();
                }
                let sequence = steps
                    .into_iter()
                    .reduce(|a, b| PathExpr::Sequence(Box::new(a), Box::new(b)))
                    .expect("a chain has links");
                push(sequence, &mut out);
            }
        }
        Ok(out)
    }

    /// `subject` related to `object` by one of the paths.
    fn role_pattern(
        &mut self,
        subject: &TermPattern,
        alternatives: Vec<PathExpr>,
        object: &TermPattern,
    ) -> GraphPattern {
        let path = |p: PathExpr| GraphPattern::Path {
            subject: subject.clone(),
            path: p,
            object: object.clone(),
        };
        match self.form {
            PrintForm::Paths => path(alternation(alternatives)),
            PrintForm::Values => {
                let (mut forward, mut backward, mut rest) = (Vec::new(), Vec::new(), Vec::new());
                for a in alternatives {
                    match a {
                        PathExpr::NamedNode(n) => forward.push(n),
                        PathExpr::Reverse(inner) => match *inner {
                            PathExpr::NamedNode(n) => backward.push(n),
                            other => rest.push(PathExpr::Reverse(Box::new(other))),
                        },
                        other => rest.push(other),
                    }
                }
                let mut branches = Vec::new();
                for (names, reversed) in [(forward, false), (backward, true)] {
                    if names.is_empty() {
                        continue;
                    }
                    let p = self.fresh();
                    let (s, o) = if reversed {
                        (object.clone(), subject.clone())
                    } else {
                        (subject.clone(), object.clone())
                    };
                    branches.push(values_join(
                        &p,
                        names,
                        GraphPattern::Bgp {
                            patterns: vec![TriplePattern {
                                subject: s,
                                predicate: NamedNodePattern::Variable(p.clone()),
                                object: o,
                            }],
                        },
                    ));
                }
                branches.extend(rest.into_iter().map(path));
                union(branches)
            }
        }
    }

    // Classes ---------------------------------------------------------------------------

    /// What makes `subject` an instance of `class` in the closure; `None` where only
    /// being one already could (a branch through itself at the same term).
    fn class(
        &mut self,
        class: u64,
        subject: &TermPattern,
        active: &mut Active,
    ) -> Result<Option<GraphPattern>, String> {
        if active.iter().any(|(c, s)| *c == class && s == subject) {
            return Ok(None);
        }
        if active.iter().any(|(c, _)| *c == class) {
            return Err(format!(
                "{} is defined through itself (recursive class definitions)",
                self.name(class)
            ));
        }
        active.push((class, subject.clone()));
        let result = self.class_branches(class, subject, active);
        active.pop();
        result
    }

    fn class_branches(
        &mut self,
        class: u64,
        subject: &TermPattern,
        active: &mut Active,
    ) -> Result<Option<GraphPattern>, String> {
        let reaching = self.paths.reaching(class);
        let mut named: Vec<u64> = Vec::new();
        let mut roles: Vec<Role> = Vec::new();
        for b in self.tbox.entailing(class) {
            match b {
                Basic::Class(c) => named.push(c),
                Basic::Exists(r) => roles.push(r),
                _ => {}
            }
        }
        for &t in &reaching {
            if self.node(t).is_some() && !named.contains(&t) {
                named.push(t);
            }
        }
        named.sort_unstable();
        for &c in &named {
            if let Some(reason) = self.rules.blocked_classes.get(&c) {
                return Err(reason.clone());
            }
            if let Some(n) = self.node(c)
                && vocabulary(n.as_str())
                && c != class
            {
                return Err(format!(
                    "{n}, of the schema vocabulary, implies {}",
                    self.name(class)
                ));
            }
        }
        let rdf_type = NamedNode::new_unchecked(RDF_TYPE);
        let type_of = |c: NamedNode| GraphPattern::Bgp {
            patterns: vec![TriplePattern {
                subject: subject.clone(),
                predicate: NamedNodePattern::NamedNode(rdf_type.clone()),
                object: TermPattern::NamedNode(c),
            }],
        };
        let mut branches: Vec<GraphPattern> = Vec::new();
        match self.form {
            PrintForm::Paths => {
                let Some(node) = self.node(class) else {
                    return Ok(None);
                };
                branches.push(GraphPattern::Path {
                    subject: subject.clone(),
                    path: PathExpr::Sequence(
                        Box::new(PathExpr::NamedNode(rdf_type.clone())),
                        Box::new(PathExpr::ZeroOrMore(Box::new(subclass_step()))),
                    ),
                    object: TermPattern::NamedNode(node),
                });
                for &c in &named {
                    if !reaching.contains(&c)
                        && let Some(n) = self.node(c)
                    {
                        branches.push(type_of(n));
                    }
                }
            }
            PrintForm::Values => {
                let nodes: Vec<NamedNode> = named.iter().filter_map(|&c| self.node(c)).collect();
                let v = self.fresh();
                branches.push(values_join(
                    &v,
                    nodes,
                    GraphPattern::Bgp {
                        patterns: vec![TriplePattern {
                            subject: subject.clone(),
                            predicate: NamedNodePattern::NamedNode(rdf_type.clone()),
                            object: TermPattern::Variable(v.clone()),
                        }],
                    },
                ));
            }
        }
        // An existential on a sub-role of another one adds no answers (the expansion is a
        // set): only the greatest are written.
        let subs: Vec<Vec<Role>> = roles.iter().map(|&r| self.tbox.sub_roles(r)).collect();
        let covered = |i: usize| {
            (0..roles.len()).any(|j| {
                j != i && subs[j].contains(&roles[i]) && (!subs[i].contains(&roles[j]) || j < i)
            })
        };
        let roles: Vec<Role> = (0..roles.len())
            .filter(|&i| !covered(i))
            .map(|i| roles[i])
            .collect();
        for role in roles {
            let alternatives = self.role_alternatives(role)?;
            let o = TermPattern::Variable(self.fresh());
            branches.push(self.role_pattern(subject, alternatives, &o));
        }
        for &c in &named {
            for &body in self.rules.bodies.get(&c).into_iter().flatten() {
                if let Some(p) = self.unfold(body, subject, active)? {
                    branches.push(p);
                }
            }
        }
        Ok(Some(union(branches)))
    }

    /// A left side beyond QL as a pattern at `subject`; `None` if it can't hold except
    /// through a membership being expanded.
    fn unfold(
        &mut self,
        e: ExprId,
        subject: &TermPattern,
        active: &mut Active,
    ) -> Result<Option<GraphPattern>, String> {
        let o = self.ontology;
        match o.class(e).clone() {
            ClassExpr::Class(c) => self.class(c, subject, active),
            ClassExpr::Thing => Ok(Some(GraphPattern::default())),
            ClassExpr::Nothing => Ok(None),
            ClassExpr::And(parts) => {
                let mut out: Option<GraphPattern> = Some(GraphPattern::default());
                for p in parts {
                    match self.unfold(p, subject, active)? {
                        Some(x) => {
                            out = out.map(|left| GraphPattern::Join {
                                left: Box::new(left),
                                right: Box::new(x),
                            });
                        }
                        None => return Ok(None),
                    }
                }
                Ok(out)
            }
            ClassExpr::Or(parts) => {
                let mut branches = Vec::new();
                for p in parts {
                    if let Some(x) = self.unfold(p, subject, active)? {
                        branches.push(x);
                    }
                }
                Ok((!branches.is_empty()).then(|| union(branches)))
            }
            ClassExpr::Some(role, filler) | ClassExpr::Min(1, role, filler) => {
                let y = TermPattern::Variable(self.fresh());
                let alternatives = self.role_alternatives(role)?;
                let edge = self.role_pattern(subject, alternatives, &y);
                match self.unfold(filler, &y, active)? {
                    Some(f) => Ok(Some(GraphPattern::Join {
                        left: Box::new(edge),
                        right: Box::new(f),
                    })),
                    None => Ok(None),
                }
            }
            ClassExpr::HasValue(role, value) => {
                let Some(v) = self.term(value) else {
                    return Err("a hasValue on a blank node".to_owned());
                };
                let alternatives = self.role_alternatives(role)?;
                Ok(Some(self.role_pattern(subject, alternatives, &v)))
            }
            ClassExpr::DataSome(p, range) | ClassExpr::DataMin(1, p, range) => {
                if !matches!(o.range(range), DataRange::Literal) {
                    return Err(format!(
                        "a data existential on {} over a datatype",
                        self.name(p)
                    ));
                }
                let y = TermPattern::Variable(self.fresh());
                let alternatives = self.role_alternatives(ObjProp::Named(p))?;
                Ok(Some(self.role_pattern(subject, alternatives, &y)))
            }
            ClassExpr::DataHasValue(p, value) => {
                let Some(v) = self.term(value) else {
                    return Err("a hasValue on a blank node".to_owned());
                };
                let alternatives = self.role_alternatives(ObjProp::Named(p))?;
                Ok(Some(self.role_pattern(subject, alternatives, &v)))
            }
            ClassExpr::OneOf(individuals) => {
                let nodes: Vec<NamedNode> =
                    individuals.iter().filter_map(|&i| self.node(i)).collect();
                match subject {
                    TermPattern::Variable(v) => Ok(Some(GraphPattern::Values {
                        variables: vec![v.clone()],
                        bindings: nodes
                            .into_iter()
                            .map(|n| vec![Some(GroundTerm::NamedNode(n))])
                            .collect(),
                    })),
                    TermPattern::NamedNode(n) => Ok(nodes.contains(n).then(GraphPattern::default)),
                    _ => Ok(None),
                }
            }
            other => Err(format!(
                "a left side the printer doesn't write ({})",
                expression_kind(&other)
            )),
        }
    }
}

/// `rdfs:subClassOf|owl:equivalentClass|^owl:equivalentClass|owl:intersectionOf/rdf:rest*/rdf:first`.
fn subclass_step() -> PathExpr {
    let named = |namespace: &str, local: &str| {
        PathExpr::NamedNode(NamedNode::new_unchecked(format!("{namespace}{local}")))
    };
    let member = PathExpr::Sequence(
        Box::new(named(OWL, "intersectionOf")),
        Box::new(PathExpr::Sequence(
            Box::new(PathExpr::ZeroOrMore(Box::new(named(RDF, "rest")))),
            Box::new(named(RDF, "first")),
        )),
    );
    alternation(vec![
        named(RDFS, "subClassOf"),
        named(OWL, "equivalentClass"),
        PathExpr::Reverse(Box::new(named(OWL, "equivalentClass"))),
        member,
    ])
}

fn atomic(role: Role, node: NamedNode) -> PathExpr {
    match role {
        ObjProp::Named(_) => PathExpr::NamedNode(node),
        ObjProp::Inverse(_) => PathExpr::Reverse(Box::new(PathExpr::NamedNode(node))),
    }
}

fn alternation(paths: Vec<PathExpr>) -> PathExpr {
    paths
        .into_iter()
        .reduce(|a, b| PathExpr::Alternative(Box::new(a), Box::new(b)))
        .expect("a role has itself as an alternative")
}

fn union(branches: Vec<GraphPattern>) -> GraphPattern {
    branches
        .into_iter()
        .reduce(|a, b| GraphPattern::Union {
            left: Box::new(a),
            right: Box::new(b),
        })
        .unwrap_or_else(nothing)
}

/// A pattern without solutions.
fn nothing() -> GraphPattern {
    GraphPattern::Values {
        variables: Vec::new(),
        bindings: Vec::new(),
    }
}

/// `VALUES ?v { names }` joined with `pattern`.
fn values_join(v: &Variable, names: Vec<NamedNode>, pattern: GraphPattern) -> GraphPattern {
    GraphPattern::Join {
        left: Box::new(GraphPattern::Values {
            variables: vec![v.clone()],
            bindings: names
                .into_iter()
                .map(|n| vec![Some(GroundTerm::NamedNode(n))])
                .collect(),
        }),
        right: Box::new(pattern),
    }
}

/// `SELECT DISTINCT` the variables among `terms` of `pattern`: each solution once, as a
/// match of one atom over the closure has it.
fn distinct(pattern: GraphPattern, terms: &[&TermPattern]) -> GraphPattern {
    let mut variables: Vec<Variable> = Vec::new();
    for t in terms {
        if let TermPattern::Variable(v) = t
            && !variables.contains(v)
        {
            variables.push(v.clone());
        }
    }
    distinct_projection(pattern, variables)
}

/// `SELECT DISTINCT variables WHERE { pattern }`; with no variables, which SPARQL can't
/// write (its writer gives `SELECT DISTINCT *`, every variable), `{ FILTER EXISTS { … } }`:
/// one empty row or none either way.
fn distinct_projection(pattern: GraphPattern, variables: Vec<Variable>) -> GraphPattern {
    if variables.is_empty() {
        return GraphPattern::Filter {
            expr: Expression::Exists(Box::new(pattern)),
            inner: Box::new(GraphPattern::default()),
        };
    }
    GraphPattern::Distinct {
        inner: Box::new(GraphPattern::Project {
            inner: Box::new(pattern),
            variables,
        }),
    }
}

/// A blank node of a basic graph pattern as the variable it stands for (expanded atoms
/// are subqueries, where a blank node would be another one).
fn variable_for_blank(t: &TermPattern) -> TermPattern {
    match t {
        TermPattern::BlankNode(b) => {
            TermPattern::Variable(Variable::new_unchecked(format!("_b_{}", b.as_str())))
        }
        other => other.clone(),
    }
}

/// Whether an IRI is of the RDF, RDFS or OWL vocabulary (structure the closure reasons
/// about, not data), annotations aside.
fn vocabulary(iri: &str) -> bool {
    let annotation = ["label", "comment", "seeAlso", "isDefinedBy"]
        .iter()
        .any(|a| iri == format!("{RDFS}{a}"));
    !annotation && [RDF, RDFS, OWL].iter().any(|ns| iri.starts_with(ns))
}

/// The properties of a path; `None` for a negated property set.
fn path_names(path: &PathExpr, out: &mut Vec<Option<NamedNode>>) {
    match path {
        PathExpr::NamedNode(n) => out.push(Some(n.clone())),
        PathExpr::Reverse(p)
        | PathExpr::ZeroOrMore(p)
        | PathExpr::OneOrMore(p)
        | PathExpr::ZeroOrOne(p) => path_names(p, out),
        PathExpr::Sequence(a, b) | PathExpr::Alternative(a, b) => {
            path_names(a, out);
            path_names(b, out);
        }
        PathExpr::NegatedPropertySet(_) => out.push(None),
    }
}

fn expression_kind(e: &ClassExpr) -> &'static str {
    match e {
        ClassExpr::All(..) | ClassExpr::DataAll(..) => "allValuesFrom",
        ClassExpr::Not(..) => "a complement",
        ClassExpr::HasSelf(..) => "hasSelf",
        ClassExpr::Min(..) | ClassExpr::Max(..) | ClassExpr::Exact(..) => "a cardinality",
        ClassExpr::DataMin(..) | ClassExpr::DataMax(..) | ClassExpr::DataExact(..) => {
            "a cardinality"
        }
        ClassExpr::DataSome(..) => "a data existential over a datatype",
        _ => "an expression",
    }
}

#[cfg(test)]
mod tests;
