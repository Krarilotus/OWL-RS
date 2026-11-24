use nrese_owl::{Diagnostic, Unsupported, normalise, read_functional};

use super::Table;

/// OWL 2 Structural Specification §11.2 and Horrocks, Kutz and Sattler's Definition 2
/// impose different role orders. A hierarchy can be in OWL 2 DL and still leave the
/// engine's universal restriction over its non-regular role unsupported.
#[test]
fn structural_membership_and_automaton_regularity_follow_their_own_orders() {
    let many_ends: String = (0..40)
        .map(|i| {
            format!(
                "Declaration(ObjectProperty(:end{i})) \
            SubObjectPropertyOf(ObjectPropertyChain(:r :end{i}) :r) "
            )
        })
        .collect();
    let inverse_target = "SubObjectPropertyOf(ObjectPropertyChain(:p :q) ObjectInverseOf(:r))";
    let named_link = format!("{inverse_target} SubObjectPropertyOf(:r :p)");
    let inverse_link = format!("{inverse_target} SubObjectPropertyOf(:r ObjectInverseOf(:p))");
    // Each row gives the hierarchy, a role to constrain, and regularity under each order.
    let cases = [
        ("many recursive ends", many_ends.as_str(), "r", true, true),
        (
            "two chains form a cycle",
            "SubObjectPropertyOf(ObjectPropertyChain(:f :b) :u)
             SubObjectPropertyOf(ObjectPropertyChain(:c :u) :b)",
            "b",
            false,
            false,
        ),
        (
            "the target reaches a chain member",
            "SubObjectPropertyOf(ObjectPropertyChain(:a :f) :b)
             SubObjectPropertyOf(:b :a)",
            "b",
            false,
            false,
        ),
        (
            "two chains linked by single inclusions",
            "SubObjectPropertyOf(ObjectPropertyChain(:a :f) :b)
             SubObjectPropertyOf(:b :c)
             SubObjectPropertyOf(ObjectPropertyChain(:c :g) :d)
             SubObjectPropertyOf(:d :a)",
            "b",
            true,
            false,
        ),
        (
            "a chain into its member's inverse",
            "SubObjectPropertyOf(ObjectPropertyChain(:p :q) ObjectInverseOf(:p))",
            "p",
            true,
            false,
        ),
        (
            "an inverse before the recursive end",
            "SubObjectPropertyOf(ObjectPropertyChain(ObjectInverseOf(:p) :p) :p)",
            "p",
            false,
            false,
        ),
        (
            "an inverse target and a named link",
            &named_link,
            "r",
            true,
            false,
        ),
        (
            "an inverse target and an inverse link",
            &inverse_link,
            "r",
            false,
            false,
        ),
        (
            "a named target reaches an inverted chain member",
            "SubObjectPropertyOf(ObjectPropertyChain(:a :f) :b)
             SubObjectPropertyOf(:b ObjectInverseOf(:a))",
            "b",
            false,
            false,
        ),
    ];
    let properties: String = ["a", "b", "c", "d", "f", "g", "p", "q", "r", "u"]
        .into_iter()
        .map(|name| format!("Declaration(ObjectProperty(:{name})) "))
        .collect();
    let mut wrong = Vec::new();
    for (name, axioms, target, structural, engine) in cases {
        let (ontology, _) = read_functional(
            &format!(
                "Prefix(:=<http://example.com/>) Ontology({properties}
                 Declaration(Class(:C)) Declaration(Class(:D))
                 {axioms} SubClassOf(:C ObjectAllValuesFrom(:{target} :D)))"
            ),
            &mut Table::default(),
        );
        assert!(
            ontology
                .diagnostics
                .iter()
                .all(|d| !matches!(d, Diagnostic::Syntax { .. })),
            "{name}: {:?}",
            ontology.diagnostics
        );
        let is_dl = !ontology.diagnostics.iter().any(Diagnostic::is_fatal);
        if is_dl != structural {
            wrong.push(format!(
                "{name}: structural regularity {is_dl}, expected {structural}"
            ));
        }
        let supported = !normalise(&ontology).unsupported.iter().any(|(_, why)| {
            matches!(
                why,
                Unsupported::IrregularRoleHierarchy | Unsupported::RoleAutomatonOutsideRegularClass
            )
        });
        if supported != engine {
            wrong.push(format!(
                "{name}: automaton regularity {supported}, expected {engine}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
