use crate::{
    cells::check,
    generator::{self, Write, case, delete, insert},
    per_cell::check_later_writes,
    triple,
};

const RETURN_BRANCH: &str = "
    ex:h rdf:first ex:A
    ex:h rdf:rest ex:a
    ex:a rdf:first ex:B
    ex:a rdf:rest ex:c
    ex:a rdf:rest rdf:nil
    ex:c rdf:first ex:D
    ex:c rdf:rest ex:a
";
const NIL_CONTINUATION: &str = "
    ex:h rdf:first ex:A
    ex:h rdf:rest rdf:nil
    rdf:nil rdf:first ex:B
    rdf:nil rdf:rest rdf:nil
";

fn intersection_member(shape: &'static str, list: &str, watch: [&str; 3]) {
    // Tables 3, 6 and 9 include every member of an accepting walk in
    // cls-int2 and scm-int, even when the walk returns to a cell.
    let mut write = insert(list);
    write
        .insert
        .extend(insert("ex:C owl:intersectionOf ex:h\nex:x rdf:type ex:C").insert);
    check(case(shape, watch, vec![write]), &[true], &[true]);
}

#[test]
fn a_return_branch_contributes_intersection_membership() {
    intersection_member(
        "intersection-return-type",
        RETURN_BRANCH,
        ["ex:x", "rdf:type", "ex:D"],
    );
}

#[test]
fn a_return_branch_contributes_an_intersection_superclass() {
    intersection_member(
        "intersection-return-class",
        RETURN_BRANCH,
        ["ex:C", "rdfs:subClassOf", "ex:D"],
    );
}

#[test]
fn continuing_from_nil_contributes_intersection_membership() {
    intersection_member(
        "intersection-nil-type",
        NIL_CONTINUATION,
        ["ex:x", "rdf:type", "ex:B"],
    );
}

#[test]
fn continuing_from_nil_contributes_an_intersection_superclass() {
    intersection_member(
        "intersection-nil-class",
        NIL_CONTINUATION,
        ["ex:C", "rdfs:subClassOf", "ex:B"],
    );
}

#[test]
fn a_key_can_use_a_walk_that_omits_a_revisited_member() {
    // Table 5 prp-key still fires when a shorter accepting walk uses
    // only the properties on which the instances agree.
    for (shape, list, data) in [
        (
            "key-return",
            RETURN_BRANCH,
            "ex:x ex:B ex:b\nex:y ex:B ex:b",
        ),
        ("key-nil", NIL_CONTINUATION, ""),
    ] {
        let mut write = insert(list);
        write.insert.extend(insert(data).insert);
        check_key(shape, write);
    }
}

fn check_key(shape: &'static str, mut write: Write) {
    write.insert.extend(
        insert(
            "ex:C owl:hasKey ex:h
         ex:x rdf:type ex:C
         ex:y rdf:type ex:C
         ex:x ex:A ex:v
         ex:y ex:A ex:v",
        )
        .insert,
    );
    check(
        case(shape, ["ex:x", "owl:sameAs", "ex:y"], vec![write]),
        &[true],
        &[true],
    );
}

#[test]
fn a_dense_return_branch_keeps_the_intersection_and_key_answers() {
    // Tables 3, 5 and 6 accept the one-cell walk even though the other
    // branch can only get to nil by returning through h.
    let mut list = insert("ex:h rdf:first ex:A\nex:h rdf:rest rdf:nil\nex:h rdf:rest ex:n0");
    for i in 0..16 {
        let cell = format!("ex:n{i}");
        list.insert.push(triple([&cell, "rdf:first", "ex:A"]));
        list.insert.push(triple([&cell, "rdf:rest", "ex:h"]));
        for j in (0..16).filter(|&j| j != i) {
            list.insert
                .push(triple([&cell, "rdf:rest", &format!("ex:n{j}")]));
        }
    }
    let mut write = list.clone();
    write
        .insert
        .extend(insert("ex:C owl:intersectionOf ex:h\nex:x rdf:type ex:A").insert);
    check(
        case(
            "dense-intersection",
            ["ex:x", "rdf:type", "ex:C"],
            vec![write],
        ),
        &[true],
        &[true],
    );
    check_key("dense-key", list);
}

fn branched_all_different(length: usize) -> Write {
    let mut write = insert("ex:ax rdf:type owl:AllDifferent\nex:ax owl:distinctMembers ex:c0");
    for i in 0..length {
        let cell = format!("ex:c{i}");
        let member = format!("ex:m{i}");
        let rest = if i + 1 == length {
            "rdf:nil".into()
        } else {
            format!("ex:c{}", i + 1)
        };
        write.insert.push(triple([&cell, "rdf:first", &member]));
        write.insert.push(triple([&cell, "rdf:rest", &rest]));
    }
    write.insert.push(triple(["ex:c0", "rdf:rest", "ex:c2"]));
    write
}

#[test]
fn an_all_different_branch_accepts_distinct_members_and_refuses_their_equality() {
    // Table 4 eq-diff2 applies to m0 and m3 on either walk and
    // refuses their equality while leaving the accepted list in place.
    check(
        case(
            "branched-all-different",
            ["ex:m0", "owl:sameAs", "ex:m3"],
            vec![branched_all_different(4), insert("ex:m0 owl:sameAs ex:m3")],
        ),
        &[true, false],
        &[false, true],
    );
}

#[test]
fn a_large_branched_all_different_stays_within_the_memory_limit() {
    use nrese_reasoner::ReasoningMode;
    use nrese_store::{ProcessMemory, StoreConfig, StoreService};
    use std::sync::Arc;

    // The ceiling covers the entire process. Give this workload its own
    // process so other oracle cases cannot spend part of its allowance.
    const CHILD: &str = "NRESE_TEST_PAIRWISE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let result =
            std::process::Command::new(std::env::current_exe().expect("oracle test binary"))
                .args([
                    "--exact",
                    "review_cells::a_large_branched_all_different_stays_within_the_memory_limit",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .expect("run the pairwise memory cell");
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success() && stdout.contains("1 passed;"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    // Table 4 eq-diff2 accepts a list of distinct terms and refuses
    // equality between its first and last members. Seven shortcuts
    // give 128 walks with more than 3,000 members apiece.
    let mut write = branched_all_different(3_100);
    for i in (2..14).step_by(2) {
        write.insert.push(triple([
            &format!("ex:c{i}"),
            "rdf:rest",
            &format!("ex:c{}", i + 2),
        ]));
    }
    let store = Arc::new(
        StoreService::new(StoreConfig {
            process_memory_bytes: ProcessMemory::Bytes(256 * 1024 * 1024),
            ..StoreConfig::in_memory()
        })
        .unwrap(),
    );
    let pipeline = crate::judges::pipeline(&store, ReasoningMode::Owl2Rl);
    assert!(crate::judges::apply(&pipeline, &write));
    assert!(!crate::judges::apply(
        &pipeline,
        &insert("ex:m0 owl:sameAs ex:m3099")
    ));
}

#[test]
fn chain_at_the_rule_encoder_boundary_reaches_its_endpoint() {
    // Table 5 prp-spo2 gives the 250-edge endpoint beside the existing 251 cell.
    check(generator::long_chain(250), &[true; 2], &[false, true]);
}

#[test]
fn a_chain_head_can_be_its_own_member() {
    // Table 5 prp-spo2 uses the head as a property at the first position.
    check(
        case(
            "self-member-head",
            ["ex:a", "ex:out", "ex:c"],
            vec![insert(
                "ex:out owl:propertyChainAxiom ex:h
             ex:h rdf:first ex:h
             ex:h rdf:rest ex:t
             ex:t rdf:first ex:q
             ex:t rdf:rest rdf:nil
             ex:a ex:h ex:b
             ex:b ex:q ex:c",
            )],
        ),
        &[true],
        &[true],
    );
}

fn heads_with_a_shared_tail() -> Write {
    // The back edge gives only the second head an unbounded list.
    insert(
        "ex:p owl:propertyChainAxiom ex:short
         ex:r owl:propertyChainAxiom ex:cycle
         ex:short rdf:first ex:s
         ex:short rdf:rest ex:tail
         ex:cycle rdf:first ex:t
         ex:cycle rdf:rest ex:cycle
         ex:cycle rdf:rest ex:tail
         ex:tail rdf:first ex:q
         ex:tail rdf:rest rdf:nil
         ex:a ex:s ex:b
         ex:c ex:t ex:b
         ex:b ex:q ex:d",
    )
}

#[test]
fn a_short_and_a_cyclic_chain_can_share_a_tail() {
    // Tables 3 and 5 give both compositions through the shared suffix.
    for watch in [["ex:a", "ex:p", "ex:d"], ["ex:c", "ex:r", "ex:d"]] {
        check(
            case(
                "mixed-form-shared-tail",
                watch,
                vec![heads_with_a_shared_tail()],
            ),
            &[true],
            &[true],
        );
    }
}

#[test]
fn merging_a_short_and_a_cyclic_head_adds_each_others_walks() {
    // Table 4 eq-rep merges the heads. The Table 5 prp-spo2 rows
    // from both former heads then follow for each property.
    let writes = vec![
        heads_with_a_shared_tail(),
        insert("ex:short owl:sameAs ex:cycle"),
    ];
    for watch in [["ex:a", "ex:r", "ex:d"], ["ex:c", "ex:p", "ex:d"]] {
        check(
            case("mixed-form-head-merge", watch, writes.clone()),
            &[true; 2],
            &[false, true],
        );
    }
}

#[test]
fn later_writes_agree_with_a_cold_per_cell_key() {
    // Table 5 prp-key needs the common value at every position, including the last.
    let mut setup = generator::long_chain(128).writes.remove(0);
    setup.insert[0] = triple(["ex:C", "owl:hasKey", "ex:c0"]);
    let last = setup
        .insert
        .iter_mut()
        .find(|row| row[0] == "ex:c127" && row[1] == "rdf:first")
        .unwrap();
    last[2] = "ex:last".into();
    setup.insert.extend(
        insert(
            "ex:a rdf:type ex:C
         ex:b rdf:type ex:C
         ex:a ex:p ex:v
         ex:b ex:p ex:v
         ex:a ex:last ex:v",
        )
        .insert,
    );
    let edge = "ex:b ex:last ex:v";
    let mut remove = delete(edge);
    remove.insert.push(triple(["ex:b", "ex:tag", "ex:v"]));
    check_later_writes(
        case(
            "warm-cold-key",
            ["ex:a", "owl:sameAs", "ex:b"],
            vec![setup, insert(edge), remove],
        ),
        true,
    );
}

#[test]
fn later_writes_agree_with_a_cold_per_cell_intersection() {
    // Table 6 cls-int1 needs every position's type, including the last.
    let mut setup = generator::long_chain(257).writes.remove(0);
    setup.insert[0] = triple(["ex:C", "owl:intersectionOf", "ex:c0"]);
    let last = setup
        .insert
        .iter_mut()
        .find(|row| row[0] == "ex:c256" && row[1] == "rdf:first")
        .unwrap();
    last[2] = "ex:Last".into();
    setup.insert.extend(insert("ex:a rdf:type ex:p").insert);
    let membership = "ex:a rdf:type ex:Last";
    let mut remove = delete(membership);
    remove.insert.push(triple(["ex:a", "ex:tag", "ex:v"]));
    check_later_writes(
        case(
            "warm-cold-intersection",
            ["ex:a", "rdf:type", "ex:C"],
            vec![setup, insert(membership), remove],
        ),
        true,
    );
}

/// Table 5 prp-adp over 65 walks: `ex:a` relates to `ex:b` by `ex:m0` and `ex:q`,
/// two properties of one walk.
fn sixty_five_walks_with_a_clash() -> Write {
    let mut write = insert(
        "ex:ax rdf:type owl:AllDisjointProperties
         ex:ax owl:members ex:h
         ex:h rdf:rest ex:t
         ex:t rdf:first ex:q
         ex:t rdf:rest rdf:nil
         ex:a ex:m0 ex:b
         ex:a ex:q ex:b",
    );
    for i in 0..65 {
        write
            .insert
            .push(triple(["ex:h", "rdf:first", &format!("ex:m{i}")]));
    }
    write
}

#[test]
fn a_delete_under_sixty_four_walks_keeps_an_older_clash() {
    // Design row 3, Table 5 prp-adp: a store loaded past the gates holds
    // the clash at 65 walks. Deleting the 65th first leaves 64 walks and
    // the same clash, so the delete is refused.
    let mut load = sixty_five_walks_with_a_clash();
    load.past_gates = true;
    check(
        case(
            "older-clash-delete",
            ["ex:a", "ex:m0", "ex:b"],
            vec![load, delete("ex:h rdf:first ex:m64")],
        ),
        &[false; 2],
        &[true; 2],
    );
}

#[test]
fn deleting_the_sixty_fifth_walk_preserves_an_older_clash() {
    // Design row 3, Table 5 prp-adp. Both raw graphs (before and after
    // the deletion) relate the same pair by two properties of one walk.
    let mut write = sixty_five_walks_with_a_clash();
    check(
        case(
            "older-clash-at-65-walks",
            ["ex:a", "ex:m0", "ex:b"],
            vec![write.clone()],
        ),
        &[false],
        &[true],
    );
    write.insert.pop();
    check(
        case(
            "older-clash-at-64-walks",
            ["ex:a", "ex:m0", "ex:b"],
            vec![write],
        ),
        &[false],
        &[true],
    );
}
