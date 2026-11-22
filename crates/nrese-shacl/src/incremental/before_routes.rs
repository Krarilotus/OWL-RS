//! Routes by which full validation can report a candidate at its source shape and focus.

use nrese_engine::TermId;
use nrese_exec::{Budget, ChargedMap, ChargedVec};
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};

use crate::graph::GraphView;
use crate::model::{Constraint, Nesting, ShapeRef, Shapes};
use crate::path;

type Focus = (ShapeRef, TermId);

pub(super) struct BeforeRoutes<'a> {
    // Sorted by child. Each edge is the reverse of a result-emitting sh:property call.
    parents: ChargedVec<(ShapeRef, ShapeRef), &'a Budget>,
}

impl<'a> BeforeRoutes<'a> {
    pub(super) fn new(
        shapes: &Shapes,
        budget: &'a Budget,
        resources: &Resources,
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
        Ok(Self { parents })
    }

    fn parents_of(&self, child: ShapeRef) -> &[(ShapeRef, ShapeRef)] {
        let first = self.parents.partition_point(|&(shape, _)| shape < child);
        let rest = &self.parents[first..];
        &rest[..rest.partition_point(|&(shape, _)| shape == child)]
    }

    /// Walk backwards from the candidate, in the before graph. Only sh:property emits
    /// a child's own results. Other constraints probe for conformance and emit their
    /// own results at the caller. The shapes are acyclic, so a route cannot be cut by
    /// the validator's recursion rule.
    pub(super) fn reaches_target<V: ReadView>(
        &self,
        shapes: &Shapes,
        graph: &GraphView<'_, V>,
        start: Focus,
        unchecked_target: impl Fn(ShapeRef, TermId) -> bool,
        budget: &Budget,
        resources: &Resources,
    ) -> Result<bool, QueryEvaluationError> {
        let mut pending = ChargedVec::new(budget);
        let mut seen = ChargedMap::new(budget);
        let mut next = Some(start);
        while let Some((shape, focus)) = next.take().or_else(|| {
            let last = pending.last().copied()?;
            pending.truncate(pending.len() - 1);
            Some(last)
        }) {
            resources.check()?;
            if unchecked_target(shape, focus) {
                return Ok(true);
            }
            let parents = self.parents_of(shape);
            // A single node-shape parent passes the focus through without allocating
            // a search frontier (the usual case for a direct property of a target).
            if let [(_, parent)] = parents
                && shapes.shapes[*parent].path.is_none()
            {
                next = Some((*parent, focus));
                continue;
            }
            let mut queue = |parent_focus: Focus| -> Result<(), QueryEvaluationError> {
                if seen.get_or_insert_with(parent_focus, || ())?.1 {
                    pending.push(parent_focus)?;
                }
                Ok(())
            };
            for &(_, parent) in parents {
                resources.check()?;
                if let Some(path) = &shapes.shapes[parent].path {
                    let previous = path::back(graph, path, std::slice::from_ref(&focus));
                    let previous = ChargedVec::new(budget).adopt_vec(previous)?;
                    for (i, &node) in previous.iter().enumerate() {
                        if i % super::CHECK_EVERY == 0 {
                            resources.check()?;
                        }
                        queue((parent, node))?;
                    }
                } else {
                    queue((parent, focus))?;
                }
            }
        }
        resources.check()?;
        Ok(false)
    }
}
