//! Reasoner v2 (docs/design/reasoner-v2.md; ROADMAP Phase 3). It replaces the v1
//! `rules-mvp` reasoner step by step.
//!
//! - [`ir`]: the rule IR and its text syntax; rules are data
//! - [`rulesets`]: the built-in rulesets (`rdfs`, `owl2-rl`), with the W3C rule names
//! - [`lists`]: list axioms (chains, keys, intersections, …) compiled to fixed-arity rules
//! - [`naive`]: the naive reference evaluator, the oracle for the fast executors
//!
//! Everything works on ids through a [`ir::Vocabulary`] that interns the rules' constants.

pub mod ir;
pub mod lists;
pub mod naive;
pub mod rulesets;
pub mod testing;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::ir::Vocabulary;
    use super::lists::ListVocabulary;
    use super::naive::{Triple, materialise};
    use super::rulesets::Ruleset;
    use super::testing::LocalVocabulary;

    const EX: &str = "http://example.com/";

    /// Parses `s p o` lines of prefixed names (`ex:`, `rdf:`, `rdfs:`, `owl:`); `_:x` are
    /// blank nodes.
    fn load(vocabulary: &mut LocalVocabulary, text: &str) -> Vec<Triple> {
        let term = |v: &mut LocalVocabulary, t: &str| -> u64 {
            if t.starts_with("_:") {
                return v.term(t);
            }
            let (prefix, local) = t.split_once(':').unwrap();
            let ns = match prefix {
                "ex" => EX,
                "rdf" => super::ir::RDF,
                "rdfs" => super::ir::RDFS,
                "owl" => super::ir::OWL,
                other => panic!("prefix {other}"),
            };
            v.iri(&format!("{ns}{local}"))
        };
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                [
                    term(vocabulary, parts[0]),
                    term(vocabulary, parts[1]),
                    term(vocabulary, parts[2]),
                ]
            })
            .collect()
    }

    fn closure(text: &str) -> (LocalVocabulary, HashSet<Triple>, Vec<String>) {
        let mut vocabulary = LocalVocabulary::default();
        let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
        let lists = ListVocabulary::new(&mut vocabulary);
        let input = load(&mut vocabulary, text);
        let result = materialise(&input, &rules, Some(&lists));
        let violations = result.violations.iter().map(|v| v.rule.clone()).collect();
        (vocabulary, result.derived, violations)
    }

    fn has(vocabulary: &mut LocalVocabulary, derived: &HashSet<Triple>, fact: &str) -> bool {
        let triple = load(vocabulary, fact)[0];
        derived.contains(&triple)
    }

    #[test]
    fn owl2_rl_parses_and_every_rule_is_safe() {
        let mut vocabulary = LocalVocabulary::default();
        let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
        // Tables 4 (6 rules), 5 (16), 6 (13), 7 (4) and 9 (18); list rules come from `lists`.
        assert_eq!(rules.len(), 57);
        assert!(rules.iter().any(|r| r.name == "cls-maxqc4"));
        Ruleset::Rdfs.rules(&mut vocabulary).unwrap();
    }

    #[test]
    fn hierarchies_domains_and_property_axioms() {
        let (mut v, d, violations) = closure(
            "ex:Cat rdfs:subClassOf ex:Mammal
             ex:Mammal rdfs:subClassOf ex:Animal
             ex:tom rdf:type ex:Cat
             ex:owns rdfs:domain ex:Person
             ex:alice ex:owns ex:tom
             ex:ancestor rdf:type owl:TransitiveProperty
             ex:a ex:ancestor ex:b
             ex:b ex:ancestor ex:c
             ex:knows rdf:type owl:SymmetricProperty
             ex:a ex:knows ex:b
             ex:hasPet owl:inverseOf ex:petOf
             ex:alice ex:hasPet ex:tom",
        );
        for fact in [
            "ex:tom rdf:type ex:Mammal",
            "ex:tom rdf:type ex:Animal",
            "ex:Cat rdfs:subClassOf ex:Animal",
            "ex:alice rdf:type ex:Person",
            "ex:a ex:ancestor ex:c",
            "ex:b ex:knows ex:a",
            "ex:tom ex:petOf ex:alice",
        ] {
            assert!(has(&mut v, &d, fact), "missing {fact}");
        }
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn list_axioms_chains_intersections_unions_keys() {
        let (mut v, d, _) = closure(
            "ex:uncle owl:propertyChainAxiom _:l1
             _:l1 rdf:first ex:parent
             _:l1 rdf:rest _:l2
             _:l2 rdf:first ex:brother
             _:l2 rdf:rest rdf:nil
             ex:x ex:parent ex:y
             ex:y ex:brother ex:z
             ex:Mother owl:intersectionOf _:i1
             _:i1 rdf:first ex:Woman
             _:i1 rdf:rest _:i2
             _:i2 rdf:first ex:Parent
             _:i2 rdf:rest rdf:nil
             ex:ann rdf:type ex:Woman
             ex:ann rdf:type ex:Parent
             ex:Pet owl:unionOf _:u1
             _:u1 rdf:first ex:Cat
             _:u1 rdf:rest _:u2
             _:u2 rdf:first ex:Dog
             _:u2 rdf:rest rdf:nil
             ex:rex rdf:type ex:Dog
             ex:Person owl:hasKey _:k1
             _:k1 rdf:first ex:ssn
             _:k1 rdf:rest rdf:nil
             ex:p1 rdf:type ex:Person
             ex:p2 rdf:type ex:Person
             ex:p1 ex:ssn ex:n42
             ex:p2 ex:ssn ex:n42",
        );
        for fact in [
            "ex:x ex:uncle ex:z",
            "ex:ann rdf:type ex:Mother",
            "ex:Mother rdfs:subClassOf ex:Woman",
            "ex:rex rdf:type ex:Pet",
            "ex:Dog rdfs:subClassOf ex:Pet",
            "ex:p1 owl:sameAs ex:p2",
        ] {
            assert!(has(&mut v, &d, fact), "missing {fact}");
        }
    }

    #[test]
    fn equality_and_consistency() {
        let (mut v, d, violations) = closure(
            "ex:hasMother rdf:type owl:FunctionalProperty
             ex:bob ex:hasMother ex:m1
             ex:bob ex:hasMother ex:m2
             ex:m1 ex:name ex:n
             ex:Cat owl:disjointWith ex:Dog
             ex:odd rdf:type ex:Cat
             ex:odd rdf:type ex:Dog",
        );
        assert!(has(&mut v, &d, "ex:m1 owl:sameAs ex:m2"));
        assert!(has(&mut v, &d, "ex:m2 ex:name ex:n"), "eq-rep-s");
        assert_eq!(violations, vec!["cax-dw".to_owned()]);
    }

    #[test]
    fn malformed_lists_are_diagnosed_not_truncated() {
        let mut vocabulary = LocalVocabulary::default();
        let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
        let lists = ListVocabulary::new(&mut vocabulary);
        let input = load(
            &mut vocabulary,
            "ex:C owl:intersectionOf _:a
             _:a rdf:first ex:A
             _:a rdf:rest _:b
             _:b rdf:first ex:B
             ex:D owl:unionOf _:c
             _:c rdf:first ex:A
             _:c rdf:rest _:c",
        );
        let result = materialise(&input, &rules, Some(&lists));
        assert_eq!(result.diagnostics.len(), 2, "{:?}", result.diagnostics);
        assert!(
            result
                .derived
                .iter()
                .all(|t| t[1] != vocabulary.iri(&format!("{}subClassOf", super::ir::RDFS)))
        );
    }

    #[test]
    fn equality_gives_lists_several_member_sequences() {
        // eq-rep-o copies `_:a rdf:first ex:Woman` to `ex:Female`; both paths are lists.
        let (mut v, d, violations) = closure(
            "ex:Woman owl:sameAs ex:Female
             ex:Mother owl:intersectionOf _:a
             _:a rdf:first ex:Woman
             _:a rdf:rest _:b
             _:b rdf:first ex:Parent
             _:b rdf:rest rdf:nil
             _:d rdf:type owl:AllDifferent
             _:d owl:distinctMembers _:l
             _:l rdf:first ex:ann
             _:l rdf:rest _:m
             _:m rdf:first ex:bob
             _:m rdf:rest rdf:nil
             ex:bob owl:sameAs ex:robert",
        );
        assert!(has(&mut v, &d, "ex:Mother rdfs:subClassOf ex:Female"));
        assert!(has(&mut v, &d, "ex:Mother rdfs:subClassOf ex:Woman"));
        assert!(violations.is_empty(), "{violations:?}");
    }
}
