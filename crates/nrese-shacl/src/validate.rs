//! Fallible SHACL preparation and raw validation. Decoding uses the owner-supplied
//! ReadView::decode_within contract; there is no allocating-decode fallback here.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write;
use std::hash::Hash;
use std::ops::Deref;
use std::sync::Arc;

use nrese_engine::{TermId, TermKind};
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation, Ticker, map_bytes};
use nrese_rdf::Term;
use nrese_sparql::value::{Value, compare, lang_matches};
use nrese_sparql::{QueryEvaluationError, ReadView, Resources};

use crate::datatype::well_formed;
use crate::graph::{GraphView, Selection, contains_within, sort_set, stopped};
use crate::model::{
    Bound, Component, Constraint, Logical, NodeKind, Path, Shape, ShapeRef, Shapes, SparqlCheck,
    Target,
};
use crate::{path, sparql};

type Result<T> = std::result::Result<T, QueryEvaluationError>;
type Work<'b> = ChargedVec<(ShapeRef, TermId), &'b Budget>;
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// Borrowed compiled paths/messages keep the existing Shapes lease in use. The only
/// owned raw paths are inline predicate overrides; no recursive path is cloned per row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RawResult<'s> {
    pub(crate) focus: TermId,
    pub(crate) path: Option<Cow<'s, Path>>,
    pub(crate) value: Option<TermId>,
    pub(crate) shape: ShapeRef,
    pub(crate) component: Component,
    pub(crate) source: Option<TermId>,
    pub(crate) messages: Option<&'s [TermId]>,
}

#[derive(Debug)]
pub(crate) struct RawChunk<'s> {
    pub(crate) results: Vec<RawResult<'s>>,
    pub(crate) failures: Vec<String>,
    pub(crate) held: Reservation,
    // A raw failure's payload keeps its original token, separately from row headers.
    pub(crate) failure_held: Option<Reservation>,
}

impl RawChunk<'_> {
    // Incremental set-difference can discard the old raw header allocation before
    // decoding; release precisely that allocation after freeing it, not its other data.
    pub(crate) fn discard_results(&mut self) {
        let old = std::mem::take(&mut self.results);
        let bytes = old.capacity() * size_of::<RawResult<'_>>();
        drop(old);
        self.held.release(bytes);
    }
}

pub(crate) struct RawBatch<'s> {
    pub(crate) chunks: ChargedVec<RawChunk<'s>>,
    pub(crate) budget: Arc<Budget>,
    pub(crate) applicable: bool,
}

impl<'s> RawBatch<'s> {
    pub(crate) fn results(&self) -> impl Iterator<Item = &RawResult<'s>> {
        self.chunks.iter().flat_map(|c| c.results.iter())
    }
    pub(crate) fn len(&self) -> usize {
        self.chunks.iter().map(|c| c.results.len()).sum()
    }
}

pub(crate) fn validate_raw<'s, V: ReadView + Sync>(
    view: &'s V,
    shapes: &'s Shapes,
    data: Selection,
    resources: &'s Resources,
) -> Result<RawBatch<'s>> {
    resources.check()?;
    let budget = Arc::new(resources.memory.part());
    let validator = Validator::new(view, shapes, data, resources, budget)?;
    let mut ticker = resources.stop().ticker().map_err(stopped)?;
    let (work, applicable) = validator.prepare(&mut ticker)?;
    let mut raw = validator.validate_all(&work)?;
    raw.applicable = applicable;
    ticker.finish().map_err(stopped)?;
    Ok(raw)
}

pub(crate) fn applicable<V: ReadView + Sync>(
    view: &V,
    shapes: &Shapes,
    data: Selection,
    resources: &Resources,
) -> Result<bool> {
    let budget = Arc::new(resources.memory.part());
    let validator = Validator::new(view, shapes, data, resources, budget)?;
    let mut ticker = resources.stop().ticker().map_err(stopped)?;
    // This standalone boolean helper keeps the old short-circuit behavior. Validation
    // itself carries its verdict from prepare; neither path needs a second Validator.
    let mut seen = ChargedVec::new(&*validator.budget);
    for shape in shapes.targeted() {
        ticker.tick(1).map_err(stopped)?;
        seen.clear();
        if constrained(shapes, shape, &mut seen, &mut ticker)?
            && !validator
                .focus_nodes(&shapes.shapes[shape], &mut ticker)?
                .is_empty()
        {
            ticker.finish().map_err(stopped)?;
            return Ok(true);
        }
    }
    ticker.finish().map_err(stopped)?;
    Ok(false)
}

pub(crate) fn constrained(
    shapes: &Shapes,
    shape: ShapeRef,
    seen: &mut ChargedVec<ShapeRef, &Budget>,
    ticker: &mut Ticker,
) -> Result<bool> {
    ticker.tick(1).map_err(stopped)?;
    if shapes.shapes[shape].deactivated {
        return Ok(false);
    }
    for &previous in seen.iter() {
        ticker.tick(1).map_err(stopped)?;
        if previous == shape {
            return Ok(false);
        }
    }
    seen.push(shape)?;
    for constraint in &shapes.shapes[shape].constraints {
        ticker.tick(1).map_err(stopped)?;
        let applies = match constraint {
            Constraint::Not(nested) | Constraint::Node(nested) | Constraint::Property(nested) => {
                constrained(shapes, *nested, seen, ticker)?
            }
            Constraint::Qualified { shape: nested, .. } => {
                constrained(shapes, *nested, seen, ticker)?
            }
            Constraint::Logical(_, nested) => {
                let mut applies = false;
                for nested in nested {
                    if constrained(shapes, *nested, seen, ticker)? {
                        applies = true;
                        break;
                    }
                }
                applies
            }
            _ => true,
        };
        if applies {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) struct Validator<'s, V: ReadView> {
    pub(crate) graph: GraphView<'s, V>,
    pub(crate) shapes: &'s Shapes,
    pub(crate) resources: &'s Resources,
    pub(crate) budget: Arc<Budget>,
    rdf_type: Option<TermId>,
    classes: HashMap<TermId, Vec<TermId>>,
    _held: Reservation,
}

/// Existing hash-table bound; old and new tables are both admitted while reserve moves.
/// Used only by the private SHACL cache/visited/tag maps, not a new resource carrier.
pub(crate) fn reserve_map<K: Eq + Hash, V, B: Deref<Target = Budget> + Clone>(
    map: &mut HashMap<K, V>,
    held: &mut Reservation<B>,
) -> Result<()> {
    if map.len() < map.capacity() {
        return Ok(());
    }
    let capacity = map
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| held.budget().refusal(usize::MAX))?;
    let mut next = Reservation::new(held.handle().clone());
    next.charge(map_bytes::<(K, V)>(capacity))?;
    map.reserve(capacity - map.len());
    debug_assert!(map_bytes::<(K, V)>(map.capacity()) <= next.bytes());
    *held = next;
    Ok(())
}

impl<'s, V: ReadView> Validator<'s, V> {
    pub(crate) fn new(
        view: &'s V,
        shapes: &'s Shapes,
        data: Selection,
        resources: &'s Resources,
        budget: Arc<Budget>,
    ) -> Result<Self> {
        resources.check()?;
        let graph = GraphView::new(view, data);
        let mut ticker = resources.stop().ticker().map_err(stopped)?;
        let sub_class_of = graph.iri(RDFS_SUB_CLASS_OF);
        let mut map_held = Reservation::new(budget.clone());
        let mut held = Reservation::new(budget.clone());
        let mut classes = HashMap::new();
        for shape in &shapes.shapes {
            let named = shape
                .targets
                .iter()
                .filter_map(|target| match target {
                    Target::Class(c) => Some(*c),
                    _ => None,
                })
                .chain(shape.constraints.iter().filter_map(|c| match c {
                    Constraint::Class(c) => Some(*c),
                    _ => None,
                }));
            for class in named {
                ticker.tick(1).map_err(stopped)?;
                if !classes.contains_key(&class) {
                    reserve_map(&mut classes, &mut map_held)?;
                    let (nodes, token) =
                        subclasses(&graph, sub_class_of, class, &budget, &mut ticker)?;
                    held.absorb(token);
                    classes.insert(class, nodes);
                }
            }
        }
        held.absorb(map_held);
        ticker.finish().map_err(stopped)?;
        Ok(Self {
            graph,
            shapes,
            resources,
            budget,
            rdf_type: graph.iri(RDF_TYPE),
            classes,
            _held: held,
        })
    }

    pub(crate) fn prepare(&self, ticker: &mut Ticker) -> Result<(Work<'_>, bool)> {
        let mut work = ChargedVec::new(&*self.budget);
        let mut seen = ChargedVec::new(&*self.budget);
        let mut applicable = false;
        for shape in self.shapes.targeted() {
            ticker.tick(1).map_err(stopped)?;
            let focus = self.focus_nodes(&self.shapes.shapes[shape], ticker)?;
            if !applicable && !focus.is_empty() {
                seen.clear();
                applicable = constrained(self.shapes, shape, &mut seen, ticker)?;
            }
            work.extend_bounded(focus.len(), focus.iter().map(|&node| (shape, node)))?;
        }
        Ok((work, applicable))
    }

    pub(crate) fn validate_all(&self, work: &[(ShapeRef, TermId)]) -> Result<RawBatch<'s>>
    where
        V: Sync,
    {
        const NODES_PER_TASK: usize = 32;
        let count = work.len().div_ceil(NODES_PER_TASK);
        let mut chunks = ChargedVec::with_capacity(self.budget.clone(), count)?;
        chunks.fill(count, |room| {
            room.try_par_fill(count, |i| {
                let mut task = Task::new(self)?;
                let mut results = ChargedVec::new(&*self.budget);
                for &(shape, node) in
                    &work[i * NODES_PER_TASK..((i + 1) * NODES_PER_TASK).min(work.len())]
                {
                    task.validate_node(shape, node, &mut results)?;
                }
                task.ticker.finish().map_err(stopped)?;
                let (results, held) = results.into_parts();
                let (failures, token) = task.failures.into_parts();
                task.held.absorb(token);
                let failure_held =
                    (!failures.is_empty()).then(|| task.held.into_shared(&self.budget));
                Ok::<_, QueryEvaluationError>(RawChunk {
                    results,
                    failures,
                    held: held.into_shared(&self.budget),
                    failure_held,
                })
            })
        })??;
        self.resources.check()?;
        Ok(RawBatch {
            chunks,
            budget: self.budget.clone(),
            applicable: false,
        })
    }

    pub(crate) fn focus_nodes(
        &self,
        shape: &Shape,
        ticker: &mut Ticker,
    ) -> Result<ChargedVec<TermId, &Budget>> {
        let budget = &*self.budget;
        let mut nodes = ChargedVec::new(budget);
        for target in &shape.targets {
            ticker.tick(1).map_err(stopped)?;
            match *target {
                Target::Node(node) => nodes.push(node)?,
                Target::Class(class) => {
                    if let Some(rdf_type) = self.rdf_type {
                        for &subclass in &self.classes[&class] {
                            ticker.tick(1).map_err(stopped)?;
                            nodes.extend_from_slice(
                                &self
                                    .graph
                                    .subjects_within(rdf_type, subclass, budget, ticker)?,
                            )?;
                        }
                    }
                }
                Target::SubjectsOf(p) => {
                    nodes.extend_from_slice(&self.graph.subjects_of_within(p, budget, ticker)?)?
                }
                Target::ObjectsOf(p) => {
                    nodes.extend_from_slice(&self.graph.objects_of_within(p, budget, ticker)?)?
                }
            }
        }
        let kept = sort_set(&mut nodes, ticker)?;
        nodes.truncate(kept);
        Ok(nodes)
    }
}

struct Task<'v, 's, V: ReadView> {
    validator: &'v Validator<'s, V>,
    stack: ChargedVec<(ShapeRef, TermId), &'v Budget>,
    ticker: Ticker,
    failures: ChargedVec<String, &'v Budget>,
    held: Reservation<&'v Budget>,
}

fn is_literal(id: TermId) -> bool {
    !matches!(
        id.kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
}

impl<'v, 's, V: ReadView> Task<'v, 's, V> {
    fn new(validator: &'v Validator<'s, V>) -> Result<Self> {
        Ok(Self {
            validator,
            stack: ChargedVec::new(&*validator.budget),
            ticker: validator.resources.stop().ticker().map_err(stopped)?,
            failures: ChargedVec::new(&*validator.budget),
            held: Reservation::new(&*validator.budget),
        })
    }

    fn validate_node<B: Deref<Target = Budget>>(
        &mut self,
        shape_ref: ShapeRef,
        focus: TermId,
        results: &mut ChargedVec<RawResult<'s>, B>,
    ) -> Result<()> {
        self.ticker.tick(1).map_err(stopped)?;
        let validator = self.validator;
        let shape = &validator.shapes.shapes[shape_ref];
        if shape.deactivated {
            return Ok(());
        }
        for previous in self.stack.iter() {
            self.ticker.tick(1).map_err(stopped)?;
            if *previous == (shape_ref, focus) {
                return Ok(());
            }
        }
        self.stack.push((shape_ref, focus))?;
        let values = match &shape.path {
            None => ChargedVec::collect(&*validator.budget, 1, [focus])?,
            Some(path) => path::values(
                &validator.graph,
                path,
                focus,
                &validator.budget,
                &mut self.ticker,
            )?,
        };
        for constraint in &shape.constraints {
            self.check(shape_ref, focus, &values, constraint, results)?;
        }
        self.stack.truncate(self.stack.len() - 1);
        Ok(())
    }

    fn conforms(&mut self, shape: ShapeRef, node: TermId) -> Result<bool> {
        let validator = self.validator;
        let mut results = ChargedVec::new(&*validator.budget);
        self.validate_node(shape, node, &mut results)?;
        Ok(results.is_empty())
    }

    fn raw(
        &self,
        shape: ShapeRef,
        focus: TermId,
        component: Component,
        value: Option<TermId>,
    ) -> RawResult<'s> {
        RawResult {
            shape,
            focus,
            component,
            value,
            path: self.validator.shapes.shapes[shape]
                .path
                .as_ref()
                .map(Cow::Borrowed),
            source: None,
            messages: None,
        }
    }

    fn each<B: Deref<Target = Budget>>(
        &mut self,
        shape: ShapeRef,
        focus: TermId,
        component: Component,
        values: &[TermId],
        results: &mut ChargedVec<RawResult<'s>, B>,
        ok: &mut dyn FnMut(&mut Self, TermId) -> Result<bool>,
    ) -> Result<()> {
        for &value in values {
            self.ticker.tick(1).map_err(stopped)?;
            if !ok(self, value)? {
                results.push(self.raw(shape, focus, component, Some(value)))?;
            }
        }
        Ok(())
    }

    fn decode(&mut self, id: TermId, held: &mut Reservation<&Budget>) -> Result<Option<Term>> {
        self.ticker.check().map_err(stopped)?;
        let term = self.validator.graph.parts().0.decode_within(id, held)?;
        self.ticker.check().map_err(stopped)?;
        Ok(term)
    }

    // Borrow lexical text from the admitted decoded term: no second String per value.
    fn text_is(&mut self, id: TermId, test: &mut dyn FnMut(&str) -> bool) -> Result<bool> {
        let validator = self.validator;
        let mut held = Reservation::new(&*validator.budget);
        let term = self.decode(id, &mut held)?;
        let result = match &term {
            Some(Term::NamedNode(n)) => test(n.as_str()),
            Some(Term::Literal(l)) => test(l.value()),
            _ => false,
        };
        self.ticker.check().map_err(stopped)?;
        Ok(result)
    }

    fn length_is(&mut self, id: TermId, threshold: u64, minimum: bool) -> Result<bool> {
        let validator = self.validator;
        let mut held = Reservation::new(&*validator.budget);
        let term = self.decode(id, &mut held)?;
        let text = match &term {
            Some(Term::NamedNode(n)) => n.as_str(),
            Some(Term::Literal(l)) => l.value(),
            _ => return Ok(false),
        };
        let mut length = 0u64;
        if minimum && threshold == 0 {
            return Ok(true);
        }
        for _ in text.chars() {
            self.ticker.tick(1).map_err(stopped)?;
            length += 1;
            // The old complete count has already been decided; don't scan a suffix
            // that cannot change this boolean. No change to result multiplicity.
            if minimum && length >= threshold {
                return Ok(true);
            }
            if !minimum && length > threshold {
                return Ok(false);
            }
        }
        Ok(if minimum {
            length >= threshold
        } else {
            length <= threshold
        })
    }

    fn value(&mut self, id: TermId, held: &mut Reservation<&Budget>) -> Result<Option<Value>> {
        let validator = self.validator;
        let mut decoded = Reservation::new(&*validator.budget);
        let Some(term) = self.decode(id, &mut decoded)? else {
            return Ok(None);
        };
        // Value::of may copy the term's owned text/triple. Admit beside the decoded term,
        // then return scalar excess after parsing. All other Value variants are inline.
        let bound = term.heap_bytes();
        held.charge(bound)?;
        self.ticker.check().map_err(stopped)?;
        let value = Value::of(&term);
        self.ticker.check().map_err(stopped)?;
        let actual = match &value {
            Value::String(s) | Value::Iri(s) | Value::Blank(s) => s.capacity(),
            Value::LangString(s, l) => s.capacity() + l.capacity(),
            Value::Other(l) => l.heap_bytes(),
            Value::Triple(t) => size_of::<nrese_rdf::Triple>() + t.heap_bytes(),
            _ => 0,
        };
        debug_assert!(actual <= bound);
        held.release(bound.saturating_sub(actual));
        Ok(Some(value))
    }

    fn language(&mut self, id: TermId, held: &mut Reservation<&Budget>) -> Result<Option<String>> {
        if id.kind() != TermKind::LangString {
            return Ok(None);
        }
        let validator = self.validator;
        let mut decoded = Reservation::new(&*validator.budget);
        let term = self.decode(id, &mut decoded)?;
        let Some(Term::Literal(literal)) = term else {
            return Ok(None);
        };
        let Some(language) = literal.language() else {
            return Ok(None);
        };
        held.charge(language.len())?;
        let tag = language.to_ascii_lowercase();
        self.ticker.check().map_err(stopped)?;
        Ok(Some(tag))
    }

    fn is_instance(&mut self, node: TermId, class: TermId) -> Result<bool> {
        let validator = self.validator;
        let Some(rdf_type) = validator.rdf_type else {
            return Ok(false);
        };
        if is_literal(node) {
            return Ok(false);
        }
        let classes =
            validator
                .graph
                .objects_within(node, rdf_type, &validator.budget, &mut self.ticker)?;
        for class_ in classes.iter() {
            self.ticker.tick(1).map_err(stopped)?;
            if validator.classes[&class].binary_search(class_).is_ok() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn has_datatype(&mut self, node: TermId, datatype: &str) -> Result<bool> {
        if !is_literal(node) {
            return Ok(false);
        }
        let validator = self.validator;
        let mut held = Reservation::new(&*validator.budget);
        let term = self.decode(node, &mut held)?;
        let result = match &term {
            Some(Term::Literal(l)) => {
                let actual = if l.language().is_some() {
                    RDF_LANG_STRING
                } else {
                    l.datatype().as_str()
                };
                actual == datatype && well_formed(datatype, l.value())
            }
            _ => false,
        };
        self.ticker.check().map_err(stopped)?;
        Ok(result)
    }

    #[expect(clippy::too_many_lines)]
    fn check<B: Deref<Target = Budget>>(
        &mut self,
        shape: ShapeRef,
        focus: TermId,
        values: &[TermId],
        constraint: &'s Constraint,
        results: &mut ChargedVec<RawResult<'s>, B>,
    ) -> Result<()> {
        self.ticker.tick(1).map_err(stopped)?;
        let validator = self.validator;
        let budget = &*validator.budget;
        match constraint {
            Constraint::Class(class) => self.each(
                shape,
                focus,
                Component::Class,
                values,
                results,
                &mut |task, v| task.is_instance(v, *class),
            )?,
            Constraint::Datatype(datatype) => self.each(
                shape,
                focus,
                Component::Datatype,
                values,
                results,
                &mut |task, v| task.has_datatype(v, datatype),
            )?,
            Constraint::NodeKind(kind) => self.each(
                shape,
                focus,
                Component::NodeKind,
                values,
                results,
                &mut |_, v| {
                    let (iri, blank, literal) = (
                        v.kind() == TermKind::Iri,
                        v.kind() == TermKind::BlankNode,
                        is_literal(v),
                    );
                    Ok(match kind {
                        NodeKind::Iri => iri,
                        NodeKind::BlankNode => blank,
                        NodeKind::Literal => literal,
                        NodeKind::BlankNodeOrIri => blank || iri,
                        NodeKind::BlankNodeOrLiteral => blank || literal,
                        NodeKind::IriOrLiteral => iri || literal,
                    })
                },
            )?,
            Constraint::MinCount(min) => {
                if (values.len() as u64) < *min {
                    results.push(self.raw(shape, focus, Component::MinCount, None))?;
                }
            }
            Constraint::MaxCount(max) => {
                if values.len() as u64 > *max {
                    results.push(self.raw(shape, focus, Component::MaxCount, None))?;
                }
            }
            Constraint::Range(bound, limit) => {
                let (component, accepted): (Component, &[Ordering]) = match bound {
                    Bound::MinExclusive => (Component::MinExclusive, &[Ordering::Greater]),
                    Bound::MinInclusive => (
                        Component::MinInclusive,
                        &[Ordering::Greater, Ordering::Equal],
                    ),
                    Bound::MaxExclusive => (Component::MaxExclusive, &[Ordering::Less]),
                    Bound::MaxInclusive => {
                        (Component::MaxInclusive, &[Ordering::Less, Ordering::Equal])
                    }
                };
                self.each(shape, focus, component, values, results, &mut |task, v| {
                    let mut held = Reservation::new(budget);
                    let value = task.value(v, &mut held)?;
                    Ok(value
                        .as_ref()
                        .and_then(|v| compare(v, limit))
                        .is_some_and(|o| accepted.contains(&o)))
                })?;
            }
            Constraint::MinLength(min) => self.each(
                shape,
                focus,
                Component::MinLength,
                values,
                results,
                &mut |task, v| task.length_is(v, *min, true),
            )?,
            Constraint::MaxLength(max) => self.each(
                shape,
                focus,
                Component::MaxLength,
                values,
                results,
                &mut |task, v| task.length_is(v, *max, false),
            )?,
            Constraint::Pattern(regex) => self.each(
                shape,
                focus,
                Component::Pattern,
                values,
                results,
                &mut |task, v| task.text_is(v, &mut |s| regex.is_match(s)),
            )?,
            Constraint::LanguageIn(ranges) => self.each(
                shape,
                focus,
                Component::LanguageIn,
                values,
                results,
                &mut |task, v| {
                    let mut held = Reservation::new(budget);
                    let language = task.language(v, &mut held)?;
                    if let Some(tag) = language {
                        for range in ranges {
                            task.ticker.tick(1).map_err(stopped)?;
                            if lang_matches(&tag, range) {
                                return Ok(true);
                            }
                        }
                    }
                    Ok(false)
                },
            )?,
            Constraint::UniqueLang => {
                let mut map = Reservation::new(budget);
                let mut text = Reservation::new(budget);
                let mut tags: HashMap<String, usize> = HashMap::new();
                for &value in values {
                    self.ticker.tick(1).map_err(stopped)?;
                    if let Some(tag) = self.language(value, &mut text)? {
                        if let Some(count) = tags.get_mut(&tag) {
                            *count += 1;
                            let bytes = tag.capacity();
                            drop(tag);
                            text.release(bytes);
                        } else {
                            reserve_map(&mut tags, &mut map)?;
                            tags.insert(tag, 1);
                        }
                    }
                }
                // One identical result per duplicated language, as before. The order of
                // these rows is indistinguishable; no language is stored in the result.
                for count in tags.values() {
                    self.ticker.tick(1).map_err(stopped)?;
                    if *count > 1 {
                        results.push(self.raw(shape, focus, Component::UniqueLang, None))?;
                    }
                }
            }
            Constraint::Equals(property) | Constraint::Disjoint(property) => {
                let other =
                    validator
                        .graph
                        .objects_within(focus, *property, budget, &mut self.ticker)?;
                let equals = matches!(constraint, Constraint::Equals(_));
                self.each(
                    shape,
                    focus,
                    if equals {
                        Component::Equals
                    } else {
                        Component::Disjoint
                    },
                    values,
                    results,
                    &mut |_, v| Ok(other.binary_search(&v).is_ok() == equals),
                )?;
                if equals {
                    for &value in other.iter() {
                        self.ticker.tick(1).map_err(stopped)?;
                        if !contains_within(values, &value, &mut self.ticker)? {
                            results.push(self.raw(shape, focus, Component::Equals, Some(value)))?;
                        }
                    }
                }
            }
            Constraint::LessThan(property) | Constraint::LessThanOrEquals(property) => {
                let strict = matches!(constraint, Constraint::LessThan(_));
                let component = if strict {
                    Component::LessThan
                } else {
                    Component::LessThanOrEquals
                };
                let ids =
                    validator
                        .graph
                        .objects_within(focus, *property, budget, &mut self.ticker)?;
                let mut right_held = Reservation::new(budget);
                let mut others = ChargedVec::with_capacity(budget, ids.len())?;
                for &id in ids.iter() {
                    others.push(self.value(id, &mut right_held)?)?;
                }
                drop(ids);
                for &id in values {
                    let mut left_held = Reservation::new(budget);
                    let left = self.value(id, &mut left_held)?;
                    for right in others.iter() {
                        self.ticker.tick(1).map_err(stopped)?;
                        let order = left
                            .as_ref()
                            .zip(right.as_ref())
                            .and_then(|(l, r)| compare(l, r));
                        if !matches!(order, Some(Ordering::Less))
                            && (strict || !matches!(order, Some(Ordering::Equal)))
                        {
                            results.push(self.raw(shape, focus, component, Some(id)))?;
                        }
                    }
                }
            }
            Constraint::Not(nested) => self.each(
                shape,
                focus,
                Component::Not,
                values,
                results,
                &mut |task, v| Ok(!task.conforms(*nested, v)?),
            )?,
            Constraint::Logical(logical, nested) => {
                let component = match logical {
                    Logical::And => Component::And,
                    Logical::Or => Component::Or,
                    Logical::Xone => Component::Xone,
                };
                self.each(shape, focus, component, values, results, &mut |task, v| {
                    let mut count = 0;
                    for &inner in nested {
                        let conforms = task.conforms(inner, v)?;
                        match logical {
                            Logical::And if !conforms => return Ok(false),
                            Logical::Or if conforms => return Ok(true),
                            Logical::Xone if conforms => count += 1,
                            _ => {}
                        }
                    }
                    Ok(match logical {
                        Logical::And => true,
                        Logical::Or => false,
                        Logical::Xone => count == 1,
                    })
                })?;
            }
            Constraint::Node(nested) => self.each(
                shape,
                focus,
                Component::Node,
                values,
                results,
                &mut |task, v| task.conforms(*nested, v),
            )?,
            Constraint::Property(nested) => {
                for &value in values {
                    self.validate_node(*nested, value, results)?;
                }
            }
            Constraint::Qualified {
                shape: nested,
                min,
                max,
                siblings,
            } => {
                let mut count = 0u64;
                for &value in values {
                    self.ticker.tick(1).map_err(stopped)?;
                    if !self.conforms(*nested, value)? {
                        continue;
                    }
                    let mut sibling_matches = false;
                    for &sibling in siblings {
                        if self.conforms(sibling, value)? {
                            sibling_matches = true;
                            break;
                        }
                    }
                    if !sibling_matches {
                        count += 1;
                    }
                }
                if min.is_some_and(|min| count < min) {
                    results.push(self.raw(shape, focus, Component::QualifiedMinCount, None))?;
                }
                if max.is_some_and(|max| count > max) {
                    results.push(self.raw(shape, focus, Component::QualifiedMaxCount, None))?;
                }
            }
            Constraint::Closed(allowed) => {
                for &value in values {
                    let edges = validator
                        .graph
                        .edges_within(value, budget, &mut self.ticker)?;
                    for &(predicate, object) in edges.iter() {
                        self.ticker.tick(1).map_err(stopped)?;
                        if !allowed.contains(&predicate) {
                            let mut row = self.raw(shape, focus, Component::Closed, Some(object));
                            row.path = Some(Cow::Owned(Path::Predicate(predicate)));
                            results.push(row)?;
                        }
                    }
                }
            }
            Constraint::HasValue(expected) => {
                if !contains_within(values, expected, &mut self.ticker)? {
                    results.push(self.raw(shape, focus, Component::HasValue, None))?;
                }
            }
            Constraint::In(allowed) => {
                self.each(shape, focus, Component::In, values, results, &mut |_, v| {
                    Ok(allowed.contains(&v))
                })?
            }
            Constraint::Sparql(check) => self.sparql(shape, focus, values, check, results)?,
        }
        Ok(())
    }

    fn failure(&mut self, focus: TermId, problem: &dyn std::fmt::Display) -> Result<()> {
        let validator = self.validator;
        let mut held = Reservation::new(&*validator.budget);
        let term = self.decode(focus, &mut held)?;
        let mut writer = Text {
            bytes: ChargedVec::new(&*validator.budget),
            refused: None,
        };
        let formatted = match &term {
            Some(term) => write!(
                writer,
                "a SPARQL constraint at focus node {term}: {problem}"
            ),
            None => write!(writer, "a SPARQL constraint at focus node : {problem}"),
        };
        if let Some(error) = writer.refused {
            return Err(error.into());
        }
        formatted.map_err(|_| {
            QueryEvaluationError::Unexpected("formatting a SHACL failure failed".into())
        })?;
        let (bytes, token) = writer.bytes.into_parts();
        let message = String::from_utf8(bytes).expect("fmt writes UTF-8");
        self.held.absorb(token);
        self.failures.push(message)?;
        self.ticker.check().map_err(stopped)?;
        Ok(())
    }

    fn sparql<B: Deref<Target = Budget>>(
        &mut self,
        shape_ref: ShapeRef,
        focus: TermId,
        values: &[TermId],
        check: &'s SparqlCheck,
        results: &mut ChargedVec<RawResult<'s>, B>,
    ) -> Result<()> {
        let validator = self.validator;
        let program = &validator.shapes.programs[check.program];
        let shape = &validator.shapes.shapes[shape_ref];
        let parameters = &validator.shapes.parameter_values[check.parameters.clone()];
        let (view, data) = validator.graph.parts();
        let raw = |value, path| RawResult {
            focus,
            path,
            value,
            shape: shape_ref,
            component: program.component,
            source: Some(program.source),
            messages: (!program.messages.is_empty()).then_some(program.messages.as_slice()),
        };
        let outcome = sparql::run(
            view,
            data,
            program,
            parameters,
            focus,
            values,
            shape.node,
            validator.shapes.graph,
            validator.resources,
            &validator.budget,
            &mut self.ticker,
        );
        match outcome {
            Err(
                error @ (QueryEvaluationError::Cancelled | QueryEvaluationError::MemoryLimit(_)),
            ) => return Err(error),
            Err(error) => self.failure(focus, &error)?,
            Ok(sparql::Outcome::Failed(failed)) => {
                for &value in failed.iter() {
                    self.ticker.tick(1).map_err(stopped)?;
                    results.push(raw(Some(value), shape.path.as_ref().map(Cow::Borrowed)))?;
                }
            }
            Ok(sparql::Outcome::Solutions { found, .. }) => {
                for solution in found.iter() {
                    self.ticker.tick(1).map_err(stopped)?;
                    if solution.failure {
                        self.failure(focus, &"the query reported ?failure")?;
                        continue;
                    }
                    let value = match &solution.value {
                        Some(term) => validator.graph.lookup(term),
                        None => shape.path.is_none().then_some(focus),
                    };
                    let path = solution
                        .path
                        .as_ref()
                        .and_then(|p| view.lookup(p.as_ref().into()))
                        .map(|p| Cow::Owned(Path::Predicate(p)))
                        .or_else(|| shape.path.as_ref().map(Cow::Borrowed));
                    results.push(raw(value, path))?;
                }
            }
        }
        Ok(())
    }
}

// A private fmt error bridge: the original typed refusal is restored at the caller.
struct Text<'b> {
    bytes: ChargedVec<u8, &'b Budget>,
    refused: Option<BudgetExceeded>,
}
impl std::fmt::Write for Text<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.bytes.extend_from_slice(text.as_bytes()).map_err(|e| {
            self.refused = Some(e);
            std::fmt::Error
        })
    }
}

fn subclasses<V: ReadView>(
    graph: &GraphView<'_, V>,
    predicate: Option<TermId>,
    class: TermId,
    budget: &Arc<Budget>,
    ticker: &mut Ticker,
) -> Result<(Vec<TermId>, Reservation)> {
    let Some(predicate) = predicate else {
        return Ok(ChargedVec::collect(budget.clone(), 1, [class])?.into_parts());
    };
    let mut seen_held = Reservation::new(&**budget);
    let mut seen = HashMap::new();
    let mut frontier = ChargedVec::new(&**budget);
    reserve_map(&mut seen, &mut seen_held)?;
    seen.insert(class, ());
    frontier.push(class)?;
    let mut next = 0;
    while next < frontier.len() {
        ticker.tick(1).map_err(stopped)?;
        let class = frontier[next];
        next += 1;
        {
            let children = graph.subjects_within(predicate, class, budget, ticker)?;
            for &child in children.iter() {
                ticker.tick(1).map_err(stopped)?;
                if !seen.contains_key(&child) {
                    reserve_map(&mut seen, &mut seen_held)?;
                    seen.insert(child, ());
                    frontier.push(child)?;
                }
            }
        }
    }
    let kept = sort_set(&mut frontier, ticker)?;
    frontier.truncate(kept);
    let (nodes, token) = frontier.into_parts();
    // Reuse the BFS allocation as the final sorted class set, retaining its charge.
    Ok((nodes, token.into_shared(budget)))
}
