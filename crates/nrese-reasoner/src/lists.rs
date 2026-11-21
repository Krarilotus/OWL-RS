//! List axioms compiled to fixed-arity rules (reasoner-v2 design §3.2, step 3).
//!
//! OWL 2 RL rules over RDF lists (`prp-spo2`, `prp-key`, `cls-int1/2`, `cls-uni`, `cls-oo`,
//! `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`) can't be written as fixed rules.
//! Instead each list axiom in the data becomes rules of its own arity. A chain of length
//! n becomes one n-atom rule, and an intersection one rule per direction. That removes
//! the `rdf:first`/`rdf:rest` helper rules that generic rulesets need.
//!
//! Every walk to `rdf:nil` supplies one `rdf:first` and one `rdf:rest` per cell.
//! The reader grounds short walks directly and uses fixed-arity rules per cell
//! where the walks are too large or cyclic. A head with no walk is diagnosed.
//!
//! Lists of any length are instantiated. The pairwise axioms (`owl:AllDifferent`,
//! `owl:AllDisjointClasses`, `owl:AllDisjointProperties`) would need a rule per pair, so
//! their lists longer than [`PAIRWISE_MEMBERS`] become one rule per kind of axiom whose
//! [`Guard::SameList`] asks an index of the members which lists they share: linear in the
//! members however many there are. The rule encoder's bounds select a form;
//! they leave no accepting walk unread.

use std::collections::{HashMap, HashSet};

use super::ir::{Atom, Guard, Head, OWL, RDF, Rule, Term, Vocabulary};

mod per_cell;
pub(crate) use per_cell::{Predicates, TableKind, internal, table_kind};

/// Read access to facts for instantiation.
pub trait Facts {
    /// Objects of `(subject, predicate, ?)`.
    fn objects(&self, subject: u64, predicate: u64) -> Vec<u64>;
    /// `(subject, object)` pairs of `(?, predicate, ?)`.
    fn pairs(&self, predicate: u64) -> Vec<(u64, u64)>;

    fn stopped(&self) -> bool {
        false
    }
}

/// The vocabulary ids the list rules use.
#[derive(Clone, PartialEq, Eq)]
pub struct ListVocabulary {
    rdf_type: u64,
    first: u64,
    rest: u64,
    nil: u64,
    same_as: u64,
    sub_class_of: u64,
    property_chain_axiom: u64,
    has_key: u64,
    intersection_of: u64,
    union_of: u64,
    one_of: u64,
    members: u64,
    distinct_members: u64,
    all_disjoint_classes: u64,
    all_disjoint_properties: u64,
    all_different: u64,
    /// The blank nodes' ids, when keys bind named individuals only.
    named_keys: Option<(u64, u64)>,
}

impl ListVocabulary {
    /// Keys that bind no individual whose id is in `blank_nodes` (`owl2-dl`).
    pub fn named_keys(mut self, blank_nodes: Option<(u64, u64)>) -> Self {
        self.named_keys = blank_nodes;
        self
    }

    pub fn new(vocabulary: &mut impl Vocabulary) -> Self {
        let mut rdf = |local: &str| vocabulary.iri(&format!("{RDF}{local}"));
        let (rdf_type, first, rest, nil) = (rdf("type"), rdf("first"), rdf("rest"), rdf("nil"));
        let mut owl = |local: &str| vocabulary.iri(&format!("{OWL}{local}"));
        Self {
            rdf_type,
            first,
            rest,
            nil,
            same_as: owl("sameAs"),
            sub_class_of: vocabulary.iri("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
            property_chain_axiom: vocabulary.iri(&format!("{OWL}propertyChainAxiom")),
            has_key: vocabulary.iri(&format!("{OWL}hasKey")),
            intersection_of: vocabulary.iri(&format!("{OWL}intersectionOf")),
            union_of: vocabulary.iri(&format!("{OWL}unionOf")),
            one_of: vocabulary.iri(&format!("{OWL}oneOf")),
            members: vocabulary.iri(&format!("{OWL}members")),
            distinct_members: vocabulary.iri(&format!("{OWL}distinctMembers")),
            all_disjoint_classes: vocabulary.iri(&format!("{OWL}AllDisjointClasses")),
            all_disjoint_properties: vocabulary.iri(&format!("{OWL}AllDisjointProperties")),
            all_different: vocabulary.iri(&format!("{OWL}AllDifferent")),
            named_keys: None,
        }
    }

    /// `rdf:nil`, where every list ends.
    pub(crate) fn nil(&self) -> u64 {
        self.nil
    }

    /// The vocabulary as named by a fact source that has renamed equal terms.
    pub(crate) fn projected(&self, named: impl Fn(u64) -> u64) -> Option<Self> {
        let vocabulary = Self {
            rdf_type: named(self.rdf_type),
            first: named(self.first),
            rest: named(self.rest),
            nil: named(self.nil),
            same_as: named(self.same_as),
            sub_class_of: named(self.sub_class_of),
            property_chain_axiom: named(self.property_chain_axiom),
            has_key: named(self.has_key),
            intersection_of: named(self.intersection_of),
            union_of: named(self.union_of),
            one_of: named(self.one_of),
            members: named(self.members),
            distinct_members: named(self.distinct_members),
            all_disjoint_classes: named(self.all_disjoint_classes),
            all_disjoint_properties: named(self.all_disjoint_properties),
            all_different: named(self.all_different),
            named_keys: self.named_keys,
        };
        (vocabulary != *self).then_some(vocabulary)
    }

    pub(crate) fn is_list_predicate(&self, p: u64) -> bool {
        [
            self.first,
            self.rest,
            self.property_chain_axiom,
            self.has_key,
            self.intersection_of,
            self.union_of,
            self.one_of,
            self.members,
            self.distinct_members,
        ]
        .contains(&p)
    }

    pub(crate) fn reads_predicate(&self, p: u64) -> bool {
        p == self.rdf_type || self.is_list_predicate(p)
    }

    /// Whether `fact` is list structure or a list axiom: the facts [`instantiate`] reads.
    pub fn is_list_fact(&self, [_, p, o]: [u64; 3]) -> bool {
        self.is_list_predicate(p)
            || (p == self.rdf_type
                && [
                    self.all_disjoint_classes,
                    self.all_disjoint_properties,
                    self.all_different,
                ]
                .contains(&o))
    }

    fn nil_is_cell(&self, facts: &impl Facts) -> bool {
        !facts.objects(self.nil, self.first).is_empty()
            && !facts.objects(self.nil, self.rest).is_empty()
    }

    pub(crate) fn leaves_out(&self, facts: &impl Facts) -> bool {
        let nil_is_cell = std::cell::OnceCell::new();
        [
            self.property_chain_axiom,
            self.has_key,
            self.intersection_of,
            self.union_of,
            self.one_of,
            self.members,
            self.distinct_members,
        ]
        .into_iter()
        .any(|p| {
            facts.pairs(p).into_iter().any(|(_, head)| {
                let nil_is_cell = *nil_is_cell.get_or_init(|| self.nil_is_cell(facts));
                if !nil_is_cell && let Some(linear) = self.linear_list(facts, head) {
                    return linear.is_err();
                }
                self.accepting_cells(facts, head).is_err()
            })
        })
    }

    /// Walks of the enumerated form, or the accepting cells of the other form.
    fn list(
        &self,
        facts: &impl Facts,
        head: u64,
        nil_is_cell: bool,
        kind: ListKind,
    ) -> Result<List, ListProblem> {
        if !nil_is_cell && let Some(linear) = self.linear_list(facts, head) {
            let walks = linear?;
            if kind.fits(&walks, self.same_as) {
                return Ok(List::Walks(walks, None));
            }
            return self.accepting_cells(facts, head).map(List::Cells);
        }
        let cells = self.accepting_cells(facts, head)?;
        if let Some((walks, cut)) = self.enumerate(facts, &cells, head, kind)
            && kind.fits(&walks, self.same_as)
        {
            let extra_members = (cut && matches!(kind, ListKind::Intersection)).then_some(cells);
            return Ok(List::Walks(walks, extra_members));
        }
        Ok(List::Cells(cells))
    }

    /// The one walk when every cell has exactly one first and one rest. In this
    /// case a broken cell or a repeated node also decides the whole reading.
    fn linear_list(
        &self,
        facts: &impl Facts,
        head: u64,
    ) -> Option<Result<Vec<Variant>, ListProblem>> {
        let (mut members, mut used) = (Vec::new(), Vec::<[u64; 3]>::new());
        let mut node = head;
        while node != self.nil {
            if facts.stopped() {
                return Some(Err(ListProblem::Malformed { node }));
            }
            if used.iter().step_by(2).any(|t| t[0] == node) {
                return Some(Err(ListProblem::Cycle { node }));
            }
            let firsts = facts.objects(node, self.first);
            let rests = facts.objects(node, self.rest);
            if firsts.is_empty() || rests.is_empty() {
                return Some(Err(ListProblem::Malformed { node }));
            }
            let ([first], [rest]) = (firsts.as_slice(), rests.as_slice()) else {
                return None;
            };
            members.push(*first);
            used.extend([[node, self.first, *first], [node, self.rest, *rest]]);
            node = *rest;
        }
        Some(Ok(vec![(members, used)]))
    }

    /// Read the graph reachable from this head, then follow its incoming edges
    /// back from nil. A cell without a first cannot supply an incoming edge.
    fn accepting_cells(
        &self,
        facts: &impl Facts,
        head: u64,
    ) -> Result<HashMap<u64, Cell>, ListProblem> {
        let mut cells = HashMap::new();
        let mut incoming = HashMap::<u64, Vec<u64>>::new();
        let mut pending = vec![head];
        while let Some(node) = pending.pop() {
            if facts.stopped() {
                return Err(ListProblem::Malformed { node: head });
            }
            if cells.contains_key(&node) {
                continue;
            }
            let firsts = facts.objects(node, self.first);
            let rests = if firsts.is_empty() {
                Vec::new()
            } else {
                facts.objects(node, self.rest)
            };
            for &rest in &rests {
                incoming.entry(rest).or_default().push(node);
                pending.push(rest);
            }
            cells.insert(node, Cell { firsts, rests });
        }

        let mut accepting = HashSet::from([self.nil]);
        pending.push(self.nil);
        while let Some(node) = pending.pop() {
            if facts.stopped() {
                return Err(ListProblem::Malformed { node: head });
            }
            for cell in incoming.remove(&node).into_iter().flatten() {
                if accepting.insert(cell) {
                    pending.push(cell);
                }
            }
        }
        if !accepting.contains(&head) {
            return Err(first_problem(&cells, head));
        }
        cells.retain(|node, cell| {
            cell.rests.retain(|rest| accepting.contains(rest));
            accepting.contains(node)
        });
        Ok(cells)
    }

    fn enumerate(
        &self,
        facts: &impl Facts,
        cells: &HashMap<u64, Cell>,
        head: u64,
        kind: ListKind,
    ) -> Option<(Vec<Variant>, bool)> {
        let conjunctive = matches!(kind, ListKind::Key | ListKind::Intersection);
        // An outgoing accepting edge from nil lets a walk return there.
        let mut cut = !cells[&self.nil].rests.is_empty();
        if !conjunctive && cut {
            return None;
        }
        // Count attempted prefixes, back edges, and the work of copying each
        // finished walk. A densely connected graph may have very few exits.
        let mut work_left = cells
            .values()
            .fold(cells.len(), |n, cell| {
                n.saturating_add(cell.firsts.len())
                    .saturating_add(cell.rests.len())
            })
            .saturating_mul(8);
        let mut variants = Vec::new();
        let (mut members, mut used) = (Vec::new(), Vec::new());
        let mut active = HashSet::from([head]);
        // Each frame is a cell and the next combination of first and rest at it.
        let mut stack = vec![(head, 0, 0)];
        while let Some((node, first, rest)) = stack.last_mut() {
            work_left = work_left.checked_sub(1)?;
            if facts.stopped() {
                return None;
            }
            let cell = &cells[&*node];
            if *node == self.nil && !members.is_empty() {
                if variants.len() == MAX_VARIANTS || members.len() > kind.member_limit() {
                    return None;
                }
                work_left = work_left.checked_sub(members.len())?;
                variants.push((members.clone(), used.clone()));
            } else if let Some(&member) = cell.firsts.get(*first)
                && let Some(&next) = cell.rests.get(*rest)
            {
                *rest += 1;
                if *rest == cell.rests.len() {
                    *first += 1;
                    *rest = 0;
                }
                if next != self.nil && !active.insert(next) {
                    if !conjunctive {
                        return None;
                    }
                    cut = true;
                    continue;
                }
                members.push(member);
                used.extend([[*node, self.first, member], [*node, self.rest, next]]);
                stack.push((next, 0, 0));
                continue;
            }
            active.remove(node);
            stack.pop();
            members.pop();
            used.truncate(2 * members.len());
        }
        Some((variants, cut))
    }

    fn each_member(
        &self,
        facts: &impl Facts,
        cells: &HashMap<u64, Cell>,
        head: u64,
        mut emit: impl FnMut(u64, Vec<[u64; 3]>),
    ) {
        for (&node, cell) in cells {
            if facts.stopped() {
                return;
            }
            if let Some(mut prefix) = self.path(facts, cells, head, node, false)
                && let Some(suffix) = self.path(facts, cells, node, self.nil, true)
            {
                let offset = prefix.len();
                prefix.extend(suffix);
                for &member in &cell.firsts {
                    if facts.stopped() {
                        return;
                    }
                    prefix[offset][2] = member;
                    emit(member, prefix.clone());
                }
            }
        }
    }

    /// One path as first/rest premises, including a first at each position.
    fn path(
        &self,
        facts: &impl Facts,
        cells: &HashMap<u64, Cell>,
        from: u64,
        to: u64,
        nonempty: bool,
    ) -> Option<Vec<[u64; 3]>> {
        if from == to && !nonempty {
            return Some(Vec::new());
        }
        let mut parent = HashMap::from([(from, from)]);
        let mut pending = std::collections::VecDeque::from([from]);
        while let Some(node) = pending.pop_front() {
            if facts.stopped() {
                return None;
            }
            for &next in &cells[&node].rests {
                if next == to {
                    let mut path = Vec::new();
                    let (mut prev, mut end) = (node, to);
                    loop {
                        path.extend([
                            [prev, self.rest, end],
                            [prev, self.first, cells[&prev].firsts[0]],
                        ]);
                        if prev == from {
                            path.reverse();
                            return Some(path);
                        }
                        end = prev;
                        prev = parent[&prev];
                    }
                }
                if let std::collections::hash_map::Entry::Vacant(entry) = parent.entry(next) {
                    entry.insert(node);
                    pending.push_back(next);
                }
            }
        }
        None
    }
}

struct Cell {
    firsts: Vec<u64>,
    rests: Vec<u64>,
}

enum List {
    /// Short walks, with a cell graph when cutting cycles or nil continuations
    /// can omit members from the outward rules of an intersection.
    Walks(Vec<Variant>, Option<HashMap<u64, Cell>>),
    Cells(HashMap<u64, Cell>),
}

/// With no walk to nil, every branch fails. Report the first failure in the
/// same order as when reading all the walks, including the offending cell.
fn first_problem(cells: &HashMap<u64, Cell>, mut node: u64) -> ListProblem {
    let mut seen = HashSet::new();
    while seen.insert(node) {
        match cells[&node].rests.first() {
            Some(&rest) => node = rest,
            None => return ListProblem::Malformed { node },
        }
    }
    ListProblem::Cycle { node }
}

/// A member sequence of a list, with the facts its path uses.
type Variant = (Vec<u64>, Vec<[u64; 3]>);

#[derive(Clone, Copy)]
enum ListKind {
    Chain,
    Key,
    Intersection,
    Union,
    Pairwise,
}

impl ListKind {
    fn fits(self, walks: &[Variant], same_as: u64) -> bool {
        walks
            .iter()
            .all(|(walk, _)| walk.len() <= self.member_limit())
            && (!matches!(self, Self::Chain)
                || walks.iter().fold(0usize, |sum, (walk, _)| {
                    let diagonals = walk.iter().filter(|&&member| member == same_as).count();
                    sum.saturating_add(1usize.checked_shl(diagonals as u32).unwrap_or(usize::MAX))
                }) <= MAX_VARIANTS)
    }

    fn member_limit(self) -> usize {
        match self {
            Self::Chain => MAX_RULE_MEMBERS,
            Self::Key => MAX_KEY_MEMBERS,
            Self::Intersection => crate::eval::MAX_RULE_ATOMS,
            Self::Pairwise | Self::Union => usize::MAX,
        }
    }
}

/// The most accepting walks compiled as ordinary rules for one list axiom.
pub const MAX_VARIANTS: usize = 64;

/// Lists of pairwise axioms up to this length become a rule per pair; longer ones are
/// checked through a [`super::ir::ListIndex`] (an implementation choice, not a limit).
pub const PAIRWISE_MEMBERS: usize = 100;

/// The most properties of a chain or a key that one rule can hold (its variables are
/// numbered by `u8`).
pub const MAX_RULE_MEMBERS: usize = 250;

/// The most properties of a key that one rule can hold: its rule has two atoms per property
/// and two more, and a rule's atoms are numbered by `u8`.
pub const MAX_KEY_MEMBERS: usize = 127;

/// Why a list axiom wasn't instantiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListProblem {
    /// `node` lacks `rdf:first` or `rdf:rest` (the list doesn't end at `rdf:nil`).
    Malformed { node: u64 },
    /// The list returns to `node`.
    Cycle { node: u64 },
}

impl ListProblem {
    /// A stable name for reports of heads with no accepting walk.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Malformed { .. } => "malformed-list",
            Self::Cycle { .. } => "cyclic-list",
        }
    }

    /// The node the problem is at, if one.
    pub const fn node(self) -> Option<u64> {
        match self {
            Self::Malformed { node } | Self::Cycle { node } => Some(node),
        }
    }
}

/// A list axiom the reasoner skipped: its rules are missing from the closure. Lists are
/// never truncated, so this is the whole effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListDiagnostic {
    /// The OWL 2 RL rules the axiom feeds (`prp-spo2`, `cls-int`, …).
    pub rules: &'static str,
    /// The axiom `(subject predicate head)`; `head` starts the list.
    pub subject: u64,
    pub predicate: u64,
    pub head: u64,
    pub problem: ListProblem,
}

impl ListDiagnostic {
    /// A sentence for reports, with `term` rendering ids.
    pub fn describe(&self, term: &dyn Fn(u64) -> String) -> String {
        let problem = match self.problem {
            ListProblem::Malformed { node } => {
                format!("node {} has no rdf:first or no rdf:rest", term(node))
            }
            ListProblem::Cycle { node } => format!("the list returns to node {}", term(node)),
        };
        format!(
            "{} {} {} skipped ({}): {problem}",
            term(self.subject),
            term(self.predicate),
            term(self.head),
            self.rules
        )
    }
}

fn v(n: usize) -> Term {
    Term::Var(u8::try_from(n).expect("list rules stay below 256 variables"))
}

fn c(id: u64) -> Term {
    Term::Const(id)
}

fn rule(name: &str, body: Vec<Atom>, guards: Vec<Guard>, head: Head) -> Rule {
    Rule {
        name: name.to_owned(),
        body,
        guards,
        head,
    }
}

/// An equality operand may use the diagonal even when no other atom binds its
/// individual. Bind that individual at any position in an RDF fact.
fn diagonal(predicate: Term) -> impl Iterator<Item = Rule> {
    (0..3).map(move |position| {
        let mut occurrence = [v(1), v(2), v(3)];
        occurrence[position] = v(0);
        rule(
            "prp-spo2",
            vec![Atom(occurrence)],
            vec![],
            Head::Facts(vec![Atom([v(0), predicate, v(0)])]),
        )
    })
}

/// Turns one list axiom (its subject and members) into rules.
type MakeRules<'a> = dyn FnMut(u64, &[u64], &mut Vec<Rule>) + 'a;

/// The rules for every list axiom in `facts`, plus diagnostics for malformed lists.
pub fn instantiate(
    vocabulary: &ListVocabulary,
    facts: &impl Facts,
) -> (Vec<Rule>, Vec<ListDiagnostic>) {
    let (rules, _, diagnostics) = instantiate_with_premises(vocabulary, facts);
    (rules, diagnostics)
}

/// [`instantiate`], with the facts each rule comes from (its premises): the axiom, the
/// list path's `rdf:first`/`rdf:rest` facts, and the axiom node's `rdf:type` facts for
/// `owl:members` / `owl:distinctMembers` axioms.
pub fn instantiate_with_premises(
    vocabulary: &ListVocabulary,
    facts: &impl Facts,
) -> (Vec<Rule>, Vec<Vec<[u64; 3]>>, Vec<ListDiagnostic>) {
    compile(vocabulary, facts, &mut Predicates::default(), RuleKind::All)
}

#[derive(Clone, Copy)]
pub(crate) enum RuleKind {
    All,
    Inference,
    Consistency,
}

pub(crate) fn compile(
    voc: &ListVocabulary,
    facts: &impl Facts,
    predicates: &mut Predicates,
    rule_kind: RuleKind,
) -> (Vec<Rule>, Vec<Vec<[u64; 3]>>, Vec<ListDiagnostic>) {
    #[cfg(test)]
    crate::witness::add(crate::witness::Site::ListGrounding, 1);
    let mut rules = Vec::new();
    let mut premises: Vec<Vec<[u64; 3]>> = Vec::new();
    let mut diagnostics: Vec<ListDiagnostic> = Vec::new();
    let nil_is_cell = std::cell::OnceCell::new();
    let mut lists = |predicate,
                     name: &'static str,
                     kind,
                     rules: &mut Vec<Rule>,
                     make: &mut MakeRules| {
        debug_assert!(voc.is_list_fact([0, predicate, 0]), "read, not a list fact");
        match (rule_kind, kind) {
            (RuleKind::Inference, ListKind::Pairwise)
            | (
                RuleKind::Consistency,
                ListKind::Chain | ListKind::Key | ListKind::Intersection | ListKind::Union,
            ) => return,
            _ => {}
        }
        for (subject, head) in facts.pairs(predicate) {
            if facts.stopped() {
                return;
            }
            let axiom = [subject, predicate, head];
            let nil_is_cell = *nil_is_cell.get_or_init(|| voc.nil_is_cell(facts));
            let list = match voc.list(facts, head, nil_is_cell, kind) {
                Ok(list) => list,
                Err(problem) => {
                    diagnostics.push(ListDiagnostic {
                        rules: name,
                        subject,
                        predicate,
                        head,
                        problem,
                    });
                    continue;
                }
            };
            let classes = if matches!(kind, ListKind::Pairwise) {
                facts.objects(subject, voc.rdf_type)
            } else {
                Vec::new()
            };
            let mut make_walk = |members: &[u64], mut used: Vec<[u64; 3]>, unary: bool| {
                let before = rules.len();
                make(subject, members, rules);
                if unary {
                    if rules
                        .get(before)
                        .is_some_and(|rule| rule.name == "cls-int1")
                    {
                        rules.remove(before);
                    }
                }
                used.push(axiom);
                for class in [
                    voc.all_disjoint_classes,
                    voc.all_disjoint_properties,
                    voc.all_different,
                ] {
                    if classes.contains(&class) {
                        used.push([subject, voc.rdf_type, class]);
                    }
                }
                debug_assert!(rules[before..].iter().all(Rule::fits));
                for _ in before..rules.len() {
                    premises.push(used.clone());
                }
            };
            match list {
                List::Walks(walks, extra_members) => {
                    let mut seen = HashSet::new();
                    for (members, used) in walks {
                        if extra_members.is_some() {
                            seen.extend(members.iter().copied());
                        }
                        make_walk(&members, used, false);
                    }
                    if let Some(cells) = extra_members {
                        voc.each_member(facts, &cells, head, |member, used| {
                            if seen.insert(member) {
                                make_walk(&[member], used, true);
                            }
                        });
                    }
                }
                List::Cells(cells) => {
                    if matches!(kind, ListKind::Union | ListKind::Intersection) {
                        voc.each_member(facts, &cells, head, |member, used| {
                            make_walk(&[member], used, true);
                        });
                    }
                    if matches!(kind, ListKind::Pairwise) {
                        for (&node, cell) in &cells {
                            if facts.stopped() {
                                return;
                            }
                            let Some(prefix) = voc.path(facts, &cells, head, node, false) else {
                                continue;
                            };
                            for (&next, tail) in &cells {
                                let Some(mut between) = voc.path(facts, &cells, node, next, true)
                                else {
                                    continue;
                                };
                                let Some(mut suffix) = voc.path(facts, &cells, next, voc.nil, true)
                                else {
                                    continue;
                                };
                                for &a in &cell.firsts {
                                    between[0][2] = a;
                                    for &b in &tail.firsts {
                                        if facts.stopped() {
                                            return;
                                        }
                                        suffix[0][2] = b;
                                        let mut used = prefix.clone();
                                        used.extend_from_slice(&between);
                                        used.extend_from_slice(&suffix);
                                        make_walk(&[a, b], used, false);
                                    }
                                }
                            }
                        }
                    }
                    if matches!(kind, ListKind::Chain)
                        && let Some(cell) = cells
                            .get(&head)
                            .filter(|cell| cell.firsts.contains(&subject))
                    {
                        for &rest in &cell.rests {
                            if cells.get(&rest).is_some_and(|tail| {
                                tail.firsts.contains(&subject) && tail.rests.contains(&voc.nil)
                            }) {
                                make_walk(
                                    &[subject, subject],
                                    vec![
                                        [head, voc.first, subject],
                                        [head, voc.rest, rest],
                                        [rest, voc.first, subject],
                                        [rest, voc.rest, voc.nil],
                                    ],
                                    false,
                                );
                            }
                        }
                    }
                    if matches!(
                        kind,
                        ListKind::Chain | ListKind::Key | ListKind::Intersection
                    ) {
                        per_cell::compile(
                            voc,
                            kind,
                            axiom,
                            &cells,
                            predicates,
                            &|| facts.stopped(),
                            |rule, used| {
                                rules.push(rule);
                                premises.push(used);
                            },
                        );
                    }
                }
            }
        }
    };
    let ty = voc.rdf_type;
    // prp-spo2: (x0 p1 x1) … (x(n-1) pn xn) -> (x0 p xn)
    lists(
        voc.property_chain_axiom,
        "prp-spo2",
        ListKind::Chain,
        &mut rules,
        &mut |p, chain, rules| {
            if chain.is_empty() {
                return;
            }
            let diagonals = chain.iter().filter(|&&q| q == voc.same_as).count();
            for mut mask in 0..1usize << diagonals {
                let mut body = Vec::new();
                for &q in chain {
                    if q == voc.same_as {
                        let reflexive = mask & 1 != 0;
                        mask >>= 1;
                        if reflexive {
                            continue;
                        }
                    }
                    let i = body.len();
                    body.push(Atom([v(i), c(q), v(i + 1)]));
                }
                if body.is_empty() {
                    rules.extend(diagonal(c(p)));
                } else {
                    let last = v(body.len());
                    rules.push(rule(
                        "prp-spo2",
                        body,
                        vec![],
                        Head::Facts(vec![Atom([v(0), c(p), last])]),
                    ));
                }
            }
        },
    );
    // prp-key: two instances of c agreeing on every key property are the same.
    lists(
        voc.has_key,
        "prp-key",
        ListKind::Key,
        &mut rules,
        &mut |class, keys, rules| {
            if keys.is_empty() || keys.contains(&voc.same_as) {
                return;
            }
            let (x, y) = (v(0), v(1));
            let mut body = vec![Atom([x, c(ty), c(class)]), Atom([y, c(ty), c(class)])];
            for (i, &p) in keys.iter().enumerate() {
                body.push(Atom([x, c(p), v(i + 2)]));
                body.push(Atom([y, c(p), v(i + 2)]));
            }
            let mut guards = vec![Guard::NotEqual(x, y)];
            // Under the Direct Semantics: no blank node as an instance or a value.
            if let Some((low, high)) = voc.named_keys {
                guards.extend((0..keys.len() + 2).map(|i| Guard::NotIn(v(i), low, high)));
            }
            rules.push(rule(
                "prp-key",
                body,
                guards,
                Head::Facts(vec![Atom([x, c(voc.same_as), y])]),
            ));
        },
    );
    // cls-int1, cls-int2, scm-int
    lists(
        voc.intersection_of,
        "cls-int",
        ListKind::Intersection,
        &mut rules,
        &mut |class, members, rules| {
            // With no members, cls-int1 has no atom to bind the instance.
            if members.is_empty() {
                return;
            }
            let body = members
                .iter()
                .map(|&ci| Atom([v(0), c(ty), c(ci)]))
                .collect();
            rules.push(rule(
                "cls-int1",
                body,
                vec![],
                Head::Facts(vec![Atom([v(0), c(ty), c(class)])]),
            ));
            for &ci in members {
                rules.push(rule(
                    "cls-int2",
                    vec![Atom([v(0), c(ty), c(class)])],
                    vec![],
                    Head::Facts(vec![Atom([v(0), c(ty), c(ci)])]),
                ));
                rules.push(rule(
                    "scm-int",
                    vec![],
                    vec![],
                    Head::Facts(vec![Atom([c(class), c(voc.sub_class_of), c(ci)])]),
                ));
            }
        },
    );
    // cls-uni, scm-uni
    lists(
        voc.union_of,
        "cls-uni",
        ListKind::Union,
        &mut rules,
        &mut |class, members, rules| {
            for &ci in members {
                rules.push(rule(
                    "cls-uni",
                    vec![Atom([v(0), c(ty), c(ci)])],
                    vec![],
                    Head::Facts(vec![Atom([v(0), c(ty), c(class)])]),
                ));
                rules.push(rule(
                    "scm-uni",
                    vec![],
                    vec![],
                    Head::Facts(vec![Atom([c(ci), c(voc.sub_class_of), c(class)])]),
                ));
            }
        },
    );
    // cls-oo: every listed individual is an instance.
    lists(
        voc.one_of,
        "cls-oo",
        ListKind::Union,
        &mut rules,
        &mut |class, members, rules| {
            let heads = members
                .iter()
                .map(|&y| Atom([c(y), c(ty), c(class)]))
                .collect();
            rules.push(rule("cls-oo", vec![], vec![], Head::Facts(heads)));
        },
    );
    // cax-adc, eq-diff2, prp-adp: pairwise over `members`, by the type of the axiom node.
    let typed = |node: u64, class: u64| {
        debug_assert!(voc.is_list_fact([node, ty, class]), "read, not a list fact");
        facts.objects(node, ty).contains(&class)
    };
    // The premise that makes two members of an AllDifferent axiom inconsistent: `a sameAs b`,
    // or, when the list names one individual twice, the axiom itself (`a sameAs a` holds by
    // eq-ref, which isn't materialised).
    let different_members = |a: u64, b: u64, node: u64| {
        if a == b {
            Atom([c(node), c(ty), c(voc.all_different)])
        } else {
            Atom([c(a), c(voc.same_as), c(b)])
        }
    };
    // Long lists of the pairwise axioms, by kind: their members indexed, and the axioms'
    // type facts (the rules' schema premises).
    let long = std::cell::RefCell::new(Long::default());
    lists(
        voc.members,
        "cax-adc, prp-adp, eq-diff2",
        ListKind::Pairwise,
        &mut rules,
        &mut |node, members, rules| {
            if members.len() > PAIRWISE_MEMBERS {
                let mut long = long.borrow_mut();
                for (index, kind) in [
                    voc.all_disjoint_classes,
                    voc.all_different,
                    voc.all_disjoint_properties,
                ]
                .into_iter()
                .enumerate()
                {
                    if typed(node, kind) {
                        long.indexes[index].add(members);
                        long.premises[index].push([node, ty, kind]);
                        if index == 1 {
                            duplicates(members, node, voc, rules, "eq-diff2");
                        }
                        // A class or property it names twice, which the index pairs with no
                        // other: the rule a short list makes of that pair.
                        for &a in &repeated(members) {
                            if index == 0 {
                                let body =
                                    vec![Atom([v(0), c(ty), c(a)]), Atom([v(0), c(ty), c(a)])];
                                rules.push(rule("cax-adc", body, vec![], Head::Inconsistent));
                            }
                            if index == 2 {
                                let body = vec![Atom([v(0), c(a), v(1)]), Atom([v(0), c(a), v(1)])];
                                rules.push(rule("prp-adp", body, vec![], Head::Inconsistent));
                            }
                        }
                    }
                }
                return;
            }
            for (i, &a) in members.iter().enumerate() {
                for &b in &members[i + 1..] {
                    if typed(node, voc.all_disjoint_classes) {
                        rules.push(rule(
                            "cax-adc",
                            vec![Atom([v(0), c(ty), c(a)]), Atom([v(0), c(ty), c(b)])],
                            vec![],
                            Head::Inconsistent,
                        ));
                    }
                    if typed(node, voc.all_different) {
                        rules.push(rule(
                            "eq-diff2",
                            vec![different_members(a, b, node)],
                            vec![],
                            Head::Inconsistent,
                        ));
                    }
                    if typed(node, voc.all_disjoint_properties) {
                        rules.push(rule(
                            "prp-adp",
                            vec![Atom([v(0), c(a), v(1)]), Atom([v(0), c(b), v(1)])],
                            vec![],
                            Head::Inconsistent,
                        ));
                    }
                }
            }
        },
    );
    lists(
        voc.distinct_members,
        "eq-diff3",
        ListKind::Pairwise,
        &mut rules,
        &mut |node, members, rules| {
            if !typed(node, voc.all_different) {
                return;
            }
            if members.len() > PAIRWISE_MEMBERS {
                let mut long = long.borrow_mut();
                long.indexes[3].add(members);
                long.premises[3].push([node, ty, voc.all_different]);
                duplicates(members, node, voc, rules, "eq-diff3");
                return;
            }
            for (i, &a) in members.iter().enumerate() {
                for &b in &members[i + 1..] {
                    rules.push(rule(
                        "eq-diff3",
                        vec![different_members(a, b, node)],
                        vec![],
                        Head::Inconsistent,
                    ));
                }
            }
        },
    );
    // One rule per kind of long axiom, its pairs checked by the index.
    let long = long.into_inner();
    let [classes, different, properties, distinct] = long.indexes;
    let shared = super::ir::SharedListIndex::new;
    let mut native = |name: &str, body: Vec<Atom>, x: Term, y: Term, index, used: Vec<[u64; 3]>| {
        rules.push(rule(
            name,
            body,
            vec![Guard::NotEqual(x, y), Guard::SameList(x, y, shared(index))],
            Head::Inconsistent,
        ));
        premises.push(used);
    };
    let [p_classes, p_different, p_properties, p_distinct] = long.premises;
    if !classes.is_empty() {
        native(
            "cax-adc",
            vec![Atom([v(0), c(ty), v(1)]), Atom([v(0), c(ty), v(2)])],
            v(1),
            v(2),
            classes,
            p_classes,
        );
    }
    for (index, used, name) in [
        (different, p_different, "eq-diff2"),
        (distinct, p_distinct, "eq-diff3"),
    ] {
        if !index.is_empty() {
            native(
                name,
                vec![Atom([v(0), c(voc.same_as), v(1)])],
                v(0),
                v(1),
                index,
                used,
            );
        }
    }
    if !properties.is_empty() {
        native(
            "prp-adp",
            vec![Atom([v(0), v(1), v(2)]), Atom([v(0), v(3), v(2)])],
            v(1),
            v(3),
            properties,
            p_properties,
        );
    }
    (rules, premises, diagnostics)
}

/// The long lists of the pairwise axioms ([`PAIRWISE_MEMBERS`]): disjoint classes,
/// different individuals (`owl:members`), disjoint properties, different individuals
/// (`owl:distinctMembers`); their indexes and the axioms' type facts.
#[derive(Default)]
struct Long {
    indexes: [super::ir::ListIndex; 4],
    premises: [Vec<[u64; 3]>; 4],
}

/// A long `owl:AllDifferent` list that names an individual twice is inconsistent by
/// itself (`a sameAs a` holds by eq-ref, which isn't materialised): the axiom's rule.
/// The members `members` names more than once, each once.
fn repeated(members: &[u64]) -> Vec<u64> {
    let mut seen = std::collections::HashSet::new();
    let mut twice: Vec<u64> = (members.iter().copied())
        .filter(|&m| !seen.insert(m))
        .collect();
    twice.sort_unstable();
    twice.dedup();
    twice
}

fn duplicates(members: &[u64], node: u64, voc: &ListVocabulary, rules: &mut Vec<Rule>, name: &str) {
    let mut seen = std::collections::HashSet::new();
    for &member in members {
        if !seen.insert(member) {
            rules.push(rule(
                name,
                vec![Atom([c(node), c(voc.rdf_type), c(voc.all_different)])],
                vec![],
                Head::Inconsistent,
            ));
            return;
        }
    }
}
