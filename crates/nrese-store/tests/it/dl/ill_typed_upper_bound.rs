use nrese_store::{ConsistencyStatus, MutationPipeline};

use super::{PREFIXES, insert, pipeline, queries::query};
use crate::support::load_past_the_pipeline;

const OFF_RULES: &str = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] .";
const MARKER: &str = ":a a :U . :d a owl:DatatypeProperty .";
const ILL_TYPED: &str = r#":a :d "abc"^^xsd:integer ."#;

fn rebuilt(data: &str) -> MutationPipeline {
    let dl = pipeline();
    load_past_the_pipeline(dl.store(), &format!("{PREFIXES}{data}"));
    let program = dl.reasoner().config().materialised_program().unwrap();
    dl.store().rematerialise(program).expect("rebuild");
    dl
}

/// An ill-typed datatype-map literal is unsatisfiable (OWL 2 Structural Specification
/// §5.7 and RDF 1.1 Semantics §7.2). The store must withhold consistency and completeness
/// while it holds one, including for a read of a named class with no range or datatype test.
fn check(name: &str, dl: &MutationPipeline, ill_typed: bool, rules: bool) {
    let store = dl.store();
    let recorded = store.dl().status();
    let (rows, read) = query(dl, "SELECT ?x WHERE { ?x a :U }");
    let verdict = recorded.or_else(|| store.dl().status()).map(|status| {
        (
            status.consistency.engine,
            status.consistency.verdict.as_str(),
        )
    });
    let consistency = store.consistency();
    let bounds = store.dl_bounds();
    assert_eq!(rows, ["a"], "{name}");
    assert_eq!(bounds.last == "rules", rules, "{name}: {bounds:?}");
    let (literals, _) = query(
        dl,
        "SELECT ?x WHERE { :a :d ?x . FILTER(DATATYPE(?x) = xsd:integer) }",
    );
    if ill_typed {
        assert_eq!(literals, ["abc"], "{name}: the input literal must be held");
    } else {
        assert!(literals.is_empty(), "{name}: {literals:?}");
    }

    println!(
        "CELL {name} store={consistency:?} dl={verdict:?} complete={} bounds={} paths={:?}",
        read.is_complete(),
        bounds.last,
        read.paths,
    );
    if ill_typed {
        assert_eq!(
            (
                consistency == ConsistencyStatus::Consistent,
                verdict.is_some_and(|(_, verdict)| verdict == "consistent"),
                read.is_complete(),
            ),
            (false, false, false),
            "{name}: a store holding an ill-typed literal must withhold consistency and completeness"
        );
    } else {
        assert_eq!(
            (consistency, read.is_complete()),
            (ConsistencyStatus::Consistent, true),
            "{name}: the same store is consistent without the literal"
        );
    }
}

#[test]
fn an_ill_typed_literal_withholds_the_word_after_a_write() {
    let dl = pipeline();
    insert(&dl, &format!("{OFF_RULES} {MARKER} {ILL_TYPED}")).expect("write applies");
    check("write", &dl, true, false);
}

#[test]
fn an_ill_typed_literal_withholds_the_word_after_a_rebuild() {
    let dl = rebuilt(&format!("{OFF_RULES} {MARKER} {ILL_TYPED}"));
    check("rebuild", &dl, true, false);
}

#[test]
fn the_write_without_the_literal_is_consistent() {
    let dl = pipeline();
    insert(&dl, &format!("{OFF_RULES} {MARKER}")).expect("write applies");
    check("clean-write", &dl, false, false);
}

#[test]
fn the_rebuild_without_the_literal_is_consistent() {
    let dl = rebuilt(&format!("{OFF_RULES} {MARKER}"));
    check("clean-rebuild", &dl, false, false);
}

#[test]
fn the_rules_route_withholds_the_word_for_the_same_literal() {
    let dl = pipeline();
    insert(&dl, &format!("{MARKER} {ILL_TYPED}")).expect("write applies");
    check("rules", &dl, true, true);
}
