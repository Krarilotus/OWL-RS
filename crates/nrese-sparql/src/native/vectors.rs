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
//! | `nrv:k` | how many nearest vectors (positive integer, default 10; no fixed maximum) |
//! | `nrv:score` | a variable: the cosine similarity or dot product, or the Euclidean distance |
//! | `nrv:rank` | a variable: the rank, from 1 |
//! | `nrv:metric` | `"cosine"` (the default), `"dot"` or `"l2"` |
//! | `nrv:exact` | `true`: compare every vector (else large spaces are searched through an HNSW graph) |
//! | `nrv:searchBudget` | positive base beam (default 64; no fixed maximum), widened for k and selectivity; zero is an error |
//!
//! The subject is the matched vector literal. Only literals the query's dataset uses as
//! objects are found (so a user sees no vector from a graph it may not read). When the
//! rest of the query binds the subject first (`?doc ex:embedding ?v` above), the search
//! is among those values only: an exact scan of them when they are few, the graph with a
//! filter otherwise. The nearest `k` are returned per query vector, nearest first.

use std::collections::{HashMap, HashSet};

use nrese_engine::vector::Metric;
use nrese_engine::{TermId, VectorQuery, VectorSearchReport, VectorStrategy};
use nrese_exec::{Budget, IdTable, Reservation, UNDEF};
use nrese_rdf::{Literal, Term, Variable};
use nrese_sparql_syntax::algebra::GraphPattern;
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern};

use crate::results::QueryEvaluationError;

use super::{Context, NativeResult, Solutions};

/// The virtual endpoint.
pub(super) const SEARCH: &str = "urn:nrese:vector:search";

/// Nearest vectors per query vector when `nrv:k` isn't given.
const DEFAULT_K: usize = 10;

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

impl Search {
    fn query(&self, vector: Vec<f32>, accepted: Option<usize>) -> VectorQuery {
        let mut query = VectorQuery {
            metric: self.metric,
            strategy: self.strategy,
            accepted,
            ..VectorQuery::new(vector, self.k)
        };
        if let Some(ef) = self.ef {
            query.ef = ef;
        }
        query
    }
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
            let value = literal
                .and_then(|l| l.value().parse::<usize>().ok())
                .ok_or_else(|| {
                    argument(format!(
                        "nrv:{option} takes a whole number supported by this platform"
                    ))
                })?;
            if value == 0 {
                return Err(argument(format!(
                    "nrv:{option} takes a positive whole number"
                )));
            }
            Ok(value)
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
            "k" => search.k = number()?,
            "searchBudget" => search.ef = Some(number()?),
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
            .and_then(|(b, column)| {
                (0..b.table.len())
                    .map(|row| {
                        let id = b.table.get(row, column);
                        // An unbound row can join any vector; its alternatives must
                        // not turn an unrestricted search into a finite candidate set.
                        (id != UNDEF).then_some(id)
                    })
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
                search.query(vector, candidates.as_ref().map(HashSet::len)),
                |query| self.snapshot.vector_search(query, &accept),
                |id| {
                    Ok((*used
                        .entry(id.raw())
                        .or_insert_with(|| self.used_as_object(id)))
                    .then_some(()))
                },
            )?;
            rows.push(
                self,
                &search,
                found.into_iter().map(|(id, d, ())| (id, d)),
                near_id,
            );
        }
        self.produced(rows.into_solutions())
    }

    /// Choose the join direction before searching. Selective exact routes bind first;
    /// broad ones retain search-first rather than materializing many RDF rows to keep
    /// k vectors. Row estimates are hints: bound IDs are deduplicated before the
    /// index chooses again. Dataset visibility is still checked on the hits.
    pub(super) fn vector_join(
        &self,
        local: &GraphPattern,
        inner: &GraphPattern,
    ) -> NativeResult<Solutions> {
        let search = parse(inner)?;
        let estimate = self.estimate(local);
        self.check()?;
        // Price the prospective candidate-filtered route. Probe execution below
        // instead has no index filter, and records expected final survivors separately.
        let probe = match &search.near {
            Near::Vector(vector) if estimate >= PROBE_ABOVE as f64 * search.k as f64 => {
                let mut query = search.query(vector.clone(), Some(estimate.ceil() as usize));
                let route = self.snapshot.vector_estimate(&query);
                if route.strategy == VectorStrategy::Approximate || estimate >= route.space as f64 {
                    // Candidate enumeration predicts no distance savings in a broad
                    // exact search. Keep the chosen kernel stable across probe rounds.
                    query.strategy = route.strategy;
                    query.probe_accepted = query.accepted.take();
                    Some(query)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(query) = probe {
            return self.vector_probe_join(local, &search, query);
        }
        let bound = self.eval(local)?;
        let found = self.vector_service(inner, Some(&bound))?;
        self.join(bound, found)
    }

    /// Search then probe: the index sees no local filter. A graph's beam must still
    /// reflect the expected survival of the later probes.
    fn vector_probe_join(
        &self,
        local: &GraphPattern,
        search: &Search,
        query: VectorQuery,
    ) -> NativeResult<Solutions> {
        self.check()?;
        // The hits are probed against `local` after the search, so the index sees no
        // filter; it is told how many vectors `local` is estimated to accept, so that its
        // beam is widened by the inverse share as for a filter it applies itself. Without
        // it the first `k` hits that pass come from an approximate list whose tail misses
        // them (recall 0.50 against 0.98, one kind in ten of 200,000 vectors).
        let found = self.nearest(
            search,
            query,
            |query| self.snapshot.vector_search(query, &|_| true),
            |id| {
                if !self.used_as_object(id) {
                    return Ok(None);
                }
                let mut table = IdTable::new(1);
                table.push_row(&[id.raw()]);
                let seed = self.produced(Solutions {
                    vars: vec![search.matched.clone()],
                    table,
                    ordered: false,
                })?;
                let rows = self.eval_from(seed, local)?;
                if rows.table.is_empty() {
                    self.consumed(&rows);
                    return Ok(None);
                }
                Ok(Some(Probed::new(self, rows)))
            },
        )?;
        let mut rows = Rows::new(search);
        rows.push(self, search, found.iter().map(|(id, d, _)| (*id, *d)), None);
        // Union consumes its inputs, also when it fails; the winners not yet handed to it
        // are released as they drop.
        let mut probed = None;
        for (_, _, winner) in found {
            let rows = winner.take();
            probed = Some(match probed.take() {
                Some(earlier) => self.union(earlier, rows)?,
                None => rows,
            });
        }
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
        let found = match self.produced(rows.into_solutions()) {
            Ok(found) => found,
            Err(error) => {
                self.consumed(&local);
                return Err(error);
            }
        };
        self.join(local, found)
    }

    /// Merge sorted search rounds with retained, successfully probed candidates.
    /// O(fetched + k) per round, probing each ID at most once. Keeps at most k payloads
    /// between probes, plus one incoming probe; a displaced payload drops at once.
    fn nearest<T>(
        &self,
        search: &Search,
        mut query: VectorQuery,
        mut fetch: impl FnMut(&VectorQuery) -> (Vec<(TermId, f32)>, VectorSearchReport),
        mut keep: impl FnMut(TermId) -> NativeResult<Option<T>>,
    ) -> NativeResult<Vec<(TermId, f32, T)>> {
        let mut seen: HashSet<u64> = HashSet::new();
        let mut kept: Vec<(TermId, f32, T)> = Vec::new();
        let mut next = Vec::new();
        loop {
            self.check()?;
            let (hits, report) = fetch(&query);
            let fetched = hits.len();
            let mut hits = hits.into_iter().peekable();
            let mut pending = kept.drain(..);
            while next.len() < search.k {
                self.check()?;
                let old_first = pending.as_slice().first().is_some_and(|old| {
                    hits.peek()
                        .is_none_or(|hit| old.1.total_cmp(&hit.1).then(old.0.cmp(&hit.0)).is_le())
                });
                if old_first {
                    next.push(pending.next().expect("pending candidate"));
                } else if let Some((id, distance)) = hits.next() {
                    if seen.insert(id.raw())
                        && let Some(rows) = keep(id)?
                    {
                        next.push((id, distance, rows));
                        // These old tail candidates cannot enter the best k even if all
                        // remaining probes fail. Free them before probing.
                        while pending.len() + next.len() > search.k {
                            drop(pending.next_back());
                        }
                    }
                } else {
                    break;
                }
            }
            drop(pending);
            self.check()?;
            std::mem::swap(&mut kept, &mut next);
            // Enough, or nothing more to fetch.
            if kept.len() == search.k || fetched < query.k || query.k >= report.space {
                return Ok(kept);
            }
            query.k = query.k.saturating_mul(4).min(report.space.max(1));
            query.ef = query.ef.max(query.k);
        }
    }
}

/// Probe rows charged to the query's budget: released if they drop, handed on by
/// [`Probed::take`].
struct Probed<'b> {
    rows: Solutions,
    held: Reservation<&'b Budget>,
}

impl<'b> Probed<'b> {
    fn new(ctx: &'b Context<'_>, rows: Solutions) -> Self {
        let bytes = rows.table.memory_bytes();
        Self {
            rows,
            held: Reservation::adopt(&*ctx.budget, bytes),
        }
    }

    /// The rows, whose new holder releases them.
    fn take(self) -> Solutions {
        self.held.transfer();
        self.rows
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
        found: impl IntoIterator<Item = (TermId, f32)>,
        near_id: Option<u64>,
    ) {
        for (rank, (id, distance)) in found.into_iter().enumerate() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, QueryOptions};
    use nrese_engine::Engine;
    use nrese_exec::SharedBudget;
    use std::cell::{Cell, RefCell};

    fn pattern(text: &str) -> GraphPattern {
        let nrese_sparql_syntax::Query::Select { pattern, .. } =
            nrese_sparql_syntax::SparqlParser::new()
                .parse_query(&format!("SELECT * WHERE {{ {text} }}"))
                .unwrap()
        else {
            panic!("select query")
        };
        let GraphPattern::Project { inner, .. } = pattern else {
            panic!("project")
        };
        *inner
    }

    #[test]
    fn broad_exact_avoids_full_binding_and_preserves_duplicate_bags() {
        use nrese_rdf::{GraphName, NamedNode, Quad};
        let engine = Engine::new(Default::default()).unwrap();
        let mut tx = engine.transaction();
        for i in 0..1_000 {
            for (predicate, value) in [("all", i), ("duplicates", i % 20)] {
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked(format!("urn:item:{i}")),
                        NamedNode::new_unchecked(format!("urn:{predicate}")),
                        Literal::new_typed_literal(
                            format!("[{value}]"),
                            NamedNode::new_unchecked(nrese_engine::vector::DATATYPE),
                        ),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                );
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let inner = pattern(
            "?v <urn:nrese:vector:near> \"[0]\" ; <urn:nrese:vector:k> 10 ; <urn:nrese:vector:metric> \"l2\" ; <urn:nrese:vector:exact> true",
        );
        let bag = |rows: &Solutions| {
            let item = rows.column(&Variable::new_unchecked("item")).unwrap();
            let vector = rows.column(&Variable::new_unchecked("v")).unwrap();
            let mut values: Vec<_> = (0..rows.table.len())
                .map(|r| (rows.table.get(r, item), rows.table.get(r, vector)))
                .collect();
            values.sort_unstable();
            values
        };
        for (predicate, count) in [("all", 10), ("duplicates", 500)] {
            let local = pattern(&format!("?item <urn:{predicate}> ?v"));
            let ctx = Context::new(&snapshot, &QueryOptions::default(), None, None);
            let bound = ctx
                .eval(&local)
                .map_err(QueryEvaluationError::from)
                .unwrap();
            let hits = ctx
                .vector_service(&inner, Some(&bound))
                .map_err(QueryEvaluationError::from)
                .unwrap();
            let truth = ctx
                .join(bound, hits)
                .map_err(QueryEvaluationError::from)
                .unwrap();
            let found = ctx
                .vector_join(&local, &inner)
                .map_err(QueryEvaluationError::from)
                .unwrap();
            assert_eq!(found.table.len(), count);
            assert_eq!(bag(&found), bag(&truth));
            ctx.consumed(&truth);
            ctx.consumed(&found);
            if predicate == "all" {
                let options = QueryOptions {
                    resources: crate::Resources {
                        memory_limit: Some(8_192),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let ctx = Context::new(&snapshot, &options, None, None);
                let found = ctx
                    .vector_join(&local, &inner)
                    .map_err(QueryEvaluationError::from)
                    .unwrap();
                assert_eq!(found.table.len(), 10, "full binding would exceed 8 KiB");
                ctx.consumed(&found);
                assert_eq!(ctx.budget.used(), 0);
            }
        }
    }

    #[test]
    fn graph_probe_join_keeps_its_recall_when_auto_would_choose_exact() {
        use nrese_rdf::{GraphName, NamedNode, Quad};
        let engine = Engine::new(Default::default()).unwrap();
        let mut seed = 7u64;
        let mut vector = || {
            (0..64)
                .map(|_| {
                    seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
                    let mut z = seed;
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                    z ^= z >> 31;
                    ((z >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
                })
                .collect::<Vec<_>>()
        };
        let mut tx = engine.transaction();
        for i in 0..24_000 {
            let item = NamedNode::new_unchecked(format!("urn:item:{i}"));
            tx.insert(
                Quad::new(
                    item.clone(),
                    NamedNode::new_unchecked("urn:embedding"),
                    Literal::new_typed_literal(
                        nrese_engine::vector::lexical(&vector()),
                        NamedNode::new_unchecked(nrese_engine::vector::DATATYPE),
                    ),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
            tx.insert(
                Quad::new(
                    item,
                    NamedNode::new_unchecked("urn:kind"),
                    Literal::from(i % 10),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let ctx = Context::new(&snapshot, &QueryOptions::default(), None, None);
        let local = pattern("?item <urn:embedding> ?v ; <urn:kind> 3");
        let estimate = ctx.estimate(&local);
        let mut shared = 0;
        for _ in 0..20 {
            let inner = pattern(&format!(
                "?v <urn:nrese:vector:near> \"{}\" ; <urn:nrese:vector:k> 10 ; <urn:nrese:vector:exact> true",
                nrese_engine::vector::lexical(&vector())
            ));
            let mut search = parse(&inner).unwrap();
            let truth = ctx
                .vector_join(&local, &inner)
                .map_err(QueryEvaluationError::from)
                .unwrap();
            search.strategy = VectorStrategy::Approximate;
            let Near::Vector(near) = &search.near else {
                unreachable!()
            };
            let mut query = search.query(near.clone(), None);
            snapshot.prepare_vector_graph(&query);
            assert!(snapshot.vector_search(&query, &|_| true).1.graph);
            query.probe_accepted = Some(estimate.ceil() as usize);
            let found = ctx
                .vector_probe_join(&local, &search, query)
                .map_err(QueryEvaluationError::from)
                .unwrap();
            let item = Variable::new_unchecked("item");
            let ids = |rows: &Solutions| {
                let column = rows.column(&item).unwrap();
                (0..rows.table.len())
                    .map(|row| rows.table.get(row, column))
                    .collect::<HashSet<_>>()
            };
            assert_eq!((truth.table.len(), found.table.len()), (10, 10));
            shared += ids(&truth).intersection(&ids(&found)).count();
            ctx.consumed(&truth);
            ctx.consumed(&found);
        }
        assert!(shared >= 190, "forced ANN recall@10: {shared}/200");
    }

    #[test]
    fn search_options_are_honored_or_rejected_never_clamped() {
        use nrese_rdf::NamedNode;
        use nrese_sparql_syntax::term::TriplePattern;

        let block = |k: &str, beam: &str| GraphPattern::Bgp {
            patterns: [("near", "[1]"), ("k", k), ("searchBudget", beam)]
                .into_iter()
                .map(|(name, value)| TriplePattern {
                    subject: TermPattern::Variable(Variable::new_unchecked("v")),
                    predicate: NamedNodePattern::NamedNode(NamedNode::new_unchecked(format!(
                        "urn:nrese:vector:{name}"
                    ))),
                    object: TermPattern::Literal(Literal::new_simple_literal(value)),
                })
                .collect(),
        };
        for (k, beam) in [(1, 1), (10_001, 1_000_001), (usize::MAX, usize::MAX)] {
            let parsed = parse(&block(&k.to_string(), &beam.to_string())).unwrap();
            assert_eq!(parsed.k, k);
            assert_eq!(parsed.ef, Some(beam));
        }
        for (k, beam, option) in [
            ("0", "64", "nrv:k"),
            ("10", "0", "nrv:searchBudget"),
            ("-1", "64", "nrv:k"),
            ("10", "18446744073709551616", "nrv:searchBudget"),
        ] {
            let error = parse(&block(k, beam)).unwrap_err().to_string();
            assert!(
                error.contains(option) && error.contains("whole number"),
                "{error}"
            );
        }
    }

    fn search(k: usize) -> Search {
        Search {
            matched: Variable::new_unchecked("v"),
            near: Near::Vector(vec![1.0]),
            k,
            score: None,
            rank: None,
            metric: Metric::L2,
            strategy: VectorStrategy::Auto,
            ef: None,
        }
    }

    fn hits(values: &[(u64, f32)]) -> Vec<(TermId, f32)> {
        values
            .iter()
            .map(|&(id, distance)| (TermId::from_raw(id), distance))
            .collect()
    }

    fn report(space: usize) -> VectorSearchReport {
        VectorSearchReport {
            space,
            graph: true,
            scanned: 0,
        }
    }

    fn payload(ctx: &Context<'_>, id: u64) -> Solutions {
        ctx.produced(Solutions {
            vars: vec![Variable::new_unchecked("v")],
            table: IdTable::from_rows(1, [&[id][..]]),
            ordered: false,
        })
        .map_err(QueryEvaluationError::from)
        .unwrap()
    }

    #[test]
    fn widening_replaces_worse_candidates_and_releases_their_rows_before_more_probes() {
        let engine = Engine::new(Default::default()).unwrap();
        let snapshot = engine.snapshot();
        let shared = SharedBudget::new(1 << 20);
        let options = QueryOptions {
            resources: crate::Resources {
                memory: nrese_exec::Envelope::new(None, Some(shared.clone())),
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let unrelated = payload(&ctx, 99);
        let bytes = unrelated.table.memory_bytes();
        let probes = RefCell::new(Vec::new());
        let buffers = RefCell::new(HashMap::new());
        let rounds = Cell::new(0);
        let found = ctx
            .nearest(
                &search(3),
                search(3).query(vec![1.0], None),
                |query| {
                    let round = rounds.get();
                    rounds.set(round + 1);
                    assert_eq!((query.k, query.ef), (if round == 0 { 3 } else { 12 }, 64));
                    let values = match round {
                        0 => vec![(0, 0.01), (8, 0.8), (9, 0.9)],
                        1 => vec![
                            (0, 0.01),
                            (1, 0.1),
                            (2, 0.2),
                            (3, 0.3),
                            (8, 0.8),
                            (9, 0.9),
                            (10, 1.0),
                            (11, 1.1),
                        ],
                        _ => panic!("unnecessary search"),
                    };
                    (hits(&values), report(100))
                },
                |id| {
                    let id = id.raw();
                    probes.borrow_mut().push(id);
                    assert!(
                        shared.used() <= bytes * 4,
                        "at most k retained payloads plus unrelated"
                    );
                    if id == 0 {
                        return Ok(None);
                    }
                    if id == 3 {
                        assert_eq!(shared.used(), bytes * 4, "9 is released before 3 is probed");
                    }
                    assert!(matches!(id, 1 | 2 | 3 | 8 | 9), "dominated suffix probed");
                    let rows = payload(&ctx, id);
                    buffers
                        .borrow_mut()
                        .insert(id, rows.table.column(0).as_ptr());
                    Ok(Some(Probed::new(&ctx, rows)))
                },
            )
            .map_err(QueryEvaluationError::from)
            .unwrap();
        assert_eq!(rounds.get(), 2);
        assert_eq!(*probes.borrow(), [0, 8, 9, 1, 2, 3]);
        assert_eq!(
            found.iter().map(|(id, _, _)| id.raw()).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(shared.used(), bytes * 4, "8 is released");
        assert_eq!(
            shared.peak(),
            bytes * 5,
            "one incoming probe may overlap the k retained bags"
        );
        for (id, _, rows) in found {
            let rows = rows.take();
            assert_eq!(
                rows.table.column(0).as_ptr(),
                buffers.borrow()[&id.raw()],
                "rows move without copying"
            );
            ctx.consumed(&rows);
        }
        assert_eq!(
            shared.used(),
            bytes,
            "unrelated reservation survives disposal"
        );
        ctx.consumed(&unrelated);
        assert_eq!(shared.used(), 0);
    }

    #[test]
    fn rounds_merge_by_total_distance_and_id_without_reprobing_accepted_or_rejected_hits() {
        let engine = Engine::new(Default::default()).unwrap();
        let snapshot = engine.snapshot();
        let ctx = Context::new(&snapshot, &QueryOptions::default(), None, None);
        for (old, second, expected) in [
            (
                vec![(0, -1.0), (8, 0.05), (9, 0.9)],
                vec![(0, -1.0), (1, 0.1), (2, 0.2)],
                vec![8, 1, 2],
            ),
            (
                vec![(0, -1.0), (8, 0.05), (9, 0.9)],
                vec![(0, -1.0), (8, 0.05), (1, 0.1), (2, 0.2)],
                vec![8, 1, 2],
            ),
            (
                vec![(0, -1.0), (8, 0.1), (9, 0.1)],
                vec![(0, -1.0), (1, 0.1), (2, 0.1), (3, 0.1)],
                vec![1, 2, 3],
            ),
            (
                vec![(0, -1.0), (8, 0.0), (9, 0.0)],
                vec![(0, -1.0), (3, -0.0), (1, 0.0), (2, 0.0)],
                vec![3, 1, 2],
            ),
        ] {
            let mut rounds = [old, second].into_iter();
            let mut seen = HashSet::new();
            let found = ctx
                .nearest(
                    &search(3),
                    search(3).query(vec![1.0], Some(100)),
                    |query| {
                        assert_eq!(query.accepted, Some(100));
                        (
                            hits(&rounds.next().expect("two rounds suffice")),
                            report(100),
                        )
                    },
                    |id| {
                        assert!(seen.insert(id.raw()), "each ID probed once");
                        Ok((id.raw() != 0).then_some(()))
                    },
                )
                .map_err(QueryEvaluationError::from)
                .unwrap();
            assert_eq!(
                found
                    .into_iter()
                    .map(|(id, _, ())| id.raw())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        // Empty/short output and exhaustion of the space keep all accepted hits.
        for (k, values, space) in [
            (1, vec![], 10),
            (3, vec![(1, 0.1)], 10),
            (3, vec![(0, 0.0), (1, 0.1), (2, 0.2)], 3),
        ] {
            let mut fetched = false;
            let found = ctx
                .nearest(
                    &search(k),
                    search(k).query(vec![1.0], None),
                    |_| {
                        assert!(!fetched);
                        fetched = true;
                        (hits(&values), report(space))
                    },
                    |id| Ok((id.raw() != 0).then_some(())),
                )
                .map_err(QueryEvaluationError::from)
                .unwrap();
            assert_eq!(
                found.len(),
                values.iter().filter(|(id, _)| *id != 0).count()
            );
        }
    }

    #[test]
    fn errors_and_cancellation_release_pending_and_emitted_payloads() {
        let engine = Engine::new(Default::default()).unwrap();
        let snapshot = engine.snapshot();
        for stop in ["probe_error", "fetch_cancel", "probe_cancel"] {
            let token = CancellationToken::new();
            let shared = SharedBudget::new(1 << 20);
            let options = QueryOptions {
                resources: crate::Resources {
                    cancellation: Some(token.clone()),
                    memory: nrese_exec::Envelope::new(None, Some(shared.clone())),
                    ..Default::default()
                },
                ..Default::default()
            };
            let ctx = Context::new(&snapshot, &options, None, None);
            let unrelated = payload(&ctx, 99);
            let mut round = 0;
            let result = ctx.nearest(
                &search(3),
                search(3).query(vec![1.0], None),
                |_| {
                    round += 1;
                    if round == 2 && stop == "fetch_cancel" {
                        token.cancel();
                    }
                    (
                        if round == 1 {
                            hits(&[(0, 0.0), (8, 0.8), (9, 0.9)])
                        } else {
                            hits(&[(0, 0.0), (1, 0.1), (2, 0.2), (3, 0.3)])
                        },
                        report(100),
                    )
                },
                |id| {
                    if id.raw() == 0 {
                        return Ok(None);
                    }
                    if id.raw() == 2 && stop == "probe_error" {
                        return Err(argument("probe failed").into());
                    }
                    let rows = payload(&ctx, id.raw());
                    if id.raw() == 2 && stop == "probe_cancel" {
                        token.cancel();
                    }
                    Ok(Some(Probed::new(&ctx, rows)))
                },
            );
            assert!(result.is_err(), "{stop}");
            assert_eq!(
                shared.used(),
                unrelated.table.memory_bytes(),
                "{stop}: no stranded candidate charges"
            );
            ctx.consumed(&unrelated);
            assert_eq!(shared.used(), 0);
        }
    }

    #[test]
    fn rejected_probe_capacity_and_unhanded_winners_release_without_ending_the_query() {
        use nrese_rdf::{GraphName, NamedNode, Quad};
        use nrese_sparql_syntax::algebra::Expression;
        let engine = Engine::new(Default::default()).unwrap();
        let mut tx = engine.transaction();
        for vector in 1..=6 {
            for item in 0..64 {
                tx.insert(
                    Quad::new(
                        NamedNode::new_unchecked(format!("urn:item:{vector}:{item}")),
                        NamedNode::new_unchecked("urn:embedding"),
                        Literal::new_typed_literal(
                            format!("[{vector}]"),
                            NamedNode::new_unchecked(nrese_engine::vector::DATATYPE),
                        ),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                );
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let local = pattern("?item <urn:embedding> ?v");
        let inner = pattern(
            "?v <urn:nrese:vector:near> \"[0]\" ; <urn:nrese:vector:k> 6 ; <urn:nrese:vector:metric> \"l2\" ; <urn:nrese:vector:exact> true",
        );
        const UNRELATED: usize = 128;
        let options = QueryOptions {
            resources: crate::Resources {
                memory: nrese_exec::Envelope::new(Some(UNRELATED + 8192 - 1), None),
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        ctx.budget.charge(UNRELATED).unwrap();
        assert_eq!(ctx.estimate(&local), 384.0, "probe-first at k=6");
        let error = match ctx
            .vector_join(&local, &inner)
            .map_err(QueryEvaluationError::from)
        {
            Err(QueryEvaluationError::MemoryLimit(error)) => error,
            _ => panic!("fourth winner's union must exceed the remaining budget"),
        };
        assert_eq!((error.used, error.requested), (UNRELATED + 2048, 6144));
        assert_eq!(
            ctx.budget.used(),
            UNRELATED,
            "pending winners release; handed-off union inputs release only once"
        );

        let ctx = Context::new(&snapshot, &QueryOptions::default(), None, None);
        ctx.budget.charge(UNRELATED).unwrap();
        let rejected = GraphPattern::Filter {
            expr: Expression::Literal(Literal::from(false)),
            inner: Box::new(local),
        };
        let query = VectorQuery {
            strategy: VectorStrategy::Exact,
            ..VectorQuery::new(vec![0.0], 1)
        };
        let id = snapshot.vector_search(&query, &|_| true).0[0].0.raw();
        let empty = ctx
            .eval_from(payload(&ctx, id), &rejected)
            .map_err(QueryEvaluationError::from)
            .unwrap();
        assert!(empty.table.is_empty());
        assert!(
            empty.table.memory_bytes() > 0,
            "rejection can retain table capacity"
        );
        ctx.consumed(&empty);
        assert_eq!(ctx.budget.used(), UNRELATED);
        let empty = ctx
            .vector_join(&rejected, &inner)
            .map_err(QueryEvaluationError::from)
            .unwrap();
        assert!(empty.table.is_empty());
        ctx.consumed(&empty);
        assert_eq!(
            ctx.budget.used(),
            UNRELATED,
            "every empty probe returns its capacity charge"
        );
    }
}
