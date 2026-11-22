//! Routes by which full validation can report a candidate at its source shape and focus.
//! At most [`MAX_AFFECTED`] pair visits per change call (the route limit the call is
//! given), with one verdict per candidate
//! pair. After the bound, compare with the full before results of its reporting roots.

use std::collections::BTreeSet;

use nrese_engine::TermId;
use nrese_exec::{Budget, ChargedMap, ChargedVec, Reservation};
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};

use crate::graph::GraphView;
use crate::model::{Constraint, Nesting, Path, ShapeRef, Shapes};
use crate::path;
use crate::validate::{RawResult, TaskResults, Validator};

use super::{CHECK_EVERY, MAX_AFFECTED};

type Focus = (ShapeRef, TermId);

pub(super) struct BeforeRoutes<'a> {
    shapes: &'a Shapes,
    budget: &'a Budget,
    resources: &'a Resources,
    // Sorted by child. Each edge is the reverse of a result-emitting sh:property call.
    parents: ChargedVec<(ShapeRef, ShapeRef), &'a Budget>,
    visits_left: usize,
    memo: Option<ChargedMap<Focus, Verdict, Reservation<&'a Budget>>>,
    whole: BTreeSet<RawResult>,
    whole_seen: ChargedMap<ShapeRef, (), Reservation<&'a Budget>>,
}

#[derive(Default)]
struct Verdict {
    target: Option<Focus>,
    results: Option<TaskResults>,
}

enum Route {
    Found(Focus),
    Missing,
    Limit,
}

impl<'a> BeforeRoutes<'a> {
    pub(super) fn new(
        shapes: &'a Shapes,
        budget: &'a Budget,
        resources: &'a Resources,
        route_limit: usize,
    ) -> Result<Self, QueryEvaluationError> {
        let mut parents = ChargedVec::new(budget);
        for (parent, compiled) in shapes.shapes.iter().enumerate() {
            resources.check()?;
            if compiled.deactivated {
                continue;
            }
            for constraint in &compiled.constraints {
                if let Constraint::Nested(nested) = constraint
                    && matches!(nested.kind, Nesting::Property)
                {
                    let child = nested.reference(0).expect("a property shape");
                    parents.push((child, parent))?;
                }
            }
        }
        parents.sort_unstable();
        Ok(Self {
            shapes,
            budget,
            resources,
            parents,
            visits_left: route_limit,
            memo: Some(ChargedMap::new(budget)),
            whole: BTreeSet::new(),
            whole_seen: ChargedMap::new(budget),
        })
    }

    pub(super) fn contains<V: ReadView + Sync>(
        &mut self,
        validator: &Validator<'_, V>,
        graph: &GraphView<'_, V>,
        result: &RawResult,
        already_checked: impl Fn(ShapeRef, TermId) -> bool,
    ) -> Result<bool, QueryEvaluationError> {
        let pair = (result.shape, result.focus);
        let Some(memo) = &self.memo else {
            return self.whole_contains(validator, result);
        };
        let target = match memo.get(&pair) {
            Some(verdict) => verdict.target,
            None => {
                let target = match self.reaches_target(validator, graph, pair)? {
                    Route::Found(target) => Some(target),
                    Route::Missing => None,
                    Route::Limit => {
                        self.memo = None;
                        return self.whole_contains(validator, result);
                    }
                };
                let memo = self.memo.as_mut().expect("still searching for routes");
                memo.insert(
                    pair,
                    Verdict {
                        target,
                        results: None,
                    },
                )?;
                target
            }
        };
        let Some((root, focus)) = target else {
            return Ok(false);
        };
        // A checked route would already have found this result if it existed before.
        // The witness itself is independent of the current after root.
        if already_checked(root, focus) {
            return Ok(false);
        }
        let memo = self.memo.as_mut().expect("still searching for routes");
        let (verdict, _) = memo.get_or_insert_with(pair, Verdict::default)?;
        let results = verdict
            .results
            .get_or_insert_with(|| validator.results_at(pair.0, pair.1));
        Ok(results.contains(result))
    }

    /// Walk backwards from the candidate, in the before graph. Only sh:property emits
    /// a child's own results. Other constraints probe for conformance and emit their
    /// own results at the caller. A route from an active target has no cycle and
    /// cannot be cut by the validator's recursion rule.
    fn reaches_target<V: ReadView>(
        &mut self,
        validator: &Validator<'_, V>,
        graph: &GraphView<'_, V>,
        start: Focus,
    ) -> Result<Route, QueryEvaluationError> {
        let mut walk = RouteWalk {
            pending: ChargedVec::new(self.budget),
            seen: ChargedMap::new(self.budget),
            visits_left: &mut self.visits_left,
        };
        if !walk.take_visit() {
            return Ok(Route::Limit);
        }
        let mut next = Some(start);
        while let Some((shape, focus)) = next.take().or_else(|| pop(&mut walk.pending)) {
            self.resources.check()?;
            if validator.is_focus(&self.shapes.shapes[shape], focus) {
                return Ok(Route::Found((shape, focus)));
            }
            let parents = parents_of(&self.parents, shape);
            // A single node-shape parent passes the focus through without allocating
            // a search frontier on an unbranched route.
            if let [(_, parent)] = parents
                && self.shapes.shapes[*parent].path.is_none()
                && walk.seen.is_empty()
            {
                if !walk.take_visit() {
                    return Ok(Route::Limit);
                }
                next = Some((*parent, focus));
                continue;
            }
            for &(_, parent) in parents {
                self.resources.check()?;
                let queued = match &self.shapes.shapes[parent].path {
                    None => walk.queue((parent, focus))?,
                    Some(Path::Predicate(predicate)) => walk.queue_all(
                        parent,
                        graph
                            .scan(None, Some(*predicate), Some(focus))
                            .map(|quad| quad.subject),
                        self.resources,
                    )?,
                    Some(path) => {
                        let previous = path::back(graph, path, std::slice::from_ref(&focus));
                        self.resources.check()?;
                        if previous.len() > MAX_AFFECTED {
                            return Ok(Route::Limit);
                        }
                        let previous = ChargedVec::new(self.budget).adopt_vec(previous)?;
                        walk.queue_all(parent, previous.iter().copied(), self.resources)?
                    }
                };
                if !queued {
                    return Ok(Route::Limit);
                }
            }
        }
        self.resources.check()?;
        Ok(Route::Missing)
    }

    /// Find the candidate shape's ancestors without following any data paths. Their
    /// targeted roots' before results cover every route. Each root is validated once,
    /// even if multiple candidates need it.
    fn whole_contains<V: ReadView + Sync>(
        &mut self,
        validator: &Validator<'_, V>,
        result: &RawResult,
    ) -> Result<bool, QueryEvaluationError> {
        if self.whole.contains(result) {
            return Ok(true);
        }
        let mut pending = ChargedVec::new(self.budget);
        let mut next = Some(result.shape);
        while let Some(shape) = next.take().or_else(|| pop(&mut pending)) {
            self.resources.check()?;
            if !self.whole_seen.get_or_insert_with(shape, || ())?.1 {
                continue;
            }
            let compiled = &self.shapes.shapes[shape];
            if !compiled.targets.is_empty() {
                let work: Vec<_> = validator
                    .focus_nodes(compiled)
                    .into_iter()
                    .map(|focus| (shape, focus))
                    .collect();
                self.resources.check()?;
                let results = validator.validate_all(&work);
                if validator.has_stopped() {
                    return Ok(false);
                }
                let mut duplicates = 0;
                for (i, previous) in results.into_iter().enumerate() {
                    if i % CHECK_EVERY == 0 {
                        self.resources.check()?;
                    }
                    let bytes = previous.retained_bytes();
                    if !self.whole.insert(previous) {
                        duplicates += bytes;
                    }
                }
                validator.release(duplicates);
            }
            for &(_, parent) in parents_of(&self.parents, shape) {
                pending.push(parent)?;
            }
        }
        Ok(self.whole.contains(result))
    }
}

fn parents_of(parents: &[(ShapeRef, ShapeRef)], child: ShapeRef) -> &[(ShapeRef, ShapeRef)] {
    let first = parents.partition_point(|&(shape, _)| shape < child);
    let rest = &parents[first..];
    &rest[..rest.partition_point(|&(shape, _)| shape == child)]
}

fn pop<T: Copy>(pending: &mut ChargedVec<T, &Budget>) -> Option<T> {
    let last = pending.last().copied()?;
    pending.truncate(pending.len() - 1);
    Some(last)
}

struct RouteWalk<'a, 'r> {
    pending: ChargedVec<Focus, &'a Budget>,
    seen: ChargedMap<Focus, (), Reservation<&'a Budget>>,
    visits_left: &'r mut usize,
}

impl RouteWalk<'_, '_> {
    fn take_visit(&mut self) -> bool {
        if let Some(left) = self.visits_left.checked_sub(1) {
            *self.visits_left = left;
            true
        } else {
            false
        }
    }

    /// False when adding a pair would exceed the call's reverse search limit.
    fn queue(&mut self, pair: Focus) -> Result<bool, QueryEvaluationError> {
        if self.seen.get(&pair).is_none() {
            if !self.take_visit() {
                return Ok(false);
            }
            self.seen.insert(pair, ())?;
            self.pending.push(pair)?;
        }
        Ok(true)
    }

    fn queue_all(
        &mut self,
        parent: ShapeRef,
        nodes: impl Iterator<Item = TermId>,
        resources: &Resources,
    ) -> Result<bool, QueryEvaluationError> {
        for (i, node) in nodes.enumerate() {
            if i % CHECK_EVERY == 0 {
                resources.check()?;
            }
            if !self.queue((parent, node))? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
