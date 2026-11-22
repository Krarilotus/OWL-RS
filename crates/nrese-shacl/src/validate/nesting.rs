//! Depth-first validation of nested shapes, with a continuation for each active caller.

use nrese_engine::TermId;
use nrese_exec::{Budget, BudgetExceeded, Grant};
use nrese_sparql::{QueryEvaluationError, ReadView};

use super::{TaskResults, Validator};
use crate::model::{Component, Constraint, Logical, NestedConstraint, Nesting, ShapeRef};
use crate::nested_walk::Suspended;
use crate::path;

type Check = (ShapeRef, TermId);

struct Frame {
    parent: Option<Check>,
    path_values: Option<Vec<TermId>>,
    retained_path: usize,
    constraint: usize,
    nested: NestedCheck,
    /// A conformance probe has its own results. Save the caller's probe results, if any,
    /// while it runs; a property check instead uses the caller's results directly.
    saved_results: Option<TaskResults>,
    probe: bool,
}

pub(super) struct Walk<'a> {
    callers: Suspended<'a, Check, Frame>,
    // A stopped walk drops its parked frames before returning their path charge.
    paths: Grant<&'a Budget>,
}

impl<'a> Walk<'a> {
    pub(super) fn new(budget: &'a Budget) -> Self {
        Self {
            callers: Suspended::new(budget),
            paths: Grant::new(budget),
        }
    }

    fn park(&mut self, check: Check, mut frame: Frame) -> Result<(), BudgetExceeded> {
        if frame.retained_path == 0
            && let Some(values) = &frame.path_values
            && values.capacity() > 0
        {
            let bytes = values.capacity() * std::mem::size_of::<TermId>();
            self.paths.take(bytes)?;
            // A resumed frame can park again with the same values. Keep their charge
            // until it finishes.
            frame.retained_path = bytes;
        }
        self.callers.park(check, frame)
    }

    fn finish(&mut self, frame: Frame) {
        let bytes = frame.retained_path;
        drop(frame);
        if bytes > 0 {
            self.paths.give_back(bytes);
        }
    }
}

impl<V: ReadView> Validator<'_, V> {
    pub(super) fn validate_node(
        &self,
        shape: ShapeRef,
        focus: TermId,
        results: &mut TaskResults,
        walk: &mut Walk<'_>,
    ) {
        if self.stopped() || self.shapes.shapes[shape].deactivated {
            return;
        }

        let mut check = (shape, focus);
        let mut frame = self.frame(check, None, false);
        let mut temporary: Option<TaskResults> = None;
        let mut conformed = None;
        loop {
            let (shape, focus) = check;
            let compiled = &self.shapes.shapes[shape];
            let Some(constraint) = compiled.constraints.get(frame.constraint) else {
                let parent = frame.parent;
                let probe = frame.probe;
                walk.finish(frame);
                let Some(parent) = parent else {
                    return;
                };
                check = parent;
                frame = walk.callers.resume(&parent);
                if probe {
                    let finished = temporary.take().expect("the probe's results");
                    conformed = Some(finished.list.is_empty());
                    temporary = frame.saved_results.take();
                }
                continue;
            };
            let values = frame
                .path_values
                .as_deref()
                .unwrap_or_else(|| std::slice::from_ref(&focus));
            let output = temporary.as_mut().unwrap_or(&mut *results);
            if let Constraint::Nested(nested) = constraint {
                let visit =
                    frame
                        .nested
                        .visit(nested, values, conformed.take(), |component, value| {
                            self.keep(output, self.core_result(shape, focus, component, value));
                        });
                if let Some((child, probe)) = visit {
                    // Poll at every nested call. Only a repeat of the same pair on the
                    // active branch conforms by recursion, even if other branches visited it.
                    if self.stopped() {
                        return;
                    }
                    if self.shapes.shapes[child.0].deactivated
                        || child == check
                        || walk.callers.contains(&child)
                    {
                        conformed = probe.then_some(true);
                        continue;
                    }
                    if probe {
                        frame.saved_results =
                            temporary.replace(TaskResults::new(&self.results_budget));
                    }
                    if let Err(refused) = walk.park(check, frame) {
                        let _ = self.stop.set(QueryEvaluationError::MemoryLimit(refused));
                        return;
                    }
                    frame = self.frame(child, Some(check), probe);
                    check = child;
                    continue;
                }
                frame.nested = NestedCheck::default();
            } else {
                self.check_local(shape, focus, values, constraint, output);
            }
            frame.constraint += 1;
        }
    }

    fn frame(&self, (shape, focus): Check, parent: Option<Check>, probe: bool) -> Frame {
        Frame {
            parent,
            path_values: self.shapes.shapes[shape]
                .path
                .as_ref()
                .map(|path| path::values(&self.graph, path, focus)),
            retained_path: 0,
            constraint: 0,
            nested: NestedCheck::default(),
            saved_results: None,
            probe,
        }
    }
}

/// Where a constraint paused to check a value against a nested shape.
#[derive(Default)]
struct NestedCheck {
    value: usize,
    member: usize,
    matches: usize,
    qualified: u64,
}

impl NestedCheck {
    /// Advances until the next child to check or the constraint is complete. The boolean
    /// says whether the child tests conformance or reports its own results (sh:property).
    fn visit(
        &mut self,
        nested: &NestedConstraint,
        values: &[TermId],
        mut conformed: Option<bool>,
        mut emit: impl FnMut(Component, Option<TermId>),
    ) -> Option<(Check, bool)> {
        if matches!(nested.kind, Nesting::Property) {
            let value = *values.get(self.value)?;
            self.value += 1;
            let shape = nested.reference(0).expect("a property shape");
            return Some(((shape, value), false));
        }

        while let Some(&value) = values.get(self.value) {
            let decided = conformed
                .take()
                .and_then(|ok| self.member_answer(nested.kind, ok));
            let ok = if let Some(ok) = decided {
                ok
            } else if let Some(shape) = nested.reference(self.member) {
                self.member += 1;
                return Some(((shape, value), true));
            } else {
                match nested.kind {
                    Nesting::Logical(Logical::And) | Nesting::Qualified { .. } => true,
                    Nesting::Logical(Logical::Or) => false,
                    Nesting::Logical(Logical::Xone) => self.matches == 1,
                    Nesting::Not | Nesting::Node => {
                        unreachable!("a single member decides the constraint")
                    }
                    Nesting::Property => unreachable!("property checks report their own results"),
                }
            };

            let component = match nested.kind {
                Nesting::Qualified { .. } => {
                    self.qualified += u64::from(ok);
                    None
                }
                Nesting::Not => Some(Component::Not),
                Nesting::Node => Some(Component::Node),
                Nesting::Logical(Logical::And) => Some(Component::And),
                Nesting::Logical(Logical::Or) => Some(Component::Or),
                Nesting::Logical(Logical::Xone) => Some(Component::Xone),
                Nesting::Property => unreachable!("property checks report their own results"),
            };
            if !ok && let Some(component) = component {
                emit(component, Some(value));
            }
            self.value += 1;
            self.member = 0;
            self.matches = 0;
        }
        if let Nesting::Qualified { min, max } = nested.kind {
            if min.is_some_and(|min| self.qualified < min) {
                emit(Component::QualifiedMinCount, None);
            }
            if max.is_some_and(|max| self.qualified > max) {
                emit(Component::QualifiedMaxCount, None);
            }
        }
        None
    }

    fn member_answer(&mut self, kind: Nesting, conforms: bool) -> Option<bool> {
        match kind {
            Nesting::Not => Some(!conforms),
            Nesting::Node => Some(conforms),
            Nesting::Logical(Logical::And) => (!conforms).then_some(false),
            Nesting::Logical(Logical::Or) => conforms.then_some(true),
            Nesting::Logical(Logical::Xone) => {
                self.matches += usize::from(conforms);
                None
            }
            // The primary shape must conform, and the siblings must not.
            Nesting::Qualified { .. } => (conforms != (self.member == 1)).then_some(false),
            Nesting::Property => unreachable!("property checks do not probe conformance"),
        }
    }
}
