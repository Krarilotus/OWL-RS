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
use nrese_engine::{TermId, VectorQuery, VectorSearchReport, VectorStrategy};
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
                |query| self.snapshot.vector_search(query, &accept),
                |id| {
                    Ok((*used
                        .entry(id.raw())
                        .or_insert_with(|| self.used_as_object(id)))
                    .then_some(()))
                },
                |_| {},
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
        let found = self.nearest(
            &search,
            vector.clone(),
            None,
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
                Ok(Some(rows))
            },
            |rows| self.consumed(&rows),
        )?;
        let mut rows = Rows::new(&search);
        rows.push(
            self,
            &search,
            found.iter().map(|(id, d, _)| (*id, *d)),
            None,
        );
        let mut winners = found.into_iter();
        let combined: NativeResult<Option<Solutions>> = (|| {
            let mut probed = None;
            for (_, _, rows) in winners.by_ref() {
                probed = Some(match probed.take() {
                    Some(earlier) => self.union(earlier, rows)?,
                    None => rows,
                });
            }
            Ok(probed)
        })();
        // Union consumes the handed-off inputs even on failure; only pending winners
        // are still ours to release.
        for (_, _, rows) in winners {
            self.consumed(&rows);
        }
        let probed = combined?;
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
    /// O(fetched + k) per round, probing each ID at most once. Keep at most k payloads
    /// between probes, plus one incoming probe; immediately dispose displaced rows.
    fn nearest<T>(
        &self,
        search: &Search,
        vector: Vec<f32>,
        accepted: Option<usize>,
        mut fetch: impl FnMut(&VectorQuery) -> (Vec<(TermId, f32)>, VectorSearchReport),
        mut keep: impl FnMut(TermId) -> NativeResult<Option<T>>,
        mut discard: impl FnMut(T),
    ) -> NativeResult<Vec<(TermId, f32, T)>> {
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
        let mut kept: Vec<(TermId, f32, T)> = Vec::new();
        let mut next = Vec::new();
        let result: NativeResult<()> = (|| {
            loop {
                self.check()?;
                let (hits, report) = fetch(&query);
                let fetched = hits.len();
                let mut hits = hits.into_iter().peekable();
                let mut pending = kept.drain(..);
                let round = (|| {
                    while next.len() < search.k {
                        self.check()?;
                        let old_first = pending.as_slice().first().is_some_and(|old| {
                            hits.peek().is_none_or(|hit| {
                                old.1.total_cmp(&hit.1).then(old.0.cmp(&hit.0)).is_le()
                            })
                        });
                        if old_first {
                            next.push(pending.next().expect("pending candidate"));
                        } else if let Some((id, distance)) = hits.next() {
                            if seen.insert(id.raw())
                                && let Some(rows) = keep(id)?
                            {
                                next.push((id, distance, rows));
                                // These old tail candidates cannot enter the best k even
                                // if all remaining probes fail. Free them before probing.
                                while pending.len() + next.len() > search.k {
                                    discard(pending.next_back().expect("displaced candidate").2);
                                }
                            }
                        } else {
                            break;
                        }
                    }
                    self.check()
                })();
                // Drain explicitly: dropping T alone need not release its reservation.
                for (_, _, rows) in pending {
                    discard(rows);
                }
                round?;
                std::mem::swap(&mut kept, &mut next);
                // Enough, or nothing more to fetch.
                if kept.len() == search.k || fetched < query.k || query.k >= report.space {
                    return Ok(());
                }
                query.k = (query.k * 4).min(report.space.max(1));
                query.ef = query.ef.max(query.k);
            }
        })();
        if let Err(error) = result {
            for (_, _, rows) in kept.drain(..).chain(next.drain(..)) {
                discard(rows);
            }
            return Err(error);
        }
        Ok(kept)
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
            shared_memory: Some(shared.clone()),
            ..Default::default()
        };
        let ctx = Context::new(&snapshot, &options, None, None);
        let unrelated = payload(&ctx, 99);
        let bytes = unrelated.table.memory_bytes();
        let probes = RefCell::new(Vec::new());
        let disposed = RefCell::new(Vec::new());
        let buffers = RefCell::new(HashMap::new());
        let rounds = Cell::new(0);
        let found = ctx
            .nearest(
                &search(3),
                vec![1.0],
                None,
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
                        assert_eq!(*disposed.borrow(), [9]);
                    }
                    assert!(matches!(id, 1 | 2 | 3 | 8 | 9), "dominated suffix probed");
                    let rows = payload(&ctx, id);
                    buffers
                        .borrow_mut()
                        .insert(id, rows.table.column(0).as_ptr());
                    Ok(Some(rows))
                },
                |rows| {
                    disposed.borrow_mut().push(rows.table.get(0, 0));
                    ctx.consumed(&rows);
                },
            )
            .map_err(QueryEvaluationError::from)
            .unwrap();
        assert_eq!(rounds.get(), 2);
        assert_eq!(*probes.borrow(), [0, 8, 9, 1, 2, 3]);
        assert_eq!(*disposed.borrow(), [9, 8]);
        assert_eq!(
            found.iter().map(|(id, _, _)| id.raw()).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(shared.used(), bytes * 4);
        assert_eq!(
            shared.peak(),
            bytes * 5,
            "one incoming probe may overlap the k retained bags"
        );
        for (id, _, rows) in found {
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
                    vec![1.0],
                    Some(100),
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
                    |_| {},
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
                    vec![1.0],
                    None,
                    |_| {
                        assert!(!fetched);
                        fetched = true;
                        (hits(&values), report(space))
                    },
                    |id| Ok((id.raw() != 0).then_some(())),
                    |_| {},
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
                cancellation: Some(token.clone()),
                shared_memory: Some(shared.clone()),
                ..Default::default()
            };
            let ctx = Context::new(&snapshot, &options, None, None);
            let unrelated = payload(&ctx, 99);
            let mut round = 0;
            let result = ctx.nearest(
                &search(3),
                vec![1.0],
                None,
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
                    Ok(Some(rows))
                },
                |rows| ctx.consumed(&rows),
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
        use nrese_sparql_syntax::{Query, algebra::Expression};
        fn pattern(text: &str) -> GraphPattern {
            let Query::Select { pattern, .. } = nrese_sparql_syntax::SparqlParser::new()
                .parse_query(&format!("SELECT * WHERE {{ {text} }}"))
                .unwrap()
            else {
                panic!()
            };
            let GraphPattern::Project { inner, .. } = pattern else {
                panic!()
            };
            *inner
        }
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
            memory_limit: Some(UNRELATED + 8192 - 1),
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
