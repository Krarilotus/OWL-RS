use nrese_shacl::Component;

use super::{Row, core_key, predicate};
use crate::change_differential::case::{Case, Step, quads};

const MAX: &str = "ex:T sh:targetNode ex:n3 ; sh:property ex:Q .
    ex:Q sh:path ex:p0 ; sh:maxCount 0 .";
const ROOM: &str = "ex:T sh:targetNode ex:n3 ; sh:property ex:Q .
    ex:Q sh:path ex:p0 ; sh:maxCount 1 .";
const NO_TARGET: &str = "ex:T sh:property ex:Q . ex:Q sh:path ex:p0 ; sh:maxCount 0 .";
const INACTIVE: &str = "ex:T sh:targetNode ex:n3 ; sh:deactivated true ; sh:property ex:Q .
    ex:Q sh:path ex:p0 ; sh:maxCount 0 .";
const VALUE: &str = "ex:n3 ex:p0 ex:v1 .";

// Spec: a change to the shapes reports the newly introduced keys, using the
// matching committed data revision for each compilation.
pub(super) fn rows() -> Vec<Row> {
    let key = core_key("n3", predicate("p0"), None, "Q", Component::MaxCount);
    let case = |before, after: &str, add, remove| {
        Case::new(
            before,
            if add { "" } else { VALUE },
            Step {
                add: quads(if add { VALUE } else { "" }),
                remove: quads(if remove { VALUE } else { "" }),
                shapes: Some(after.to_owned()),
                ..Step::default()
            },
        )
    };
    let mut rows = vec![
        Row::new(
            "target-node-add",
            "Adding targetNode introduces its violation.",
            case(NO_TARGET, MAX, false, false),
            [],
            [key.clone()],
        )
        .for_shapes(),
        Row::new(
            "target-node-remove",
            "Removing targetNode withdraws its violation.",
            case(MAX, NO_TARGET, false, false),
            [key.clone()],
            [],
        )
        .for_shapes(),
    ];
    for (id, description, before, after, was_bad, is_bad) in [
        (
            "add",
            "Adding a failing property shape introduces its result.",
            "ex:T sh:targetNode ex:n3 .",
            MAX,
            false,
            true,
        ),
        (
            "tighten",
            "A tightened cardinality introduces a result.",
            ROOM,
            MAX,
            false,
            true,
        ),
        (
            "loosen",
            "A loosened cardinality removes the result.",
            MAX,
            ROOM,
            true,
            false,
        ),
        (
            "deactivate",
            "Deactivating a shape removes its result.",
            MAX,
            INACTIVE,
            true,
            false,
        ),
        (
            "reactivate",
            "Reactivating a shape restores its result.",
            INACTIVE,
            MAX,
            false,
            true,
        ),
        (
            "empty",
            "An empty shapes graph has no results.",
            MAX,
            "",
            true,
            false,
        ),
    ] {
        for (data_change, suffix) in [(false, "same-data"), (true, "changed-data")] {
            rows.push(
                Row::new(
                    format!("shapes-{id}-{suffix}"),
                    description,
                    case(before, after, data_change && is_bad, data_change && was_bad),
                    was_bad.then(|| key.clone()),
                    is_bad.then(|| key.clone()),
                )
                .for_shapes(),
            );
        }
    }
    let mut initial = case("", MAX, false, false);
    initial.shapes = None;
    rows.push(
        Row::new(
            "shapes-no-before",
            "With no earlier compilation, every later result is introduced.",
            initial,
            [],
            [key.clone()],
        )
        .for_shapes(),
    );
    let shared = "ex:A sh:targetNode ex:n0 ; sh:property ex:R .
        ex:B sh:targetNode ex:n1 ; sh:property ex:R .
        ex:R sh:path ex:p1 ; sh:property ex:Q .
        ex:Q sh:path ex:p0 ; sh:maxCount 0 .";
    let duplicate = Case::new(
        "",
        "ex:n0 ex:p1 ex:n3 . ex:n1 ex:p1 ex:n3 . ex:n3 ex:p0 ex:v1 .",
        Step {
            shapes: Some(shared.to_owned()),
            ..Step::default()
        },
    );
    rows.push(
        Row::new(
            "shapes-shared-target",
            "Two distinct targets produce one introduced key for their shared violation.",
            duplicate,
            [],
            [key],
        )
        .for_shapes(),
    );
    // One route: the child's own target has nothing to report.
    let parent = "ex:T sh:targetNode ex:n0 ; sh:path ex:p1 .";
    let nested = format!(
        "{parent} ex:T sh:property ex:Q .
        ex:Q sh:targetNode ex:n2 ; sh:path ex:p0 ; sh:maxCount 2 ."
    );
    let one_route = Case::new(
        parent,
        "ex:n0 ex:p1 ex:n1 . ex:n1 ex:p0 ex:v1 , ex:v2 , ex:v3 .",
        Step {
            shapes: Some(nested),
            ..Step::default()
        },
    );
    rows.push(
        Row::new(
            "shapes-nested-one-route",
            "A property child a parent gains reports its one result at the parent's value.",
            one_route,
            [],
            [core_key(
                "n1",
                predicate("p0"),
                None,
                "Q",
                Component::MaxCount,
            )],
        )
        .for_shapes(),
    );
    rows
}
