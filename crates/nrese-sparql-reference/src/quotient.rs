//! Canonical equality answers (`QueryOptions::equality_canonical`), the reference's own
//! way: the query over the quotient of the dataset by its `owl:sameAs` classes, made
//! explicitly. The dataset is the snapshot's closure read with every identity expanded
//! (strict reads), each term then replaced by its class's representative and each
//! statement kept once; the query's constants are replaced the same way, in its patterns
//! here and in its expressions per solution (with what `IRI()` makes), by the reference's
//! own evaluation (`Context::resolve_exists`). Nothing of the executor's canonical
//! answers is used: not its canonical reads, its rewrite of the query, nor its handling
//! of `IRI()`.
//!
//! A triple term's class is that of its parts: it is replaced part by part.

use std::collections::HashMap;

use nrese_engine::{ReadModel, Snapshot};
use nrese_rdf::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use nrese_sparql::QueryOptions;
use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::{GraphPattern, PropertyPathExpression};
use nrese_sparql_syntax::term::{
    GroundTerm, GroundTriple, NamedNodePattern, TermPattern, TriplePattern,
};

use crate::Dataset;

/// Each identity of a class with its representative.
pub(crate) struct Names(HashMap<Term, Term>);

impl Names {
    /// The classes canonical answers apply to `query` with: canonical answers asked for,
    /// a model with inferred statements, the store's default graph as the query's, and
    /// some class.
    pub(crate) fn of(snapshot: &Snapshot, query: &Query, options: &QueryOptions) -> Option<Self> {
        let default_dataset = options
            .dataset
            .as_ref()
            .is_none_or(|d| d.is_default_dataset())
            && query.dataset().is_none();
        if !options.equality_canonical
            || options.read_model == ReadModel::Asserted
            || !default_dataset
        {
            return None;
        }
        let classes = snapshot.equality_classes()?;
        let mut names = HashMap::new();
        for (representative, members) in classes.iter() {
            let representative = snapshot.decode(nrese_engine::TermId::from_raw(representative))?;
            for &member in members {
                let member = snapshot.decode(nrese_engine::TermId::from_raw(member))?;
                names.insert(member, representative.clone());
            }
        }
        (!names.is_empty()).then_some(Self(names))
    }

    /// `term`'s representative (`term` outside every class).
    pub(crate) fn term(&self, term: Term) -> Term {
        if let Term::Triple(t) = &term {
            let subject = NamedOrBlankNode::try_from(self.term(t.subject.clone().into()));
            if let Ok(subject) = subject {
                let triple =
                    Triple::new(subject, self.iri(&t.predicate), self.term(t.object.clone()));
                return triple.into();
            }
        }
        self.0.get(&term).cloned().unwrap_or(term)
    }

    fn iri(&self, n: &NamedNode) -> NamedNode {
        match self.term(n.clone().into()) {
            Term::NamedNode(m) => m,
            _ => n.clone(),
        }
    }

    /// The quotient of `dataset`: every term by its representative, every statement once.
    pub(crate) fn dataset(&self, dataset: &Dataset) -> Dataset {
        let subject = |s: &NamedOrBlankNode| -> NamedOrBlankNode {
            NamedOrBlankNode::try_from(self.term(s.clone().into())).unwrap_or_else(|_| s.clone())
        };
        let graph = |g: &GraphName| match g {
            GraphName::DefaultGraph => GraphName::DefaultGraph,
            GraphName::NamedNode(n) => GraphName::NamedNode(self.iri(n)),
            GraphName::BlankNode(b) => match self.term(b.clone().into()) {
                Term::NamedNode(n) => GraphName::NamedNode(n),
                Term::BlankNode(b) => GraphName::BlankNode(b),
                _ => GraphName::BlankNode(b.clone()),
            },
        };
        let mut out = Dataset::new(dataset.quads.iter().map(|q| {
            Quad::new(
                subject(&q.subject),
                self.iri(&q.predicate),
                self.term(q.object.clone()),
                graph(&q.graph_name),
            )
        }));
        out.graphs = dataset.graphs.iter().map(subject).collect();
        out
    }

    fn term_pattern(&self, t: &TermPattern) -> TermPattern {
        match t {
            TermPattern::NamedNode(n) => TermPattern::NamedNode(self.iri(n)),
            TermPattern::Literal(l) => match self.term(l.clone().into()) {
                Term::Literal(m) => TermPattern::Literal(m),
                _ => t.clone(),
            },
            TermPattern::Triple(t) => TermPattern::Triple(Box::new(TriplePattern {
                subject: self.term_pattern(&t.subject),
                predicate: self.predicate(&t.predicate),
                object: self.term_pattern(&t.object),
            })),
            other => other.clone(),
        }
    }

    fn ground(&self, t: &GroundTerm) -> GroundTerm {
        match t {
            GroundTerm::NamedNode(n) => GroundTerm::NamedNode(self.iri(n)),
            GroundTerm::Literal(l) => match self.term(l.clone().into()) {
                Term::Literal(m) => GroundTerm::Literal(m),
                _ => t.clone(),
            },
            GroundTerm::Triple(g) => GroundTerm::Triple(Box::new(GroundTriple {
                subject: self.iri(&g.subject),
                predicate: self.iri(&g.predicate),
                object: self.ground(&g.object),
            })),
        }
    }

    fn predicate(&self, p: &NamedNodePattern) -> NamedNodePattern {
        match p {
            NamedNodePattern::NamedNode(n) => NamedNodePattern::NamedNode(self.iri(n)),
            other => other.clone(),
        }
    }

    fn path(&self, path: &PropertyPathExpression) -> PropertyPathExpression {
        use PropertyPathExpression as P;
        let b = |x: &P| Box::new(self.path(x));
        match path {
            P::NamedNode(n) => P::NamedNode(self.iri(n)),
            P::Reverse(x) => P::Reverse(b(x)),
            P::Sequence(xs) => P::Sequence(xs.iter().map(|x| self.path(x)).collect()),
            P::Alternative(xs) => P::Alternative(xs.iter().map(|x| self.path(x)).collect()),
            P::ZeroOrMore(x) => P::ZeroOrMore(b(x)),
            P::OneOrMore(x) => P::OneOrMore(b(x)),
            P::ZeroOrOne(x) => P::ZeroOrOne(b(x)),
            P::NegatedPropertySet(ns) => {
                P::NegatedPropertySet(ns.iter().map(|n| self.iri(n)).collect())
            }
        }
    }

    /// `pattern` with the constants of its triple patterns, paths, `VALUES` and `GRAPH`
    /// names replaced; expressions are left to the evaluation, solution by solution.
    pub(crate) fn pattern(&self, pattern: &GraphPattern) -> GraphPattern {
        use GraphPattern as G;
        let b = |x: &G| Box::new(self.pattern(x));
        match pattern {
            G::Bgp { patterns } => G::Bgp {
                patterns: patterns
                    .iter()
                    .map(|t| TriplePattern {
                        subject: self.term_pattern(&t.subject),
                        predicate: self.predicate(&t.predicate),
                        object: self.term_pattern(&t.object),
                    })
                    .collect(),
            },
            G::Path {
                subject,
                path,
                object,
            } => G::Path {
                subject: self.term_pattern(subject),
                path: self.path(path),
                object: self.term_pattern(object),
            },
            G::Join { inputs } => G::Join {
                inputs: inputs.iter().map(|x| self.pattern(x)).collect(),
            },
            G::Union { inputs } => G::Union {
                inputs: inputs.iter().map(|x| self.pattern(x)).collect(),
            },
            G::LeftJoin {
                left,
                right,
                expression,
            } => G::LeftJoin {
                left: b(left),
                right: b(right),
                expression: expression.clone(),
            },
            G::Lateral { left, right } => G::Lateral {
                left: b(left),
                right: b(right),
            },
            G::Filter { expr, inner } => G::Filter {
                expr: expr.clone(),
                inner: b(inner),
            },
            G::Graph { name, inner } => G::Graph {
                name: self.predicate(name),
                inner: b(inner),
            },
            G::Extend { inner, bindings } => G::Extend {
                inner: b(inner),
                bindings: bindings.clone(),
            },
            G::Minus { left, right } => G::Minus {
                left: b(left),
                right: b(right),
            },
            G::Values {
                variables,
                bindings,
            } => G::Values {
                variables: variables.clone(),
                bindings: bindings
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|t| t.as_ref().map(|t| self.ground(t)))
                            .collect()
                    })
                    .collect(),
            },
            G::OrderBy { inner, expression } => G::OrderBy {
                inner: b(inner),
                expression: expression.clone(),
            },
            G::Project { inner, variables } => G::Project {
                inner: b(inner),
                variables: variables.clone(),
            },
            G::Distinct { inner } => G::Distinct { inner: b(inner) },
            G::Reduced { inner } => G::Reduced { inner: b(inner) },
            G::Slice {
                inner,
                start,
                length,
            } => G::Slice {
                inner: b(inner),
                start: *start,
                length: *length,
            },
            G::Group {
                inner,
                variables,
                aggregates,
            } => G::Group {
                inner: b(inner),
                variables: variables.clone(),
                aggregates: aggregates.clone(),
            },
            G::Service { .. } => pattern.clone(),
        }
    }
}
