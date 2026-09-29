//! Reasoner v2 (docs/design/reasoner-v2.md; ROADMAP Phase 3). It replaces the v1
//! `rules-mvp` reasoner step by step.
//!
//! - [`ir`]: the rule IR and its text syntax; rules are data
//! - [`rulesets`]: the built-in rulesets (`rdfs`, `owl2-rl`), with the W3C rule names
//! - [`lists`]: list axioms (chains, keys, intersections, …) compiled to fixed-arity rules
//! - [`naive`]: the naive reference evaluator, the oracle for the fast executors
//! - [`eval`]: rule evaluation over any [`eval::Source`]: grounding, planning, joins
//! - [`batch`]: the batch executor: schema grounding and parallel semi-naive evaluation
//! - [`delta`]: the delta executor: maintenance under inserts and deletes (DRed)
//!
//! Everything works on ids through a [`ir::Vocabulary`] that interns the rules' constants.

pub mod batch;
pub mod delta;
pub mod eval;
pub mod ir;
pub mod lists;
pub mod naive;
pub mod rulesets;
pub mod testing;
#[cfg(test)]
mod v1_scenarios;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::batch::{self, Schema};
    use super::ir::Vocabulary;
    use super::lists::ListVocabulary;
    use super::naive::{Triple, Violation, materialise};
    use super::rulesets::Ruleset;
    use super::testing::LocalVocabulary;

    const EX: &str = "http://example.com/";

    /// Parses `s p o` lines of prefixed names (`ex:`, `rdf:`, `rdfs:`, `owl:`); `_:x` are
    /// blank nodes and `"…"^^<…>` literals (without spaces) are taken as they are.
    fn load(vocabulary: &mut LocalVocabulary, text: &str) -> Vec<Triple> {
        let term = |v: &mut LocalVocabulary, t: &str| -> u64 {
            if t.starts_with("_:") || t.starts_with('"') {
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

    /// `eq-ref`'s consequences for consistency without materialising `x sameAs x`: an
    /// individual different from itself, and an AllDifferent list naming one individual
    /// twice, are inconsistent.
    #[test]
    fn reflexive_equality_is_checked_without_materialising_it() {
        let (_, derived, violations) = closure("ex:x owl:differentFrom ex:x");
        assert_eq!(violations, vec!["eq-diff1".to_owned()]);
        assert!(derived.is_empty(), "no reflexive sameAs is materialised");
        for property in ["members", "distinctMembers"] {
            let (_, _, violations) = closure(&format!(
                "_:d rdf:type owl:AllDifferent
                 _:d owl:{property} _:l1
                 _:l1 rdf:first ex:a
                 _:l1 rdf:rest _:l2
                 _:l2 rdf:first ex:a
                 _:l2 rdf:rest rdf:nil"
            ));
            assert!(
                violations.iter().any(|v| v.starts_with("eq-diff")),
                "{property}: {violations:?}"
            );
        }
        // Different individuals in an AllDifferent list are fine.
        let (_, _, violations) = closure(
            "_:d rdf:type owl:AllDifferent
             _:d owl:members _:l1
             _:l1 rdf:first ex:a
             _:l1 rdf:rest _:l2
             _:l2 rdf:first ex:b
             _:l2 rdf:rest rdf:nil",
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    /// The schema is closed before the instance data (batch), and that pre-closure derives
    /// `ex:a rdf:type ex:C` from the enumeration alone, which the data also asserts: it
    /// must not be reported as derived (`both` checks that, and batch = naive).
    #[test]
    fn schema_pre_closure_does_not_report_asserted_facts() {
        let (mut vocabulary, derived, _) = closure(
            "ex:C owl:equivalentClass _:e
             _:e owl:oneOf _:l1
             _:l1 rdf:first ex:a
             _:l1 rdf:rest _:l2
             _:l2 rdf:first ex:b
             _:l2 rdf:rest rdf:nil
             ex:a rdf:type ex:C",
        );
        let mut fact = |s: &str, o: &str| {
            use super::ir::Vocabulary;
            [
                vocabulary.iri(&format!("{EX}{s}")),
                vocabulary.iri(&format!("{}type", super::ir::RDF)),
                vocabulary.iri(&format!("{EX}{o}")),
            ]
        };
        assert!(derived.contains(&fact("b", "C")));
        assert!(!derived.contains(&fact("a", "C")), "asserted");
    }

    /// The naive closure of `text`, after checking that the batch executor agrees.
    fn closure(text: &str) -> (LocalVocabulary, HashSet<Triple>, Vec<String>) {
        let mut vocabulary = LocalVocabulary::default();
        let input = load(&mut vocabulary, text);
        let (derived, violations) = both(&mut vocabulary, &input);
        (
            vocabulary,
            derived,
            violations.into_iter().map(|v| v.rule).collect(),
        )
    }

    /// Runs both executors on `input` and asserts that they agree.
    fn both(
        vocabulary: &mut LocalVocabulary,
        input: &[Triple],
    ) -> (HashSet<Triple>, Vec<Violation>) {
        let rules = Ruleset::Owl2Rl.rules(vocabulary).unwrap();
        let lists = ListVocabulary::new(vocabulary);
        let schema = Schema::owl(vocabulary);
        let naive = materialise(input, &rules, Some(&lists));
        let batch = batch::materialise(input, &rules, Some(&lists), &schema);
        // The grouped entry point (what the store uses) derives the same.
        let mut sorted: Vec<Triple> = input.to_vec();
        sorted.sort_unstable_by_key(|&[s, p, o]| (p, o, s));
        sorted.dedup();
        let mut groups: Vec<(u64, Vec<(u64, u64)>)> = Vec::new();
        for [s, p, o] in sorted {
            match groups.last_mut() {
                Some((last, pairs)) if *last == p => pairs.push((o, s)),
                _ => groups.push((p, vec![(o, s)])),
            }
        }
        let grouped = batch::materialise_grouped(groups, &rules, Some(&lists), &schema);
        // Derived facts never include asserted ones (the schema pre-closure must filter).
        let asserted: HashSet<Triple> = input.iter().copied().collect();
        assert!(
            batch.derived.iter().all(|t| !asserted.contains(t)),
            "asserted fact reported as derived"
        );
        assert_eq!(grouped.derived, batch.derived, "grouped input");
        assert_eq!(grouped.violations, batch.violations, "grouped input");
        let mut expected: Vec<Triple> = naive.derived.iter().copied().collect();
        expected.sort_unstable();
        if expected != batch.derived {
            let text = |t: &Triple| {
                let [s, p, o] = t.map(|id| vocabulary.text(id).to_owned());
                format!("{s} {p} {o}")
            };
            let missing: Vec<String> = expected
                .iter()
                .filter(|t| batch.derived.binary_search(t).is_err())
                .take(8)
                .map(text)
                .collect();
            let extra: Vec<String> = batch
                .derived
                .iter()
                .filter(|t| !naive.derived.contains(*t))
                .take(8)
                .map(text)
                .collect();
            let input: Vec<String> = input.iter().map(text).collect();
            panic!("batch differs: missing {missing:#?}, extra {extra:#?}, input {input:#?}");
        }
        let mut violations = naive.violations;
        violations.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
        assert_eq!(violations, batch.violations, "violations differ");
        (naive.derived, violations)
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
        // Plus the reflexive eq-diff1 (x differentFrom x), eq-ref's consistency part.
        assert_eq!(rules.len(), 58);
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
        let problems: Vec<(&str, &str)> = result
            .diagnostics
            .iter()
            .map(|d| (d.rules, d.problem.kind()))
            .collect();
        assert_eq!(
            problems,
            [("cls-int", "malformed-list"), ("cls-uni", "cyclic-list")]
        );
        let b = vocabulary.term("_:b");
        assert_eq!(result.diagnostics[0].problem.node(), Some(b));
        assert!(
            result
                .derived
                .iter()
                .all(|t| t[1] != vocabulary.iri(&format!("{}subClassOf", super::ir::RDFS)))
        );
        let batch = batch::materialise(&input, &rules, Some(&lists), &Schema::owl(&mut vocabulary));
        assert_eq!(batch.diagnostics, result.diagnostics);
    }

    /// A commit reports the list axioms it made uninstantiable, and only those: a broken
    /// list already in the state isn't reported again.
    #[test]
    fn commits_report_the_list_problems_they_introduce() {
        use super::delta::{MemoryBase, Rules, program, update};
        use super::lists::ListProblem;
        let mut vocabulary = LocalVocabulary::default();
        let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
        let lists = ListVocabulary::new(&mut vocabulary);
        let schema = Schema::owl(&mut vocabulary);
        let compiled = Rules {
            rules: &rules,
            lists: Some(&lists),
            schema: &schema,
        };
        let mut asserted = load(
            &mut vocabulary,
            "ex:D owl:unionOf ex:c
             ex:c rdf:first ex:A
             ex:c rdf:rest ex:c
             ex:x ex:p ex:y",
        );
        asserted.sort_unstable();
        let inferred = batch::materialise(&asserted, &rules, Some(&lists), &schema).derived;
        let old = program(&MemoryBase::new(&asserted, &inferred), compiled);
        assert_eq!(old.list_diagnostics.len(), 1, "the cyclic list");
        let ex_a = vocabulary.iri(&format!("{EX}a"));

        let mut commit = |asserted: &mut Vec<Triple>, text: &str, cache| {
            let added = load(&mut vocabulary, text);
            asserted.extend(&added);
            asserted.sort_unstable();
            let base = MemoryBase::new(asserted, &inferred);
            update(&base, &added, &[], compiled, Some(cache))
        };
        let broken = commit(
            &mut asserted,
            "ex:C owl:intersectionOf ex:a
             ex:a rdf:first ex:A",
            &old,
        );
        assert_eq!(
            broken
                .diagnostics
                .iter()
                .map(|d| (d.rules, d.problem))
                .collect::<Vec<_>>(),
            [("cls-int", ListProblem::Malformed { node: ex_a })]
        );
        let after = broken.program.expect("list facts changed the program");
        assert_eq!(after.list_diagnostics.len(), 2);
        let unrelated = commit(&mut asserted, "ex:y ex:p ex:z", &after);
        assert!(
            unrelated.diagnostics.is_empty(),
            "{:?}",
            unrelated.diagnostics
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

    /// Runs a v1 scenario (full IRIs, `_:` blank nodes) on both executors: the expected
    /// inferences must be derived, and a violation reported iff `violation` says so.
    pub(super) fn scenario(
        data: &[(&str, &str, &str)],
        expected: &[(&str, &str, &str)],
        violation: Option<bool>,
    ) {
        let mut vocabulary = LocalVocabulary::default();
        let term = |v: &mut LocalVocabulary, t: &str| {
            if t.starts_with("_:") {
                v.term(t)
            } else {
                v.iri(t)
            }
        };
        let input: Vec<Triple> = data
            .iter()
            .map(|&(s, p, o)| {
                [
                    term(&mut vocabulary, s),
                    term(&mut vocabulary, p),
                    term(&mut vocabulary, o),
                ]
            })
            .collect();
        let (derived, violations) = both(&mut vocabulary, &input);
        for &(s, p, o) in expected {
            let fact = [
                term(&mut vocabulary, s),
                term(&mut vocabulary, p),
                term(&mut vocabulary, o),
            ];
            assert!(
                derived.contains(&fact) || input.contains(&fact),
                "missing ({s}, {p}, {o})"
            );
        }
        if let Some(expected) = violation {
            assert_eq!(
                !violations.is_empty(),
                expected,
                "violations: {violations:?}"
            );
        }
    }

    /// A xorshift generator: `next(n)` is in `0..n`.
    fn rng(seed: u64) -> impl FnMut(usize) -> usize {
        let mut state = seed;
        move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        }
    }

    /// A random small ontology (lines for [`load`]) over a vocabulary that exercises every
    /// rule table: hierarchies, property axioms and characteristics, restrictions,
    /// cardinalities, equality, lists and the consistency rules.
    fn random_ontology(next: &mut dyn FnMut(usize) -> usize) -> Vec<String> {
        const CLASS_AXIOMS: [&str; 5] = [
            "rdfs:subClassOf",
            "owl:equivalentClass",
            "owl:disjointWith",
            "owl:complementOf",
            "rdfs:subClassOf",
        ];
        const PROPERTY_AXIOMS: [&str; 5] = [
            "rdfs:subPropertyOf",
            "owl:equivalentProperty",
            "owl:inverseOf",
            "owl:propertyDisjointWith",
            "rdfs:subPropertyOf",
        ];
        const CHARACTERISTICS: [&str; 7] = [
            "owl:TransitiveProperty",
            "owl:SymmetricProperty",
            "owl:FunctionalProperty",
            "owl:InverseFunctionalProperty",
            "owl:IrreflexiveProperty",
            "owl:AsymmetricProperty",
            "owl:ObjectProperty",
        ];
        const CARDINALITY: [&str; 2] = [
            "\"1\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>",
            "\"0\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>",
        ];
        let class = |n: usize| format!("ex:C{n}");
        let property = |n: usize| format!("ex:p{n}");
        let individual = |n: usize| format!("ex:i{n}");
        let mut lines: Vec<String> = Vec::new();
        let list = |lines: &mut Vec<String>, name: &str, members: [String; 2]| {
            let [a, b] = members;
            lines.push(format!("_:{name}a rdf:first {a}"));
            lines.push(format!("_:{name}a rdf:rest _:{name}b"));
            lines.push(format!("_:{name}b rdf:first {b}"));
            lines.push(format!("_:{name}b rdf:rest rdf:nil"));
            format!("_:{name}a")
        };
        for n in 0..(10 + next(30)) {
            let line = match next(12) {
                0 | 1 => format!(
                    "{} {} {}",
                    class(next(5)),
                    CLASS_AXIOMS[next(5)],
                    class(next(5))
                ),
                2 => format!(
                    "{} {} {}",
                    property(next(4)),
                    PROPERTY_AXIOMS[next(5)],
                    property(next(4))
                ),
                3 => {
                    let axiom = ["rdfs:domain", "rdfs:range"][next(2)];
                    format!("{} {axiom} {}", property(next(4)), class(next(5)))
                }
                4 => format!(
                    "{} rdf:type {}",
                    property(next(4)),
                    CHARACTERISTICS[next(7)]
                ),
                5 | 6 => format!("{} rdf:type {}", individual(next(6)), class(next(5))),
                7 => {
                    // A restriction: onProperty plus a filler.
                    let c = class(next(5));
                    lines.push(format!("{c} owl:onProperty {}", property(next(4))));
                    match next(5) {
                        0 => format!("{c} owl:someValuesFrom {}", class(next(5))),
                        1 => format!("{c} owl:allValuesFrom {}", class(next(5))),
                        2 => format!("{c} owl:hasValue {}", individual(next(6))),
                        n => format!("{c} owl:maxCardinality {}", CARDINALITY[n - 3]),
                    }
                }
                8 => {
                    let relation = ["owl:sameAs", "owl:differentFrom", "rdf:type"][next(3)];
                    if relation == "rdf:type" {
                        format!("{} rdf:type owl:Nothing", class(next(5)))
                    } else {
                        format!("{} {relation} {}", individual(next(6)), individual(next(6)))
                    }
                }
                9 => {
                    let name = format!("l{n}");
                    match next(4) {
                        0 => {
                            let head = list(&mut lines, &name, [class(next(5)), class(next(5))]);
                            format!("{} owl:intersectionOf {head}", class(next(5)))
                        }
                        1 => {
                            let head = list(&mut lines, &name, [class(next(5)), class(next(5))]);
                            format!("{} owl:unionOf {head}", class(next(5)))
                        }
                        2 => {
                            let members = [property(next(4)), property(next(4))];
                            let head = list(&mut lines, &name, members);
                            format!("{} owl:propertyChainAxiom {head}", property(next(4)))
                        }
                        _ => {
                            let head =
                                list(&mut lines, &name, [property(next(4)), property(next(4))]);
                            format!("{} owl:hasKey {head}", class(next(5)))
                        }
                    }
                }
                _ => format!(
                    "{} {} {}",
                    individual(next(6)),
                    property(next(4)),
                    individual(next(6))
                ),
            };
            lines.push(line);
        }
        lines
    }

    /// Equality-heavy data: the batch executor's equality module equals the generic
    /// `eq-rep-*` rules of the naive evaluator (chains of `sameAs` that merge classes over
    /// several rounds, equal predicates, equal objects).
    #[test]
    fn equality_module_equals_the_generic_rules() {
        let mut lines = Vec::new();
        for i in 0..11 {
            lines.push(format!("ex:a{i} owl:sameAs ex:a{}", i + 1));
        }
        for i in 0..4 {
            lines.push(format!("ex:b{i} owl:sameAs ex:b{}", i + 1));
        }
        lines.extend(
            [
                "ex:a3 ex:knows ex:b2",
                "ex:b0 ex:p ex:a7",
                "ex:p owl:sameAs ex:q",
                "ex:q rdfs:domain ex:C",
                "ex:a11 rdf:type ex:D",
                "ex:D rdfs:subClassOf ex:E",
                "ex:b4 owl:sameAs ex:a0",
            ]
            .map(str::to_owned),
        );
        let (_, derived, _) = closure(&lines.join(
            "
",
        ));
        assert!(derived.len() > 1000, "{}", derived.len());
    }

    /// The delta executor's equality module under random deletes and re-inserts of an
    /// equality-heavy ontology: equal to rematerialisation after every change.
    #[test]
    fn delta_equality_equals_rematerialisation() {
        use super::delta::{MemoryBase, Rules, update};

        let mut lines = Vec::new();
        for i in 0..7 {
            lines.push(format!("ex:a{i} owl:sameAs ex:a{}", i + 1));
        }
        for i in 0..3 {
            lines.push(format!("ex:b{i} owl:sameAs ex:b{}", i + 1));
        }
        lines.extend(
            [
                "ex:a3 ex:knows ex:b2",
                "ex:b0 ex:p ex:a7",
                "ex:p owl:sameAs ex:q",
                "ex:q rdfs:domain ex:C",
                "ex:a7 rdf:type ex:D",
                "ex:D rdfs:subClassOf ex:E",
                "ex:b3 owl:sameAs ex:a0",
                "ex:knows rdf:type owl:SymmetricProperty",
            ]
            .map(str::to_owned),
        );
        let mut vocabulary = LocalVocabulary::default();
        let pool = load(
            &mut vocabulary,
            &lines.join(
                "
",
            ),
        );
        let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
        let lists = ListVocabulary::new(&mut vocabulary);
        let schema = Schema::owl(&mut vocabulary);
        let compiled = Rules {
            rules: &rules,
            lists: Some(&lists),
            schema: &schema,
        };
        let closure = |asserted: &[Triple]| {
            batch::materialise(asserted, &rules, Some(&lists), &schema).derived
        };
        let mut next = rng(7);
        let mut asserted: Vec<Triple> = pool.clone();
        asserted.sort_unstable();
        let mut inferred = closure(&asserted);
        for step in 0..60 {
            let fact = pool[next(pool.len())];
            let deleting = asserted.binary_search(&fact).is_ok();
            let mut after: Vec<Triple> = asserted.iter().copied().filter(|&t| t != fact).collect();
            if !deleting {
                after.push(fact);
                after.sort_unstable();
            }
            let stack: Vec<Triple> = inferred
                .iter()
                .copied()
                .filter(|t| after.binary_search(t).is_err())
                .collect();
            let base = MemoryBase::new(&after, &stack);
            let new = !deleting && inferred.binary_search(&fact).is_err();
            let (ins, del): (&[Triple], &[Triple]) = if deleting {
                (&[], std::slice::from_ref(&fact))
            } else if new {
                (std::slice::from_ref(&fact), &[])
            } else {
                (&[], &[])
            };
            let result = update(&base, ins, del, compiled, None);
            let removal: HashSet<Triple> = result.remove.iter().copied().collect();
            let mut maintained: Vec<Triple> = stack
                .into_iter()
                .filter(|t| !removal.contains(t))
                .chain(result.insert)
                .collect();
            maintained.sort_unstable();
            maintained.dedup();
            assert_eq!(maintained, closure(&after), "step {step}");
            inferred = maintained;
            asserted = after;
        }
    }

    /// Random small ontologies: the batch executor equals the naive one.
    #[test]
    fn batch_equals_naive_on_random_ontologies() {
        let mut next = rng(0x9E37_79B9_7F4A_7C15);
        let (mut derived, mut violations, mut rules) =
            (0, 0, std::collections::BTreeSet::<String>::new());
        for case in 0..400 {
            let lines = random_ontology(&mut next);
            let mut vocabulary = LocalVocabulary::default();
            let input = load(&mut vocabulary, &lines.join("\n"));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                both(&mut vocabulary, &input)
            }));
            let Ok((d, v)) = result else {
                panic!("case {case}:\n{}", lines.join("\n"));
            };
            derived += d.len();
            violations += v.len();
            rules.extend(v.into_iter().map(|v| v.rule));
        }
        // The cases exercise derivations and a spread of consistency rules.
        assert!(derived > 20_000, "{derived}");
        assert!(rules.len() >= 8, "{violations} violations of {rules:?}");
    }

    /// Random ontologies under random insert/delete sequences: after every change, the
    /// inferred facts the delta executor maintains equal a rematerialisation, and the
    /// violations it reports are exactly the new ones.
    #[test]
    fn delta_equals_rematerialisation_under_random_changes() {
        use super::delta::{MemoryBase, Rules, update};

        // NRESE_FUZZ_CASES and NRESE_FUZZ_SEED widen the sweep locally.
        let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
        let cases = env("NRESE_FUZZ_CASES").unwrap_or(300);
        let mut next = rng(env("NRESE_FUZZ_SEED").unwrap_or(0x2545_F491_4F6C_DD1D));
        let (mut changes, mut inserted, mut removed) = (0, 0, 0);
        for case in 0..cases {
            let lines = random_ontology(&mut next);
            let mut vocabulary = LocalVocabulary::default();
            let pool = load(&mut vocabulary, &lines.join("\n"));
            let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
            let lists = ListVocabulary::new(&mut vocabulary);
            let schema = Schema::owl(&mut vocabulary);
            let closure =
                |asserted: &[Triple]| batch::materialise(asserted, &rules, Some(&lists), &schema);
            let mut asserted: Vec<Triple> = pool.iter().copied().filter(|_| next(10) < 7).collect();
            asserted.sort_unstable();
            asserted.dedup();
            let mut before = closure(&asserted);
            let mut inferred = before.derived.clone();
            // Odd cases carry the ground program from change to change, even ones rebuild it.
            let mut cache: Option<super::eval::GroundProgram> = None;
            let compiled = Rules {
                rules: &rules,
                lists: Some(&lists),
                schema: &schema,
            };
            for step in 0..4 {
                // Insert some facts of the pool, delete some asserted ones.
                let insert: Vec<Triple> = pool
                    .iter()
                    .copied()
                    .filter(|f| asserted.binary_search(f).is_err() && next(4) == 0)
                    .collect();
                let delete: Vec<Triple> =
                    asserted.iter().copied().filter(|_| next(6) == 0).collect();
                let mut after: Vec<Triple> = asserted
                    .iter()
                    .copied()
                    .filter(|f| !delete.contains(f))
                    .chain(insert.iter().copied())
                    .collect();
                after.sort_unstable();
                after.dedup();
                // Facts new to the state: neither asserted nor inferred before.
                let insert: Vec<Triple> = insert
                    .into_iter()
                    .filter(|f| !delete.contains(f) && inferred.binary_search(f).is_err())
                    .collect();
                // The engine drops inferred statements that become asserted.
                let stack: Vec<Triple> = inferred
                    .iter()
                    .copied()
                    .filter(|f| after.binary_search(f).is_err())
                    .collect();
                let base = MemoryBase::new(&after, &stack);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    update(&base, &insert, &delete, compiled, cache.as_ref())
                }));
                let context = || {
                    let text = |t: &Triple| {
                        let [s, p, o] = t.map(|id| vocabulary.text(id).to_owned());
                        format!("{s} {p} {o}")
                    };
                    format!(
                        "case {case} step {step}\ninsert {:#?}\ndelete {:#?}\nasserted before {:#?}",
                        insert.iter().map(text).collect::<Vec<_>>(),
                        delete.iter().map(text).collect::<Vec<_>>(),
                        asserted.iter().map(text).collect::<Vec<_>>()
                    )
                };
                let Ok(result) = result else {
                    panic!("{}", context());
                };
                changes += 1;
                let expected = closure(&after);
                if case % 2 == 1 {
                    if let Some(program) = &result.program {
                        cache = Some(program.clone());
                    }
                } else {
                    cache = None;
                }
                {
                    let removal: HashSet<Triple> = result.remove.iter().copied().collect();
                    let mut maintained: Vec<Triple> = stack
                        .iter()
                        .copied()
                        .filter(|f| !removal.contains(f))
                        .chain(result.insert.iter().copied())
                        .collect();
                    maintained.sort_unstable();
                    maintained.dedup();
                    if maintained != expected.derived {
                        let text = |t: &Triple| {
                            let [s, p, o] = t.map(|id| vocabulary.text(id).to_owned());
                            format!("{s} {p} {o}")
                        };
                        let missing: Vec<String> = expected
                            .derived
                            .iter()
                            .filter(|f| maintained.binary_search(f).is_err())
                            .map(text)
                            .collect();
                        let extra: Vec<String> = maintained
                            .iter()
                            .filter(|f| expected.derived.binary_search(f).is_err())
                            .map(text)
                            .collect();
                        panic!("{}\nmissing {missing:#?}\nextra {extra:#?}", context());
                    }
                    inserted += result.insert.len();
                    removed += result.remove.len();
                    // Reported violations: all of them are violations of the new state,
                    // and every violation the old state didn't have is reported.
                    let new_state: HashSet<&Violation> = expected.violations.iter().collect();
                    let old_state: HashSet<&Violation> = before.violations.iter().collect();
                    let reported: HashSet<&Violation> = result.violations.iter().collect();
                    assert!(
                        reported.is_subset(&new_state),
                        "{}\n{reported:?}",
                        context()
                    );
                    for violation in new_state.difference(&old_state) {
                        let terms: Vec<&str> = violation
                            .bindings
                            .iter()
                            .map(|&id| vocabulary.text(id))
                            .collect();
                        assert!(
                            reported.contains(violation),
                            "{}\nunreported {} {terms:?}",
                            context(),
                            violation.rule
                        );
                    }
                    inferred = maintained;
                }
                asserted = after;
                before = expected;
            }
        }
        assert_eq!(changes, cases * 4);
        assert!(
            inserted > 1000 && removed > 1000,
            "{inserted} inserted, {removed} removed"
        );
    }
}
