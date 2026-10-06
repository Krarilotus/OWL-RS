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
use super::ir::Rule;
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
    })
}
