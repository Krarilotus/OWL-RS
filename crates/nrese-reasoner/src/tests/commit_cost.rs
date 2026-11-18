//! A commit pays for what it changes. The recursion analysis over the ground program, whose
//! size follows the schema (one `owl:sameAs` partner read for each class and property it
//! links), is read only to keep an overdelete candidate that a non-recursive rule still
//! derives: a commit that deletes nothing reads none of it, and a delete that has a
//! candidate gets the same answer as a rematerialisation.

use std::collections::HashSet;

use super::load;
use crate::batch::{self, Schema};
use crate::delta::policy::{Plan, Policy};
use crate::delta::{MemoryBase, Rules, update_with};
use crate::ir::Triple;
use crate::lists::ListVocabulary;
use crate::rulesets::Ruleset;
use crate::vocabulary::LocalVocabulary;
use crate::witness::{Site, get};

/// The `owl:sameAs` facts of a [`chain`].
#[derive(Clone, Copy, Debug)]
enum Equal {
    /// None.
    Nothing,
    /// Two individuals, one of them a member of the chain's first class.
    Individuals,
    /// `ex:D` and `ex:E`, the class under the chain's second in `ex:D`'s place.
    Classes,
}

/// A subclass chain of `classes` classes under OWL 2 RL, with `ex:a` typed by the first
/// class and by `ex:D`, under the chain's second class (by its own superclass, or with
/// [`Equal::Classes`] through its equality with `ex:E` and `ex:E`'s): deleting `ex:a`'s
/// first type leaves its type of the second class derived without the first.
fn chain(vocabulary: &mut LocalVocabulary, classes: usize, equal: Equal) -> Vec<Triple> {
    let mut text: Vec<String> = (0..classes)
        .map(|i| format!("ex:C{i} rdfs:subClassOf ex:C{}", i + 1))
        .collect();
    text.push("ex:a rdf:type ex:C0".to_owned());
    text.push("ex:a rdf:type ex:D".to_owned());
    let place: &[&str] = match equal {
        Equal::Nothing => &["ex:D rdfs:subClassOf ex:C1"],
        Equal::Individuals => &[
            "ex:D rdfs:subClassOf ex:C1",
            "ex:b owl:sameAs ex:c",
            "ex:b rdf:type ex:C0",
        ],
        Equal::Classes => &["ex:D owl:sameAs ex:E", "ex:E rdfs:subClassOf ex:C1"],
    };
    text.extend(place.iter().map(|line| (*line).to_owned()));
    let mut asserted = load(vocabulary, &text.join("\n"));
    asserted.sort_unstable();
    asserted
}

/// One commit of `inserted` and `deleted` over `asserted` and its closure, under DRed:
/// the maintained inferred stack against a rematerialisation's, and the partner reads it
/// took on this thread.
fn commit(
    vocabulary: &mut LocalVocabulary,
    asserted: &[Triple],
    inserted: &[Triple],
    deleted: &[Triple],
) -> (Vec<Triple>, Vec<Triple>, u64) {
    let rules = Ruleset::Owl2Rl.rules(vocabulary).unwrap();
    let lists = ListVocabulary::new(vocabulary);
    let schema = Schema::owl(vocabulary);
    let compiled = Rules {
        rules: &rules,
        lists: Some(&lists),
        schema: &schema,
        equality: crate::representatives::same_as(&rules, schema.same_as()),
    };
    let closure =
        |asserted: &[Triple]| batch::materialise(asserted, &rules, Some(&lists), &schema).derived;
    let before = closure(asserted);
    let mut after: Vec<Triple> = asserted
        .iter()
        .copied()
        .filter(|t| !deleted.contains(t))
        .chain(inserted.iter().copied())
        .collect();
    after.sort_unstable();
    after.dedup();
    let stack: Vec<Triple> = before
        .iter()
        .copied()
        .filter(|t| after.binary_search(t).is_err())
        .collect();
    let base = MemoryBase::new(&after, &stack);
    let policy = Policy {
        force: Some(Plan::Dred),
        ..Policy::default()
    };
    let reads = get(Site::PartnerRead);
    let result = update_with(
        &base,
        inserted,
        deleted,
        compiled,
        None,
        crate::eval::NEVER,
        policy,
    )
    .expect("never stopped");
    let reads = get(Site::PartnerRead) - reads;
    assert_eq!(result.policy.plan, Plan::Dred, "the plan forced");
    let removal: HashSet<Triple> = result.remove.iter().copied().collect();
    let mut maintained: Vec<Triple> = stack
        .into_iter()
        .filter(|t| !removal.contains(t))
        .chain(result.insert)
        .collect();
    maintained.sort_unstable();
    maintained.dedup();
    (maintained, closure(&after), reads)
}

/// An insert-only commit reads no `owl:sameAs` partners, whatever the schema's size: the
/// recursion analysis is not built where no candidate asks for a proof.
#[test]
fn an_insert_reads_nothing_that_grows_with_the_schema() {
    for classes in [10, 1000] {
        let mut vocabulary = LocalVocabulary::default();
        let asserted = chain(&mut vocabulary, classes, Equal::Nothing);
        let inserted = load(&mut vocabulary, "ex:w ex:value ex:v");
        let (maintained, expected, reads) = commit(&mut vocabulary, &asserted, &inserted, &[]);
        assert_eq!(maintained, expected, "{classes} classes: rematerialised");
        assert_eq!(reads, 0, "{classes} classes: partner reads of an insert");
    }
}

/// A delete that leaves no overdelete candidate (a fact no rule derives anything from)
/// reads no `owl:sameAs` partners either, whatever the schema's size.
#[test]
fn a_delete_without_candidates_reads_nothing_that_grows_with_the_schema() {
    for classes in [10, 1000] {
        let mut vocabulary = LocalVocabulary::default();
        let mut asserted = chain(&mut vocabulary, classes, Equal::Nothing);
        let deleted = load(&mut vocabulary, "ex:w ex:value ex:v");
        asserted.extend(deleted.iter().copied());
        asserted.sort_unstable();
        let (maintained, expected, reads) = commit(&mut vocabulary, &asserted, &[], &deleted);
        assert_eq!(maintained, expected, "{classes} classes: rematerialised");
        assert_eq!(
            reads, 0,
            "{classes} classes: partner reads of a delete without candidates"
        );
    }
}

/// A delete with a candidate that a non-recursive rule still derives keeps it, with or
/// without `owl:sameAs` facts in the store, between individuals or between classes (where
/// the kept derivation runs through the equality): the answer is a rematerialisation's, and
/// the recursion analysis was built for it.
#[test]
fn a_delete_with_a_candidate_keeps_what_remains_derived() {
    for equal in [Equal::Nothing, Equal::Individuals, Equal::Classes] {
        let mut vocabulary = LocalVocabulary::default();
        let asserted = chain(&mut vocabulary, 10, equal);
        let deleted = load(&mut vocabulary, "ex:a rdf:type ex:C0");
        let kept = load(&mut vocabulary, "ex:a rdf:type ex:C1")[0];
        let (maintained, expected, reads) = commit(&mut vocabulary, &asserted, &[], &deleted);
        assert!(expected.binary_search(&kept).is_ok(), "rematerialised");
        assert_eq!(maintained, expected, "{equal:?}");
        assert!(reads > 0, "{equal:?}: the analysis a candidate asks for");
    }
}
