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

/// One rule instance: `:s :p :m` and `:m :p :o` asserted (in that order), `:s :p :o`
/// derived; the closure, the ground program and the asserted facts. With each premise in `n`
/// graphs and every set kept (`cap = n`), the instance's premise product has n² unions, of
/// which `minimal` keeps n.
fn product_input() -> (Vec<Triple>, crate::eval::GroundProgram, Vec<Triple>) {
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        "chain: (?x rdfs:p ?y), (?y rdfs:p ?z) -> (?x rdfs:p ?z)",
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let asserted = load(&mut vocabulary, "ex:s rdfs:p ex:m\nex:m rdfs:p ex:o");
    let derived = load(&mut vocabulary, "ex:s rdfs:p ex:o");
    let ground = program(
        &MemoryBase::new(&asserted, &[]),
        Rules {
            rules: &rules,
            lists: None,
            schema: &schema,
        },
    );
    let mut closure: Vec<Triple> = asserted.iter().chain(&derived).copied().collect();
    closure.sort_unstable();
    (closure, ground, asserted)
}

/// Review r1 C1 (`notes/review-p2-c5-r1/review.md`): the premise product, n² unions before
/// `minimal` keeps n, is admitted on the working budget before it is built. Under a budget
/// below its bytes (and far above what the kept sets take) the evaluation ends `Refused`,
/// within the limit, holding nothing; with an ample one it answers. Two rows: unions held
/// inline (graphs below 128) and unions whose words spill (graphs past 128).
#[test]
fn a_premise_product_is_admitted_before_it_is_built() {
    let n = 60u32;
    for (what, a, b) in [("inline", 0, n), ("spilled", 0, 200)] {
        let (closure, ground, asserted) = product_input();
        let graphs_of = |fact: Triple| -> Vec<u32> {
            match asserted.iter().position(|&f| f == fact) {
                Some(0) => (a..a + n).collect(),
                Some(_) => (b..b + n).collect(),
                None => Vec::new(),
            }
        };
        // What the product's unions hold: their slots, and each union's words on the heap.
        use crate::graph_sets::GraphSet;
        let words: usize = (a..a + n)
            .flat_map(|i| (b..b + n).map(move |j| (i, j)))
            .map(|(i, j)| GraphSet::of(i).union(&GraphSet::of(j)).heap_bytes())
            .sum();
        assert_eq!(
            words > 0,
            what == "spilled",
            "{what}: the row's unions spill or don't"
        );
        let product = (n * n) as usize * size_of::<GraphSet>() + words;
        let tight = Arc::new(Budget::new(product / 2));
        let result = support_sets(
            &closure,
            &ground,
            &[],
            &graphs_of,
            n as usize,
            Limits::unlimited(&tight),
        );
        assert!(
            matches!(result, Err(Stopped::Refused(_))),
            "{what}: {:?}",
            result.map(|sets| sets.len())
        );
        assert!(tight.peak() <= tight.limit(), "{what}: past the limit");
        assert_eq!(
            tight.used(),
            0,
            "{what}: a refused evaluation holds nothing"
        );
        let ample = Arc::new(Budget::unlimited());
        let sets = support_sets(
            &closure,
            &ground,
            &[],
            &graphs_of,
            n as usize,
            Limits::unlimited(&ample),
        )
        .unwrap();
        let head = closure
            .iter()
            .find(|f| !asserted.contains(f))
            .copied()
            .unwrap();
        assert_eq!(
            sets[&head].sets.len(),
            n as usize,
            "{what}: the head keeps n sets"
        );
        assert!(
            ample.peak() >= product,
            "{what}: the bound proves nothing: {}",
            ample.peak()
        );
    }
}
