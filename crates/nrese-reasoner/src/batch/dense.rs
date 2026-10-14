//! The ids a materialisation meets, numbered densely in their order: its input's, and
//! those its rules, schema and list vocabulary name. A rule derives no id it doesn't
//! meet, so the numbers cover the closure; each fits 32 bits, so the working set packs a
//! pair into one word ([`Representation`]), and the order of the numbers is the ids', so runs sorted
//! by number are sorted by id, and a range of ids (a guard's) is a range of numbers.
//!
//! Number 0 stands for no id (the bindings a violation leaves unbound read 0, as before):
//! the ids are numbered from 1.

use super::*;
use crate::ir::Guard;

/// The ids of a materialisation and their numbers ([`Dense::number`]).
pub(super) struct Dense {
    /// `ids[n]` is the id numbered `n`, ascending; `ids[0]` is 0, for no id.
    ids: Arc<[u64]>,
}

impl Dense {
    /// The numbering of `ids` (any order, repeated); `None` if they are 2^32 or more.
    pub(super) fn new(mut ids: Vec<u64>, capacity: usize) -> Option<Self> {
        ids.par_sort_unstable();
        ids.dedup();
        if ids.len() > capacity.min(u32::MAX as usize) {
            return None;
        }
        let mut table = Vec::with_capacity(ids.len() + 1);
        table.push(0);
        table.extend(ids);
        Some(Self { ids: table.into() })
    }

    /// The ids `input`, `rules`, `schema` and `lists` meet.
    pub(super) fn of(
        input: &Input,
        rules: &[Rule],
        lists: Option<&ListVocabulary>,
        schema: &Schema,
        capacity: usize,
    ) -> Option<Self> {
        let mut ids: Vec<u64> = match input {
            Input::Facts(facts) => facts
                .par_iter()
                .flat_map_iter(|t| t.iter().copied())
                .collect(),
            Input::Grouped(groups) => groups
                .par_iter()
                .flat_map_iter(|(p, pairs)| {
                    std::iter::once(*p).chain(pairs.iter().flat_map(|pair| pair.iter().copied()))
                })
                .collect(),
        };
        for rule in rules {
            for atom in rule.body.iter().chain(match &rule.head {
                Head::Facts(atoms) => atoms.as_slice(),
                Head::Inconsistent => &[],
            }) {
                ids.extend(atom.0.iter().filter_map(constant));
            }
            for guard in &rule.guards {
                match guard {
                    Guard::NotEqual(a, b) => ids.extend([a, b].into_iter().filter_map(constant)),
                    Guard::SameList(a, b, index) => {
                        ids.extend([a, b].into_iter().filter_map(constant));
                        ids.extend(index.ids());
                    }
                    Guard::NotIn(term, _, _) => ids.extend(constant(term)),
                }
            }
        }
        ids.extend(schema.ids());
        ids.extend(lists.iter().flat_map(|lists| lists.ids()));
        Self::new(ids, capacity)
    }

    /// The number of `id`, which the materialisation meets.
    #[inline]
    pub(super) fn number(&self, id: u64) -> u64 {
        self.ids[1..]
            .binary_search(&id)
            .map(|at| at as u64 + 1)
            .expect("an id the materialisation meets")
    }

    /// The id numbered `number`.
    #[inline]
    pub(super) fn id(&self, number: u64) -> u64 {
        self.ids[number as usize]
    }

    /// Each number's id, ascending ([`Self::id`]).
    pub(super) fn table(&self) -> Arc<[u64]> {
        Arc::clone(&self.ids)
    }

    /// The numbers of the ids in `low..=high`, as `low..=high` of numbers (empty, low
    /// above high, where no id met lies in it).
    fn numbers_within(&self, low: u64, high: u64) -> (u64, u64) {
        let ids = &self.ids[1..];
        let first = ids.partition_point(|&id| id < low) as u64 + 1;
        let after = ids.partition_point(|&id| id <= high) as u64;
        (first, after)
    }

    pub(super) fn term(&self, term: Term) -> Term {
        match term {
            Term::Const(c) => Term::Const(self.number(c)),
            variable => variable,
        }
    }

    pub(super) fn rule(&self, rule: &Rule) -> Rule {
        let atom = |a: &crate::ir::Atom| crate::ir::Atom(a.0.map(|t| self.term(t)));
        let number = |id: u64| self.number(id);
        Rule {
            name: rule.name.clone(),
            body: rule.body.iter().map(atom).collect(),
            guards: rule
                .guards
                .iter()
                .map(|guard| match guard {
                    Guard::NotEqual(a, b) => Guard::NotEqual(self.term(*a), self.term(*b)),
                    Guard::NotIn(term, low, high) => {
                        let (low, high) = self.numbers_within(*low, *high);
                        Guard::NotIn(self.term(*term), low, high)
                    }
                    Guard::SameList(a, b, index) => {
                        Guard::SameList(self.term(*a), self.term(*b), index.map_ids(&number))
                    }
                })
                .collect(),
            head: match &rule.head {
                Head::Facts(atoms) => Head::Facts(atoms.iter().map(atom).collect()),
                Head::Inconsistent => Head::Inconsistent,
            },
        }
    }

    pub(super) fn schema(&self, schema: &Schema) -> Schema {
        schema.map_ids(&|id| self.number(id))
    }

    pub(super) fn lists(&self, lists: &ListVocabulary) -> ListVocabulary {
        lists.map_ids(&|id| self.number(id))
    }

    /// `input` in numbers: facts as numbers, or each predicate's pairs packed in their
    /// order (numbering keeps it).
    pub(super) fn store(&self, input: Input) -> WorkingStore<u64> {
        match input {
            Input::Facts(facts) => WorkingStore::<u64>::dense(
                facts
                    .into_par_iter()
                    .map(|t| t.map(|id| self.number(id)))
                    .collect(),
            ),
            Input::Grouped(groups) => WorkingStore::<u64>::from_groups(
                groups
                    .into_par_iter()
                    .map(|(p, pairs)| {
                        // At their exact size: collected in place, the numbers would keep
                        // the pairs' allocation, twice theirs.
                        let mut numbers = Vec::with_capacity(pairs.len());
                        numbers.extend(pairs.iter().map(|&[o, s]| {
                            <u64 as Representation>::pair(self.number(o), self.number(s))
                        }));
                        (self.number(p), numbers)
                    })
                    .collect(),
            ),
        }
    }

    /// `result`, computed in numbers, in ids again.
    pub(super) fn back(&self, result: &mut Materialisation) {
        let id = |n: u64| self.id(n);
        result
            .derived
            .par_iter_mut()
            .for_each(|fact| *fact = fact.map(id));
        for relation in &mut result.relations {
            relation.predicate = id(relation.predicate);
            relation.attach_ids(self.table());
        }
        for violation in &mut result.violations {
            violation.bindings.iter_mut().for_each(|n| *n = id(*n));
        }
        for diagnostic in &mut result.diagnostics {
            diagnostic.subject = id(diagnostic.subject);
            diagnostic.predicate = id(diagnostic.predicate);
            diagnostic.head = id(diagnostic.head);
            if let crate::lists::ListProblem::Malformed { node }
            | crate::lists::ListProblem::Cycle { node } = &mut diagnostic.problem
            {
                *node = id(*node);
            }
        }
        if !result.classes.is_empty() {
            let pairs: Vec<(u64, u64)> = result
                .classes
                .classes()
                .flat_map(|(representative, members)| {
                    members.iter().map(move |&m| (id(representative), id(m)))
                })
                .collect();
            let mut classes = EqualityClasses::default();
            classes.union_all(&pairs);
            result.classes = classes;
        }
    }
}

fn constant(term: &Term) -> Option<u64> {
    match term {
        Term::Const(c) => Some(*c),
        Term::Var(_) => None,
    }
}
