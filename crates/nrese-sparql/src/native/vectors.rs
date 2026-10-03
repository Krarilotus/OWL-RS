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

/// The search goes first when the pattern joined to it is estimated at more than this
/// many rows per vector wanted.
const PROBE_ABOVE: usize = 50;

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
        let mut rows = Rows::new(&search);
        let mut used: HashMap<u64, bool> = HashMap::new();
        for (near_id, vector) in queries {
            self.check()?;
            let accept = |id: TermId| {
                candidates
                    .as_ref()
                    .is_none_or(|set| set.contains(&id.raw()))
            };
            let found = self.nearest(
                &search,
                vector,
                candidates.as_ref().map(HashSet::len),
                &accept,
                &mut |id| {
                    Ok(*used
                        .entry(id.raw())
                        .or_insert_with(|| self.used_as_object(id)))
                },
            )?;
            rows.push(self, &search, &found, near_id);
        }
        self.produced(rows.into_solutions())
    }

    /// `Join(local, SERVICE nrv:search { inner })`. Where `local` is estimated to bind
    /// many rows and the query vector is given, the search goes first: its hits, nearest
    /// first, are kept while `local` evaluated from the hit has rows (a probe per hit
    /// through the indexes), until `k` are kept; `local` is then the rows of those
    /// probes. Otherwise `local` goes first and its values of the matched variable are
    /// the candidates ([`Self::vector_service`]).
    pub(super) fn vector_join(
        &self,
        local: &GraphPattern,
        inner: &GraphPattern,
    ) -> NativeResult<Solutions> {
        let search = parse(inner)?;
        let probe_first = match &search.near {
            Near::Vector(_) => self.estimate(local) >= (PROBE_ABOVE * search.k) as f64,
            Near::Variable(_) => false,
        };
        if !probe_first {
            let bound = self.eval(local)?;
            let found = self.vector_service(inner, Some(&bound))?;
            return self.join(bound, found);
        }
        let Near::Vector(vector) = &search.near else {
            unreachable!("checked above")
        };
        self.check()?;
        let mut probed: Option<Solutions> = None;
        let found = self.nearest(&search, vector.clone(), None, &|_| true, &mut |id| {
            if !self.used_as_object(id) {
                return Ok(false);
            }
            let mut table = IdTable::new(1);
            table.push_row(&[id.raw()]);
            let seed = Solutions {
                vars: vec![search.matched.clone()],
                table,
                ordered: false,
            };
            let rows = self.eval_from(seed, local)?;
            if rows.table.is_empty() {
                return Ok(false);
            }
            probed = Some(match probed.take() {
                Some(earlier) => self.union(earlier, rows)?,
                None => rows,
            });
            Ok(true)
        })?;
        let mut rows = Rows::new(&search);
        rows.push(self, &search, &found, None);
        let found = self.produced(rows.into_solutions())?;
        let local = match probed {
            Some(rows) => rows,
            None => {
                let mut vars = vec![search.matched.clone()];
                super::bound_variables(local, &mut vars);
                let width = vars.len();
                Solutions {
                    vars,
                    table: IdTable::new(width),
                    ordered: false,
                }
            }
        };
        self.join(local, found)
    }

    /// The `search.k` vector literals nearest to `vector` that `accept` takes (ids, for
    /// the index; `accepted` of them, if known) and `keep` keeps (checked in order,
    /// nearest first): the index is asked for more until `k` are kept or there are no
    /// more (a literal no statement uses any more stays in the dictionary, a probe may
    /// fail).
    fn nearest(
        &self,
        search: &Search,
        vector: Vec<f32>,
        accepted: Option<usize>,
        accept: &(dyn Fn(TermId) -> bool + Sync),
        keep: &mut dyn FnMut(TermId) -> NativeResult<bool>,
    ) -> NativeResult<Vec<(TermId, f32)>> {
        let mut query = VectorQuery {
            metric: search.metric,
            strategy: search.strategy,
            accepted,
            ..VectorQuery::new(vector, search.k)
        };
        if let Some(ef) = search.ef {
            query.ef = ef;
        }
        let mut seen: HashSet<u64> = HashSet::new();
        let mut kept: Vec<(TermId, f32)> = Vec::new();
        loop {
            self.check()?;
            let (hits, report) = self.snapshot.vector_search(&query, accept);
            let fetched = hits.len();
            for (id, distance) in hits {
                if kept.len() == search.k {
                    break;
                }
                if seen.insert(id.raw()) && keep(id)? {
                    kept.push((id, distance));
                }
            }
            // Enough, or nothing more to fetch.
            if kept.len() == search.k || fetched < query.k || query.k >= report.space {
                kept.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
                return Ok(kept);
            }
            query.k = (query.k * 4).min(report.space.max(1));
            query.ef = query.ef.max(query.k);
        }
    }
}

/// The rows of a search: the matched vector, its score and rank, and the query vector
/// where a variable gives it.
struct Rows {
    vars: Vec<Variable>,
    table: IdTable,
    near_column: bool,
}

impl Rows {
    fn new(search: &Search) -> Self {
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
        let table = IdTable::new(vars.len());
        Self {
            vars,
            table,
            near_column,
        }
    }

    fn push(
        &mut self,
        context: &Context<'_>,
        search: &Search,
        found: &[(TermId, f32)],
        near_id: Option<u64>,
    ) {
        for (rank, &(id, distance)) in found.iter().enumerate() {
            let mut row = vec![id.raw()];
            if search.score.is_some() {
                let score = f64::from(search.metric.score(distance));
                row.push(context.id(&Term::from(Literal::from(score))));
            }
            if search.rank.is_some() {
                row.push(context.id(&Term::from(Literal::from(rank as i64 + 1))));
            }
            if self.near_column {
                row.push(near_id.unwrap_or(UNDEF));
            }
            self.table.push_row(&row);
        }
    }

    fn into_solutions(self) -> Solutions {
        Solutions {
            vars: self.vars,
            table: self.table,
            ordered: false,
        }
    }
}

/// Whether `name` is the virtual vector search endpoint.
pub(super) fn is_search(name: &NamedNodePattern) -> bool {
    matches!(name, NamedNodePattern::NamedNode(node) if node.as_str() == SEARCH)
}
