//! List axioms compiled to fixed-arity rules (reasoner-v2 design §3.2, step 3).
//!
//! OWL 2 RL rules over RDF lists (`prp-spo2`, `prp-key`, `cls-int1/2`, `cls-uni`, `cls-oo`,
//! `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`) can't be written as fixed rules.
//! Instead each list axiom in the data becomes rules of its own arity. A chain of length
//! n becomes one n-atom rule, and an intersection one rule per direction. That removes
//! the `rdf:first`/`rdf:rest` helper rules that generic rulesets need.
//!
//! Lists are read from the facts: each cell on a walk to `rdf:nil` supplies one
//! `rdf:first` and one `rdf:rest`. Only cells on such a walk contribute enumerated
//! rules. Past the 64-walk cap, the pairwise fallback still reads cells as written.
//! A head with no walk is diagnosed. For a cycle the reader marks the axiom
//! incomplete and keeps the rules of walks previously read up to `rdf:nil`.
//!
//! Lists of any length are instantiated. The pairwise axioms (`owl:AllDifferent`,
//! `owl:AllDisjointClasses`, `owl:AllDisjointProperties`) would need a rule per pair, so
//! their lists longer than [`PAIRWISE_MEMBERS`] become one rule per kind of axiom whose
//! [`Guard::SameList`] asks an index of the members which lists they share: linear in the
//! members however many there are. The only length limit left is the rule format's: a
//! property chain over more than [`MAX_RULE_MEMBERS`] properties, or a key over more than
//! [`MAX_KEY_MEMBERS`], can't be one rule (variables and atoms are numbered by `u8`) and is
//! diagnosed.

use std::collections::{HashMap, HashSet};

use super::ir::{Atom, Guard, Head, OWL, RDF, Rule, Term, Vocabulary};

/// Read access to facts for instantiation.
pub trait Facts {
    /// Objects of `(subject, predicate, ?)`.
    fn objects(&self, subject: u64, predicate: u64) -> Vec<u64>;
    /// `(subject, object)` pairs of `(?, predicate, ?)`.
    fn pairs(&self, predicate: u64) -> Vec<(u64, u64)>;
    /// The same cells with no two names merged, where these facts merge equal names
    /// ([`instantiate_written`]: a list whose merged cells are past the cap too).
    fn unmerged(&self) -> Option<&dyn Unmerged> {
        None
    }
}

/// List cells as asserted, each node its own ([`Facts::unmerged`]).
pub trait Unmerged {
    /// The objects of `(node, predicate, ?)` as asserted, unrenamed.
    fn cells(&self, node: u64, predicate: u64) -> Vec<u64>;
    /// The names equal to `x`, `x` among them, if `x` is the same as another name.
    fn equal(&self, x: u64) -> Option<&[u64]>;
    /// The name the closure gives `x`.
    fn named(&self, x: u64) -> u64;
    /// Whether the run has stopped: then nothing more is read.
    fn stopped(&self) -> bool;
}

/// [`Unmerged`] cells read as [`Facts`] for a walk.
struct Cells<'a>(&'a dyn Unmerged);

impl Facts for Cells<'_> {
    fn objects(&self, node: u64, predicate: u64) -> Vec<u64> {
        self.0.cells(node, predicate)
    }

    fn pairs(&self, _: u64) -> Vec<(u64, u64)> {
        Vec::new()
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

    /// Walks to `rdf:nil`, with the facts along each and any accepting cycle.
    fn list(
        &self,
        facts: &impl Facts,
        head: u64,
        nil_is_cell: bool,
    ) -> (Result<Vec<Variant>, ListProblem>, Option<ListProblem>) {
        if !nil_is_cell && let Some(linear) = self.linear_list(facts, head) {
            return (linear, None);
        }
        let cells = match self.accepting_cells(facts, head) {
            Ok(cells) => cells,
            Err(problem) => return (Err(problem), None),
        };
        let Err(cycle) = check_acyclic(&cells, head) else {
            return (self.enumerate(&cells, head), None);
        };
        // Preserve the previous reader's result and report the accepting
        // cycle. The previous reader stopped on its first visit to nil.
        (self.list_as_written(facts, head), Some(cycle))
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
        cells: &HashMap<u64, Cell>,
        head: u64,
    ) -> Result<Vec<Variant>, ListProblem> {
        let mut variants = Vec::new();
        let (mut members, mut used) = (Vec::new(), Vec::new());
        // Each frame is a cell and the next combination of first and rest at it.
        let mut stack = vec![(head, 0, 0)];
        while let Some((node, first, rest)) = stack.last_mut() {
            let cell = &cells[&*node];
            if *node == self.nil {
                if variants.len() == MAX_VARIANTS {
                    return Err(ListProblem::TooManyVariants);
                }
                variants.push((members.clone(), used.clone()));
            } else if let Some(&member) = cell.firsts.get(*first) {
                let next = cell.rests[*rest];
                members.push(member);
                used.extend([[*node, self.first, member], [*node, self.rest, next]]);
                *rest += 1;
                if *rest == cell.rests.len() {
                    *first += 1;
                    *rest = 0;
                }
                stack.push((next, 0, 0));
                continue;
            }
            stack.pop();
            members.pop();
            used.truncate(2 * members.len());
        }
        Ok(variants)
    }

    /// The earlier reading, for cycles through nil and the asserted pairwise fallback.
    fn list_as_written(
        &self,
        facts: &(impl Facts + ?Sized),
        head: u64,
    ) -> Result<Vec<Variant>, ListProblem> {
        let mut variants = Vec::new();
        let mut path = Vec::new();
        let mut nodes = Vec::new();
        let mut used = Vec::new();
        self.walk_as_written(facts, head, &mut nodes, &mut path, &mut used, &mut variants)?;
        Ok(variants)
    }

    /// The member sequences of the list of the axiom `(subject predicate head)` over
    /// `unmerged`: walked from the heads the axiom was asserted with, or, where none of them
    /// starts a list, from each name equal to `head`; its members named as the closure names
    /// them; past [`MAX_VARIANTS`] distinct ones, none.
    fn unmerged_list(
        &self,
        unmerged: &dyn Unmerged,
        [subject, predicate, head]: [u64; 3],
    ) -> Result<Vec<Variant>, ListProblem> {
        let (one, other) = ([subject], [head]);
        let subjects = unmerged.equal(subject).unwrap_or(&one);
        let asserted: Vec<u64> = subjects
            .iter()
            .flat_map(|&s| unmerged.cells(s, predicate))
            .collect();
        let variants = self.walks(unmerged, &asserted)?;
        match variants.is_empty() {
            true => self.walks(unmerged, unmerged.equal(head).unwrap_or(&other)),
            false => Ok(variants),
        }
    }

    /// The distinct member sequences of the lists from `starts` over `unmerged`, named as the
    /// closure names them; past [`MAX_VARIANTS`], none. A start that is no list adds none.
    fn walks(&self, unmerged: &dyn Unmerged, starts: &[u64]) -> Result<Vec<Variant>, ListProblem> {
        let mut variants: Vec<Variant> = Vec::new();
        for &start in starts {
            if unmerged.stopped() {
                return Ok(Vec::new());
            }
            for (members, used) in self
                .list_as_written(&Cells(unmerged), start)
                .unwrap_or_default()
            {
                let members: Vec<u64> = members.iter().map(|&m| unmerged.named(m)).collect();
                if variants.iter().any(|(seen, _)| *seen == members) {
                    continue;
                }
                if variants.len() == MAX_VARIANTS {
                    return Err(ListProblem::TooManyVariants);
                }
                variants.push((members, used));
            }
        }
        Ok(variants)
    }

    fn walk_as_written(
        &self,
        facts: &(impl Facts + ?Sized),
        node: u64,
        nodes: &mut Vec<u64>,
        path: &mut Vec<u64>,
        used: &mut Vec<[u64; 3]>,
        variants: &mut Vec<Variant>,
    ) -> Result<(), ListProblem> {
        if node == self.nil {
            if variants.len() == MAX_VARIANTS {
                return Err(ListProblem::TooManyVariants);
            }
            variants.push((path.clone(), used.clone()));
            return Ok(());
        }
        if nodes.contains(&node) {
            return Err(ListProblem::Cycle { node });
        }
        let (firsts, rests) = (
            facts.objects(node, self.first),
            facts.objects(node, self.rest),
        );
        if firsts.is_empty() || rests.is_empty() {
            return Err(ListProblem::Malformed { node });
        }
        nodes.push(node);
        for &first in &firsts {
            path.push(first);
            used.push([node, self.first, first]);
            for &rest in &rests {
                used.push([node, self.rest, rest]);
                self.walk_as_written(facts, rest, nodes, path, used, variants)?;
                used.pop();
            }
            used.pop();
            path.pop();
        }
        nodes.pop();
        Ok(())
    }
}

struct Cell {
    firsts: Vec<u64>,
    rests: Vec<u64>,
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

/// Check the accepting graph independently of the enumeration cap. A repeat
/// on the current path is a cycle; a previously finished tail can be shared.
fn check_acyclic(cells: &HashMap<u64, Cell>, head: u64) -> Result<(), ListProblem> {
    let mut active = HashSet::from([head]);
    let mut done = HashSet::new();
    let mut stack = vec![(head, cells[&head].rests.iter())];
    while let Some((node, rests)) = stack.last_mut() {
        if let Some(&rest) = rests.next() {
            if done.contains(&rest) {
                continue;
            }
            if !active.insert(rest) {
                return Err(ListProblem::Cycle { node: rest });
            }
            stack.push((rest, cells[&rest].rests.iter()));
        } else {
            active.remove(node);
            done.insert(*node);
            stack.pop();
        }
    }
    Ok(())
}

/// A member sequence of a list, with the facts its path uses.
type Variant = (Vec<u64>, Vec<[u64; 3]>);

#[derive(Clone, Copy)]
enum ListKind {
    Chain,
    Key,
    Class,
    Pairwise { written_predicate: u64 },
}

impl ListKind {
    fn member_limit(self) -> usize {
        match self {
            Self::Chain => MAX_RULE_MEMBERS,
            Self::Key => MAX_KEY_MEMBERS,
            Self::Class | Self::Pairwise { .. } => usize::MAX,
        }
    }
}

/// The most accepting walks one list axiom may have (see `ListVocabulary::list`).
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
    /// Equality gives the list more than [`MAX_VARIANTS`] member sequences.
    TooManyVariants,
    /// A list with more than `most` members ([`MAX_RULE_MEMBERS`], [`MAX_KEY_MEMBERS`], or
    /// a rule past [`crate::eval::MAX_RULE_ATOMS`]).
    TooLong { members: usize, most: usize },
}

impl ListProblem {
    /// A stable name for reports: `malformed-list`, `cyclic-list`,
    /// `too-many-list-variants` or `list-too-long`.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Malformed { .. } => "malformed-list",
            Self::Cycle { .. } => "cyclic-list",
            Self::TooManyVariants => "too-many-list-variants",
            Self::TooLong { .. } => "list-too-long",
        }
    }

    /// The node the problem is at, if one.
    pub const fn node(self) -> Option<u64> {
        match self {
            Self::Malformed { node } | Self::Cycle { node } => Some(node),
            Self::TooManyVariants | Self::TooLong { .. } => None,
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
            ListProblem::TooManyVariants => {
                format!("owl:sameAs gives it more than {MAX_VARIANTS} member sequences")
            }
            ListProblem::TooLong { members, most } => {
                format!("it has {members} members, more than a rule holds ({most})")
            }
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
    instantiate_written(vocabulary, facts, None)
}

/// [`instantiate_with_premises`], and a list past [`MAX_VARIANTS`] over `facts` (equality's
/// `rdf:first` copies multiply its member sequences) read from `written` as well: the facts
/// as written, a subset of `facts`. Its checks from that reading (rules whose head is
/// [`Head::Inconsistent`]) are added, never an inference, and it is reported past the cap
/// all the same: what the rules derive is what they derive over `facts`, the checks only
/// add to those, and every list left out over `facts` is reported.
pub fn instantiate_written(
    vocabulary: &ListVocabulary,
    facts: &impl Facts,
    written: Option<&dyn Facts>,
) -> (Vec<Rule>, Vec<Vec<[u64; 3]>>, Vec<ListDiagnostic>) {
    instantiate_projected(vocabulary, None, facts, written)
}

pub(crate) fn instantiate_projected(
    vocabulary: &ListVocabulary,
    projection: Option<&ListVocabulary>,
    facts: &impl Facts,
    written: Option<&dyn Facts>,
) -> (Vec<Rule>, Vec<Vec<[u64; 3]>>, Vec<ListDiagnostic>) {
    let voc = projection.unwrap_or(vocabulary);
    let mut rules = Vec::new();
    let mut premises: Vec<Vec<[u64; 3]>> = Vec::new();
    let mut diagnostics: Vec<ListDiagnostic> = Vec::new();
    let nil_is_cell = std::cell::OnceCell::new();
    let mut lists =
        |predicate, name: &'static str, kind, rules: &mut Vec<Rule>, make: &mut MakeRules| {
            debug_assert!(voc.is_list_fact([0, predicate, 0]), "read, not a list fact");
            for (subject, head) in facts.pairs(predicate) {
                let mut diagnose = |problem| {
                    let diagnostic = ListDiagnostic {
                        rules: name,
                        subject,
                        predicate,
                        head,
                        problem,
                    };
                    if !diagnostics.contains(&diagnostic) {
                        diagnostics.push(diagnostic);
                    }
                };
                // Only `owl:members` and `owl:distinctMembers` lists are read as written: their
                // rules are checks alone, never an inference, and never too long for a rule.
                let checks = matches!(kind, ListKind::Pairwise { .. });
                let nil_is_cell = *nil_is_cell.get_or_init(|| {
                    !facts.objects(voc.nil, voc.first).is_empty()
                        && !facts.objects(voc.nil, voc.rest).is_empty()
                });
                let (walks, cycle) = voc.list(facts, head, nil_is_cell);
                if let Some(problem) = cycle {
                    diagnose(problem);
                }
                let read = match (walks, written, kind) {
                    (
                        Err(ListProblem::TooManyVariants),
                        Some(written),
                        ListKind::Pairwise { written_predicate },
                    ) => {
                        diagnose(ListProblem::TooManyVariants);
                        // Where equality merges cells as written past the cap too, each
                        // node's own cells, as the list was asserted.
                        let unmerged = |problem: ListProblem| match written.unmerged() {
                            Some(unmerged) if problem == ListProblem::TooManyVariants => vocabulary
                                .unmerged_list(unmerged, [subject, written_predicate, head]),
                            _ => Err(problem),
                        };
                        match voc.list_as_written(written, head).or_else(unmerged) {
                            Ok(variants) => Ok(variants),
                            Err(_) => continue,
                        }
                    }
                    (read, _, _) => read,
                };
                match read {
                    Ok(variants) => {
                        let most = kind.member_limit();
                        for (members, used) in variants {
                            let before = rules.len();
                            let limit = if members.len() > most {
                                Some(most)
                            } else {
                                make(subject, &members, rules);
                                (!rules[before..].iter().all(Rule::fits))
                                    .then_some(crate::eval::MAX_RULE_ATOMS)
                            };
                            if let Some(most) = limit {
                                diagnose(ListProblem::TooLong {
                                    members: members.len(),
                                    most,
                                });
                                rules.truncate(before);
                                continue;
                            }
                            let mut facts_used = used;
                            facts_used.push([subject, predicate, head]);
                            if checks {
                                for class in [
                                    voc.all_disjoint_classes,
                                    voc.all_disjoint_properties,
                                    voc.all_different,
                                ] {
                                    if facts.objects(subject, voc.rdf_type).contains(&class) {
                                        facts_used.push([subject, voc.rdf_type, class]);
                                    }
                                }
                            }
                            for _ in before..rules.len() {
                                premises.push(facts_used.clone());
                            }
                        }
                    }
                    Err(problem) if cycle.is_none() || problem == ListProblem::TooManyVariants => {
                        diagnose(problem);
                    }
                    Err(_) => {}
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
            let body = chain
                .iter()
                .enumerate()
                .map(|(i, &pi)| Atom([v(i), c(pi), v(i + 1)]))
                .collect();
            rules.push(rule(
                "prp-spo2",
                body,
                vec![],
                Head::Facts(vec![Atom([v(0), c(p), v(chain.len())])]),
            ));
        },
    );
    // prp-key: two instances of c agreeing on every key property are the same.
    lists(
        voc.has_key,
        "prp-key",
        ListKind::Key,
        &mut rules,
        &mut |class, keys, rules| {
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
        ListKind::Class,
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
        ListKind::Class,
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
        ListKind::Class,
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
        ListKind::Pairwise {
            written_predicate: vocabulary.members,
        },
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
        ListKind::Pairwise {
            written_predicate: vocabulary.distinct_members,
        },
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
