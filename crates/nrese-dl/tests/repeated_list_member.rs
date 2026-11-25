use nrese_dl::beyond::{Options, Verdict, consistency};

#[path = "../examples/support/read.rs"]
mod read;

/// OWL 2 Structural Specification §2.3. In functional syntax too, removing
/// duplicate operands leaves distinct individuals that can coexist.
#[test]
fn different_individuals_with_a_repeated_operand_has_a_model() {
    let mut terms = read::Table::default();
    let (ontology, _) = nrese_owl::read_functional(
        "Prefix(:=<http://example.org/>) Ontology(DifferentIndividuals(:a :a :b))",
        &mut terms,
    );
    let answer = consistency(&ontology, &Options::default());
    assert!(
        matches!(answer.verdict, Verdict::Consistent { .. }),
        "{answer:?}"
    );
}
