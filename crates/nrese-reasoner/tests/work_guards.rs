//! Guards of the rule work (docs/design/performance.md §0): counts of what the batch
//! executor's joins enumerate on the LUBM-shaped input under OWL 2 RL. Counts don't
//! depend on the machine or the thread count, so a lost win shows here at once.

use nrese_reasoner::batch::{self, Materialisation, Schema};
use nrese_reasoner::delta::{self, MemoryBase};
use nrese_reasoner::eval::GroundProgram;
use nrese_reasoner::ir::{Head, OWL, RDF, RDFS, Rule, Vocabulary};
use nrese_reasoner::lists::ListVocabulary;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

#[path = "support/lubm.rs"]
mod lubm;
use lubm::{DEPARTMENTS, cascade, declarations, grouped, lubm_like};

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

/// Equality by representatives over [`lubm_like`], plus a `sameAs` cascade of `depth`
/// merges, as the store runs it (grouped input, the closure expanded).
fn with_cascade(depth: usize) -> Materialisation {
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let mut facts = lubm_like(&mut vocabulary, DEPARTMENTS);
    if depth > 0 {
        let extra = cascade(&mut vocabulary, &facts, depth);
        facts.extend(extra);
    }
    batch::materialise_representatives_until(
        batch::Input::Grouped(grouped(facts)),
        &rules,
        Some(&lists),
        &schema,
        batch::Listing::Expanded,
        nrese_reasoner::eval::NEVER,
    )
    .expect("never stopped")
}

/// #4 of the investigation of 6 October 2026: equality by representatives merges classes
/// within its rounds and rewrites only the facts a merge touches, so a cascade of merges
/// costs one materialisation. Before (133671c), each merge re-materialised the whole
/// closure in an outer pass: depth 8 took 9 passes, about 9 times the joins' work.
#[test]
fn a_cascade_of_merges_costs_one_materialisation() {
    let plain = with_cascade(0);
    let merged = with_cascade(8);
    assert_eq!(merged.merges, 8, "one merge per round of the cascade");
    let classes: Vec<usize> = merged.classes.classes().map(|(_, m)| m.len()).collect();
    assert_eq!(classes.len(), 8, "{classes:?}");
    let bindings = |m: &Materialisation| m.counters.bindings.iter().sum::<u64>();
    assert!(
        bindings(&merged) * 10 <= bindings(&plain) * 11,
        "{} bindings with the cascade, {} without: at most 1.1 times",
        bindings(&merged),
        bindings(&plain)
    );
}

/// #10 of the investigation of 6 October 2026: a recent run that is the delta (shared,
/// right after a fold) has no old part, so reading it as old checks nothing against the
/// delta. On this input every old read of a recent run is such a read: no check (a
/// binary search into the delta per pair before: 36 on 6 October; LUBM 100 854,105).
#[test]
fn old_reads_of_a_recent_run_that_is_the_delta_check_nothing() {
    let (result, _) = materialise();
    assert_eq!(
        result.counters.old_checks, 0,
        "pairs checked against the delta"
    );
}

/// #12 of the investigation of 6 October 2026: equality by copying (the module that
/// replaces `eq-rep-*` when equality isn't by representatives) expands only the facts
/// that mention a term with a `sameAs` partner. It scanned the partners of every term of
/// every new fact before: 117,216 scans here (5,733 since), where one merge touches a few
/// facts.
#[test]
fn equality_by_copying_expands_only_facts_with_partners() {
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let mut facts = lubm_like(&mut vocabulary, DEPARTMENTS);
    let extra = cascade(&mut vocabulary, &facts, 1);
    facts.extend(extra);
    let result = batch::materialise_grouped(grouped(facts), &rules, Some(&lists), &schema);
    assert!(
        result.counters.member_scans <= MEMBER_SCANS,
        "{} scans for sameAs partners, at most {MEMBER_SCANS}",
        result.counters.member_scans
    );
}

/// See [`equality_by_copying_expands_only_facts_with_partners`].
const MEMBER_SCANS: u64 = 5_733;

/// A deep hierarchy as the fast suite's `rl-hierarchy` has it, small: a spine of 200
/// classes, each declared, with 1,000 instances at its bottom, and 10,000 instances of a
/// shallow branch.
fn spine() -> Materialisation {
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let ty = vocabulary.iri(&format!("{RDF}type"));
    let sub = vocabulary.iri(&format!("{RDFS}subClassOf"));
    let class = vocabulary.iri(&format!("{OWL}Class"));
    let mut iri = |name: String| vocabulary.iri(&format!("{}{name}", lubm::EX));
    let mut facts = Vec::new();
    for s in 0..200 {
        let c = iri(format!("S{s}"));
        facts.push([c, ty, class]);
        if s > 0 {
            facts.push([c, sub, iri(format!("S{}", s - 1))]);
        }
    }
    let leaf = iri("Leaf".to_owned());
    facts.push([leaf, ty, class]);
    facts.push([leaf, sub, iri("S0".to_owned())]);
    for i in 0..11_000 {
        let class = if i < 1_000 {
            iri("S199".to_owned())
        } else {
            leaf
        };
        facts.push([iri(format!("x{i}")), ty, class]);
    }
    batch::materialise_grouped(grouped(facts), &rules, Some(&lists), &schema)
}

/// The fast suite's `rl-hierarchy` finding (6 October 2026): a deep hierarchy re-derived
/// every inherited type once per ancestor, as `cax-sco` read the types it had derived
/// itself (quadratic in the depth). Closed rule families read the delta without what
/// they produced: 20,130,000 bindings on this input before, 428,000
/// since (the derived facts the same 240,506).
#[test]
fn a_deep_hierarchy_derives_each_inherited_type_once() {
    let result = spine();
    let bindings: u64 = result.counters.bindings.iter().sum();
    assert!(
        bindings <= SPINE_BINDINGS,
        "{bindings} bindings ({:?} by round) for {} derived facts, at most {SPINE_BINDINGS}",
        result.counters.bindings,
        result.derived.len()
    );
}

/// See [`a_deep_hierarchy_derives_each_inherited_type_once`].
const SPINE_BINDINGS: u64 = 428_000;
