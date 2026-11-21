use nrese_rdf::NamedNode;
use nrese_reasoner::{ReasoningMode, Ruleset, delta::policy::Plan};
use nrese_store::ReadScope;

use crate::{
    cells::check,
    generator::{case, delete, insert},
    judges, triple,
};

const CYCLIC_CHAIN: &str = "
    ex:out owl:propertyChainAxiom ex:h
    ex:h rdf:first ex:p
    ex:h rdf:rest ex:t
    ex:t rdf:first ex:q
    ex:t rdf:rest ex:h
    ex:t rdf:rest rdf:nil
";

#[test]
fn long_intersection_requires_every_class() {
    // Spec: Table 6 cls-int1 types a in the intersection of all 300 classes.
    // With only 299 memberships there is no grounding for b.
    let mut schema = insert("ex:All owl:intersectionOf ex:c0");
    let mut members = insert("ex:b ex:tag ex:a");
    for i in 0..300 {
        let cell = format!("ex:c{i}");
        let class = format!("ex:C{i}");
        let rest = if i == 299 {
            "rdf:nil".into()
        } else {
            format!("ex:c{}", i + 1)
        };
        schema.insert.push(triple([&cell, "rdf:first", &class]));
        schema.insert.push(triple([&cell, "rdf:rest", &rest]));
        members.insert.push(triple(["ex:a", "rdf:type", &class]));
        if i < 299 {
            members.insert.push(triple(["ex:b", "rdf:type", &class]));
        }
    }
    let mut fixture = case(
        "long-intersection",
        ["ex:a", "rdf:type", "ex:All"],
        vec![schema, members],
    );
    check(fixture.clone(), &[true; 2], &[false, true]);
    fixture.watch = triple(["ex:b", "rdf:type", "ex:All"]);
    check(fixture, &[true; 2], &[false; 2]);
}

#[test]
fn sixty_fifth_first_adds_only_its_walk() {
    // Spec: Tables 3 and 5 add the 65th prp-spo2 grounding with its first,
    // then retract it on deletion, leaving the other 64 groundings intact.
    let mut setup = insert(
        "
        ex:out owl:propertyChainAxiom ex:h
        ex:h rdf:rest ex:t
        ex:t rdf:first ex:q
        ex:t rdf:rest rdf:nil
        ex:a ex:p0 ex:b
        ex:b ex:q ex:c
        ex:d ex:p64 ex:b
        ",
    );
    setup
        .insert
        .extend((0..64).map(|i| triple(["ex:h", "rdf:first", &format!("ex:p{i}")])));
    let added = "ex:h rdf:first ex:p64";
    let mut remove = delete(added);
    remove.insert.push(triple(["ex:a", "ex:tag", "ex:c"]));
    let mut fixture = case(
        "form-switch",
        ["ex:d", "ex:out", "ex:c"],
        vec![setup, insert(added), remove],
    );
    check(fixture.clone(), &[true; 3], &[false, true, false]);
    fixture.watch = triple(["ex:a", "ex:out", "ex:c"]);
    check(fixture.clone(), &[true; 3], &[true; 3]);
    let store = judges::open("replication", None);
    let pipeline = judges::pipeline(&store, ReasoningMode::Owl2Rl);
    assert!(judges::apply(&pipeline, &fixture.writes[0]));
    for write in &fixture.writes[1..] {
        assert!(judges::apply(&pipeline, write));
        assert_ne!(
            pipeline.last_reasoning_run().unwrap().work.plan,
            Plan::Rebuild
        );
    }
}

#[test]
fn all_different_refuses_a_repeated_position_value() {
    // Spec: Tables 3 and 4 eq-diff2 compare distinct positions, including
    // two visits to the same cyclic cell. In both walks a is different from itself.
    for (shape, list) in [
        (
            "repeated-member",
            "ex:t rdf:rest ex:u\nex:u rdf:first ex:a\nex:u rdf:rest rdf:nil",
        ),
        ("repeated-cell", "ex:t rdf:rest ex:h\nex:t rdf:rest rdf:nil"),
    ] {
        let mut setup = insert(
            "
            ex:ax rdf:type owl:AllDifferent
            ex:ax owl:distinctMembers ex:h
            ex:h rdf:first ex:a
            ex:h rdf:rest ex:t
            ex:t rdf:first ex:b
            ",
        );
        setup.insert.extend(insert(list).insert);
        check(
            case(shape, ["ex:a", "owl:sameAs", "ex:a"], vec![setup]),
            &[false],
            &[true],
        );
    }
}

#[test]
fn cyclic_union_intersection_and_key_use_their_simple_walks() {
    // Spec: Tables 3, 5 and 6 derive each of these facts from a one-cell
    // accepting walk. Going around its loop does not add a conjunct.
    let schema = "
        ex:U owl:unionOf ex:u
        ex:u rdf:first ex:A
        ex:u rdf:rest ex:u
        ex:u rdf:rest rdf:nil
        ex:I owl:intersectionOf ex:i
        ex:i rdf:first ex:A
        ex:i rdf:rest ex:i
        ex:i rdf:rest rdf:nil
        ex:K owl:hasKey ex:k
        ex:k rdf:first ex:p
        ex:k rdf:rest ex:k
        ex:k rdf:rest rdf:nil
    ";
    for (shape, data, watch) in [
        (
            "cyclic-union",
            "ex:a rdf:type ex:A",
            ["ex:a", "rdf:type", "ex:U"],
        ),
        (
            "cyclic-intersection",
            "ex:a rdf:type ex:A",
            ["ex:a", "rdf:type", "ex:I"],
        ),
        (
            "cyclic-key",
            "ex:a rdf:type ex:K\nex:b rdf:type ex:K\nex:a ex:p ex:v\nex:b ex:p ex:v",
            ["ex:a", "owl:sameAs", "ex:b"],
        ),
    ] {
        check(
            case(shape, watch, vec![insert(schema), insert(data)]),
            &[true; 2],
            &[false, true],
        );
    }
}

#[test]
fn two_step_self_chain_gives_transitive_closure_inside_a_cycle() {
    // Spec: Table 5 prp-spo2 gives p(p(a,b),p(b,c)) and then composes
    // that new p edge with p(c,d), even when the list has longer walks too.
    check(
        case(
            "cyclic-transitivity",
            ["ex:a", "ex:p", "ex:d"],
            vec![
                insert(
                    "
                    ex:p owl:propertyChainAxiom ex:h
                    ex:h rdf:first ex:p
                    ex:h rdf:rest ex:t
                    ex:t rdf:first ex:p
                    ex:t rdf:rest ex:h
                    ex:t rdf:rest rdf:nil
                    ",
                ),
                insert("ex:a ex:p ex:b\nex:b ex:p ex:c\nex:c ex:p ex:d"),
            ],
        ),
        &[true; 2],
        &[false, true],
    );
}

#[test]
fn keys_share_a_tail_and_still_require_their_own_class() {
    // Spec: Table 5 prp-key equates instances of the key's own class
    // when they share a value for every member of one walk of that key.
    let mut schema = insert(
        "
        ex:C1 owl:hasKey ex:h1
        ex:C2 owl:hasKey ex:h2
        ex:h1 rdf:first ex:id1
        ex:h1 rdf:rest ex:t
        ex:h2 rdf:first ex:id2
        ex:h2 rdf:rest ex:t
        ex:t rdf:rest rdf:nil
        ",
    );
    schema
        .insert
        .extend((0..65).map(|i| triple(["ex:t", "rdf:first", &format!("ex:p{i}")])));
    let data = insert(
        "
        ex:a rdf:type ex:C1
        ex:b rdf:type ex:C1
        ex:c rdf:type ex:C2
        ex:d rdf:type ex:C2
        ex:a ex:id1 ex:id
        ex:b ex:id1 ex:id
        ex:c ex:id1 ex:id
        ex:c ex:id2 ex:id
        ex:d ex:id2 ex:id
        ex:a ex:p0 ex:value
        ex:b ex:p0 ex:value
        ex:c ex:p0 ex:value
        ex:d ex:p0 ex:value
        ",
    );
    let writes = vec![schema, data];
    for (shape, watch, present) in [
        ("shared-key-tail-c1", ["ex:a", "owl:sameAs", "ex:b"], true),
        ("shared-key-tail-c2", ["ex:c", "owl:sameAs", "ex:d"], true),
        (
            "shared-key-tail-across-classes",
            ["ex:a", "owl:sameAs", "ex:c"],
            false,
        ),
    ] {
        check(
            case(shape, watch, writes.clone()),
            &[true; 2],
            &[false, present],
        );
    }
}

#[test]
fn equating_two_chain_cells_matches_each_nonempty_walk() {
    // Spec: Table 4 eq-rep merges the list operands and rest links, so
    // Tables 3 and 5 match q1, q2, and q1 followed by q2.
    let writes = vec![insert(
        "
        ex:p owl:propertyChainAxiom ex:h
        ex:h rdf:first ex:q1
        ex:h rdf:rest ex:t
        ex:t rdf:first ex:q2
        ex:t rdf:rest rdf:nil
        ex:t owl:sameAs ex:h
        ex:a0 ex:q1 ex:a1
        ex:a1 ex:q2 ex:a2
        ",
    )];
    for watch in [
        ["ex:a0", "ex:p", "ex:a1"],
        ["ex:a1", "ex:p", "ex:a2"],
        ["ex:a0", "ex:p", "ex:a2"],
    ] {
        check(
            case("merged-chain-cells", watch, writes.clone()),
            &[true],
            &[true],
        );
    }
}

#[test]
fn an_equality_derived_inside_a_run_merges_per_cell_relations() {
    // Spec: Table 5 prp-fp equates h with t. Table 4 eq-rep makes
    // q a first of h and rdf:nil a rest, so q alone entails the new row.
    let setup = insert(CYCLIC_CHAIN);
    let data = insert(
        "
        ex:cell rdf:type owl:FunctionalProperty
        ex:owner ex:cell ex:h
        ex:owner ex:cell ex:t
        ex:b ex:q ex:c
        ",
    );
    check(
        case(
            "per-cell-merge",
            ["ex:b", "ex:out", "ex:c"],
            vec![setup, data],
        ),
        &[true; 2],
        &[false, true],
    );
}

#[test]
fn per_cell_conclusions_have_public_proofs_and_clash_witnesses() {
    // Spec: Tables 3 and 5 prp-spo2 derive the chain from the asserted
    // walk and data edges. prp-pdw refuses a disjoint edge on that pair.
    let store = judges::open("replication", None);
    let pipeline = judges::pipeline(&store, ReasoningMode::Owl2Rl);
    let mut setup = insert(CYCLIC_CHAIN);
    setup
        .insert
        .extend(insert("ex:a ex:p ex:b\nex:b ex:q ex:c").insert);
    assert!(judges::apply(&pipeline, &setup));
    assert!(judges::ask(&store, &triple(["ex:a", "ex:out", "ex:c"])).0);
    let [s, p, o] = ["a", "out", "c"]
        .map(|term| NamedNode::new_unchecked(format!("http://example.org/{term}")));
    let steps = store
        .explain_statement(
            Ruleset::Owl2Rl,
            &ReadScope::All,
            s.as_ref().into(),
            p.as_ref().into(),
            o.as_ref().into(),
        )
        .unwrap()
        .expect("a chain explanation");
    assert_eq!(steps[0].rule.as_deref(), Some("prp-spo2"));
    let axiom = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
    for predicate in [axiom, "http://example.org/p", "http://example.org/q"] {
        assert!(
            steps
                .iter()
                .any(|step| step.predicate == predicate && step.origin == "asserted")
        );
    }
    assert!(
        steps
            .iter()
            .all(|step| step == &steps[0] || step.origin == "asserted")
    );
    assert!(!judges::apply(
        &pipeline,
        &insert("ex:out owl:propertyDisjointWith ex:r\nex:a ex:r ex:c")
    ));
    let witness = pipeline
        .last_reasoning_run()
        .unwrap()
        .primary_reject
        .expect("clash witness");
    assert_eq!(witness.violated_constraint, "prp-pdw");
    assert!(
        witness
            .evidence
            .iter()
            .any(|fact| fact.predicate == "http://example.org/out")
    );
    assert!(witness.evidence.iter().any(|fact| fact.predicate == axiom));
}

#[test]
fn later_writes_maintain_per_cell_support_with_or_without_a_warm_table() {
    // Spec: Table 5 prp-spo2 derives the row when its final data edge arrives
    // and loses it when that edge is deleted. Both writes use the delta.
    let setup = insert(CYCLIC_CHAIN);
    let data = insert("ex:a ex:p ex:b\nex:b ex:q ex:c");
    let mut remove = delete("ex:b ex:q ex:c");
    remove.insert.push(triple(["ex:a", "ex:tag", "ex:c"]));
    let fixture = case(
        "per-cell-later-write",
        ["ex:a", "ex:out", "ex:c"],
        vec![setup, data, remove],
    );
    check(fixture.clone(), &[true; 3], &[false, true, false]);
    for &mode in judges::MODES {
        for cold in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = judges::open(mode, Some(dir.path()));
            let mut pipeline = judges::pipeline(&store, ReasoningMode::Owl2Rl);
            for (i, step) in fixture.writes.iter().enumerate() {
                if cold && i != 0 {
                    drop(pipeline);
                    drop(store);
                    store = judges::open(mode, Some(dir.path()));
                    pipeline = judges::pipeline(&store, ReasoningMode::Owl2Rl);
                }
                assert!(judges::apply(&pipeline, step));
                assert_eq!(
                    judges::ask(&store, &fixture.watch),
                    (i == 1, "complete".into())
                );
                if i != 0 {
                    assert_ne!(
                        pipeline.last_reasoning_run().unwrap().work.plan,
                        Plan::Rebuild
                    );
                }
            }
        }
    }
}
