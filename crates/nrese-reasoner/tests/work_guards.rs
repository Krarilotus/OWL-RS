//! Guards of the rule work (docs/design/performance.md §0): counts of what the batch
//! executor's joins enumerate on the LUBM-shaped input under OWL 2 RL. Counts don't
//! depend on the machine or the thread count, so a lost win shows here at once.

use nrese_reasoner::batch::{self, Materialisation, Schema};
use nrese_reasoner::delta::{self, MemoryBase};
use nrese_reasoner::eval::GroundProgram;
use nrese_reasoner::ir::{Head, OWL, RDF, RDFS, Rule, Triple, Vocabulary};
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
        batch::Output::Facts,
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

/// B3a: a deletion that splits an `owl:sameAs` class recomputes the class's facts at once
/// (overdeleted, and every copy within the parts the equalities left put back in bulk)
/// instead of running DRed's equality rules over every pair of members. The fast suite's
/// shape at 200 members: two chains joined by a bridge, a value per member. With DRed,
/// a 500-member split took 68 s in the store (75 s, 7.7 GB on 7 October 2026); recomputed,
/// 0.45 s in memory, a rematerialisation of everything 1.18 s.
#[test]
fn a_split_class_is_recomputed_not_copied_through() {
    use nrese_reasoner::delta::policy::{Plan, Policy};
    use nrese_reasoner::eval::NEVER;
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let same_as = vocabulary.iri(&format!("{OWL}sameAs"));
    let val = vocabulary.iri(&format!("{}val", lubm::EX));
    let size = 200;
    let member = |v: &mut LocalVocabulary, j: usize| v.iri(&format!("{}g{j}", lubm::EX));
    let mut asserted = Vec::new();
    for j in 0..size {
        let x = member(&mut vocabulary, j);
        let value = vocabulary.literal(&format!("g{j}"), "http://www.w3.org/2001/XMLSchema#string");
        asserted.push([x, val, value]);
        if j + 1 < size {
            asserted.push([x, same_as, member(&mut vocabulary, j + 1)]);
        }
    }
    let bridge = [
        member(&mut vocabulary, size / 2 - 1),
        same_as,
        member(&mut vocabulary, size / 2),
    ];
    asserted.sort_unstable();
    let before = batch::materialise(&asserted, &rules, Some(&lists), &schema);
    let after: Vec<Triple> = asserted.iter().copied().filter(|&t| t != bridge).collect();
    let base = MemoryBase::new(&after, &before.derived);
    let compiled = delta::Rules {
        rules: &rules,
        lists: Some(&lists),
        schema: &schema,
    };
    let update = delta::update_with(
        &base,
        &[],
        &[bridge],
        compiled,
        None,
        NEVER,
        Policy::default(),
    )
    .expect("never stopped");
    let fresh = batch::materialise(&after, &rules, Some(&lists), &schema);
    // The class's inferred facts: every pair of members, and every member's value on each.
    let region = (size * size - (size - 1) + size * size - size) as u64;
    let kept = fresh.derived.len() as u64;
    let policy = &update.policy;
    assert_eq!(policy.plan, Plan::Recompute, "{policy:?}");
    assert!(
        policy.dred_estimate > policy.recompute_estimate,
        "{policy:?}"
    );
    assert_eq!(
        (policy.region_terms, policy.region_facts, policy.overdeleted),
        (size as u64, region, region),
        "the class's facts overdeleted, and nothing copied through it: {policy:?}"
    );
    assert_eq!(
        policy.rederived, kept,
        "what the parts keep, put back: {policy:?}"
    );
    let removal: std::collections::HashSet<Triple> = update.remove.iter().copied().collect();
    let mut maintained: Vec<Triple> = before
        .derived
        .into_iter()
        .filter(|t| !removal.contains(t))
        .chain(update.insert)
        .collect();
    maintained.sort_unstable();
    assert_eq!(maintained, fresh.derived);
}

/// A transitive relation closed once by the module takes the transitivity joins for the
/// few pairs other rules add later, instead of a closure of all of it every round: a mark
/// walks a step a round along a chain of 300 (302 rounds), each step adding one `reach`
/// pair beside a 301-node path's closure (45,451 pairs in all). Closed again in every
/// round, the module read 13,590,751 pairs on 13 October 2026; now 301, the first round's.
#[test]
fn a_closed_transitive_relation_takes_joins_for_what_others_add() {
    let mut vocabulary = LocalVocabulary::default();
    let program = nrese_reasoner::n3::compile(
        "walk",
        "@prefix e: <http://example.org/> .
         { ?x e:next ?y } => { ?x e:reach ?y } .
         { ?x e:reach ?y . ?y e:reach ?z } => { ?x e:reach ?z } .
         { ?x e:mark ?m . ?x e:step ?y } => { ?y e:mark ?m } .
         { ?x e:mark ?m } => { ?m e:reach ?x } .",
        &mut vocabulary,
    )
    .expect("the program compiles");
    let schema = Schema::owl(&mut vocabulary);
    let mut iri = |name: &str| vocabulary.iri(&format!("http://example.org/{name}"));
    let (next, step, mark, reach) = (iri("next"), iri("step"), iri("mark"), iri("reach"));
    let n: Vec<u64> = (0..=300).map(|i| iri(&format!("n{i}"))).collect();
    let w: Vec<u64> = (0..=300).map(|i| iri(&format!("w{i}"))).collect();
    let mut facts: Vec<Triple> = Vec::new();
    for i in 0..300 {
        facts.push([n[i], next, n[i + 1]]);
        facts.push([w[i], step, w[i + 1]]);
    }
    facts.push([w[0], mark, n[0]]);
    let result = batch::materialise_grouped(grouped(facts), &program.rules, None, &schema);
    let read: u64 = result.counters.closure_pairs.iter().sum();
    let reach = result.derived.iter().filter(|t| t[1] == reach).count() as u64;
    assert!(result.rounds > 300, "{} rounds", result.rounds);
    assert!(
        read <= 2 * reach,
        "the module read {read} pairs for {reach} reach pairs in {} rounds",
        result.rounds
    );
}
