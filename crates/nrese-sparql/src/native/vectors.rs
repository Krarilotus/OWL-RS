//! Vector similarity search through a virtual `SERVICE` (research designs §2): the vector
//! literals nearest to a query vector, joined to the rest of the query like any pattern.
//!
//! ```sparql
//! PREFIX nrv: <urn:nrese:vector:>
//! SELECT ?doc ?score WHERE {
//!   ?doc ex:embedding ?v .
//!   SERVICE nrv:search {
//!     ?v nrv:near "[0.12, -0.5, 0.33]"^^nrv:vector ;
//!        nrv:k 10 ;
//!        nrv:score ?score .
//!   }
//! }
//! ```
//!
//! | Predicate | Object |
//! |---|---|
//! | `nrv:near` | the query vector: a vector literal, or a variable the rest of the query binds to one (a search per value) |
//! | `nrv:k` | how many nearest vectors (default 10) |
//! | `nrv:score` | a variable: the cosine similarity or dot product, or the Euclidean distance |
//! | `nrv:rank` | a variable: the rank, from 1 |
//! | `nrv:metric` | `"cosine"` (the default), `"dot"` or `"l2"` |
//! | `nrv:exact` | `true`: compare every vector (else large spaces are searched through an HNSW graph) |
//! | `nrv:searchBudget` | the graph search's beam (default 64): wider finds more of the true nearest |
//!
//! The subject is the matched vector literal. Only literals the query's dataset uses as
//! objects are found (so a user sees no vector from a graph it may not read). When the
//! rest of the query binds the subject first (`?doc ex:embedding ?v` above), the search
//! is among those values only: an exact scan of them when they are few, the graph with a
//! filter otherwise. The nearest `k` are returned per query vector, nearest first.

use std::collections::{HashMap, HashSet};

use nrese_engine::vector::Metric;
use nrese_engine::{TermId, VectorQuery, VectorStrategy};
use nrese_exec::{IdTable, UNDEF};
use nrese_rdf::{Literal, Term, Variable};
use nrese_sparql_syntax::algebra::GraphPattern;
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern};

use crate::results::QueryEvaluationError;

use super::{Context, NativeResult, Solutions};

/// The virtual endpoint.
pub(super) const SEARCH: &str = "urn:nrese:vector:search";

/// Nearest vectors per query vector when `nrv:k` isn't given.
const DEFAULT_K: usize = 10;

/// The most `nrv:k` takes.
const MAX_K: usize = 10_000;

/// A search, as its block states it.
#[derive(Debug, Clone)]
struct Search {
    matched: Variable,
    near: Near,
    k: usize,
    score: Option<Variable>,
    rank: Option<Variable>,
    metric: Metric,
    strategy: VectorStrategy,
    ef: Option<usize>,
}

#[derive(Debug, Clone)]
enum Near {
    Vector(Vec<f32>),
    Variable(Variable),
}

fn argument(message: impl Into<String>) -> QueryEvaluationError {
    QueryEvaluationError::Argument(format!("SERVICE nrv:search: {}", message.into()))
}

/// The search `inner` (the block of `SERVICE nrv:search`) states.
fn parse(inner: &GraphPattern) -> Result<Search, QueryEvaluationError> {
    let GraphPattern::Bgp { patterns } = inner else {
        return Err(argument("the block is triple patterns on one variable"));
    };
    let mut matched: Option<Variable> = None;
    let mut search = Search {
        matched: Variable::new_unchecked("unset"),
        near: Near::Vector(Vec::new()),
        k: DEFAULT_K,
        score: None,
        rank: None,
        metric: Metric::default(),
        strategy: VectorStrategy::Auto,
        ef: None,
    };
    let mut near = None;
    for triple in patterns {
        let TermPattern::Variable(subject) = &triple.subject else {
            return Err(argument("the subject is a variable (the vector found)"));
        };
        if matched.as_ref().is_some_and(|m| m != subject) {
            return Err(argument("every pattern has the same subject"));
        }
        matched = Some(subject.clone());
        let NamedNodePattern::NamedNode(predicate) = &triple.predicate else {
            return Err(argument("the predicates are nrv: options"));
        };
        let Some(option) = predicate
            .as_str()
            .strip_prefix(nrese_engine::vector::NAMESPACE)
        else {
            return Err(argument(format!("{predicate} is not an nrv: option")));
        };
        let variable = match &triple.object {
            TermPattern::Variable(v) => Some(v.clone()),
            _ => None,
        };
        let literal = match &triple.object {
            TermPattern::Literal(l) => Some(l),
            _ => None,
        };
        let number = || -> Result<usize, QueryEvaluationError> {
            literal
                .and_then(|l| l.value().parse::<usize>().ok())
                .ok_or_else(|| argument(format!("nrv:{option} takes a whole number")))
        };
        match option {
            "near" => {
                near = Some(match (literal, variable) {
                    (Some(l), _) => Near::Vector(
                        nrese_engine::vector::parse(l.value())
                            .map_err(|error| argument(format!("nrv:near: {error}")))?,
                    ),
                    (None, Some(v)) => Near::Variable(v),
                    _ => return Err(argument("nrv:near takes a vector literal or a variable")),
                });
            }
            "k" => search.k = number()?.clamp(1, MAX_K),
            "searchBudget" => search.ef = Some(number()?.clamp(1, 100 * MAX_K)),
            "score" => {
                search.score = Some(variable.ok_or_else(|| argument("nrv:score takes a variable"))?)
            }
            "rank" => {
                search.rank = Some(variable.ok_or_else(|| argument("nrv:rank takes a variable"))?)
            }
            "metric" => {
                search.metric = literal
                    .and_then(|l| Metric::from_name(l.value()))
                    .ok_or_else(|| argument("nrv:metric is \"cosine\", \"dot\" or \"l2\""))?;
            }
            "exact" => {
                let exact = literal
                    .and_then(|l| match l.value() {
                        "true" | "1" => Some(true),
                        "false" | "0" => Some(false),
                        _ => None,
                    })
                    .ok_or_else(|| argument("nrv:exact takes true or false"))?;
                search.strategy = match exact {
                    true => VectorStrategy::Exact,
                    false => VectorStrategy::Auto,
                };
            }
            other => return Err(argument(format!("unknown option nrv:{other}"))),
        }
    }
    search.matched = matched.ok_or_else(|| argument("the block is empty"))?;
    search.near = near.ok_or_else(|| argument("nrv:near gives the query vector"))?;
    Ok(search)
}

impl Context<'_> {
    /// `SERVICE nrv:search { inner }`, joined to `bound` if given (its values of the
    /// matched variable are the candidates, its values of a `nrv:near` variable the
    /// query vectors).
    pub(super) fn vector_service(
        &self,
        inner: &GraphPattern,
        bound: Option<&Solutions>,
    ) -> NativeResult<Solutions> {
        self.check()?;
        let search = parse(inner)?;
        // The candidates the rest of the query allows, if it binds the matched variable.
        let candidates: Option<HashSet<u64>> = bound
            .and_then(|b| Some((b, b.column(&search.matched)?)))
            .map(|(b, column)| {
                (0..b.table.len())
                    .map(|row| b.table.get(row, column))
                    .filter(|&id| id != UNDEF)
                    .collect()
            });
        // The query vectors, each with the id it is bound to (for a `nrv:near` variable).
        let queries: Vec<(Option<u64>, Vec<f32>)> = match &search.near {
            Near::Vector(vector) => vec![(None, vector.clone())],
            Near::Variable(variable) => {
                let Some((b, column)) = bound.and_then(|b| Some((b, b.column(variable)?))) else {
                    return Err(argument(format!(
                        "nrv:near {variable} has no value: bind it outside the block"
                    ))
                    .into());
                };
                let mut ids: Vec<u64> = (0..b.table.len())
                    .map(|row| b.table.get(row, column))
                    .filter(|&id| id != UNDEF)
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                ids.into_iter()
                    .filter_map(|id| match self.term(id) {
                        Some(Term::Literal(l)) => nrese_engine::vector::parse(l.value())
                            .ok()
                            .map(|vector| (Some(id), vector)),
                        _ => None,
                    })
                    .collect()
            }
        };
        let mut vars = vec![search.matched.clone()];
        vars.extend(search.score.iter().cloned());
        vars.extend(search.rank.iter().cloned());
        let near_column = match &search.near {
            Near::Variable(variable) if *variable != search.matched => {
                vars.push(variable.clone());
                true
            }
            _ => false,
        };
        let mut table = IdTable::new(vars.len());
        let mut used: HashMap<u64, bool> = HashMap::new();
        for (near_id, vector) in queries {
            self.check()?;
            let found = self.nearest_used(&search, vector, candidates.as_ref(), &mut used);
            for (rank, (id, distance)) in found.into_iter().enumerate() {
                let mut row = vec![id.raw()];
                if search.score.is_some() {
                    let score = f64::from(search.metric.score(distance));
                    row.push(self.id(&Term::from(Literal::from(score))));
                }
                if search.rank.is_some() {
                    row.push(self.id(&Term::from(Literal::from(rank as i64 + 1))));
                }
                if near_column {
                    row.push(near_id.unwrap_or(UNDEF));
                }
                table.push_row(&row);
            }
        }
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }

    /// The `search.k` vector literals nearest to `vector` among `candidates` (all if
    /// `None`) that the active graph uses as objects: the index is asked for more until
    /// enough of its hits are used (a literal no statement uses any more stays in the
    /// dictionary). `used` remembers the checks.
    fn nearest_used(
        &self,
        search: &Search,
        vector: Vec<f32>,
        candidates: Option<&HashSet<u64>>,
        used: &mut HashMap<u64, bool>,
    ) -> Vec<(TermId, f32)> {
        let mut query = VectorQuery {
            metric: search.metric,
            strategy: search.strategy,
            accepted: candidates.map(HashSet::len),
            ..VectorQuery::new(vector, search.k)
        };
        if let Some(ef) = search.ef {
            query.ef = ef;
        }
        let accept = |id: TermId| candidates.is_none_or(|set| set.contains(&id.raw()));
        loop {
            let (hits, report) = self.snapshot.vector_search(&query, &accept);
            let fetched = hits.len();
            let kept: Vec<(TermId, f32)> = hits
                .into_iter()
                .filter(|(id, _)| {
                    *used
                        .entry(id.raw())
                        .or_insert_with(|| self.used_as_object(*id))
                })
                .take(search.k)
                .collect();
            // Enough, or nothing more to fetch.
            if kept.len() == search.k || fetched < query.k || query.k >= report.space {
                return kept;
            }
            query.k = (query.k * 4).min(report.space.max(1));
            query.ef = query.ef.max(query.k);
        }
    }
}

/// Whether `name` is the virtual vector search endpoint.
pub(super) fn is_search(name: &NamedNodePattern) -> bool {
    matches!(name, NamedNodePattern::NamedNode(node) if node.as_str() == SEARCH)
}
