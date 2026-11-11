//! An ontology's anonymous individuals in nominals and `hasValue`, which OWL 2 DL forbids
//! (*OWL 2 Structural Specification* §11.2): the Direct Semantics still gives them a
//! meaning (§2.3.7–2.4), one individual each, existential over the whole ontology. Read
//! from the functional syntax as the DL examples read it (the ORE corpus's files).

#[path = "../../examples/support/read.rs"]
mod read;

use nrese_dl::classify::{self, Taxonomy};
use nrese_dl::verdict::Verdict;
use nrese_owl::{Diagnostic, Ontology, Term};
use nrese_rdf::NamedNode;

const EX: &str = "http://example.org/anonymous#";

/// The ontology of `axioms`, classified, and the id of each of `names`.
fn classified<const N: usize>(axioms: &str, names: [&str; N]) -> (Ontology, Taxonomy, [Term; N]) {
    let mut table = read::Table::default();
    let text = format!("Prefix(:=<{EX}>)\nOntology(<{EX}o>\n{axioms}\n)");
    let mut reader = nrese_owl::FunctionalReader::new(&mut table);
    reader.read(&text);
    let o = reader.finish();
    let ids = names.map(|n| table.id(NamedNode::new_unchecked(format!("{EX}{n}")).into()));
    let taxonomy = classify::classify(&o, &classify::Options::default());
    (o, taxonomy, ids)
}

fn reported(o: &Ontology) -> usize {
    (o.diagnostics.iter())
        .filter(|d| matches!(d, Diagnostic::AnonymousIndividual { .. }))
        .count()
}

/// - Input: `A ⊑ ∃p.{_:b}`, `D ≡ ∃p.{_:b}`, `E ≡ ∃p.{_:c}`.
/// - Expected: the two anonymous fillers reported (not OWL 2 DL), the classification
///   complete, `A ⊑ D` (both name the one individual `_:b`), and not `A ⊑ E` (`_:c` is
///   another individual, maybe the same, maybe not).
#[test]
fn an_anonymous_has_value_filler_is_one_individual() {
    let (o, t, [a, d, e]) = classified(
        "Declaration(Class(:A)) Declaration(Class(:D)) Declaration(Class(:E))
         Declaration(ObjectProperty(:p))
         SubClassOf(:A ObjectHasValue(:p _:b))
         EquivalentClasses(:D ObjectHasValue(:p _:b))
         EquivalentClasses(:E ObjectHasValue(:p _:c))",
        ["A", "D", "E"],
    );
    assert_eq!(reported(&o), 2, "{:?}", o.diagnostics);
    assert!(t.complete(), "{:?}", t.incomplete);
    let subsumptions = &t.classification.subsumptions;
    assert!(subsumptions.contains(&(a, d)), "{subsumptions:?}");
    assert!(!subsumptions.contains(&(a, e)), "{subsumptions:?}");
}

/// - Input: `A ≡ {_:b}`, `D ≡ {_:b}`, `E ≡ {_:c}`.
/// - Expected: reported, complete, `A ≡ D` (one individual), and not `A ⊑ E`.
#[test]
fn an_anonymous_nominal_member_is_one_individual() {
    let (o, t, [a, d, e]) = classified(
        "Declaration(Class(:A)) Declaration(Class(:D)) Declaration(Class(:E))
         EquivalentClasses(:A ObjectOneOf(_:b))
         EquivalentClasses(:D ObjectOneOf(_:b))
         EquivalentClasses(:E ObjectOneOf(_:c))",
        ["A", "D", "E"],
    );
    assert_eq!(reported(&o), 2, "{:?}", o.diagnostics);
    assert!(t.complete(), "{:?}", t.incomplete);
    let subsumptions = &t.classification.subsumptions;
    assert!(subsumptions.contains(&(a, d)), "{subsumptions:?}");
    assert!(subsumptions.contains(&(d, a)), "{subsumptions:?}");
    assert!(!subsumptions.contains(&(a, e)), "{subsumptions:?}");
}

/// - Input: a key on `C` over `k`; `C(a)` and `C(_:b)` with the same `k` value; `D(_:b)`,
///   `¬D(a)`; and `_:b` also a `hasValue` filler.
/// - Expected: consistent. A key applies to named individuals only (Direct Semantics
///   §2.3.5), and `_:b` stays anonymous: were it named, the key would make it `a`, a
///   contradiction.
#[test]
fn an_anonymous_filler_stays_out_of_keys() {
    let (o, t, _) = classified(
        "Declaration(Class(:C)) Declaration(Class(:D)) Declaration(Class(:F))
         Declaration(ObjectProperty(:p)) Declaration(DataProperty(:k))
         Declaration(NamedIndividual(:a))
         HasKey(:C () (:k))
         ClassAssertion(:C :a) ClassAssertion(:C _:b)
         DataPropertyAssertion(:k :a \"1\") DataPropertyAssertion(:k _:b \"1\")
         ClassAssertion(:D _:b) ClassAssertion(ObjectComplementOf(:D) :a)
         SubClassOf(:F ObjectHasValue(:p _:b))",
        [],
    );
    assert_eq!(reported(&o), 1, "{:?}", o.diagnostics);
    assert!(t.complete(), "{:?}", t.incomplete);
    assert_eq!(t.classification.consistency, Verdict::Consistent);
}
