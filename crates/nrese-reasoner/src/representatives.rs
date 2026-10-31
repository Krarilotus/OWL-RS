//! Equality by representatives (work package W4, stage A).
//!
//! OWL 2 RL's equality rules (`eq-rep-s/p/o`) make every fact true of every identity of
//! its terms: a class of k identities turns one fact into up to k³ copies, and the store,
//! the joins and the answers carry all of them. Here each `owl:sameAs` class has one
//! representative (its smallest id), every fact is rewritten to representatives, and the
//! rules run on the rewritten facts without the replacement rules.
//!
//! The batch executor does it within its semi-naive rounds (egglog's rebuild; Zhang et
//! al., POPL 2023): a round's new `sameAs` between two representatives merges their
//! classes, and only the facts that mention the representative that lost its place are
//! rewritten, into the next round's delta ([`super::batch`]). So equality costs one
//! materialisation plus the facts its merges touch, however long a cascade of merges is;
//! before 6 October 2026 every merge re-materialised the whole closure, until a pass
//! merged nothing.
//!
//! The replicated closure is the representative closure expanded: a fact holds iff the
//! fact of its terms' representatives is in the representative closure
//! ([`EqualityClasses::expand`]). The property test checks exactly that, and that the
//! same consistency rules fire.

use super::batch::{self, Schema};
use super::ir::{Atom, Guard, Head, Rule, SharedClasses, Term};
use super::ir::{Triple, Violation};
use super::lists::ListVocabulary;

/// The `owl:sameAs` classes of a closure: each term's representative, and each
/// representative's members (only for classes of two or more). The union kernel the
/// engine shares ([`nrese_exec::classes`]).
pub use nrese_exec::classes::Classes as EqualityClasses;

/// A closure over representatives.
#[derive(Debug, Default)]
pub struct RepresentativeClosure {
    /// Every fact of the closure over representatives, the rewritten input included,
    /// sorted, without duplicates.
    pub facts: Vec<Triple>,
    pub classes: EqualityClasses,
    /// Violations over representatives, sorted.
    pub violations: Vec<Violation>,
    pub diagnostics: Vec<super::lists::ListDiagnostic>,
    /// Rounds of the closure, and in how many of them classes merged.
    pub rounds: usize,
    pub merges: usize,
    /// The batch materialisations run: one (merges are rebuilt within its rounds).
    pub passes: usize,
    /// The times and work of every pass, summed (`passes` counts them).
    pub phases: batch::Phases,
}

/// The `owl:sameAs` id of rules that do equality reasoning (`eq-rep-s`), if they do.
pub fn same_as(rules: &[Rule]) -> Option<u64> {
    let rule = rules.iter().find(|r| r.name == "eq-rep-s")?;
    match rule.body.first()?.0[1] {
        super::ir::Term::Const(id) => Some(id),
        super::ir::Term::Var(_) => None,
    }
}

/// The rules without the replacement rules, which rewriting stands in for.
pub fn without_replacement(rules: &[Rule]) -> Vec<Rule> {
    rules
        .iter()
        .filter(|r| !batch::is_replacement_rule(r))
        .cloned()
        .collect()
}

/// What [`over_classes`] changed: a rule that concludes facts. Whether it changed any is
/// whether the run has classes.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Guarded {
    pub facts: bool,
}

/// `rules` as they run over representatives: their guards hold where members of the
/// classes satisfy them ([`Guard::OverClasses`]). Over representatives a guard sees one
/// term per class, so `?x != ?y` failed for two members of one class, and a rule needing
/// them to differ missed what the replicated closure derives (Sol's review of DMW-AB-mem,
/// r1 S1). Left as they are, so the built-in rulesets run as before:
/// - rules whose one guard is `?a != ?b` and whose every conclusion is `?a owl:sameAs ?b`
///   (`prp-fp`, `prp-ifp`, `cls-maxc2`, `cls-maxqc3/4`, `prp-key`): with `?a` and `?b` in
///   one class they would conclude what the class holds;
/// - the long lists' rules ([`Guard::SameList`]): their index is built from the lists as
///   the run reads them, over representatives, so it names no other member of a class.
///
/// `classes` are the run's, made with the first rule changed: a run with none has none.
pub(crate) fn over_classes(
    rules: &mut [Rule],
    same_as: u64,
    classes: &mut Option<SharedClasses>,
) -> Guarded {
    let mut guarded = Guarded::default();
    for rule in rules {
        let lists = rule.guards.iter().any(|g| matches!(g, Guard::SameList(..)));
        if rule.guards.is_empty() || lists || concludes_its_inequality_equal(rule, same_as) {
            continue;
        }
        let guards = std::mem::take(&mut rule.guards);
        rule.guards = vec![Guard::OverClasses {
            guards,
            bound: Vec::new(),
            classes: classes.get_or_insert_with(SharedClasses::default).clone(),
        }];
        guarded.facts |= matches!(rule.head, Head::Facts(_));
    }
    guarded
}

/// Whether the rule's one guard is `?a != ?b` and each conclusion `?a owl:sameAs ?b`.
fn concludes_its_inequality_equal(rule: &Rule, same_as: u64) -> bool {
    let ([Guard::NotEqual(a, b)], Head::Facts(heads)) = (rule.guards.as_slice(), &rule.head) else {
        return false;
    };
    heads.iter().all(|&Atom([s, p, o])| {
        p == Term::Const(same_as) && ((s, o) == (*a, *b) || (s, o) == (*b, *a))
    })
}

/// Whether members of the classes of the guards' variables' values (a constant stands for
/// itself, an unbound term is decided later) can be chosen, one per term, so that every
/// guard holds.
pub(crate) fn members_satisfy(
    guards: &[Guard],
    value: &dyn Fn(Term) -> Option<u64>,
    classes: &SharedClasses,
) -> bool {
    let classes = classes.read();
    let mut terms: Vec<Term> = Vec::new();
    for term in guards.iter().flat_map(Guard::terms) {
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    let choices: Vec<Vec<Option<u64>>> = terms
        .iter()
        .map(|&term| match (term, value(term)) {
            (Term::Var(_), Some(id)) => classes.class_of(id).map_or_else(
                || vec![Some(id)],
                |members| members.iter().map(|&m| Some(m)).collect(),
            ),
            (_, id) => vec![id],
        })
        .collect();
    // Each guard is checked as soon as its terms are chosen.
    let mut checks: Vec<Vec<&Guard>> = vec![Vec::new(); terms.len()];
    for guard in guards {
        let last = guard
            .terms()
            .iter()
            .filter_map(|t| terms.iter().position(|u| u == t))
            .max();
        checks[last.unwrap_or(0)].push(guard);
    }
    fn choose(
        depth: usize,
        terms: &[Term],
        choices: &[Vec<Option<u64>>],
        checks: &[Vec<&Guard>],
        chosen: &mut [Option<u64>],
    ) -> bool {
        if depth == terms.len() {
            return true;
        }
        for &choice in &choices[depth] {
            chosen[depth] = choice;
            let value = |term: Term| {
                terms
                    .iter()
                    .position(|&t| t == term)
                    .and_then(|i| chosen[i])
            };
            if checks[depth].iter().all(|guard| guard.holds(value))
                && choose(depth + 1, terms, choices, checks, chosen)
            {
                return true;
            }
        }
        false
    }
    choose(0, &terms, &choices, &checks, &mut vec![None; terms.len()])
}

/// The closure of `input` under `rules` over representatives of `owl:sameAs` classes.
/// Rules without equality reasoning give the plain closure (no classes).
pub fn materialise(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> RepresentativeClosure {
    materialise_until(input, rules, lists, schema, super::eval::NEVER).expect("never stopped")
}

/// [`materialise`], polling `stop` in every round.
pub fn materialise_until(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<RepresentativeClosure, super::delta::Interrupted> {
    let result = batch::materialise_representatives_until(
        batch::Input::Facts(input.to_vec()),
        rules,
        lists,
        schema,
        batch::Listing::Representatives,
        stop,
    )?;
    Ok(RepresentativeClosure {
        facts: result.derived,
        classes: result.classes,
        violations: result.violations,
        diagnostics: result.diagnostics,
        rounds: result.rounds,
        merges: result.merges,
        passes: 1,
        phases: result.phases,
    })
}
