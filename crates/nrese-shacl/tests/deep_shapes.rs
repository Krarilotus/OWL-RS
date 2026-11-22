//! A shapes graph is well-formed however deeply its shapes nest (SHACL §2: the values of
//! `sh:node`, `sh:property`, `sh:not`, `sh:and`, `sh:or`, `sh:xone` and
//! `sh:qualifiedValueShape` are shapes, with no limit on how they nest), so every entry that
//! walks shape structure takes a chain of 100,000 nested shapes: compiling it, validating
//! data against it, and validating a change of that data. One test per entry and kind of
//! nesting, each run alone in a process of its own: a walk that recursed once per nested
//! shape would need far more than a thread's stack (a frame per shape), overflow it and end
//! the process, which fails its test; one left recursive for a single kind or entry still
//! fails.

use nrese_engine::{Engine, EngineConfig, GraphSelector, Snapshot, TermId};
use nrese_exec::measure;
use nrese_rdf::{GraphNameRef, NamedNodeRef, QuadRef};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{Selection, Shapes, compile, validate, validate_changes};
use nrese_sparql::Resources;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

const SHAPES: &str = "http://example.com/shapes";
const LENGTH: usize = 100_000;

/// How one shape of a chain refers to the next: Turtle for the referring shape's own
/// statements, given the next shape's name.
type Link = fn(&str) -> String;

/// The kinds of nesting: the parameter, whether the nested shapes are property shapes, and
/// the link.
const KINDS: [(&str, bool, Link); 7] = [
    ("sh:node", false, |next| format!("sh:node {next}")),
    ("sh:property", true, |next| format!("sh:property {next}")),
    ("sh:not", false, |next| format!("sh:not {next}")),
    ("sh:and", false, |next| format!("sh:and ( {next} )")),
    ("sh:or", false, |next| format!("sh:or ( {next} )")),
    ("sh:xone", false, |next| format!("sh:xone ( {next} )")),
    ("sh:qualifiedValueShape", true, |next| {
        format!("sh:qualifiedValueShape {next} ; sh:qualifiedMinCount 1")
    }),
];

/// The chain `ex:s0` … `ex:s{LENGTH}` of `kind`, `ex:s0` targeting `ex:a`, the last shape
/// requiring `sh:class ex:C`. For property shapes, `ex:s1` on have the path `ex:p` and
/// `ex:s0` has `ex:s1` as its `sh:property`.
fn chain(kind: &str) -> String {
    let (_, property, link) = KINDS
        .iter()
        .find(|(name, ..)| *name == kind)
        .expect("a kind of nesting");
    let mut turtle = String::from(
        "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:s0 a sh:NodeShape ; sh:targetNode ex:a .\n",
    );
    for i in 0..=LENGTH {
        let path = if *property && i > 0 {
            " ; sh:path ex:p"
        } else {
            ""
        };
        let next = match i {
            LENGTH => " ; sh:class ex:C".to_owned(),
            0 if *property => " ; sh:property ex:s1".to_owned(),
            _ => format!(" ; {}", link(&format!("ex:s{}", i + 1))),
        };
        turtle.push_str(&format!("ex:s{i} ex:in ex:chain{path}{next} .\n"));
    }
    turtle
}

fn insert(tx: &mut nrese_engine::Transaction<'_>, turtle: &str, graph: GraphNameRef<'_>) {
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        let quad = quad.expect("turtle");
        tx.insert(QuadRef::new(
            &quad.subject,
            &quad.predicate,
            &quad.object,
            graph,
        ));
    }
}

/// The data: `ex:a ex:p ex:a`, so `ex:a` is its own value at every level of a chain of
/// property shapes.
const DATA: &str = "@prefix ex: <http://example.com/> . ex:a ex:p ex:a .";

fn compiled(snapshot: &Snapshot, kind: &str) -> Shapes {
    let graph = snapshot
        .lookup(NamedNodeRef::new_unchecked(SHAPES).into())
        .expect("the shapes graph");
    compile(snapshot, Selection::asserted(GraphSelector::Exact(graph)))
        .unwrap_or_else(|errors| panic!("{kind}: {errors:?}"))
}

enum Entry {
    /// Compiling the chain: every shape of it counted.
    Compile,
    /// Validating the data: `ex:a` isn't an `ex:C`, so it fails the last shape and with it,
    /// level by level, `ex:s0` (for `sh:not`, the chain's even length gives the same), one
    /// result in all.
    Validate,
    /// Validating a change (`ex:a a ex:D` added, a statement the last shape reads): `ex:a`
    /// fails `ex:s0` before and after it, so the change introduces nothing.
    Change,
}

fn deep(entry: Entry, kind: &str) {
    if !measure::alone() {
        return;
    }
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let mut tx = engine.transaction();
    insert(
        &mut tx,
        &chain(kind),
        GraphNameRef::from(NamedNodeRef::new_unchecked(SHAPES)),
    );
    insert(&mut tx, DATA, GraphNameRef::DefaultGraph);
    tx.commit().expect("commit");
    let before = engine.snapshot();
    let shapes = compiled(&before, kind);
    let data = Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let resources = Resources::default();
    match entry {
        Entry::Compile => assert_eq!(shapes.len(), LENGTH + 1, "{kind}"),
        Entry::Validate => {
            let report = validate(&before, &shapes, data, &resources).expect("validated");
            assert_eq!(report.results.len(), 1, "{kind}");
        }
        Entry::Change => {
            let mut tx = engine.transaction();
            insert(
                &mut tx,
                "@prefix ex: <http://example.com/> . ex:a a ex:D .",
                GraphNameRef::DefaultGraph,
            );
            let changed: Vec<_> = tx.inserted().collect();
            let after = tx.pending_snapshot();
            let report = validate_changes(&before, &after, &shapes, data, &changed, &resources)
                .expect("validated");
            assert_eq!(report.results.len(), 0, "{kind}");
        }
    }
}

/// One test per entry and kind of nesting.
macro_rules! deep_tests {
    ($($test:ident: $entry:ident, $kind:literal;)*) => {
        $(
            #[test]
            fn $test() {
                deep(Entry::$entry, $kind);
            }
        )*
    };
}

deep_tests! {
    a_long_sh_node_chain_compiles: Compile, "sh:node";
    a_long_sh_property_chain_compiles: Compile, "sh:property";
    a_long_sh_not_chain_compiles: Compile, "sh:not";
    a_long_sh_and_chain_compiles: Compile, "sh:and";
    a_long_sh_or_chain_compiles: Compile, "sh:or";
    a_long_sh_xone_chain_compiles: Compile, "sh:xone";
    a_long_sh_qualified_value_shape_chain_compiles: Compile, "sh:qualifiedValueShape";
    a_long_sh_node_chain_validates: Validate, "sh:node";
    a_long_sh_property_chain_validates: Validate, "sh:property";
    a_long_sh_not_chain_validates: Validate, "sh:not";
    a_long_sh_and_chain_validates: Validate, "sh:and";
    a_long_sh_or_chain_validates: Validate, "sh:or";
    a_long_sh_xone_chain_validates: Validate, "sh:xone";
    a_long_sh_qualified_value_shape_chain_validates: Validate, "sh:qualifiedValueShape";
    a_change_under_a_long_sh_node_chain_validates: Change, "sh:node";
    a_change_under_a_long_sh_property_chain_validates: Change, "sh:property";
    a_change_under_a_long_sh_not_chain_validates: Change, "sh:not";
    a_change_under_a_long_sh_and_chain_validates: Change, "sh:and";
    a_change_under_a_long_sh_or_chain_validates: Change, "sh:or";
    a_change_under_a_long_sh_xone_chain_validates: Change, "sh:xone";
    a_change_under_a_long_sh_qualified_value_shape_chain_validates: Change, "sh:qualifiedValueShape";
}
