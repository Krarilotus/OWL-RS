//! A shapes graph can be well-formed however deeply its shapes nest (SHACL §2: the values of
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
use nrese_shacl::{Selection, Shapes, compile, nesting_parameters, validate, validate_changes};
use nrese_sparql::Resources;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

const SHAPES: &str = "http://example.com/shapes";
const LENGTH: usize = 100_000;

const PREFIXES: &str =
    "@prefix ex: <http://example.com/> . @prefix sh: <http://www.w3.org/ns/shacl#> .\n";
const TARGET: &str = "ex:s0 a sh:NodeShape ; sh:targetNode ex:a .\n";

enum Link {
    /// Turtle for the referring shape's statements, given the next shape's name.
    Direct {
        property: bool,
        statement: fn(&str) -> String,
    },
    QualifiedSibling,
}

/// The chain `ex:s0` … `ex:s{LENGTH}` of `kind`, `ex:s0` targeting `ex:a`, the last shape
/// requiring `sh:class ex:C`. For property shapes, `ex:s1` onward have the path `ex:p` and
/// `ex:s0` has `ex:s1` as its `sh:property`.
fn chain(link: &Link) -> (String, usize) {
    let Link::Direct {
        property,
        statement,
    } = link
    else {
        return sibling_chain();
    };
    let mut turtle = format!("{PREFIXES}{TARGET}");
    for i in 0..=LENGTH {
        let path = if *property && i > 0 {
            " ; sh:path ex:p"
        } else {
            ""
        };
        let next = match i {
            LENGTH => " ; sh:class ex:C".to_owned(),
            0 if *property => " ; sh:property ex:s1".to_owned(),
            _ => format!(" ; {}", statement(&format!("ex:s{}", i + 1))),
        };
        turtle.push_str(&format!("ex:s{i} ex:in ex:chain{path}{next} .\n"));
    }
    (turtle, LENGTH + 1)
}

/// Between the first and last property shapes, only the qualified sibling references
/// lead down the chain. Each primary shape always conforms. Every link therefore flips
/// conformance, so the first property shape gives the same answer as the last.
fn sibling_chain() -> (String, usize) {
    use std::fmt::Write;

    let mut turtle = format!("{PREFIXES}{TARGET}ex:s0 sh:property ex:s1 .\n");
    for i in 1..=LENGTH {
        let next = i + 1;
        writeln!(
            turtle,
            "ex:s{i} sh:path ex:p ; sh:qualifiedValueShape ex:always ;
                sh:qualifiedMinCount 1 ; sh:qualifiedValueShapesDisjoint true .
             ex:parent{i} sh:property ex:s{i}, ex:bridge{i} .
             ex:bridge{i} sh:qualifiedValueShape ex:s{next} ."
        )
        .expect("write Turtle");
    }
    writeln!(turtle, "ex:s{} sh:path ex:p ; sh:class ex:C .", LENGTH + 1).expect("write Turtle");
    // The root, the chain's property shapes, and ex:always.
    (turtle, LENGTH + 3)
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
    /// Removing `ex:a a ex:C` makes the last shape fail, introducing one result.
    Change,
}

fn deep(entry: Entry, kind: &str) {
    if !measure::alone() {
        return;
    }
    let (_, link) = KINDS
        .iter()
        .find(|(name, _)| *name == kind)
        .expect("a kind of nesting");
    let (turtle, shape_count) = chain(link);
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let mut tx = engine.transaction();
    insert(
        &mut tx,
        &turtle,
        GraphNameRef::from(NamedNodeRef::new_unchecked(SHAPES)),
    );
    drop(turtle);
    insert(&mut tx, DATA, GraphNameRef::DefaultGraph);
    let class = QuadRef::new(
        NamedNodeRef::new_unchecked("http://example.com/a"),
        NamedNodeRef::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        NamedNodeRef::new_unchecked("http://example.com/C"),
        GraphNameRef::DefaultGraph,
    );
    if matches!(entry, Entry::Change) {
        tx.insert(class);
    }
    tx.commit().expect("commit");
    let before = engine.snapshot();
    let shapes = compiled(&before, kind);
    let data = Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let resources = Resources::default();
    match entry {
        Entry::Compile => assert_eq!(shapes.len(), shape_count, "{kind}"),
        Entry::Validate => {
            let report = validate(&before, &shapes, data, &resources).expect("validated");
            assert_eq!(report.results.len(), 1, "{kind}");
        }
        Entry::Change => {
            let mut tx = engine.transaction();
            tx.remove(class);
            let changed: Vec<_> = tx.deleted().collect();
            let after = tx.pending_snapshot();
            let report = validate_changes(&before, &after, &shapes, data, &changed, &resources)
                .expect("validated");
            assert_eq!(report.results.len(), 1, "{kind}");
        }
    }
}

/// Every fixture supplies one test for each entry.
macro_rules! deep_tests {
    ($($kind:literal => $link:expr; $compile:ident, $validate:ident, $change:ident;)*) => {
        const KINDS: &[(&str, Link)] = &[$(($kind, $link)),*];
        $(
            #[test]
            fn $compile() {
                deep(Entry::Compile, $kind);
            }

            #[test]
            fn $validate() {
                deep(Entry::Validate, $kind);
            }

            #[test]
            fn $change() {
                deep(Entry::Change, $kind);
            }
        )*
    };
}

deep_tests! {
    "node" => Link::Direct { property: false, statement: |next| format!("sh:node {next}") };
    a_long_sh_node_chain_compiles, a_long_sh_node_chain_validates,
    a_change_under_a_long_sh_node_chain_validates;

    "property" => Link::Direct { property: true, statement: |next| format!("sh:property {next}") };
    a_long_sh_property_chain_compiles, a_long_sh_property_chain_validates,
    a_change_under_a_long_sh_property_chain_validates;

    "not" => Link::Direct { property: false, statement: |next| format!("sh:not {next}") };
    a_long_sh_not_chain_compiles, a_long_sh_not_chain_validates,
    a_change_under_a_long_sh_not_chain_validates;

    "and" => Link::Direct { property: false, statement: |next| format!("sh:and ( {next} )") };
    a_long_sh_and_chain_compiles, a_long_sh_and_chain_validates,
    a_change_under_a_long_sh_and_chain_validates;

    "or" => Link::Direct { property: false, statement: |next| format!("sh:or ( {next} )") };
    a_long_sh_or_chain_compiles, a_long_sh_or_chain_validates,
    a_change_under_a_long_sh_or_chain_validates;

    "xone" => Link::Direct { property: false, statement: |next| format!("sh:xone ( {next} )") };
    a_long_sh_xone_chain_compiles, a_long_sh_xone_chain_validates,
    a_change_under_a_long_sh_xone_chain_validates;

    "qualifiedValueShape" => Link::Direct { property: true, statement: |next| {
        format!("sh:qualifiedValueShape {next} ; sh:qualifiedMinCount 1")
    } };
    a_long_sh_qualified_value_shape_chain_compiles, a_long_sh_qualified_value_shape_chain_validates,
    a_change_under_a_long_sh_qualified_value_shape_chain_validates;

    "qualifiedValueShapesDisjoint" => Link::QualifiedSibling;
    a_long_sh_qualified_sibling_chain_compiles, a_long_sh_qualified_sibling_chain_validates,
    a_change_under_a_long_sh_qualified_sibling_chain_validates;
}

#[test]
fn every_nesting_parameter_has_a_deep_chain_fixture() {
    let mut tested: Vec<_> = KINDS.iter().map(|&(name, _)| name).collect();
    let mut accepted: Vec<_> = nesting_parameters().collect();
    tested.sort_unstable();
    accepted.sort_unstable();
    assert_eq!(tested, accepted);
}
