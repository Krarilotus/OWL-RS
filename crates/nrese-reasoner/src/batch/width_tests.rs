//! Bounded tests use the real selector and the same solver under both layouts.

use super::*;
use crate::ir::{Atom, Guard, ListIndex, SharedListIndex};

fn schema() -> Schema {
    Schema::owl(&mut crate::vocabulary::LocalVocabulary::default())
}

fn count(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> usize {
    dense::Dense::of(
        &Input::Facts(input.to_vec()),
        rules,
        lists,
        schema,
        usize::MAX,
    )
    .unwrap()
    .table()
    .len()
        - 1
}

fn run_layout(
    input: &[Triple],
    rules: &[Rule],
    schema: &Schema,
    capacity: usize,
    output: Output,
) -> Materialisation {
    materialise_test_capacity(
        Input::Facts(input.to_vec()),
        rules,
        None,
        schema,
        None,
        output,
        NEVER,
        capacity,
    )
    .expect("representation selection succeeds")
}

fn rule(name: &str, body: Vec<Atom>, guards: Vec<Guard>, head: Head) -> Rule {
    Rule {
        name: name.into(),
        body,
        guards,
        head,
    }
}

#[test]
fn fact_and_consistency_dispatch_read_both_store_specializations() {
    fn dispatch<R: Representation>() {
        let p = 1 << 30;
        let body = vec![Atom([Term::Var(0), Term::Const(p), Term::Var(1)])];
        let rules = [
            rule(
                "copy",
                body.clone(),
                vec![],
                Head::Facts(vec![Atom([Term::Var(0), Term::Const(p + 1), Term::Var(1)])]),
            ),
            rule("check", body, vec![], Head::Inconsistent),
        ];
        let store = WorkingStore::<R>::new(vec![[1, p, 2]]);
        let mut program = GroundProgram::default();
        program.ground(&store, &schema(), &rules);
        assert_eq!(program.variants(&store), [(0, 0)]);
        assert_eq!(program.consistency_variants(&store), [(0, 0)]);
    }
    dispatch::<u64>();
    dispatch::<[u64; 2]>();
}

#[test]
fn capacity_sentinels_and_full_raw_ids_are_exact() {
    let ids = [
        0,
        10,
        u32::MAX as u64,
        u32::MAX as u64 + 1,
        1 << 63,
        u64::MAX - 1,
        u64::MAX,
    ];
    assert!(dense::Dense::new(ids.to_vec(), 6).is_none());
    let dense = dense::Dense::new(ids.to_vec(), 7).unwrap();
    assert_eq!(dense.number(0), 1);
    assert_eq!(dense.id(0), 0); // Unbound, distinct from raw zero's rank.
    for (i, id) in ids.into_iter().enumerate() {
        assert_eq!(dense.number(id), i as u64 + 1);
        assert_eq!(dense.id(i as u64 + 1), id);
    }
    assert!(dense::Dense::new(ids.repeat(3), 7).is_some());
    let small = dense::Dense::new(vec![0, 10, 20, 30, 40, 50, 60], 7).unwrap();
    let range = |low, high| {
        let r = rule(
            "range",
            vec![],
            vec![Guard::NotIn(Term::Var(0), low, high)],
            Head::Facts(vec![]),
        );
        small.rule(&r).guards[0].clone()
    };
    assert_eq!(range(60, 60), Guard::NotIn(Term::Var(0), 7, 7));
    assert_eq!(range(61, u64::MAX), Guard::NotIn(Term::Var(0), 8, 7));
    assert_eq!(range(21, 29), Guard::NotIn(Term::Var(0), 4, 3));
    assert_eq!(range(60, 50), Guard::NotIn(Term::Var(0), 7, 6));
    let last = u32::MAX as u64;
    assert_eq!(<u64 as Representation>::pair(last, last), u64::MAX);
    assert_eq!(<u64 as Representation>::first(u64::MAX), last);
    assert_eq!(std::mem::size_of::<u64>(), 8);
    assert_eq!(
        std::mem::size_of::<<u64 as Representation>::Candidate>(),
        16
    );
    assert_eq!(std::mem::size_of::<[u64; 2]>(), 16);
    assert_eq!(
        std::mem::size_of::<<[u64; 2] as Representation>::Candidate>(),
        24
    );

    let [a, p, b, q, c, _, _] = ids;
    let input = [[a, p, b], [b, p, c], [u64::MAX, p, a]];
    let v = Term::Var;
    let k = Term::Const;
    let rules = [rule(
        "copy",
        vec![Atom([v(0), k(p), v(1)])],
        vec![],
        Head::Facts(vec![Atom([v(0), k(q), v(1)])]),
    )];
    let schema = schema();
    let n = count(&input, &rules, None, &schema);
    let expected = crate::naive::materialise(&input, &rules, None);
    for cap in [n - 1, n, n + 1] {
        let facts = run_layout(&input, &rules, &schema, cap, Output::Facts);
        assert_eq!(
            facts
                .derived
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            expected.derived
        );
        assert_eq!(facts.counters.ids_bytes > 0, cap >= n);
        let output = run_layout(&input, &rules, &schema, cap, Output::Relations);
        assert_eq!(
            crate::tests::relations_hold(&output.relations),
            facts.derived
        );
        for relation in output.relations {
            for run in relation.by_subject {
                assert_eq!(
                    matches!(run, nrese_engine::PairChunks::Numbers { .. }),
                    cap >= n
                );
            }
        }
    }
    let output = DerivedRelation::of_facts(input.to_vec());
    assert_eq!(crate::tests::relations_hold(&output), {
        let mut facts = input.to_vec();
        facts.sort_unstable();
        facts
    });
    assert!(
        output
            .iter()
            .flat_map(|r| &r.by_subject)
            .all(|r| matches!(r, nrese_engine::PairChunks::Ids(_)))
    );
}

#[test]
fn selection_includes_rules_guards_hidden_schema_and_list_vocabulary() {
    let mut vocabulary = crate::vocabulary::LocalVocabulary::default();
    let schema = Schema::owl(&mut vocabulary);
    let empty = Input::Facts(vec![]);
    let baseline = count(&[], &[], None, &schema);
    assert!(dense::Dense::of(&empty, &[], None, &schema, baseline).is_some());
    let constant = u64::MAX;
    let head = rule(
        "constant",
        vec![],
        vec![],
        Head::Facts(vec![Atom([
            Term::Const(constant),
            Term::Const(constant),
            Term::Const(constant),
        ])]),
    );
    let guard = rule(
        "guard",
        vec![],
        vec![Guard::NotEqual(Term::Const(constant), Term::Const(0))],
        Head::Facts(vec![]),
    );
    for r in [head, guard] {
        assert!(dense::Dense::of(&empty, &[r], None, &schema, baseline).is_none());
    }
    let hidden = schema
        .clone()
        .hiding(std::collections::HashMap::from([(constant, false)]));
    assert!(dense::Dense::of(&empty, &[], None, &hidden, baseline).is_none());
    let lists = ListVocabulary::new(&mut vocabulary);
    assert!(count(&[], &[], Some(&lists), &schema) > baseline);
    assert!(dense::Dense::of(&empty, &[], Some(&lists), &schema, baseline).is_none());
}

#[test]
fn same_list_members_alone_cross_capacity_and_agree_with_the_oracle() {
    let (a, b, p, q, unused) = (
        1 << 40,
        (1 << 40) + 1,
        (1 << 40) + 2,
        (1 << 40) + 3,
        u64::MAX,
    );
    let v = Term::Var;
    let k = Term::Const;
    let input = [[a, p, b]];
    let schema = schema();
    let mut index = ListIndex::default();
    index.add(&[a, b]);
    index.add(&[unused]); // Not in a fact, vocabulary, guard term, or schema.
    let guard = Guard::SameList(v(0), v(1), SharedListIndex::new(index));
    let copy = rule(
        "list-copy",
        vec![Atom([v(0), k(p), v(1)])],
        vec![],
        Head::Facts(vec![Atom([v(0), k(q), v(1)])]),
    );
    let baseline = count(&input, std::slice::from_ref(&copy), None, &schema);
    let mut copy = copy;
    copy.guards.push(guard.clone());
    let violation = rule(
        "list-violation",
        copy.body.clone(),
        vec![guard],
        Head::Inconsistent,
    );
    let rules = [copy, violation];
    assert_eq!(count(&input, &rules, None, &schema), baseline + 1);
    let oracle = crate::naive::materialise(&input, &rules, None);
    for cap in [baseline, baseline + 1] {
        let result = run_layout(&input, &rules, &schema, cap, Output::Facts);
        assert_eq!(result.counters.ids_bytes > 0, cap > baseline);
        assert_eq!(
            result
                .derived
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            oracle.derived
        );
        assert_eq!(result.violations, oracle.violations);
    }
}

#[test]
fn guards_and_generic_walk_preserve_full_ids_and_empty_ranges() {
    let (p, q) = (u64::MAX, u64::MAX - 1);
    let v = Term::Var;
    let k = Term::Const;
    let input = [[0, p, 0], [1 << 63, p, 1 << 63], [0, q, 1 << 63]];
    let rules = [
        rule(
            "variable-predicate",
            vec![Atom([v(0), v(1), v(0)])],
            vec![Guard::NotIn(v(0), 10, 20)],
            Head::Facts(vec![Atom([v(0), k(q), v(0)]), Atom([v(0), k(q), k(5)])]),
        ),
        rule(
            "bodiless",
            vec![],
            vec![],
            Head::Facts(vec![Atom([k(7), k(p), k(8)])]),
        ),
        rule(
            "violation",
            vec![Atom([v(0), k(p), v(0)])],
            vec![Guard::NotIn(v(0), u64::MAX, u64::MAX)],
            Head::Inconsistent,
        ),
    ];
    let schema = schema();
    let oracle = crate::naive::materialise(&input, &rules, None);
    for cap in [0, usize::MAX] {
        let result = run_layout(&input, &rules, &schema, cap, Output::Facts);
        assert_eq!(
            result
                .derived
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            oracle.derived
        );
        let mut violations = oracle.violations.clone();
        violations.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
        assert_eq!(result.violations, violations);
    }
}

#[test]
fn wide_incremental_store_growth_and_family_identity_are_unrestricted() {
    let p = u64::MAX;
    let mut store = Store::new(vec![[0, p, 1 << 63]]);
    let fresh: Vec<Triple> = (1..=12).map(|s| [s, p, u64::MAX - s]).collect();
    store.advance(fresh.clone());
    store.advance(fresh); // Known-ID reinsertion is not a new allocation/refusal.
    assert!(store.contains([0, p, 1 << 63]));
    for s in 1..=12 {
        assert!(store.contains([s, p, u64::MAX - s]));
    }
    assert_eq!(store.facts_where(&|_| true).len(), 13);
    assert_ne!(Family::Schema(!p), Transitive::family(p));
    let mut relation = Relation::<[u64; 2]> {
        closed_by: Some(Family::Schema(!p)),
        ..Default::default()
    };
    let (pairs, marks) = new_pairs(vec![], vec![vec![[0, u64::MAX]]]);
    relation.advance(pairs, marks);
    let mut read = Vec::new();
    relation.scan(
        None,
        None,
        Seg::DeltaNotBy(Transitive::family(p)),
        &mut |s, o| read.push([s, o]),
    );
    assert_eq!(read, [[0, u64::MAX]]);
    read.clear();
    relation.scan(
        None,
        None,
        Seg::DeltaNotBy(Family::Schema(!p)),
        &mut |s, o| read.push([s, o]),
    );
    assert!(read.is_empty());
}

#[test]
fn full_id_insert_delete_cycles_and_support_updates_match_cold_recomputation() {
    use crate::delta::{MemoryBase, Rules, update};
    use crate::graph_sets::{Change, support_sets, update_support_sets};
    let (p, q) = (u64::MAX, u64::MAX - 1);
    let v = Term::Var;
    let rules = [rule(
        "copy",
        vec![Atom([v(0), Term::Const(p), v(1)])],
        vec![],
        Head::Facts(vec![Atom([v(0), Term::Const(q), v(1)])]),
    )];
    let schema = schema();
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let mut program = GroundProgram::default();
    program.add(rules[0].clone());
    let pool: Vec<Triple> = (0..12).map(|n| [n, p, (1 << 63) + n]).collect();
    let mut asserted = Vec::new();
    let mut inferred = Vec::new();
    let mut previous = HashMap::new();
    for fact in pool.iter().chain(pool.iter()).chain(pool.iter()).copied() {
        let deleting = asserted.contains(&fact);
        let before: Vec<Triple> = asserted.iter().chain(&inferred).copied().collect();
        if deleting {
            asserted.retain(|&t| t != fact);
        } else {
            asserted.push(fact);
            asserted.sort_unstable();
        }
        let base = MemoryBase::new(&asserted, &inferred);
        let changed = std::slice::from_ref(&fact);
        let (inserted, deleted): (&[Triple], &[Triple]) = if deleting {
            (&[], changed)
        } else {
            (changed, &[])
        };
        let result = update(&base, inserted, deleted, compiled, None);
        inferred.retain(|t| !result.remove.contains(t));
        inferred.extend(result.insert);
        inferred.sort_unstable();
        inferred.dedup();
        let cold = run_layout(&asserted, &rules, &schema, 0, Output::Facts);
        assert_eq!(inferred, cold.derived);
        let after: Vec<Triple> = asserted.iter().chain(&inferred).copied().collect();
        let mut reachable = before.clone();
        reachable.extend(&after);
        let source = Store::new(reachable);
        let touched: Vec<Triple> = before
            .iter()
            .filter(|t| !after.contains(t))
            .chain(after.iter().filter(|t| !before.contains(t)))
            .copied()
            .collect();
        let graphs = |f| {
            if asserted.contains(&f) {
                vec![11]
            } else {
                vec![]
            }
        };
        let prior = |f| previous.get(&f).cloned().unwrap_or_default();
        let present = |f| after.contains(&f);
        let change = Change {
            source: &source,
            in_closure: &present,
            touched: &touched,
            previous: &prior,
            limit: usize::MAX,
        };
        let changed = update_support_sets(&change, &program, &[], &graphs, usize::MAX).unwrap();
        let expected = support_sets(&after, &program, &[], &graphs, usize::MAX);
        for &f in &after {
            assert_eq!(
                changed.get(&f).cloned().unwrap_or_else(|| prior(f)),
                expected.get(&f).cloned().unwrap_or_default()
            );
        }
        previous = expected;
    }
}

#[test]
fn old_complement_family_collision_cannot_suppress_closure() {
    use crate::ir::Vocabulary;
    let mut vocabulary = crate::vocabulary::LocalVocabulary::default();
    let original = Schema::owl(&mut vocabulary);
    let ty = original.rdf_type();
    let sco = vocabulary.iri(&format!("{}subClassOf", crate::ir::RDFS));
    let schema = original.map_ids(&|id| {
        if id == ty {
            u64::MAX
        } else if id == sco {
            0
        } else {
            id
        }
    });
    let (a, b, c, d) = (1 << 40, (1 << 40) + 1, (1 << 40) + 2, (1 << 40) + 3);
    let ty = schema.rdf_type();
    let (v, k) = (Term::Var, Term::Const);
    let rules = [
        crate::eval::transitivity(ty),
        rule(
            "cax-sco",
            vec![Atom([v(0), k(ty), v(1)]), Atom([v(1), k(0), v(2)])],
            vec![],
            Head::Facts(vec![Atom([v(0), k(ty), v(2)])]),
        ),
    ];
    let input = [[a, ty, b], [b, ty, c], [b, 0, d], [c, 0, d]];
    let oracle = crate::naive::materialise(&input, &rules, None);
    for capacity in [0, usize::MAX] {
        let result = run_layout(&input, &rules, &schema, capacity, Output::Facts);
        assert_eq!(
            result
                .derived
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            oracle.derived
        );
        assert!(result.derived.contains(&[a, ty, d]));
    }
}

#[test]
fn stops_discard_partial_closures_in_both_layouts() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let p = u64::MAX;
    let input = [[0, p, 1], [1, p, 1 << 63], [1 << 63, p, p]];
    let rules = [crate::eval::transitivity(p)];
    let schema = schema();
    let committed = Store::new(input.to_vec());
    for cap in [0, usize::MAX] {
        let polls = AtomicUsize::new(0);
        let counting = || {
            polls.fetch_add(1, Ordering::Relaxed);
            false
        };
        let complete = materialise_test_capacity(
            Input::Facts(input.to_vec()),
            &rules,
            None,
            &schema,
            None,
            Output::Facts,
            &counting,
            cap,
        )
        .unwrap();
        assert!(!complete.derived.is_empty());
        let total = polls.load(Ordering::Relaxed);
        for at in [0, 1, total / 2, total - 1] {
            let seen = AtomicUsize::new(0);
            let stop = || seen.fetch_add(1, Ordering::Relaxed) >= at;
            assert_eq!(
                materialise_test_capacity(
                    Input::Facts(input.to_vec()),
                    &rules,
                    None,
                    &schema,
                    None,
                    Output::Facts,
                    &stop,
                    cap
                )
                .err(),
                Some(Interrupted)
            );
            assert_eq!(committed.facts_where(&|_| true).len(), input.len());
            for fact in input {
                assert!(committed.contains(fact));
            }
        }
    }
}

/// Isolate the existing process-global backstop from parallel tests in this binary.
#[test]
#[cfg(not(miri))]
fn process_memory_stop_remains_interruption_and_preserves_prior_state() {
    const CHILD: &str = "NRESE_RULES_WIDTH_MEMORY_STOP_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "batch::width_tests::process_memory_stop_remains_interruption_and_preserves_prior_state",
                "--test-threads=1"])
            .env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    let schema = schema();
    let input = [[0, u64::MAX, 1 << 63]];
    let prior = Store::new(input.to_vec());
    nrese_exec::memory::set_process_limit(1);
    assert!(crate::eval::over_memory_limit());
    for capacity in [0, usize::MAX] {
        assert_eq!(
            materialise_test_capacity(
                Input::Facts(input.to_vec()),
                &[],
                None,
                &schema,
                None,
                Output::Relations,
                NEVER,
                capacity
            )
            .err(),
            Some(Interrupted)
        );
        assert!(
            crate::eval::over_memory_limit(),
            "callers still classify a memory stop"
        );
        assert!(prior.contains(input[0]));
        assert_eq!(prior.facts_where(&|_| true), input);
    }
}

#[test]
fn all_equality_listings_and_diagnostics_agree_across_layouts() {
    use crate::ir::Vocabulary;
    let mut vocabulary = crate::vocabulary::LocalVocabulary::default();
    let rules = crate::rulesets::Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .unwrap();
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let same = vocabulary.iri(&format!("{}sameAs", crate::ir::OWL));
    let first = vocabulary.iri(&format!("{}first", crate::ir::RDF));
    let union = vocabulary.iri(&format!("{}unionOf", crate::ir::OWL));
    let (a, b, c, p, q, list) = (
        u64::MAX - 5,
        u64::MAX - 4,
        u64::MAX - 3,
        u64::MAX - 2,
        u64::MAX - 1,
        u64::MAX,
    );
    let input = [
        [a, same, b],
        [b, same, c],
        [p, same, q],
        [c, p, a],
        [a, union, list],
        [list, first, b],
    ]; // Missing rest: original-ID diagnostic.
    let replicated = crate::naive::materialise(&input, &rules, Some(&lists));
    for listing in [Listing::Representatives, Listing::Stored, Listing::Expanded] {
        let run = |cap, output| {
            materialise_test_capacity(
                Input::Facts(input.to_vec()),
                &rules,
                Some(&lists),
                &schema,
                Some(listing),
                output,
                NEVER,
                cap,
            )
            .unwrap()
        };
        let narrow = run(usize::MAX, Output::Facts);
        let wide = run(0, Output::Facts);
        assert_eq!(wide.derived, narrow.derived);
        assert_eq!(wide.violations, narrow.violations);
        assert_eq!(wide.diagnostics, narrow.diagnostics);
        let classes = |r: &Materialisation| {
            let mut classes: Vec<_> = r
                .classes
                .classes()
                .map(|(rep, members)| {
                    let mut members = members.to_vec();
                    members.sort_unstable();
                    (rep, members)
                })
                .collect();
            classes.sort_unstable();
            classes
        };
        assert_eq!(classes(&wide), classes(&narrow));
        if listing == Listing::Expanded {
            assert_eq!(
                wide.derived
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>(),
                replicated.derived
            );
        }
        for cap in [0, usize::MAX] {
            let relations = run(cap, Output::Relations);
            let mut facts: Vec<Triple> = relations
                .relations
                .iter()
                .flat_map(DerivedRelation::facts)
                .collect();
            facts.sort_unstable();
            facts.dedup();
            assert_eq!(facts, wide.derived);
        }
    }
}
