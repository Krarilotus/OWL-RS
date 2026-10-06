//! Guards of the rule work (docs/design/performance.md §0): counts of what the batch
//! executor's joins enumerate on the LUBM-shaped input under OWL 2 RL. Counts don't
//! depend on the machine or the thread count, so a lost win shows here at once.

use nrese_reasoner::batch::{self, Materialisation, Schema};
use nrese_reasoner::delta::{self, MemoryBase};
use nrese_reasoner::eval::GroundProgram;
use nrese_reasoner::ir::{Head, Rule};
use nrese_reasoner::lists::ListVocabulary;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

#[path = "support/lubm.rs"]
mod lubm;
use lubm::{DEPARTMENTS, declarations, grouped, lubm_like};

/// OWL 2 RL over [`lubm_like`] with univ-bench's [`declarations`], as the store runs it
/// (grouped input), with the ground program of its closure (as the delta executor
/// grounds it).
fn materialise() -> (Materialisation, GroundProgram) {
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let mut facts = lubm_like(&mut vocabulary, DEPARTMENTS);
    facts.extend(declarations(&mut vocabulary));
    let result = batch::materialise_grouped(grouped(facts.clone()), &rules, Some(&lists), &schema);
    let base = MemoryBase::new(&facts, &result.derived);
    let program = delta::program(
        &base,
        delta::Rules {
            rules: &rules,
            lists: Some(&lists),
            schema: &schema,
        },
    );
    (result, program)
}

/// Whether a head atom of `rule` is one of its body atoms.
fn derives_a_premise(rule: &Rule) -> bool {
    matches!(&rule.head, Head::Facts(heads) if heads.iter().any(|head| rule.body.contains(head)))
}

/// #1 of the investigation of 6 October 2026: reflexive schema facts (`C subClassOf C`,
/// `p subPropertyOf p`, `C equivalentClass C`) ground into tautologies such as
/// `(?x type C) → (?x type C)`, which re-derive every type and property fact once more.
/// None is kept. With them (133671c), this input's program has 23 such instances and
/// the joins enumerate 336,205 bindings; without, 239,403 (−29 %).
#[test]
fn no_ground_instance_derives_its_own_premise() {
    let (result, program) = materialise();
    let tautologies = program
        .rules
        .iter()
        .filter(|r| derives_a_premise(r))
        .count();
    let bindings: u64 = result.counters.bindings.iter().sum();
    assert!(
        tautologies == 0 && bindings <= BINDINGS,
        "{tautologies} ground instances deriving their own premise; {bindings} bindings \
         ({:?} by round), at most {BINDINGS}",
        result.counters.bindings
    );
}

/// See [`no_ground_instance_derives_its_own_premise`].
const BINDINGS: u64 = 239_403;
