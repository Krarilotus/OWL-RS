use std::collections::HashSet;
use std::sync::Arc;

use nrese_exec::Budget;
use nrese_reasoner::batch::{self, Schema};
use nrese_reasoner::delta::{MemoryBase, Rules, introduced_violations, update};
use nrese_reasoner::eval::OperationStop;
use nrese_reasoner::ir::{OWL, RDF, RDFS, Vocabulary};
use nrese_reasoner::lists::{ListVocabulary, PAIRWISE_MEMBERS};
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

#[derive(Clone, Copy)]
enum Clash {
    /// The subject has both original types. The alternate class has no instances.
    Survives,
    /// The subject has class/1 and the alternate type. The list adds their disjointness.
    Introduced,
    /// The subject has both original types. The update deletes its alternate type.
    SurvivesMixedUpdate,
    /// The subject starts with class/1. The update asserts its class/0 type.
    InsertedType,
    /// The subject starts with class/1. The update gives it class/0 through a subclass.
    DerivedType,
    /// The subject has both original types. The update makes an untyped alias equal to it.
    ExpandedEquality,
}

// Spec (delta's introduced-violations contract): the report is the set of witnesses
// in the fully materialised after-state minus those in the fully materialised before-state.
fn check_list_insertion(len: usize, clash: Clash) -> Vec<String> {
    let mut v = LocalVocabulary::default();
    let schema = Schema::owl(&mut v);
    let lists = ListVocabulary::new(&mut v);
    let first = v.iri(&format!("{RDF}first"));
    let rest = v.iri(&format!("{RDF}rest"));
    let nil = v.iri(&format!("{RDF}nil"));
    let kind = v.iri(&format!("{OWL}AllDisjointClasses"));
    let members = v.iri(&format!("{OWL}members"));
    let list = v.iri("http://example.com/list");
    let subject = v.iri("http://example.com/subject");
    let ty = schema.rdf_type();
    let class0 = v.iri("http://example.com/class/0");
    let class1 = v.iri("http://example.com/class/1");
    let cells: Vec<_> = (0..len)
        .map(|i| v.iri(&format!("http://example.com/cell/{i}")))
        .collect();
    let mut before = vec![
        [list, ty, kind],
        [list, members, cells[0]],
        [subject, ty, class1],
    ];
    for (i, &cell) in cells.iter().enumerate() {
        let class = v.iri(&format!("http://example.com/class/{i}"));
        before.extend([
            [cell, first, class],
            [cell, rest, cells.get(i + 1).copied().unwrap_or(nil)],
        ]);
    }
    let alternate = v.iri("http://example.com/alternate");
    let alternate_type = [subject, ty, alternate];
    let original_type = [subject, ty, class0];
    let mut inserted = vec![[cells[0], first, alternate]];
    let mut deleted = Vec::new();
    let mut equality = None;
    let mut rules = Vec::new();
    let witness_counts = match clash {
        Clash::Survives => {
            before.push(original_type);
            (1, 1, 0)
        }
        Clash::Introduced => {
            before.push(alternate_type);
            (0, 1, 1)
        }
        Clash::SurvivesMixedUpdate => {
            before.extend([original_type, alternate_type]);
            deleted.push(alternate_type);
            (1, 1, 0)
        }
        Clash::InsertedType => {
            inserted.push(original_type);
            (0, 1, 1)
        }
        Clash::DerivedType => {
            let subclass = v.iri("http://example.com/Sub");
            let sub_class_of = v.iri(&format!("{RDFS}subClassOf"));
            before.push([subclass, sub_class_of, class0]);
            inserted.push([subject, ty, subclass]);
            rules = Ruleset::Owl2Rl.rules(&mut v).unwrap();
            rules.retain(|rule| rule.name == "cax-sco");
            (0, 1, 1)
        }
        Clash::ExpandedEquality => {
            let alias = v.iri("http://example.com/alias");
            let same_as = v.iri(&format!("{OWL}sameAs"));
            before.push(original_type);
            inserted.push([subject, same_as, alias]);
            rules = Ruleset::Owl2Rl.rules(&mut v).unwrap();
            rules.retain(|rule| {
                matches!(
                    rule.name.as_str(),
                    "eq-sym" | "eq-trans" | "eq-rep-s" | "eq-rep-p" | "eq-rep-o"
                )
            });
            equality = Some(same_as);
            (1, 2, 1)
        }
    };
    let after: Vec<_> = before
        .iter()
        .copied()
        .filter(|f| !deleted.contains(f))
        .chain(inserted.iter().copied())
        .collect();
    let old = batch::materialise(&before, &rules, Some(&lists), &schema);
    let new = batch::materialise(&after, &rules, Some(&lists), &schema);
    let old_witnesses: HashSet<_> = old.violations.to_vec().into_iter().collect();
    let new_witnesses: HashSet<_> = new.violations.to_vec().into_iter().collect();
    let expected: HashSet<_> = new_witnesses.difference(&old_witnesses).cloned().collect();
    assert_eq!(
        (old_witnesses.len(), new_witnesses.len(), expected.len()),
        witness_counts,
        "fixture: before={old_witnesses:?}, after={new_witnesses:?}"
    );

    let compiled = Rules {
        rules: &rules,
        schema: &schema,
        lists: Some(&lists),
        equality,
    };
    let base = MemoryBase::new(&after, &old.derived);
    let derived: Vec<_> = new
        .derived
        .into_iter()
        .filter(|f| !old.derived.contains(f))
        .collect();
    let mut mismatches = Vec::new();
    for threads in [1, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let mut compare = |path, reported: Vec<_>| {
            let reported: HashSet<_> = reported.into_iter().collect();
            if reported != expected {
                mismatches.push(format!(
                    "introduced {path} len={len} threads={threads}: expected {expected:?}, got {reported:?}"
                ));
            }
        };
        let result = pool.install(|| update(&base, &inserted, &deleted, compiled, None));
        compare("delta", result.violations.to_vec());
        if deleted.is_empty() {
            let report = pool
                .install(|| {
                    introduced_violations(
                        &base,
                        &inserted,
                        &derived,
                        compiled,
                        None,
                        &OperationStop::new(&|| false),
                        &Arc::new(Budget::unlimited()),
                    )
                })
                .unwrap();
            compare("bulk", report.to_vec());
        }
    }
    mismatches
}

#[test]
fn long_list_insert_keeps_old_clash_old() {
    let mismatches = check_list_insertion(PAIRWISE_MEMBERS + 1, Clash::Survives);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn pairwise_list_insert_keeps_old_clash_old() {
    for len in [2, PAIRWISE_MEMBERS] {
        let mismatches = check_list_insertion(len, Clash::Survives);
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }
}

#[test]
fn long_list_insert_reports_new_clash() {
    let mismatches = check_list_insertion(PAIRWISE_MEMBERS + 1, Clash::Introduced);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn long_list_insert_reports_clash_from_asserted_type() {
    let mismatches = check_list_insertion(PAIRWISE_MEMBERS + 1, Clash::InsertedType);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn long_list_insert_reports_clash_from_derived_type() {
    let mismatches = check_list_insertion(PAIRWISE_MEMBERS + 1, Clash::DerivedType);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn long_list_insert_reports_clash_from_expanded_equality() {
    let mismatches = check_list_insertion(PAIRWISE_MEMBERS + 1, Clash::ExpandedEquality);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn long_list_mixed_update_keeps_old_clash_old() {
    let mismatches = check_list_insertion(PAIRWISE_MEMBERS + 1, Clash::SurvivesMixedUpdate);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
