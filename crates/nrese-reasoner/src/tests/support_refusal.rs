//! A support-set evaluation whose round's derivations don't fit its working budget
//! (SYN-21, B-703): the charge is refused before they are allocated, the evaluation ends
//! `Refused`, never a result missing what the refused derivations would have given, and
//! nothing stays charged.

use std::sync::Arc;

use nrese_exec::Budget;

use super::load;
use crate::batch::Schema;
use crate::delta::{MemoryBase, Rules, program};
use crate::graph_sets::{Limits, Stopped, support_sets};
use crate::ir::{Triple, parse_rules};
use crate::vocabulary::LocalVocabulary;

/// One driver joined with two atoms of `N` facts: N² bindings in one morsel, each a
/// derivation of one of N heads.
const N: usize = 300;

/// The input's closure, ground program and graphs: every fact in graph 0.
fn input() -> (Vec<Triple>, crate::eval::GroundProgram) {
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        "join: (?s rdfs:p ?o), (?x rdfs:q ?y), (?u rdfs:q ?v) -> (?s rdfs:r ?v)",
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let text: Vec<String> = (0..N)
        .map(|i| format!("ex:x{i} rdfs:q ex:y{i}"))
        .chain(["ex:a rdfs:p ex:b".to_owned()])
        .collect();
    let mut facts = load(&mut vocabulary, &text.join("\n"));
    facts.sort_unstable();
    let ground = program(
        &MemoryBase::new(&facts, &[]),
        Rules {
            rules: &rules,
            lists: None,
            schema: &schema,
        },
    );
    let heads: String = (0..N).map(|i| format!("ex:a rdfs:r ex:y{i}\n")).collect();
    let mut closure = facts;
    closure.extend(load(&mut vocabulary, &heads));
    closure.sort_unstable();
    (closure, ground)
}

/// Under a working budget of 16 KiB (the morsel finds 90,000 derivations of 56 bytes; its
/// first reduction comes at 65,536), the evaluation is `Refused` and holds nothing; with
/// an unlimited one it gives every fact its set.
#[test]
fn a_refused_round_ends_the_evaluation_and_holds_nothing() {
    let (closure, ground) = input();
    let in_one_graph = |_: Triple| vec![0];
    let tight = Arc::new(Budget::new(16 << 10));
    let result = support_sets(
        &closure,
        &ground,
        &[],
        &in_one_graph,
        16,
        Limits::unlimited(&tight),
    );
    assert!(
        matches!(result, Err(Stopped::Refused(_))),
        "{:?}",
        result.map(|sets| sets.len())
    );
    assert!(
        tight.peak() <= tight.limit(),
        "{} past the limit",
        tight.peak()
    );
    assert_eq!(tight.used(), 0, "a refused evaluation holds nothing");
    let ample = Arc::new(Budget::unlimited());
    let sets = support_sets(
        &closure,
        &ground,
        &[],
        &in_one_graph,
        16,
        Limits::unlimited(&ample),
    )
    .unwrap();
    assert_eq!(sets.len(), closure.len());
    assert!(
        ample.peak() > tight.limit(),
        "the bound proves nothing: {}",
        ample.peak()
    );
}
