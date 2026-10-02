//! A SPARQL 1.1 evaluator written straight from the specification's algebra (§18), as the
//! reference of NRESE's differential tests (docs/plan/2026-10-01-oxigraph-migration.md).
//!
//! It evaluates the algebra the way §18.5 and §18.6 define it: solutions as maps from
//! variables to terms, multisets as lists, every operator on the whole of its inputs, no
//! plan, no index, no rewrite. What the native executor does differently to be fast
//! (join orders, index choices, filter pushdown, set evaluation, sideways information
//! passing, worst-case optimal joins) it doesn't do at all; where both agree, those
//! optimisations kept the results.
//!
//! **Shared with the executor.** The values of expressions and of `SUM`, `AVG` and
//! `GROUP_CONCAT` ([`nrese_sparql::expression::Evaluator`], [`nrese_sparql::value`]), and
//! the choices the specification leaves to an implementation: the order between values
//! `<` can't compare, what `REDUCED` removes (nothing), `DESCRIBE`'s description (the
//! triples a resource is the subject of, following blank nodes). Function semantics are
//! checked against the W3C suite and against Jena (benches/oracle), not here.
//!
//! **Known deviations of the executor** that the reference reproduces, so that the
//! differential tests check everything else, each named in [`Deviations`] and meant to be
//! turned off as the executor is fixed (completion plan 4.3, 4.6).
//!
//! Slow on purpose: for test datasets of a few hundred statements.

use std::collections::{HashMap, HashSet};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot};
use nrese_rdf::{
    BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple, Variable,
};
use nrese_sparql::expression::Evaluator;
use nrese_sparql::value;
use nrese_sparql::{
    QueryDatasetSpecification, QueryEvaluationError, QueryOptions, QueryResults, QuerySolutionIter,
    QueryTripleIter,
};
use nrese_sparql_syntax::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern, OrderExpression,
    PropertyPathExpression, QueryDataset,
};
use nrese_sparql_syntax::term::{
    GraphNamePattern, GroundQuadPattern, GroundTermPattern, NamedNodePattern,
    QuadPattern as PatternQuad, TermPattern, TriplePattern,
};
use nrese_sparql_syntax::{GraphUpdateOperation, Query, Update};

/// The executor's known deviations from the specification that the reference follows too.
#[derive(Debug, Clone, Copy)]
pub struct Deviations {
    /// A path alternative `(p|q)` gives each pair once (the specification: a UNION, with
    /// duplicates).
    pub alternative_paths_distinct: bool,
}

impl Default for Deviations {
    fn default() -> Self {
        Self {
            alternative_paths_distinct: true,
        }
    }
}

/// A dataset: quads, the default graph's with [`GraphName::DefaultGraph`].
#[derive(Debug, Clone, Default)]
pub struct Dataset {
    quads: HashSet<Quad>,
    /// Named graphs the store has beyond those of `quads`: a read model filters
    /// statements, not graphs, so a graph with no statement of the model still exists.
    graphs: HashSet<NamedOrBlankNode>,
    pub deviations: Deviations,
}

type Solution = HashMap<Variable, Term>;
type TripleOf = (Term, NamedNode, Term);

fn error(message: impl Into<String>) -> QueryEvaluationError {
    QueryEvaluationError::Unexpected(message.into())
}

impl Dataset {
    pub fn new(quads: impl IntoIterator<Item = Quad>) -> Self {
        Self {
            quads: quads.into_iter().collect(),
            graphs: HashSet::new(),
            deviations: Deviations::default(),
        }
    }

    /// What `snapshot` holds in `model`.
    pub fn from_snapshot(snapshot: &Snapshot, model: ReadModel) -> Self {
        let pattern = QuadPattern {
            subject: None,
            predicate: None,
            object: None,
            graph: GraphSelector::Any,
        };
        let mut dataset = Self::new(
            snapshot
                .quads_for_pattern_in(model, &pattern)
                .filter_map(|quad| snapshot.decode_quad(quad)),
        );
        dataset.graphs = snapshot
            .named_graphs()
            .filter_map(|id| NamedOrBlankNode::try_from(snapshot.decode(id)?).ok())
            .collect();
        dataset
    }

    pub fn quads(&self) -> impl Iterator<Item = &Quad> {
        self.quads.iter()
    }

    pub fn insert(&mut self, quad: Quad) {
        self.quads.insert(quad);
    }

    fn named_graphs(&self) -> Vec<NamedOrBlankNode> {
        let mut graphs: Vec<NamedOrBlankNode> = self
            .quads
            .iter()
            .filter_map(|q| match &q.graph_name {
                GraphName::NamedNode(n) => Some(n.clone().into()),
                GraphName::BlankNode(b) => Some(b.clone().into()),
                GraphName::DefaultGraph => None,
            })
            .chain(self.graphs.iter().cloned())
            .collect();
        graphs.sort_by_key(|a| a.to_string());
        graphs.dedup();
        graphs
    }

    fn graph_triples(&self, graph: &GraphName) -> Vec<TripleOf> {
        self.quads
            .iter()
            .filter(|q| &q.graph_name == graph)
            .map(|q| {
                (
                    q.subject.clone().into(),
                    q.predicate.clone(),
                    q.object.clone(),
                )
            })
            .collect()
    }

    /// Evaluates `query` with `options`' dataset (`dataset`, `union_default_graph`).
    pub fn query(
        &self,
        query: &Query,
        options: &QueryOptions,
    ) -> Result<QueryResults<'static>, QueryEvaluationError> {
        let (dataset, pattern, base) = match query {
            Query::Select {
                dataset,
                pattern,
                base_iri,
            }
            | Query::Ask {
                dataset,
                pattern,
                base_iri,
            }
            | Query::Construct {
                dataset,
                pattern,
                base_iri,
                ..
            }
            | Query::Describe {
                dataset,
                pattern,
                base_iri,
            } => (dataset.as_ref(), pattern, base_iri.clone()),
        };
        let ctx = Context::new(self, options, dataset, base);
        let solutions = ctx.eval(pattern, &ctx.default_graph, &Solution::new())?;
        match query {
            Query::Select { .. } => {
                let variables = output_variables(pattern);
                let rows: Vec<Vec<Option<Term>>> = solutions
                    .into_iter()
                    .map(|s| variables.iter().map(|v| s.get(v).cloned()).collect())
                    .collect();
                Ok(QueryResults::Solutions(QuerySolutionIter::new(
                    variables.into(),
                    rows.into_iter().map(Ok),
                )))
            }
            Query::Ask { .. } => Ok(QueryResults::Boolean(!solutions.is_empty())),
            Query::Construct { template, .. } => {
                let triples = construct(template, &solutions);
                Ok(QueryResults::Graph(QueryTripleIter::new(
                    triples.into_iter().map(Ok),
                )))
            }
            Query::Describe { .. } => {
                let triples = ctx.describe(&solutions);
                Ok(QueryResults::Graph(QueryTripleIter::new(
                    triples.into_iter().map(Ok),
                )))
            }
        }
    }

    /// Applies `update`, operation by operation (a failing request may leave part of it).
    pub fn update(
        &mut self,
        update: &Update,
        options: &QueryOptions,
    ) -> Result<(), QueryEvaluationError> {
        for operation in &update.operations {
            match operation {
                GraphUpdateOperation::InsertData { data } => {
                    let mut fresh = HashMap::new();
                    for quad in data {
                        let mut rename = |b: &BlankNode| -> BlankNode {
                            fresh
                                .entry(b.clone())
                                .or_insert_with(BlankNode::default)
                                .clone()
                        };
                        let subject: NamedOrBlankNode = match &quad.subject {
                            NamedOrBlankNode::BlankNode(b) => rename(b).into(),
                            other => other.clone(),
                        };
                        let object = match &quad.object {
                            Term::BlankNode(b) => rename(b).into(),
                            other => other.clone(),
                        };
                        self.quads.insert(Quad::new(
                            subject,
                            quad.predicate.clone(),
                            object,
                            graph_of(&quad.graph_name),
                        ));
                    }
                }
                GraphUpdateOperation::DeleteData { data } => {
                    for quad in data {
                        let object = Term::from(quad.object.clone());
                        self.quads.remove(&Quad::new(
                            quad.subject.clone(),
                            quad.predicate.clone(),
                            object,
                            graph_of(&quad.graph_name),
                        ));
                    }
                }
                GraphUpdateOperation::DeleteInsert {
                    delete,
                    insert,
                    using,
                    pattern,
                } => {
                    let ctx = Context::new(self, options, using.as_ref(), update.base_iri.clone());
                    let solutions = ctx.eval(pattern, &ctx.default_graph, &Solution::new())?;
                    let mut deletes = Vec::new();
                    let mut inserts = Vec::new();
                    for solution in &solutions {
                        for quad in delete {
                            if let Some(q) = ground_quad(quad, solution) {
                                deletes.push(q);
                            }
                        }
                        let mut fresh = HashMap::new();
                        for quad in insert {
                            if let Some(q) = template_quad(quad, solution, &mut fresh) {
                                inserts.push(q);
                            }
                        }
                    }
                    for quad in deletes {
                        self.quads.remove(&quad);
                    }
                    self.quads.extend(inserts);
                }
                GraphUpdateOperation::Load { silent, source, .. } => {
                    if !silent {
                        return Err(error(format!(
                            "LOAD <{}> is not supported",
                            source.as_str()
                        )));
                    }
                }
                GraphUpdateOperation::Create { .. } => {}
                GraphUpdateOperation::Clear { graph, .. }
                | GraphUpdateOperation::Drop { graph, .. } => {
                    use nrese_sparql_syntax::algebra::GraphTarget;
                    self.quads.retain(|q| match graph {
                        GraphTarget::NamedNode(n) => {
                            q.graph_name != GraphName::NamedNode(n.clone())
                        }
                        GraphTarget::DefaultGraph => q.graph_name != GraphName::DefaultGraph,
                        GraphTarget::NamedGraphs => q.graph_name == GraphName::DefaultGraph,
                        GraphTarget::AllGraphs => false,
                    });
                }
            }
        }
        Ok(())
    }
}

fn graph_of(graph: &nrese_sparql_syntax::term::GraphName) -> GraphName {
    match graph {
        nrese_sparql_syntax::term::GraphName::NamedNode(n) => n.clone().into(),
        nrese_sparql_syntax::term::GraphName::DefaultGraph => GraphName::DefaultGraph,
    }
}

/// The variables of a SELECT's results: its projection.
fn output_variables(pattern: &GraphPattern) -> Vec<Variable> {
    match pattern {
        GraphPattern::Project { variables, .. } => variables.clone(),
        GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::OrderBy { inner, .. } => output_variables(inner),
        other => {
            let mut out = Vec::new();
            in_scope(other, &mut out);
            out
        }
    }
}

/// The variables in scope of `pattern` (§18.2.1), in order of appearance.
fn in_scope(pattern: &GraphPattern, out: &mut Vec<Variable>) {
    let push = |v: &Variable, out: &mut Vec<Variable>| {
        if !out.contains(v) {
            out.push(v.clone());
        }
    };
    let term = |t: &TermPattern, out: &mut Vec<Variable>| {
        if let TermPattern::Variable(v) = t
            && !out.contains(v)
        {
            out.push(v.clone());
        }
    };
    match pattern {
        GraphPattern::Bgp { patterns } => {
            for t in patterns {
                term(&t.subject, out);
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    push(v, out);
                }
                term(&t.object, out);
            }
        }
        GraphPattern::Path {
            subject, object, ..
        } => {
            term(subject, out);
            term(object, out);
        }
        GraphPattern::Join { left, right }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Union { left, right } => {
            in_scope(left, out);
            in_scope(right, out);
        }
        GraphPattern::Minus { left, .. } => in_scope(left, out),
        GraphPattern::Graph { name, inner } => {
            if let NamedNodePattern::Variable(v) = name {
                push(v, out);
            }
            in_scope(inner, out);
        }
        GraphPattern::Extend {
            inner, variable, ..
        } => {
            in_scope(inner, out);
            push(variable, out);
        }
        GraphPattern::Values { variables, .. } | GraphPattern::Project { variables, .. } => {
            for v in variables {
                push(v, out);
            }
        }
        GraphPattern::Filter { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Service { inner, .. } => in_scope(inner, out),
        GraphPattern::Group {
            variables,
            aggregates,
            ..
        } => {
            for v in variables {
                push(v, out);
            }
            for (v, _) in aggregates {
                push(v, out);
            }
        }
        #[allow(unreachable_patterns)]
        _ => {}
    }
}

/// Where a pattern reads: the dataset's default graph, or a named graph. Its triples by
/// predicate and by subject: only which triples are looked at, not what matches.
#[derive(Clone)]
struct Graph {
    /// Unique per `Graph::new`; a clone has the same triples and keeps it.
    id: usize,
    triples: Vec<TripleOf>,
    all: Vec<usize>,
    by_subject: HashMap<Term, Vec<usize>>,
    by_predicate: HashMap<NamedNode, Vec<usize>>,
    by_object: HashMap<Term, Vec<usize>>,
}

impl Graph {
    fn new(triples: Vec<TripleOf>) -> Self {
        let mut by_subject: HashMap<Term, Vec<usize>> = HashMap::new();
        let mut by_predicate: HashMap<NamedNode, Vec<usize>> = HashMap::new();
        let mut by_object: HashMap<Term, Vec<usize>> = HashMap::new();
        for (i, (s, p, o)) in triples.iter().enumerate() {
            by_subject.entry(s.clone()).or_default().push(i);
            by_predicate.entry(p.clone()).or_default().push(i);
            by_object.entry(o.clone()).or_default().push(i);
        }
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            all: (0..triples.len()).collect(),
            triples,
            by_subject,
            by_predicate,
            by_object,
        }
    }

    /// The triples a pattern with these terms (where known) can match: the shortest of
    /// the lists for its known terms. Every triple of the list is still matched against
    /// the whole pattern, so this is an index, not a filter.
    fn candidates(
        &self,
        subject: Option<&Term>,
        predicate: Option<&NamedNode>,
        object: Option<&Term>,
    ) -> &[usize] {
        let lists = [
            subject.map(|s| self.by_subject.get(s)),
            predicate.map(|p| self.by_predicate.get(p)),
            object.map(|o| self.by_object.get(o)),
        ];
        let mut best: &[usize] = &self.all;
        for list in lists.into_iter().flatten() {
            let list = list.map_or(&[][..], Vec::as_slice);
            if list.len() < best.len() {
                best = list;
            }
        }
        best
    }

    fn nodes(&self) -> Vec<Term> {
        let mut nodes: Vec<Term> = self
            .triples
            .iter()
            .flat_map(|(s, _, o)| [s.clone(), o.clone()])
            .collect();
        nodes.sort_by_key(ToString::to_string);
        nodes.dedup();
        nodes
    }
}

struct Context<'d> {
    dataset: &'d Dataset,
    default_graph: Graph,
    /// The named graphs the query may read.
    named: Vec<NamedOrBlankNode>,
    evaluator: Evaluator,
    /// `EXISTS` answers per pattern, graph and values (the answer depends on nothing else).
    exists: std::cell::RefCell<HashMap<ExistsKey, bool>>,
}

type ExistsKey = (usize, usize, Vec<(String, Term)>);

/// A graph's identity: unique per graph built (not its address, which a graph built
/// after another one is dropped can reuse).
fn graph_key(graph: &Graph) -> usize {
    graph.id
}

impl<'d> Context<'d> {
    fn new(
        dataset: &'d Dataset,
        options: &QueryOptions,
        own: Option<&QueryDataset>,
        base: Option<nrese_rdf::Iri<String>>,
    ) -> Self {
        let specification: Option<QueryDatasetSpecification> = options
            .dataset
            .clone()
            .or_else(|| own.cloned().map(Into::into))
            .filter(|s| !s.is_default_dataset());
        let all = dataset.named_graphs();
        let (default_graph, named) = match specification {
            None if options.union_default_graph => (merge(dataset, None), all),
            None => (
                Graph::new(dataset.graph_triples(&GraphName::DefaultGraph)),
                all,
            ),
            Some(specification) => {
                let default = match specification.default_graph_graphs() {
                    None => merge(dataset, None),
                    Some(graphs) => merge(dataset, Some(graphs)),
                };
                // A store has a named graph while it holds a statement: a graph the
                // query names and the store lacks is empty, and not in the dataset.
                let named = match specification.available_named_graphs() {
                    None => all,
                    Some(graphs) => {
                        let mut listed: Vec<NamedOrBlankNode> =
                            graphs.iter().filter(|g| all.contains(g)).cloned().collect();
                        listed.sort_by_key(ToString::to_string);
                        listed.dedup();
                        listed
                    }
                };
                (default, named)
            }
        };
        Self {
            dataset,
            default_graph,
            named,
            evaluator: Evaluator::with_base(base),
            exists: std::cell::RefCell::default(),
        }
    }

    /// The solutions of `pattern` in `graph`; `outer` holds the values put in by an
    /// `EXISTS` (substitution, §18.6): its variables stand for those values.
    fn eval(
        &self,
        pattern: &GraphPattern,
        graph: &Graph,
        outer: &Solution,
    ) -> Result<Vec<Solution>, QueryEvaluationError> {
        Ok(match pattern {
            GraphPattern::Bgp { patterns } => {
                let mut solutions = vec![Solution::new()];
                for triple in patterns {
                    let mut next = Vec::new();
                    for solution in &solutions {
                        let known = |t: &TermPattern| -> Option<Term> {
                            match t {
                                TermPattern::NamedNode(n) => Some(n.clone().into()),
                                TermPattern::Literal(l) => Some(l.clone().into()),
                                TermPattern::Variable(v) => {
                                    solution.get(v).or_else(|| outer.get(v)).cloned()
                                }
                                _ => None,
                            }
                        };
                        let predicate = match &triple.predicate {
                            NamedNodePattern::NamedNode(n) => Some(n.clone()),
                            NamedNodePattern::Variable(v) => {
                                match solution.get(v).or_else(|| outer.get(v)) {
                                    Some(Term::NamedNode(n)) => Some(n.clone()),
                                    Some(_) => continue,
                                    None => None,
                                }
                            }
                        };
                        let (subject, object) = (known(&triple.subject), known(&triple.object));
                        for &i in
                            graph.candidates(subject.as_ref(), predicate.as_ref(), object.as_ref())
                        {
                            let (s, p, o) = &graph.triples[i];
                            let mut extended = solution.clone();
                            if bind_term(&triple.subject, s, &mut extended, outer)
                                && bind_predicate(&triple.predicate, p, &mut extended, outer)
                                && bind_term(&triple.object, o, &mut extended, outer)
                            {
                                next.push(extended);
                            }
                        }
                    }
                    solutions = next;
                }
                solutions
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => {
                let pairs = self.path_pairs(path, graph, subject, object, outer);
                let mut out = Vec::new();
                for (s, o) in pairs {
                    let mut solution = Solution::new();
                    if bind_term(subject, &s, &mut solution, outer)
                        && bind_term(object, &o, &mut solution, outer)
                    {
                        out.push(solution);
                    }
                }
                out
            }
            GraphPattern::Join { left, right } => {
                let left = self.eval(left, graph, outer)?;
                let right = self.eval(right, graph, outer)?;
                join(&left, &right)
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                let left = self.eval(left, graph, outer)?;
                let right = self.eval(right, graph, outer)?;
                let index = Compatible::new(&left, &right);
                let mut out = Vec::new();
                for l in &left {
                    let mut matched = false;
                    for r in index.candidates(l) {
                        if let Some(merged) = merge_solutions(l, r) {
                            let keep = match expression {
                                Some(e) => self.test(e, &merged, graph, outer, 0)?,
                                None => true,
                            };
                            if keep {
                                matched = true;
                                out.push(merged);
                            }
                        }
                    }
                    if !matched {
                        out.push(l.clone());
                    }
                }
                out
            }
            GraphPattern::Filter { expr, inner } => {
                let mut out = Vec::new();
                for (i, s) in self.eval(inner, graph, outer)?.into_iter().enumerate() {
                    if self.test(expr, &s, graph, outer, i as u64)? {
                        out.push(s);
                    }
                }
                out
            }
            GraphPattern::Union { left, right } => {
                let mut out = self.eval(left, graph, outer)?;
                out.extend(self.eval(right, graph, outer)?);
                out
            }
            GraphPattern::Graph { name, inner } => {
                let mut out = Vec::new();
                for g in &self.named {
                    let term: Term = g.clone().into();
                    let fixed = match name {
                        NamedNodePattern::NamedNode(n) => Some(Term::from(n.clone())),
                        NamedNodePattern::Variable(v) => outer.get(v).cloned(),
                    };
                    if fixed.as_ref().is_some_and(|f| f != &term) {
                        continue;
                    }
                    let named = Graph::new(self.dataset.graph_triples(&match g {
                        NamedOrBlankNode::NamedNode(n) => GraphName::NamedNode(n.clone()),
                        NamedOrBlankNode::BlankNode(b) => GraphName::BlankNode(b.clone()),
                    }));
                    for mut s in self.eval(inner, &named, outer)? {
                        if let NamedNodePattern::Variable(v) = name
                            && !outer.contains_key(v)
                        {
                            match s.get(v) {
                                Some(bound) if bound != &term => continue,
                                _ => {
                                    s.insert(v.clone(), term.clone());
                                }
                            }
                        }
                        out.push(s);
                    }
                }
                out
            }
            GraphPattern::Extend {
                inner,
                variable,
                expression,
            } => {
                let mut out = Vec::new();
                for (i, mut s) in self.eval(inner, graph, outer)?.into_iter().enumerate() {
                    if !s.contains_key(variable)
                        && let Some(value) = self.value(expression, &s, graph, outer, i as u64)?
                    {
                        s.insert(variable.clone(), value);
                    }
                    out.push(s);
                }
                out
            }
            GraphPattern::Minus { left, right } => {
                let left = self.eval(left, graph, outer)?;
                let right = self.eval(right, graph, outer)?;
                left.into_iter()
                    .filter(|l| {
                        !right.iter().any(|r| {
                            r.keys().any(|k| l.contains_key(k)) && merge_solutions(l, r).is_some()
                        })
                    })
                    .collect()
            }
            GraphPattern::Values {
                variables,
                bindings,
            } => bindings
                .iter()
                .filter_map(|row| {
                    let mut s = Solution::new();
                    for (v, value) in variables.iter().zip(row) {
                        if let Some(value) = value {
                            let term = Term::from(value.clone());
                            if outer.get(v).is_some_and(|o| o != &term) {
                                return None;
                            }
                            s.insert(v.clone(), term);
                        }
                    }
                    Some(s)
                })
                .collect(),
            GraphPattern::OrderBy { inner, expression } => {
                let solutions = self.eval(inner, graph, outer)?;
                let mut keyed = Vec::with_capacity(solutions.len());
                for (i, s) in solutions.into_iter().enumerate() {
                    let mut keys = Vec::new();
                    for key in expression {
                        let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = key;
                        keys.push(self.value(e, &s, graph, outer, i as u64)?);
                    }
                    keyed.push((keys, s));
                }
                keyed.sort_by(|(a, _), (b, _)| {
                    for (index, key) in expression.iter().enumerate() {
                        let order = value::order(a[index].as_ref(), b[index].as_ref());
                        let order = match key {
                            OrderExpression::Asc(_) => order,
                            OrderExpression::Desc(_) => order.reverse(),
                        };
                        if order.is_ne() {
                            return order;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
                keyed.into_iter().map(|(_, s)| s).collect()
            }
            GraphPattern::Project { inner, variables } => {
                // The subquery sees the outer values only of the variables it projects.
                let visible: Solution = outer
                    .iter()
                    .filter(|(v, _)| variables.contains(v))
                    .map(|(v, t)| (v.clone(), t.clone()))
                    .collect();
                self.eval(inner, graph, &visible)?
                    .into_iter()
                    .map(|s| {
                        s.into_iter()
                            .filter(|(v, _)| variables.contains(v))
                            .collect()
                    })
                    .collect()
            }
            GraphPattern::Distinct { inner } => {
                let mut seen = HashSet::new();
                self.eval(inner, graph, outer)?
                    .into_iter()
                    .filter(|s| seen.insert(key(s)))
                    .collect()
            }
            GraphPattern::Reduced { inner } => self.eval(inner, graph, outer)?,
            GraphPattern::Slice {
                inner,
                start,
                length,
            } => self
                .eval(inner, graph, outer)?
                .into_iter()
                .skip(*start)
                .take(length.unwrap_or(usize::MAX))
                .collect(),
            GraphPattern::Group {
                inner,
                variables,
                aggregates,
            } => self.group(inner, variables, aggregates, graph, outer)?,
            GraphPattern::Service { .. } => {
                return Err(error("SERVICE isn't evaluated by the reference"));
            }
            #[allow(unreachable_patterns)]
            _ => return Err(error("a construct the reference doesn't evaluate")),
        })
    }

    /// The value of `expression` for solution number `index` (`None`: an error).
    fn value(
        &self,
        expression: &Expression,
        solution: &Solution,
        graph: &Graph,
        outer: &Solution,
        index: u64,
    ) -> Result<Option<Term>, QueryEvaluationError> {
        let resolved = self.resolve_exists(expression, solution, graph, outer)?;
        let binding = |v: &Variable| solution.get(v).or_else(|| outer.get(v)).cloned();
        Ok(self
            .evaluator
            .in_solution(index, || self.evaluator.eval(&resolved, &binding)))
    }

    /// The effective boolean value of `expression` as a filter: errors are false.
    fn test(
        &self,
        expression: &Expression,
        solution: &Solution,
        graph: &Graph,
        outer: &Solution,
        index: u64,
    ) -> Result<bool, QueryEvaluationError> {
        let resolved = self.resolve_exists(expression, solution, graph, outer)?;
        let binding = |v: &Variable| solution.get(v).or_else(|| outer.get(v)).cloned();
        Ok(self
            .evaluator
            .in_solution(index, || self.evaluator.filter(&resolved, &binding)))
    }

    /// `expression` with each `EXISTS { p }` replaced by its value for `solution`: `p`
    /// evaluated with the solution's values put in (§18.6).
    fn resolve_exists(
        &self,
        expression: &Expression,
        solution: &Solution,
        graph: &Graph,
        outer: &Solution,
    ) -> Result<Expression, QueryEvaluationError> {
        let r = |e: &Expression| -> Result<Box<Expression>, QueryEvaluationError> {
            Ok(Box::new(self.resolve_exists(e, solution, graph, outer)?))
        };
        let list = |l: &[Expression]| -> Result<Vec<Expression>, QueryEvaluationError> {
            l.iter()
                .map(|e| self.resolve_exists(e, solution, graph, outer))
                .collect()
        };
        Ok(match expression {
            Expression::Exists(pattern) => {
                let mut values = outer.clone();
                values.extend(solution.iter().map(|(v, t)| (v.clone(), t.clone())));
                let memo_key = (
                    std::ptr::from_ref(&**pattern) as usize,
                    graph_key(graph),
                    key(&values),
                );
                let known = self.exists.borrow().get(&memo_key).copied();
                let found = match known {
                    Some(found) => found,
                    None => {
                        let found = !self.eval(pattern, graph, &values)?.is_empty();
                        self.exists.borrow_mut().insert(memo_key, found);
                        found
                    }
                };
                Expression::Literal(Literal::from(found))
            }
            Expression::Or(a, b) => Expression::Or(r(a)?, r(b)?),
            Expression::And(a, b) => Expression::And(r(a)?, r(b)?),
            Expression::Equal(a, b) => Expression::Equal(r(a)?, r(b)?),
            Expression::SameTerm(a, b) => Expression::SameTerm(r(a)?, r(b)?),
            Expression::Greater(a, b) => Expression::Greater(r(a)?, r(b)?),
            Expression::GreaterOrEqual(a, b) => Expression::GreaterOrEqual(r(a)?, r(b)?),
            Expression::Less(a, b) => Expression::Less(r(a)?, r(b)?),
            Expression::LessOrEqual(a, b) => Expression::LessOrEqual(r(a)?, r(b)?),
            Expression::Add(a, b) => Expression::Add(r(a)?, r(b)?),
            Expression::Subtract(a, b) => Expression::Subtract(r(a)?, r(b)?),
            Expression::Multiply(a, b) => Expression::Multiply(r(a)?, r(b)?),
            Expression::Divide(a, b) => Expression::Divide(r(a)?, r(b)?),
            Expression::UnaryPlus(a) => Expression::UnaryPlus(r(a)?),
            Expression::UnaryMinus(a) => Expression::UnaryMinus(r(a)?),
            Expression::Not(a) => Expression::Not(r(a)?),
            Expression::In(a, l) => Expression::In(r(a)?, list(l)?),
            Expression::If(a, b, c) => Expression::If(r(a)?, r(b)?, r(c)?),
            Expression::Coalesce(l) => Expression::Coalesce(list(l)?),
            Expression::FunctionCall(f, l) => Expression::FunctionCall(f.clone(), list(l)?),
            other => other.clone(),
        })
    }

    /// `GROUP BY` and the aggregates (§18.5.1).
    fn group(
        &self,
        inner: &GraphPattern,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
        graph: &Graph,
        outer: &Solution,
    ) -> Result<Vec<Solution>, QueryEvaluationError> {
        let solutions = self.eval(inner, graph, outer)?;
        let mut groups: Vec<(Vec<Option<Term>>, Vec<Solution>)> = Vec::new();
        for s in solutions {
            let key: Vec<Option<Term>> = variables.iter().map(|v| s.get(v).cloned()).collect();
            match groups.iter_mut().find(|(k, _)| k == &key) {
                Some((_, members)) => members.push(s),
                None => groups.push((key, vec![s])),
            }
        }
        // Without GROUP BY, aggregates over nothing still give one group.
        if groups.is_empty() && variables.is_empty() {
            groups.push((Vec::new(), Vec::new()));
        }
        let mut out = Vec::new();
        for (key, members) in groups {
            let mut result = Solution::new();
            for (v, value) in variables.iter().zip(key) {
                if let Some(value) = value {
                    result.insert(v.clone(), value);
                }
            }
            for (target, aggregate) in aggregates {
                if let Some(value) = self.aggregate(aggregate, &members, graph, outer)? {
                    result.insert(target.clone(), value);
                }
            }
            out.push(result);
        }
        Ok(out)
    }

    fn aggregate(
        &self,
        aggregate: &AggregateExpression,
        members: &[Solution],
        graph: &Graph,
        outer: &Solution,
    ) -> Result<Option<Term>, QueryEvaluationError> {
        let integer = |n: usize| Some(Literal::from(n as i64).into());
        match aggregate {
            AggregateExpression::CountSolutions { distinct } => {
                if *distinct {
                    let mut seen: Vec<&Solution> = Vec::new();
                    for m in members {
                        if !seen.contains(&m) {
                            seen.push(m);
                        }
                    }
                    Ok(integer(seen.len()))
                } else {
                    Ok(integer(members.len()))
                }
            }
            AggregateExpression::FunctionCall {
                name,
                expr,
                distinct,
            } => {
                let mut values = Vec::with_capacity(members.len());
                for (i, m) in members.iter().enumerate() {
                    values.push(self.value(expr, m, graph, outer, i as u64)?);
                }
                // COUNT skips errors and SAMPLE takes the first value; one error makes the
                // others unbound (the executor's choice, as §18.5.1 leaves errors open).
                let tolerant = matches!(name, AggregateFunction::Count | AggregateFunction::Sample);
                if !tolerant && values.iter().any(Option::is_none) {
                    return Ok(None);
                }
                let mut values: Vec<Term> = values.into_iter().flatten().collect();
                if *distinct {
                    let mut kept: Vec<Term> = Vec::new();
                    for v in values {
                        if !kept.contains(&v) {
                            kept.push(v);
                        }
                    }
                    values = kept;
                }
                Ok(match name {
                    AggregateFunction::Count => integer(values.len()),
                    AggregateFunction::Sample => values.into_iter().next(),
                    AggregateFunction::Min => values.into_iter().reduce(|best, v| {
                        if value::order(Some(&v), Some(&best)).is_lt() {
                            v
                        } else {
                            best
                        }
                    }),
                    AggregateFunction::Max => values.into_iter().reduce(|best, v| {
                        if value::order(Some(&v), Some(&best)).is_gt() {
                            v
                        } else {
                            best
                        }
                    }),
                    AggregateFunction::Sum => value::sum(&values),
                    AggregateFunction::Avg => value::average(&values),
                    AggregateFunction::GroupConcat { separator } => {
                        value::group_concat(&values, separator.as_deref().unwrap_or(" "))
                    }
                    #[allow(unreachable_patterns)]
                    _ => None,
                })
            }
        }
    }

    /// The pairs `path` connects in `graph` (§18.4): a list, so sequences and alternatives
    /// keep their duplicates; `*`, `+` and `?` give each pair once.
    fn path_pairs(
        &self,
        path: &PropertyPathExpression,
        graph: &Graph,
        subject: &TermPattern,
        object: &TermPattern,
        outer: &Solution,
    ) -> Vec<(Term, Term)> {
        let fixed = |t: &TermPattern| -> Option<Term> {
            match t {
                TermPattern::NamedNode(n) => Some(n.clone().into()),
                TermPattern::Literal(l) => Some(l.clone().into()),
                TermPattern::Variable(v) => outer.get(v).cloned(),
                _ => None,
            }
        };
        let (start, end) = (fixed(subject), fixed(object));
        let pairs = self.pairs(path, graph, start.as_ref(), end.as_ref());
        pairs
            .into_iter()
            .filter(|(s, o)| {
                start.as_ref().is_none_or(|x| x == s) && end.as_ref().is_none_or(|x| x == o)
            })
            .collect()
    }

    fn pairs(
        &self,
        path: &PropertyPathExpression,
        graph: &Graph,
        start: Option<&Term>,
        end: Option<&Term>,
    ) -> Vec<(Term, Term)> {
        let distinct = |mut pairs: Vec<(Term, Term)>| {
            let mut seen = HashSet::new();
            pairs.retain(|p| seen.insert(p.clone()));
            pairs
        };
        match path {
            PropertyPathExpression::NamedNode(p) => graph
                .candidates(start, Some(p), end)
                .iter()
                .map(|&i| &graph.triples[i])
                // The candidates are an index (possibly the object's): match in full.
                .filter(|(s, q, o)| {
                    q == p && start.is_none_or(|x| x == s) && end.is_none_or(|x| x == o)
                })
                .map(|(s, _, o)| (s.clone(), o.clone()))
                .collect(),
            PropertyPathExpression::Reverse(inner) => self
                .pairs(inner, graph, end, start)
                .into_iter()
                .map(|(s, o)| (o, s))
                .collect(),
            PropertyPathExpression::Sequence(a, b) => {
                let left = self.pairs(a, graph, start, None);
                let right = self.pairs(b, graph, None, end);
                let mut out = Vec::new();
                // One pair per path: a pair two intermediate nodes connect comes twice.
                for (x, m) in &left {
                    for (n, y) in &right {
                        if m == n {
                            out.push((x.clone(), y.clone()));
                        }
                    }
                }
                out
            }
            PropertyPathExpression::Alternative(a, b) => {
                let mut out = self.pairs(a, graph, start, end);
                out.extend(self.pairs(b, graph, start, end));
                if self.dataset.deviations.alternative_paths_distinct {
                    distinct(out)
                } else {
                    out
                }
            }
            PropertyPathExpression::NegatedPropertySet(excluded) => graph
                .triples
                .iter()
                .filter(|(_, q, _)| !excluded.contains(q))
                .map(|(s, _, o)| (s.clone(), o.clone()))
                .collect(),
            PropertyPathExpression::ZeroOrOne(inner) => {
                let mut out = self.zero_length(graph, start, end);
                out.extend(self.pairs(inner, graph, start, end));
                distinct(out)
            }
            PropertyPathExpression::OneOrMore(inner) => {
                distinct(self.closure(inner, graph, start, end, false))
            }
            PropertyPathExpression::ZeroOrMore(inner) => {
                distinct(self.closure(inner, graph, start, end, true))
            }
        }
    }

    /// The pairs of the zero-length path: each node of the graph with itself, and a fixed
    /// end with itself (§18.4, "ALP"; also a term the graph doesn't have).
    fn zero_length(
        &self,
        graph: &Graph,
        start: Option<&Term>,
        end: Option<&Term>,
    ) -> Vec<(Term, Term)> {
        match start.or(end) {
            Some(term) => vec![(term.clone(), term.clone())],
            None => graph.nodes().into_iter().map(|n| (n.clone(), n)).collect(),
        }
    }

    /// `path+` (`path*` with `reflexive`): from each start, everything it reaches.
    fn closure(
        &self,
        path: &PropertyPathExpression,
        graph: &Graph,
        start: Option<&Term>,
        end: Option<&Term>,
        reflexive: bool,
    ) -> Vec<(Term, Term)> {
        let step = self.pairs(path, graph, None, None);
        let nodes = graph.nodes();
        // Everything `from` reaches in one or more steps along `edges`.
        let reach = |from: &Term, edges: &HashMap<&Term, Vec<&Term>>| -> Vec<Term> {
            let mut reached: HashSet<&Term> = HashSet::new();
            let mut order = Vec::new();
            let mut frontier = vec![from];
            while let Some(node) = frontier.pop() {
                for &next in edges.get(node).into_iter().flatten() {
                    if reached.insert(next) {
                        order.push(next.clone());
                        frontier.push(next);
                    }
                }
            }
            order
        };
        let mut out = Vec::new();
        if let (None, Some(e)) = (start, end) {
            // Only the pairs ending at `e`: walk backwards from it.
            let mut backwards: HashMap<&Term, Vec<&Term>> = HashMap::new();
            for (a, b) in &step {
                backwards.entry(b).or_default().push(a);
            }
            if reflexive {
                out.push((e.clone(), e.clone()));
            }
            for s in reach(e, &backwards) {
                out.push((s, e.clone()));
            }
            return out;
        }
        let mut forwards: HashMap<&Term, Vec<&Term>> = HashMap::new();
        for (a, b) in &step {
            forwards.entry(a).or_default().push(b);
        }
        let starts: Vec<Term> = match start {
            Some(s) => vec![s.clone()],
            None => nodes.clone(),
        };
        for s in starts {
            if reflexive {
                out.push((s.clone(), s.clone()));
            }
            for r in reach(&s, &forwards) {
                out.push((s.clone(), r));
            }
        }
        out
    }

    /// DESCRIBE: for each term the solutions bind, the triples it is the subject of in
    /// the default graph, and those of the blank nodes they lead to.
    fn describe(&self, solutions: &[Solution]) -> Vec<Triple> {
        let mut todo: Vec<Term> = Vec::new();
        for s in solutions {
            for (v, t) in s {
                if v.as_str().starts_with(BLANK) {
                    continue;
                }
                if !todo.contains(t) {
                    todo.push(t.clone());
                }
            }
        }
        let mut done: Vec<Term> = Vec::new();
        let mut out = Vec::new();
        while let Some(node) = todo.pop() {
            if done.contains(&node) {
                continue;
            }
            done.push(node.clone());
            let Ok(subject) = NamedOrBlankNode::try_from(node.clone()) else {
                continue;
            };
            for (s, p, o) in &self.default_graph.triples {
                if s == &node {
                    out.push(Triple::new(subject.clone(), p.clone(), o.clone()));
                    if matches!(o, Term::BlankNode(_)) {
                        todo.push(o.clone());
                    }
                }
            }
        }
        out
    }
}

/// A solution as a hashable key: its bindings in variable order.
fn key(solution: &Solution) -> Vec<(String, Term)> {
    let mut key: Vec<(String, Term)> = solution
        .iter()
        .map(|(v, t)| (v.as_str().to_owned(), t.clone()))
        .collect();
    key.sort_by(|a, b| a.0.cmp(&b.0));
    key
}

/// The RDF merge of `graphs` (all graphs if `None`): each triple once.
fn merge(dataset: &Dataset, graphs: Option<&[GraphName]>) -> Graph {
    let mut seen = HashSet::new();
    let triples = dataset
        .quads
        .iter()
        .filter(|q| graphs.is_none_or(|g| g.contains(&q.graph_name)))
        .map(|q| {
            (
                Term::from(q.subject.clone()),
                q.predicate.clone(),
                q.object.clone(),
            )
        })
        .filter(|t| seen.insert(t.clone()))
        .collect();
    Graph::new(triples)
}

/// Blank nodes of a pattern are variables no query can name (§18.2.1): they join where
/// the parser's translation shares them (a path sequence becomes two paths through one),
/// and no projection lists them.
fn blank_variable(b: &nrese_sparql_syntax::term::BlankNode) -> Variable {
    Variable::new_unchecked(format!("{BLANK}{}", b.as_str()))
}

const BLANK: &str = "__blank_";

fn bind_term(
    pattern: &TermPattern,
    value: &Term,
    solution: &mut Solution,
    outer: &Solution,
) -> bool {
    match pattern {
        TermPattern::NamedNode(n) => matches!(value, Term::NamedNode(m) if m == n),
        TermPattern::Literal(l) => matches!(value, Term::Literal(m) if m == l),
        TermPattern::BlankNode(b) => bind_variable(&blank_variable(b), value, solution, outer),
        TermPattern::Variable(v) => bind_variable(v, value, solution, outer),
        // SPARQL 1.2: a triple term pattern matches a triple term part by part.
        TermPattern::Triple(pattern) => match value {
            Term::Triple(triple) => {
                bind_term(
                    &pattern.subject,
                    &Term::from(triple.subject.clone()),
                    solution,
                    outer,
                ) && bind_predicate(&pattern.predicate, &triple.predicate, solution, outer)
                    && bind_term(&pattern.object, &triple.object, solution, outer)
            }
            _ => false,
        },
    }
}

/// A template's term with a solution's values; a blank node is fresh per solution
/// (`fresh` maps its label). `None` if a variable is unbound or a triple term would get a
/// subject that is no IRI or blank node.
fn instantiate(
    t: &TermPattern,
    solution: &Solution,
    fresh: &mut HashMap<String, BlankNode>,
) -> Option<Term> {
    Some(match t {
        TermPattern::NamedNode(n) => n.clone().into(),
        TermPattern::Literal(l) => l.clone().into(),
        TermPattern::BlankNode(b) => fresh
            .entry(b.as_str().to_owned())
            .or_default()
            .clone()
            .into(),
        TermPattern::Variable(v) => solution.get(v)?.clone(),
        TermPattern::Triple(t) => Triple::new(
            NamedOrBlankNode::try_from(instantiate(&t.subject, solution, fresh)?).ok()?,
            predicate_of(&t.predicate, solution)?,
            instantiate(&t.object, solution, fresh)?,
        )
        .into(),
    })
}

/// A `DELETE` template's term with a solution's values.
fn instantiate_ground(t: &GroundTermPattern, solution: &Solution) -> Option<Term> {
    Some(match t {
        GroundTermPattern::NamedNode(n) => n.clone().into(),
        GroundTermPattern::Literal(l) => l.clone().into(),
        GroundTermPattern::Variable(v) => solution.get(v)?.clone(),
        GroundTermPattern::Triple(t) => Triple::new(
            NamedOrBlankNode::try_from(instantiate_ground(&t.subject, solution)?).ok()?,
            predicate_of(&t.predicate, solution)?,
            instantiate_ground(&t.object, solution)?,
        )
        .into(),
    })
}

/// A template's predicate with a solution's values.
fn predicate_of(p: &NamedNodePattern, solution: &Solution) -> Option<NamedNode> {
    match p {
        NamedNodePattern::NamedNode(n) => Some(n.clone()),
        NamedNodePattern::Variable(v) => match solution.get(v)? {
            Term::NamedNode(n) => Some(n.clone()),
            _ => None,
        },
    }
}

fn bind_predicate(
    pattern: &NamedNodePattern,
    value: &NamedNode,
    solution: &mut Solution,
    outer: &Solution,
) -> bool {
    match pattern {
        NamedNodePattern::NamedNode(n) => n == value,
        NamedNodePattern::Variable(v) => bind_variable(v, &value.clone().into(), solution, outer),
    }
}

fn bind_variable(v: &Variable, value: &Term, solution: &mut Solution, outer: &Solution) -> bool {
    if let Some(fixed) = outer.get(v) {
        return fixed == value;
    }
    match solution.get(v) {
        Some(bound) => bound == value,
        None => {
            solution.insert(v.clone(), value.clone());
            true
        }
    }
}

fn merge_solutions(a: &Solution, b: &Solution) -> Option<Solution> {
    let mut merged = a.clone();
    for (v, t) in b {
        match merged.get(v) {
            Some(bound) if bound != t => return None,
            Some(_) => {}
            None => {
                merged.insert(v.clone(), t.clone());
            }
        }
    }
    Some(merged)
}

/// The right-hand rows a left-hand row can be compatible with. Rows can only be
/// compatible if they agree on the variables both sides bind in every row, so the right
/// side is grouped by those; each candidate is still checked in full, and the candidates
/// keep their order, so this is an index, not a change of meaning.
struct Compatible<'a> {
    right: &'a [Solution],
    shared: Vec<Variable>,
    groups: HashMap<Vec<Term>, Vec<&'a Solution>>,
}

impl<'a> Compatible<'a> {
    fn new(left: &[Solution], right: &'a [Solution]) -> Self {
        let always = |rows: &[Solution]| -> Vec<Variable> {
            let Some(first) = rows.first() else {
                return Vec::new();
            };
            let mut vars: Vec<Variable> = first
                .keys()
                .filter(|v| rows.iter().all(|r| r.contains_key(*v)))
                .cloned()
                .collect();
            vars.sort();
            vars
        };
        let right_vars = always(right);
        let shared: Vec<Variable> = always(left)
            .into_iter()
            .filter(|v| right_vars.contains(v))
            .collect();
        let mut groups: HashMap<Vec<Term>, Vec<&Solution>> = HashMap::new();
        if !shared.is_empty() {
            for r in right {
                groups.entry(Self::key(&shared, r)).or_default().push(r);
            }
        }
        Self {
            right,
            shared,
            groups,
        }
    }

    fn key(shared: &[Variable], row: &Solution) -> Vec<Term> {
        shared.iter().map(|v| row[v].clone()).collect()
    }

    fn candidates(&self, left: &Solution) -> Box<dyn Iterator<Item = &'a Solution> + '_> {
        if self.shared.is_empty() {
            return Box::new(self.right.iter());
        }
        Box::new(
            self.groups
                .get(&Self::key(&self.shared, left))
                .into_iter()
                .flatten()
                .copied(),
        )
    }
}

fn join(left: &[Solution], right: &[Solution]) -> Vec<Solution> {
    let index = Compatible::new(left, right);
    let mut out = Vec::new();
    for l in left {
        for r in index.candidates(l) {
            if let Some(merged) = merge_solutions(l, r) {
                out.push(merged);
            }
        }
    }
    out
}

/// CONSTRUCT (§16.2): the template per solution, fresh blank nodes per solution, triples
/// with an unbound or ill-placed term left out; each triple once.
fn construct(template: &[TriplePattern], solutions: &[Solution]) -> Vec<Triple> {
    let mut out: Vec<Triple> = Vec::new();
    let mut seen = HashSet::new();
    for solution in solutions {
        let mut fresh: HashMap<String, BlankNode> = HashMap::new();
        let mut term = |t: &TermPattern| instantiate(t, solution, &mut fresh);
        for triple in template {
            let (Some(s), Some(o)) = (term(&triple.subject), term(&triple.object)) else {
                continue;
            };
            let p = match &triple.predicate {
                NamedNodePattern::NamedNode(n) => n.clone(),
                NamedNodePattern::Variable(v) => match solution.get(v) {
                    Some(Term::NamedNode(n)) => n.clone(),
                    _ => continue,
                },
            };
            let Ok(s) = NamedOrBlankNode::try_from(s) else {
                continue;
            };
            let triple = Triple::new(s, p, o);
            if seen.insert(triple.clone()) {
                out.push(triple);
            }
        }
    }
    out
}

fn ground_quad(quad: &GroundQuadPattern, solution: &Solution) -> Option<Quad> {
    let term = |t: &GroundTermPattern| instantiate_ground(t, solution);
    let subject = NamedOrBlankNode::try_from(term(&quad.subject)?).ok()?;
    let predicate = match &quad.predicate {
        NamedNodePattern::NamedNode(n) => n.clone(),
        NamedNodePattern::Variable(v) => match solution.get(v)? {
            Term::NamedNode(n) => n.clone(),
            _ => return None,
        },
    };
    let object = term(&quad.object)?;
    Some(Quad::new(
        subject,
        predicate,
        object,
        graph_pattern(&quad.graph_name, solution)?,
    ))
}

fn template_quad(
    quad: &PatternQuad,
    solution: &Solution,
    fresh: &mut HashMap<String, BlankNode>,
) -> Option<Quad> {
    let mut term = |t: &TermPattern| instantiate(t, solution, fresh);
    let subject = NamedOrBlankNode::try_from(term(&quad.subject)?).ok()?;
    let object = term(&quad.object)?;
    let predicate = match &quad.predicate {
        NamedNodePattern::NamedNode(n) => n.clone(),
        NamedNodePattern::Variable(v) => match solution.get(v)? {
            Term::NamedNode(n) => n.clone(),
            _ => return None,
        },
    };
    Some(Quad::new(
        subject,
        predicate,
        object,
        graph_pattern(&quad.graph_name, solution)?,
    ))
}

/// The graph an update template's quad goes to. A variable bound to a blank node names a
/// graph too: RDF 1.1 datasets allow blank node graph names, the store holds them, and
/// `GRAPH ?g` finds them, so an update can reach the graphs a query sees. (SPARQL 1.1
/// Update lists literals, not blank nodes, among the constructs a template leaves out.)
fn graph_pattern(graph: &GraphNamePattern, solution: &Solution) -> Option<GraphName> {
    Some(match graph {
        GraphNamePattern::NamedNode(n) => n.clone().into(),
        GraphNamePattern::DefaultGraph => GraphName::DefaultGraph,
        GraphNamePattern::Variable(v) => match solution.get(v)? {
            Term::NamedNode(n) => n.clone().into(),
            Term::BlankNode(b) => b.clone().into(),
            Term::Literal(_) | Term::Triple(_) => return None,
        },
    })
}

/// Evaluates `query` on what `snapshot` holds in `options.read_model`: the reference's
/// answer to what [`nrese_sparql::evaluate_query`] answers.
pub fn evaluate_query(
    snapshot: &Snapshot,
    query: &Query,
    options: &QueryOptions,
) -> Result<QueryResults<'static>, QueryEvaluationError> {
    Dataset::from_snapshot(snapshot, options.read_model).query(query, options)
}
