//! The reasoner's closure tests against the naive oracle, and the v1 scenarios
//! (crate-internal: they use the `naive` oracle, which is built for tests only).

mod v1_scenarios;

use std::collections::HashSet;

use super::batch::{self, Schema};
use super::ir::Vocabulary;
use super::lists::ListVocabulary;
use super::naive::{Triple, Violation, materialise};
use super::rulesets::Ruleset;
use super::vocabulary::LocalVocabulary;

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

/// Lists of any length are reasoned with (the audit of 2 October: a cap of 100
/// members skipped long `owl:AllDifferent` lists and missed inconsistencies). The
/// pairwise axioms over long lists are one rule each, checked through the members'
/// index; the rest expand as before.
#[test]
fn long_lists_are_reasoned_with_not_skipped() {
    let n = 300;
    let list = |name: &str, members: &[String]| -> String {
        let mut lines = String::new();
        for (i, member) in members.iter().enumerate() {
            let next = match i + 1 == members.len() {
                true => "rdf:nil".to_owned(),
                false => format!("_:{name}{}", i + 1),
            };
            lines.push_str(&format!(
                "_:{name}{i} rdf:first {member}\n_:{name}{i} rdf:rest {next}\n"
            ));
        }
        lines
    };
    let people: Vec<String> = (0..n).map(|i| format!("ex:p{i}")).collect();
    for (property, rule) in [("members", "eq-diff2"), ("distinctMembers", "eq-diff3")] {
        let axiom = format!(
            "_:d rdf:type owl:AllDifferent\n_:d owl:{property} _:l0\n{}",
            list("l", &people)
        );
        let (_, _, violations) = closure(&axiom);
        assert!(violations.is_empty(), "{property}: {violations:?}");
        // The pair (both ways), and the member sequence equality makes, in which p250
        // stands twice.
        let (_, _, violations) = closure(&format!("{axiom}ex:p7 owl:sameAs ex:p250\n"));
        assert!(
            !violations.is_empty() && violations.iter().all(|v| v == rule),
            "{property}: {violations:?}"
        );
        // An individual listed twice is inconsistent by itself.
        let mut twice = people.clone();
        twice.push("ex:p3".to_owned());
        let axiom = format!(
            "_:d rdf:type owl:AllDifferent\n_:d owl:{property} _:l0\n{}",
            list("l", &twice)
        );
        let (_, _, violations) = closure(&axiom);
        assert!(
            violations.contains(&rule.to_owned()),
            "{property}: {violations:?}"
        );
    }
    let classes: Vec<String> = (0..n).map(|i| format!("ex:C{i}")).collect();
    let disjoint = format!(
        "_:d rdf:type owl:AllDisjointClasses\n_:d owl:members _:l0\n{}",
        list("l", &classes)
    );
    let (_, _, violations) = closure(&format!("{disjoint}ex:x rdf:type ex:C1\n"));
    assert!(violations.is_empty(), "{violations:?}");
    let (_, _, violations) = closure(&format!(
        "{disjoint}ex:x rdf:type ex:C1\nex:x rdf:type ex:C299\n"
    ));
    assert_eq!(violations, vec!["cax-adc".to_owned()]);
    let properties: Vec<String> = (0..n).map(|i| format!("ex:q{i}")).collect();
    let disjoint = format!(
        "_:d rdf:type owl:AllDisjointProperties\n_:d owl:members _:l0\n{}",
        list("l", &properties)
    );
    let (_, _, violations) = closure(&format!("{disjoint}ex:x ex:q5 ex:y\nex:x ex:q200 ex:y\n"));
    assert_eq!(violations, vec!["prp-adp".to_owned()]);
    // Linear axioms over long lists: an enumeration and a union.
    let (vocabulary, derived, _) = closure(&format!(
        "ex:E owl:oneOf _:l0\n{}ex:U owl:unionOf _:m0\n{}ex:x rdf:type ex:C123\n",
        list("l", &people),
        list("m", &classes)
    ));
    let mut vocabulary = vocabulary;
    let ty = vocabulary.iri(&format!("{}type", super::ir::RDF));
    let id = |v: &mut LocalVocabulary, local: &str| v.iri(&format!("{EX}{local}"));
    let (p299, e, x, u) = (
        id(&mut vocabulary, "p299"),
        id(&mut vocabulary, "E"),
        id(&mut vocabulary, "x"),
        id(&mut vocabulary, "U"),
    );
    assert!(derived.contains(&[p299, ty, e]), "cls-oo over 300 members");
    assert!(derived.contains(&[x, ty, u]), "cls-uni over 300 members");
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
fn both(vocabulary: &mut LocalVocabulary, input: &[Triple]) -> (HashSet<Triple>, Vec<Violation>) {
    both_with(Ruleset::Owl2Rl, vocabulary, input)
}

/// [`both`] for any ruleset.
fn both_with(
    ruleset: Ruleset,
    vocabulary: &mut LocalVocabulary,
    input: &[Triple],
) -> (HashSet<Triple>, Vec<Violation>) {
    let rules = ruleset.rules(vocabulary).unwrap();
    let lists = ListVocabulary::new(vocabulary);
    let lists = ruleset.has_list_rules().then_some(&lists);
    let schema = Schema::owl(vocabulary);
    let naive = materialise(input, &rules, lists);
    let batch = batch::materialise(input, &rules, lists, &schema);
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
    let grouped = batch::materialise_grouped(groups, &rules, lists, &schema);
    // Derived facts never include asserted ones (the schema pre-closure must filter).
    let asserted: HashSet<Triple> = input.iter().copied().collect();
    assert!(
        batch.derived.iter().all(|t| !asserted.contains(t)),
        "asserted fact reported as derived"
    );
    assert_eq!(grouped.derived, batch.derived, "grouped input");
    // The relations kept without their object order are never looked up by object.
    assert_eq!(batch.counters.lookups_without_order, 0);
    assert_eq!(grouped.counters.lookups_without_order, 0);
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

/// Every profile parses, adds only OWL 2 RL rules that exist, and derives what its
/// definition says and not what the next profile adds.
#[test]
fn profiles_derive_what_they_define() {
    use super::rulesets::ALL;
    let mut vocabulary = LocalVocabulary::default();
    let owl: Vec<String> = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .unwrap()
        .into_iter()
        .map(|rule| rule.name)
        .collect();
    for ruleset in ALL {
        let rules = ruleset.rules(&mut vocabulary).unwrap();
        assert!(!rules.is_empty(), "{}", ruleset.name());
        for name in ruleset.owl_rules().unwrap_or_default() {
            assert!(
                owl.contains(&name.to_string()),
                "{name} in {}",
                ruleset.name()
            );
        }
        assert_eq!(Ruleset::from_name(ruleset.name()), Some(ruleset));
    }
    assert_eq!(
        Ruleset::RdfsFull
            .axiom_triples(&mut vocabulary)
            .unwrap()
            .len(),
        50
    );
    // dt-type1: the 32 datatypes OWL 2 RL supports.
    assert_eq!(
        Ruleset::Owl2Rl
            .axiom_triples(&mut vocabulary)
            .unwrap()
            .len(),
        32
    );

    let data = "ex:Cat rdfs:subClassOf ex:Animal
         ex:tom rdf:type ex:Cat
         ex:owns rdfs:domain ex:Person
         ex:alice ex:owns ex:tom
         ex:ancestor rdf:type owl:TransitiveProperty
         ex:a ex:ancestor ex:b
         ex:b ex:ancestor ex:c
         ex:tom owl:sameAs ex:thomas
         ex:HasPet owl:someValuesFrom owl:Thing
         ex:HasPet owl:onProperty ex:owns
         ex:Owner owl:hasValue ex:tom
         ex:Owner owl:onProperty ex:owns
         ex:Cat owl:disjointWith ex:Person
         ex:tom rdf:type ex:Person";
    let run = |ruleset: Ruleset| {
        let mut vocabulary = LocalVocabulary::default();
        let input = load(&mut vocabulary, data);
        let (derived, violations) = both_with(ruleset, &mut vocabulary, &input);
        let has = |fact: &str| {
            let triple = load(&mut vocabulary.clone(), fact)[0];
            derived.contains(&triple)
        };
        let facts = [
            "ex:tom rdf:type ex:Animal",
            "ex:alice rdf:type ex:Person",
            "ex:owns rdf:type rdf:Property",
            "ex:tom rdf:type rdfs:Resource",
            "ex:a ex:ancestor ex:c",
            "ex:thomas rdf:type ex:Cat",
            "ex:alice rdf:type ex:HasPet",
            "ex:alice rdf:type ex:Owner",
        ]
        .map(has);
        (facts, !violations.is_empty())
    };
    // subclass, domain, rdfD2, rdfs4a, transitivity, sameAs, someValuesFrom Thing,
    // hasValue; and whether the disjointness is a violation.
    let t = true;
    let f = false;
    assert_eq!(run(Ruleset::Rdfs), ([t, t, f, f, f, f, f, f], f));
    assert_eq!(run(Ruleset::RdfsFull), ([t, t, t, t, f, f, f, f], f));
    assert_eq!(run(Ruleset::RdfsPlus), ([t, t, f, f, t, t, f, f], f));
    assert_eq!(run(Ruleset::OwlHorst), ([t, t, f, f, t, t, t, t], f));
    assert_eq!(run(Ruleset::Owl2Ql), ([t, t, f, f, f, f, t, f], t));
    assert_eq!(run(Ruleset::Owl2Rl), ([t, t, f, f, t, t, t, t], t));
}

/// OWL 2 QL has reflexive properties (RL hasn't): every term used as an individual is
/// related to itself, and nothing else is. OWL2Bench QL's q01 missed these pairs
/// (office batch B, 5 October 2026).
#[test]
fn a_reflexive_property_relates_every_individual_to_itself_under_ql() {
    let data = "ex:knows rdf:type owl:ReflexiveProperty
         ex:C rdf:type owl:Class
         ex:a rdf:type ex:C
         ex:q rdf:type owl:ObjectProperty
         ex:b ex:q ex:c
         ex:d rdf:type owl:DatatypeProperty
         ex:e ex:d \"v\"^^<http://www.w3.org/2001/XMLSchema#string>
         ex:f rdf:type owl:NamedIndividual";
    let run = |ruleset: Ruleset| {
        let mut vocabulary = LocalVocabulary::default();
        let input = load(&mut vocabulary, data);
        let (derived, _) = both_with(ruleset, &mut vocabulary, &input);
        let knows = vocabulary.iri(&format!("{EX}knows"));
        let mut selves: Vec<String> = derived
            .iter()
            .filter(|t| t[1] == knows)
            .map(|t| {
                assert_eq!(t[0], t[2], "only pairs of a term with itself");
                vocabulary.text(t[0]).to_owned()
            })
            .collect();
        selves.sort();
        selves
    };
    let expected: Vec<String> = ["a", "b", "c", "e", "f"]
        .iter()
        .map(|local| format!("<{EX}{local}>"))
        .collect();
    assert_eq!(run(Ruleset::Owl2Ql), expected);
    assert!(run(Ruleset::Owl2Rl).is_empty());
}

/// Random small ontologies: the batch executor equals the naive one under every
/// profile.
#[test]
fn every_profile_batch_equals_naive() {
    let mut next = rng(0x5851_F42D_4C95_7F2D);
    for ruleset in super::rulesets::ALL {
        let mut derived = 0;
        for case in 0..60 {
            let lines = random_ontology(&mut next);
            let mut vocabulary = LocalVocabulary::default();
            let input = load(&mut vocabulary, &lines.join("\n"));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                both_with(ruleset, &mut vocabulary, &input)
            }));
            let Ok((d, _)) = result else {
                panic!("{} case {case}:\n{}", ruleset.name(), lines.join("\n"));
            };
            derived += d.len();
        }
        // The cases must derive something (a fair share: 60 small ontologies give
        // 200 or more under the fixed seed, down to about 150 under others).
        assert!(derived > 120, "{}: {derived}", ruleset.name());
    }
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

/// A stopped update ends at its next check with `Interrupted`, wherever the stop comes:
/// between rounds, in a job morsel, in proofs, rederivation or the consistency phase.
#[test]
fn stopped_updates_are_interrupted() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::delta::{Interrupted, MemoryBase, Rules, update, update_until};
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let compiled = Rules {
        rules: &rules,
        lists: Some(&lists),
        schema: &schema,
    };
    let mut text: Vec<String> = (0..30)
        .map(|i| format!("ex:C{i} rdfs:subClassOf ex:C{}", i + 1))
        .collect();
    text.extend((0..300).map(|j| format!("ex:x{j} rdf:type ex:D")));
    text.push("ex:C0 owl:disjointWith ex:E".to_owned());
    let mut asserted = load(
        &mut vocabulary,
        &text.join(
            "
",
        ),
    );
    asserted.sort_unstable();
    let inferred = batch::materialise(&asserted, &rules, Some(&lists), &schema).derived;
    let added = load(&mut vocabulary, "ex:D rdfs:subClassOf ex:C0");
    asserted.extend(&added);
    asserted.sort_unstable();
    let base = MemoryBase::new(&asserted, &inferred);
    let full = update(&base, &added, &[], compiled, None);
    assert!(full.insert.len() > 300 * 30, "{}", full.insert.len());

    let polls = AtomicUsize::new(0);
    let counting = || {
        polls.fetch_add(1, Ordering::Relaxed);
        false
    };
    let counted = update_until(&base, &added, &[], compiled, None, &counting).expect("runs");
    assert_eq!(counted.insert, full.insert);
    let n = polls.load(Ordering::Relaxed);
    assert!(n > 10, "{n} polls");
    for k in [0, 1, n / 2, n - 1] {
        let seen = AtomicUsize::new(0);
        let stop = || seen.fetch_add(1, Ordering::Relaxed) >= k;
        let result = update_until(&base, &added, &[], compiled, None, &stop);
        assert_eq!(result.err(), Some(Interrupted), "stop at poll {k} of {n}");
    }
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
    // Varied by `NRESE_FUZZ_SEED` (a number) for bug hunts over many seeds.
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut state = (seed ^ fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15)).max(1);
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
                        let head = list(&mut lines, &name, [property(next(4)), property(next(4))]);
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

/// Unnamed union classes (W7): on random ontologies whose domains and ranges are
/// anonymous unions, some also used where memberships are consumed, the closure with
/// the hidden classes' memberships left out is the full closure without them, under
/// every ruleset; classes that are consumed are never hidden.
#[test]
fn hidden_unnamed_classes_leave_the_rest_of_the_closure() {
    use super::unnamed::{UnnamedVocabulary, hidden_classes_in};
    let mut next = rng(0x000D_DBA1_1C1A_55E5);
    let (mut hidden_total, mut dropped_total) = (0, 0);
    for ruleset in super::rulesets::ALL {
        for case in 0..60 {
            let mut lines = random_ontology(&mut next);
            for u in 0..1 + next(4) {
                let head = format!("_:u{u}l0");
                lines.push(format!("_:u{u} owl:unionOf {head}"));
                for m in 0..2 {
                    lines.push(format!("_:u{u}l{m} rdf:first ex:C{}", next(5)));
                    let rest = if m == 1 {
                        "rdf:nil".to_owned()
                    } else {
                        format!("_:u{u}l{}", m + 1)
                    };
                    lines.push(format!("_:u{u}l{m} rdf:rest {rest}"));
                }
                if next(2) == 0 {
                    lines.push(format!("_:u{u} rdf:type owl:Class"));
                }
                let axiom = ["rdfs:domain", "rdfs:range"][next(2)];
                lines.push(format!("ex:p{} {axiom} _:u{u}", next(4)));
                // Now and then a use that consumes the memberships.
                match next(6) {
                    0 => lines.push(format!("_:u{u} rdfs:subClassOf ex:C{}", next(5))),
                    1 => lines.push(format!("ex:C{} owl:disjointWith _:u{u}", next(5))),
                    _ => {}
                }
            }
            for _ in 0..next(8) {
                lines.push(format!("ex:i{} ex:p{} ex:i{}", next(6), next(4), next(6)));
            }
            let mut vocabulary = LocalVocabulary::default();
            let input = load(
                &mut vocabulary,
                &lines.join(
                    "
",
                ),
            );
            let unnamed = UnnamedVocabulary::new(&mut vocabulary);
            let names = vocabulary.clone();
            let rules = ruleset.rules(&mut vocabulary).unwrap();
            let things = rules.iter().any(|r| r.name == "scm-cls");
            let hidden = hidden_classes_in(
                &input,
                &unnamed,
                &|id| names.text(id).starts_with("_:"),
                things,
            );
            let lists = ListVocabulary::new(&mut vocabulary);
            let lists = ruleset.has_list_rules().then_some(&lists);
            let schema = Schema::owl(&mut vocabulary);
            let full = batch::materialise(&input, &rules, lists, &schema);
            let rdf_type = vocabulary.iri(&format!("{}type", super::ir::RDF));
            let expected: HashSet<Triple> = full
                .derived
                .iter()
                .copied()
                .filter(|t| !(t[1] == rdf_type && hidden.contains_key(&t[2])))
                .collect();
            let dropped = full.derived.len() - expected.len();
            let schema = Schema::owl(&mut vocabulary).hiding(hidden.clone());
            let lean = batch::materialise(&input, &rules, lists, &schema);
            let got: HashSet<Triple> = lean.derived.iter().copied().collect();
            if got != expected {
                let text = |t: &Triple| {
                    let [s, p, o] = t.map(|id| vocabulary.text(id).to_owned());
                    format!("{s} {p} {o}")
                };
                let missing: Vec<String> = expected.difference(&got).take(6).map(text).collect();
                let extra: Vec<String> = got.difference(&expected).take(6).map(text).collect();
                panic!(
                    "{} case {case}: missing {missing:#?} extra {extra:#?}
{}",
                    ruleset.name(),
                    lines.join(
                        "
"
                    )
                );
            }
            assert_eq!(
                lean.violations.iter().map(|v| &v.rule).collect::<Vec<_>>(),
                full.violations.iter().map(|v| &v.rule).collect::<Vec<_>>(),
                "{} case {case}",
                ruleset.name()
            );
            // A consumed class is never hidden.
            for &class in hidden.keys() {
                let consumed = input.iter().any(|t| {
                    (t[0] == class && t[1] != unnamed.union_of() && t[1] != rdf_type)
                        || (t[2] == class && vocabulary.text(t[1]).contains("disjointWith"))
                });
                assert!(!consumed, "{}", vocabulary.text(class));
            }
            hidden_total += hidden.len();
            dropped_total += dropped;
        }
    }
    assert!(
        hidden_total > 300 && dropped_total > 300,
        "{hidden_total} {dropped_total}"
    );
}

/// Equality by representatives: on random ontologies with extra `sameAs` and data,
/// under every ruleset with equality, the representative closure expanded is exactly
/// the replicated closure, and the same consistency rules fire.
#[test]
fn representatives_expand_to_the_replicated_closure() {
    use super::representatives;
    let mut next = rng(0x1234_5678_9ABC_DEF1);
    let (mut classes_seen, mut merges) = (0, 0);
    for ruleset in [Ruleset::Owl2Rl, Ruleset::RdfsPlus, Ruleset::OwlHorst] {
        for case in 0..120 {
            let mut lines = random_ontology(&mut next);
            for _ in 0..next(6) {
                lines.push(format!("ex:i{} owl:sameAs ex:i{}", next(6), next(6)));
            }
            for _ in 0..next(10) {
                lines.push(format!("ex:i{} ex:p{} ex:i{}", next(6), next(4), next(6)));
            }
            // Equal properties and classes (punning), now and then.
            if next(3) == 0 {
                lines.push(format!("ex:p{} owl:sameAs ex:p{}", next(4), next(4)));
            }
            if next(4) == 0 {
                lines.push(format!("ex:C{} owl:sameAs ex:C{}", next(5), next(5)));
            }
            let mut vocabulary = LocalVocabulary::default();
            let input = load(
                &mut vocabulary,
                &lines.join(
                    "
",
                ),
            );
            let rules = ruleset.rules(&mut vocabulary).unwrap();
            let lists = ListVocabulary::new(&mut vocabulary);
            let lists = ruleset.has_list_rules().then_some(&lists);
            let schema = Schema::owl(&mut vocabulary);
            let replicated = batch::materialise(&input, &rules, lists, &schema);
            let mut expected: HashSet<Triple> = input.iter().copied().collect();
            expected.extend(replicated.derived.iter().copied());
            let closure = representatives::materialise(&input, &rules, lists, &schema);
            let expanded: HashSet<Triple> = closure
                .facts
                .iter()
                .flat_map(|&f| closure.classes.expand(f))
                .collect();
            if expanded != expected {
                let text = |t: &Triple| {
                    let [s, p, o] = t.map(|id| vocabulary.text(id).to_owned());
                    format!("{s} {p} {o}")
                };
                let missing: Vec<String> =
                    expected.difference(&expanded).take(6).map(text).collect();
                let extra: Vec<String> = expanded.difference(&expected).take(6).map(text).collect();
                panic!(
                    "{} case {case}: missing {missing:#?} extra {extra:#?}
{}",
                    ruleset.name(),
                    lines.join(
                        "
"
                    )
                );
            }
            let rules_fired = |v: &[Violation]| -> std::collections::BTreeSet<String> {
                v.iter().map(|v| v.rule.clone()).collect()
            };
            assert_eq!(
                rules_fired(&closure.violations),
                rules_fired(&replicated.violations),
                "{} case {case}
{}",
                ruleset.name(),
                lines.join(
                    "
"
                )
            );
            classes_seen += usize::from(!closure.classes.is_empty());
            merges += closure.merges;
            // The representative closure is never larger.
            assert!(closure.facts.len() <= expected.len());
        }
    }
    assert!(
        classes_seen > 150 && merges > 150,
        "{classes_seen} {merges}"
    );
}

/// Equality by representatives ends when a rule head names a constant that isn't its
/// class's representative, and the rewritten fact feeds a rule joining on the
/// representative. Found by the DL bounds (U1's Skolem constants, 4 October 2026): the
/// head re-derived `x p constant` in every round, which counted as new, and the closure
/// never ended.
#[test]
fn a_head_constant_that_is_no_representative_ends_the_closure() {
    use super::ir::parse_rules;
    use super::representatives;
    let mut vocabulary = LocalVocabulary::default();
    // ex:a is interned first: it represents its class with owl:Thing.
    let input = load(
        &mut vocabulary,
        "ex:a owl:sameAs owl:Thing
         ex:a rdf:type owl:Restriction
         ex:s rdf:type owl:Class",
    );
    let text = [
        "eq-sym:   (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)",
        "eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)",
        "eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)",
        "eq-rep-p: (?p owl:sameAs ?q), (?s ?p ?o) -> (?s ?q ?o)",
        "eq-rep-o: (?o owl:sameAs ?t), (?s ?p ?o) -> (?s ?p ?t)",
        "head: (?x rdf:type owl:Class) -> (?x rdfs:seeAlso owl:Thing), (?x owl:sameAs ?x)",
        "join: (?x rdfs:seeAlso ?y), (?y rdf:type owl:Restriction) -> (?x rdf:type owl:DeprecatedClass)",
    ];
    let rules = parse_rules(&text.join("\n"), &mut vocabulary).unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let replicated = batch::materialise(&input, &rules, None, &schema);
    let mut expected: HashSet<Triple> = input.iter().copied().collect();
    expected.extend(replicated.derived.iter().copied());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let stop = move || std::time::Instant::now() >= deadline;
    let closure = representatives::materialise_until(&input, &rules, None, &schema, &stop)
        .expect("the closure ends");
    let expanded: HashSet<Triple> = closure
        .facts
        .iter()
        .flat_map(|&f| closure.classes.expand(f))
        .collect();
    assert_eq!(expanded, expected);
    let deprecated = [
        vocabulary.iri(&format!("{EX}s")),
        vocabulary.iri(&format!("{}type", super::ir::RDF)),
        vocabulary.iri(&format!("{}DeprecatedClass", super::ir::OWL)),
    ];
    assert!(closure.facts.contains(&deprecated));
    assert!(
        closure
            .facts
            .iter()
            .all(|&t| closure.classes.rewrite(t) == t),
        "every fact is over representatives"
    );
}

/// G13 of the investigation of 6 October 2026, as it describes it: rule heads name
/// constants that aren't their class's representative and derive `rep sameAs c` (and
/// facts over `c`) in every pass, and a rule joins on the representative. The closure
/// ends after the pass that merges and one that confirms, with the replicated closure's
/// facts. (Fixed by da4e92c before the investigation's commit; kept as its reproduction.)
#[test]
fn head_constants_deriving_their_own_class_end_the_closure() {
    use super::ir::parse_rules;
    use super::representatives;
    let mut vocabulary = LocalVocabulary::default();
    // (The rule parser knows the OWL namespace, not ex:, so the constants live there.)
    // owl:a is interned first: it represents its class with owl:b and owl:c.
    let input = load(
        &mut vocabulary,
        "owl:a owl:sameAs owl:b
         ex:x rdf:type owl:K
         ex:y owl:q owl:c",
    );
    let text = [
        "eq-sym:   (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)",
        "eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)",
        "eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)",
        "eq-rep-p: (?p owl:sameAs ?q), (?s ?p ?o) -> (?s ?q ?o)",
        "eq-rep-o: (?o owl:sameAs ?t), (?s ?p ?o) -> (?s ?p ?t)",
        "skolem: (?x rdf:type owl:K) -> (owl:a owl:sameAs owl:b), (owl:b owl:sameAs owl:c), (?x owl:p owl:c)",
        "join: (?x owl:p owl:a), (?y owl:q owl:a) -> (?x owl:r ?y)",
    ];
    let rules = parse_rules(&text.join("\n"), &mut vocabulary).unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let replicated = batch::materialise(&input, &rules, None, &schema);
    let mut expected: HashSet<Triple> = input.iter().copied().collect();
    expected.extend(replicated.derived.iter().copied());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let stop = move || std::time::Instant::now() >= deadline;
    let closure = representatives::materialise_until(&input, &rules, None, &schema, &stop)
        .expect("the closure ends");
    let expanded: HashSet<Triple> = closure
        .facts
        .iter()
        .flat_map(|&f| closure.classes.expand(f))
        .collect();
    assert_eq!(expanded, expected);
    let joined = load(&mut vocabulary, "ex:x owl:r ex:y")[0];
    assert!(
        closure.facts.contains(&joined),
        "the join on the representative"
    );
    assert!(closure.passes <= 3, "{} passes", closure.passes);
}

/// Closed rule families (`cax-sco`, `prp-spo1`) don't read the part of the delta they
/// produced: on deep class and property hierarchies, with an equivalence cycle, types
/// that domains and ranges produce deep in the hierarchy, and a property under
/// `rdf:type` (`prp-spo1` producing into the relation whose closed part `cax-sco` owns),
/// the batch closure equals the naive one.
#[test]
fn closed_families_skip_their_own_output_exactly() {
    let mut lines: Vec<String> = Vec::new();
    for i in 0..40 {
        lines.push(format!("ex:C{} rdfs:subClassOf ex:C{i}", i + 1));
    }
    lines.push("ex:C20 owl:equivalentClass ex:E".to_owned());
    lines.push("ex:E rdfs:subClassOf ex:C25".to_owned());
    lines.push("ex:B rdfs:subClassOf ex:C33".to_owned());
    for i in 0..8 {
        lines.push(format!("ex:p{} rdfs:subPropertyOf ex:p{i}", i + 1));
    }
    lines.extend(
        [
            "ex:p3 rdfs:domain ex:C38",
            "ex:p6 rdfs:range ex:B",
            "ex:q rdfs:subPropertyOf rdf:type",
            "ex:p0 owl:inverseOf ex:r",
            "ex:r rdfs:domain ex:C30",
            "ex:a rdf:type ex:C40",
            "ex:b rdf:type ex:C10",
            "ex:c ex:p8 ex:d",
            "ex:e ex:q ex:C39",
            "ex:f ex:p5 ex:a",
        ]
        .map(str::to_owned),
    );
    let (mut vocabulary, derived, _) = closure(&lines.join("\n"));
    for fact in [
        "ex:a rdf:type ex:C0",
        "ex:c rdf:type ex:C0",
        "ex:d rdf:type ex:C0",
        "ex:e rdf:type ex:C0",
        "ex:c ex:p0 ex:d",
        "ex:a rdf:type ex:E",
    ] {
        assert!(has(&mut vocabulary, &derived, fact), "{fact}");
    }
}

/// The fuzz campaign's `NRESE_FUZZ_SEED=36` (owl2-rl case 96), reduced: two equal
/// properties, `p3` (interned first) representing `p0`, and the chain `p3 ∘ p3 ⊑ p0`
/// stated on `p0`: the axiom is over the representative only once equality rewrote it,
/// in the second round, and its transitivity rule must then still be registered (delta
/// grounding used to leave transitivity rules to full grounding).
#[test]
fn chains_over_equal_properties_close_under_representatives() {
    use super::representatives;
    let text = "_:l rdf:first ex:p3
         _:l rdf:rest _:m
         _:m rdf:first ex:p3
         _:m rdf:rest rdf:nil
         ex:p0 owl:propertyChainAxiom _:l
         ex:p3 owl:inverseOf ex:p0
         ex:i0 ex:p3 ex:i3
         ex:p0 owl:sameAs ex:p3";
    let mut vocabulary = LocalVocabulary::default();
    let input = load(&mut vocabulary, text);
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let replicated = batch::materialise(&input, &rules, Some(&lists), &schema);
    let mut expected: HashSet<Triple> = input.iter().copied().collect();
    expected.extend(replicated.derived.iter().copied());
    let closure = representatives::materialise(&input, &rules, Some(&lists), &schema);
    let expanded: HashSet<Triple> = closure
        .facts
        .iter()
        .flat_map(|&f| closure.classes.expand(f))
        .collect();
    let reflexive = load(&mut vocabulary, "ex:i0 ex:p0 ex:i0")[0];
    assert!(
        expected.contains(&reflexive),
        "the replicated closure has it"
    );
    assert_eq!(expanded, expected);
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
    let closure =
        |asserted: &[Triple]| batch::materialise(asserted, &rules, Some(&lists), &schema).derived;
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

/// A materialisation stops when asked, at any poll, and a stop that never fires
/// changes nothing (completion plan 1.7).
#[test]
fn materialisation_stops_when_asked() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut next = rng(77);
    let lines = random_ontology(&mut next);
    let mut vocabulary = LocalVocabulary::default();
    let input = load(
        &mut vocabulary,
        &lines.join(
            "
",
        ),
    );
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let mut groups: Vec<(u64, Vec<(u64, u64)>)> = Vec::new();
    let mut sorted = input.clone();
    sorted.sort_unstable_by_key(|&[s, p, o]| (p, o, s));
    sorted.dedup();
    for [s, p, o] in sorted {
        match groups.last_mut() {
            Some((last, pairs)) if *last == p => pairs.push((o, s)),
            _ => groups.push((p, vec![(o, s)])),
        }
    }
    let full = batch::materialise(&input, &rules, Some(&lists), &schema);
    let polls = AtomicUsize::new(0);
    let count = || {
        polls.fetch_add(1, Ordering::Relaxed);
        false
    };
    let again =
        batch::materialise_grouped_until(groups.clone(), &rules, Some(&lists), &schema, &count)
            .expect("never stopped");
    assert_eq!(again.derived, full.derived);
    let total = polls.load(Ordering::Relaxed);
    assert!(total > 2, "{total} polls");
    for at in [0, 1, total / 2] {
        let seen = AtomicUsize::new(0);
        let stop = || seen.fetch_add(1, Ordering::Relaxed) >= at;
        assert!(
            batch::materialise_grouped_until(groups.clone(), &rules, Some(&lists), &schema, &stop)
                .is_err(),
            "stopped at poll {at}"
        );
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
    // The cases exercise derivations and a spread of consistency rules (over 20 000
    // derivations under the fixed seed, down to about 19 000 under others).
    assert!(derived > 15_000, "{derived}");
    assert!(rules.len() >= 8, "{violations} violations of {rules:?}");
}

/// List axioms edited under the delta executor (the milestone review's R1): its list
/// rules follow the list's current members, as a rematerialisation's do, and commits
/// that touch list facts don't pile up copies of the list-derived consistency rules.
/// Both for short lists (a rule per pair) and long ones (one rule asking the members'
/// index).
#[test]
fn list_rules_follow_list_edits_under_the_delta_executor() {
    use super::delta::{MemoryBase, Rules, program, update};

    for n in [3, super::lists::PAIRWISE_MEMBERS + 50] {
        let mut vocabulary = LocalVocabulary::default();
        let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
        let lists = ListVocabulary::new(&mut vocabulary);
        let schema = Schema::owl(&mut vocabulary);
        let compiled = Rules {
            rules: &rules,
            lists: Some(&lists),
            schema: &schema,
        };
        let closure =
            |asserted: &[Triple]| batch::materialise(asserted, &rules, Some(&lists), &schema);
        // AllDifferent(p0 … p(n-1)); the last member is `c` of the review's example.
        let mut text = "_:d rdf:type owl:AllDifferent\n_:d owl:members _:l0\n".to_owned();
        for i in 0..n {
            let next = match i + 1 == n {
                true => "rdf:nil".to_owned(),
                false => format!("_:l{}", i + 1),
            };
            text.push_str(&format!(
                "_:l{i} rdf:first ex:p{i}\n_:l{i} rdf:rest {next}\n"
            ));
        }
        let mut asserted = load(&mut vocabulary, &text);
        asserted.sort_unstable();
        let mut inferred = closure(&asserted).derived;
        let mut cache = program(&MemoryBase::new(&asserted, &inferred), compiled);
        // One commit through the cached program; checked against a rematerialisation.
        // Returns the violations the commit reported and the number of consistency
        // rules of the program after it.
        let mut commit = |insert: &str, delete: &str, step: &str| -> (Vec<String>, usize) {
            let insert = load(&mut vocabulary, insert);
            let delete = load(&mut vocabulary, delete);
            let mut after: Vec<Triple> = asserted
                .iter()
                .copied()
                .filter(|f| !delete.contains(f))
                .chain(insert.iter().copied())
                .collect();
            after.sort_unstable();
            after.dedup();
            let new: Vec<Triple> = insert
                .iter()
                .copied()
                .filter(|f| inferred.binary_search(f).is_err())
                .collect();
            let stack: Vec<Triple> = inferred
                .iter()
                .copied()
                .filter(|f| after.binary_search(f).is_err())
                .collect();
            let result = update(
                &MemoryBase::new(&after, &stack),
                &new,
                &delete,
                compiled,
                Some(&cache),
            );
            let expected = closure(&after);
            let removal: HashSet<Triple> = result.remove.iter().copied().collect();
            let mut maintained: Vec<Triple> = stack
                .iter()
                .copied()
                .filter(|f| !removal.contains(f))
                .chain(result.insert.iter().copied())
                .collect();
            maintained.sort_unstable();
            maintained.dedup();
            assert_eq!(maintained, expected.derived, "n = {n}, {step}");
            let reported: HashSet<&Violation> = result.violations.iter().collect();
            let holds: HashSet<&Violation> = expected.violations.iter().collect();
            assert!(reported.is_subset(&holds), "n = {n}, {step}: {reported:?}");
            if let Some(program) = result.program {
                cache = program;
            }
            asserted = after;
            inferred = expected.derived;
            let rules = result.violations.iter().map(|v| v.rule.clone()).collect();
            (rules, cache.consistency.len())
        };
        // c leaves the list; then `p0 sameAs c` is consistent.
        let (last, before) = (n - 1, n - 2);
        let (reported, _) = commit(
            &format!("_:l{before} rdf:rest rdf:nil"),
            &format!(
                "_:l{before} rdf:rest _:l{last}\n_:l{last} rdf:first ex:p{last}\n_:l{last} rdf:rest rdf:nil"
            ),
            "c removed",
        );
        assert!(reported.is_empty(), "n = {n}: {reported:?}");
        let (reported, _) = commit(&format!("ex:p0 owl:sameAs ex:p{last}"), "", "p0 sameAs c");
        assert!(reported.is_empty(), "n = {n}, c is no member: {reported:?}");
        // Commits touching other lists leave the consistency rules as they are.
        let (_, settled) = commit("ex:x ex:p ex:y", "", "no list");
        for i in 0..5 {
            let (_, now) = commit(
                &format!(
                    "ex:U{i} owl:unionOf _:u{i}\n_:u{i} rdf:first ex:A{i}\n_:u{i} rdf:rest rdf:nil"
                ),
                "",
                "another list",
            );
            assert_eq!(
                now, settled,
                "n = {n}: list commit {i} added consistency rules"
            );
        }
        // A pair still in the list is inconsistent.
        let (reported, _) = commit("ex:p0 owl:sameAs ex:p1", "", "p0 sameAs p1");
        assert!(
            reported.iter().any(|rule| rule == "eq-diff2"),
            "n = {n}: {reported:?}"
        );
    }
}

/// The oracle of `graph_sets` (its test calls it): random ontologies whose statements
/// are spread over three graphs; for each set of graphs, the facts with a support set
/// inside it equal the closure of its statements.
pub(super) fn support_sets_against_closures() {
    use super::delta::{MemoryBase, Rules, program};
    let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(150);
    let mut next = rng(env("NRESE_FUZZ_SEED").unwrap_or(0x6a5e_7c0d_2026_1003));
    let mut checked = 0;
    for case in 0..cases {
        let lines = random_ontology(&mut next);
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
        // Each statement in one graph, a few in two.
        let mut placed: Vec<(Triple, u32)> = Vec::new();
        for &fact in &pool {
            placed.push((fact, next(3) as u32));
            if next(6) == 0 {
                placed.push((fact, next(3) as u32));
            }
        }
        let mut asserted: Vec<Triple> = placed.iter().map(|&(f, _)| f).collect();
        asserted.sort_unstable();
        asserted.dedup();
        let closure = |facts: &[Triple]| {
            let mut all = batch::materialise(facts, &rules, Some(&lists), &schema).derived;
            all.extend_from_slice(facts);
            all.sort_unstable();
            all.dedup();
            all
        };
        let full = closure(&asserted);
        let inferred: Vec<Triple> = full
            .iter()
            .copied()
            .filter(|f| asserted.binary_search(f).is_err())
            .collect();
        let ground = program(&MemoryBase::new(&asserted, &inferred), compiled);
        let graphs_of = |fact: Triple| -> Vec<u32> {
            placed
                .iter()
                .filter(|&&(f, _)| f == fact)
                .map(|&(_, g)| g)
                .collect()
        };
        let sets = super::graph_sets::support_sets(&full, &ground, &[], &graphs_of, usize::MAX);
        for readable in 0u32..8 {
            let data: Vec<Triple> = placed
                .iter()
                .filter(|&&(_, g)| readable & (1 << g) != 0)
                .map(|&(f, _)| f)
                .collect();
            let mut data = data;
            data.sort_unstable();
            data.dedup();
            let expected = closure(&data);
            let visible: Vec<Triple> = full
                .iter()
                .copied()
                .filter(|f| {
                    sets.get(f).is_some_and(|sets| {
                        sets.iter()
                            .any(|set| set.graphs().all(|g| readable & (1 << g) != 0))
                    })
                })
                .collect();
            if visible != expected {
                let text = |t: &Triple| {
                    let [s, p, o] = t.map(|id| vocabulary.text(id).to_owned());
                    format!("{s} {p} {o}")
                };
                let wrong: Vec<Triple> = visible
                    .iter()
                    .filter(|f| expected.binary_search(f).is_err())
                    .chain(
                        expected
                            .iter()
                            .filter(|f| visible.binary_search(f).is_err()),
                    )
                    .copied()
                    .collect();
                // Each wrong fact's one-step derivations, with its premises' sets.
                let store = super::batch::Store::new(full.clone());
                let why: Vec<String> = wrong
                    .iter()
                    .flat_map(|&f| {
                        ground
                            .named_derivations(&store, f, 20)
                            .into_iter()
                            .map(move |d| (f, d))
                    })
                    .map(|(f, (rule, body))| {
                        let premises: Vec<String> = body
                            .iter()
                            .map(|b| format!("{} {:?}", text(b), sets.get(b)))
                            .collect();
                        format!("{} <- {rule}: {}", text(&f), premises.join(" ; "))
                    })
                    .collect();
                let ontology: Vec<String> = placed
                    .iter()
                    .map(|(f, g)| format!("{} @{g}", text(f)))
                    .collect();
                panic!(
                    "case {case}, graphs {readable:03b}: wrong {:#?}\nwhy {why:#?}\nontology:\n{}",
                    wrong.iter().map(text).collect::<Vec<_>>(),
                    ontology.join("\n")
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 0);
}

/// The oracle of `graph_sets::update_support_sets` (its test calls it): random
/// ontologies over three graphs, then random changes of where instance statements
/// are (added to a graph, removed from one, moved); the update's sets equal those
/// computed afresh for every fact of the closure after the change.
pub(super) fn support_set_updates_against_recomputation() {
    use super::delta::{MemoryBase, Rules, program};
    use super::graph_sets::{Change, support_sets, update_support_sets};
    let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(150);
    let mut next = rng(env("NRESE_FUZZ_SEED").unwrap_or(0x5e75_d1ff_2026_1003));
    let (mut updated, mut refused) = (0, 0);
    for case in 0..cases {
        let lines = random_ontology(&mut next);
        let mut vocabulary = LocalVocabulary::default();
        let pool = load(&mut vocabulary, &lines.join("\n"));
        let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
        let lists = ListVocabulary::new(&mut vocabulary);
        let schema = Schema::owl(&mut vocabulary);
        let compiled = Rules {
            rules: &rules,
            lists: Some(&lists),
            schema: &schema,
        };
        let closure = |facts: &[Triple]| {
            let mut all = batch::materialise(facts, &rules, Some(&lists), &schema).derived;
            all.extend_from_slice(facts);
            all.sort_unstable();
            all.dedup();
            all
        };
        // The state for statements placed in graphs: closure, program, sets.
        let state = |placed: &[(Triple, u32)]| {
            let mut asserted: Vec<Triple> = placed.iter().map(|&(f, _)| f).collect();
            asserted.sort_unstable();
            asserted.dedup();
            let full = closure(&asserted);
            let inferred: Vec<Triple> = full
                .iter()
                .copied()
                .filter(|f| asserted.binary_search(f).is_err())
                .collect();
            let ground = program(&MemoryBase::new(&asserted, &inferred), compiled);
            (full, ground)
        };
        let graphs_in = |placed: &[(Triple, u32)], fact: Triple| -> Vec<u32> {
            let mut graphs: Vec<u32> = placed
                .iter()
                .filter(|&&(f, _)| f == fact)
                .map(|&(_, g)| g)
                .collect();
            graphs.sort_unstable();
            graphs.dedup();
            graphs
        };
        let mut placed: Vec<(Triple, u32)> = pool.iter().map(|&f| (f, next(3) as u32)).collect();
        let (before, ground_before) = state(&placed);
        let sets_before = support_sets(
            &before,
            &ground_before,
            &[],
            &|f| graphs_in(&placed, f),
            usize::MAX,
        );
        // Change placements of statements no grounding rests on.
        let premises = ground_before.schema_premises();
        let movable: Vec<usize> = (0..placed.len())
            .filter(|&i| !premises.contains(&placed[i].0))
            .collect();
        if movable.is_empty() {
            continue;
        }
        let old_placed = placed.clone();
        for _ in 0..=next(3) {
            let i = movable[next(movable.len())];
            if placed[i].1 == u32::MAX {
                continue;
            }
            match next(3) {
                0 => placed[i].1 = (placed[i].1 + 1) % 3,
                1 => placed.push((placed[i].0, next(3) as u32)),
                _ => placed[i].1 = u32::MAX,
            }
        }
        placed.retain(|&(_, g)| g != u32::MAX);
        let (after, ground_after) = state(&placed);
        if ground_after.schema_premises() != premises {
            continue;
        }
        // Touched: a fact whose graphs changed, or that entered or left the closure.
        let mut touched: Vec<Triple> = old_placed
            .iter()
            .chain(&placed)
            .map(|&(f, _)| f)
            .filter(|&f| graphs_in(&old_placed, f) != graphs_in(&placed, f))
            .collect();
        touched.extend(before.iter().filter(|f| after.binary_search(f).is_err()));
        touched.extend(after.iter().filter(|f| before.binary_search(f).is_err()));
        touched.sort_unstable();
        touched.dedup();
        let removed: Vec<Triple> = before
            .iter()
            .copied()
            .filter(|f| after.binary_search(f).is_err())
            .collect();
        let mut reachable = after.clone();
        reachable.extend_from_slice(&removed);
        let source = super::batch::Store::new(reachable);
        let previous = |f: Triple| sets_before.get(&f).cloned().unwrap_or_default();
        let change = Change {
            source: &source,
            in_closure: &|f| after.binary_search(&f).is_ok(),
            touched: &touched,
            previous: &previous,
            limit: usize::MAX,
        };
        let Some(update) = update_support_sets(
            &change,
            &ground_after,
            &[],
            &|f| graphs_in(&placed, f),
            usize::MAX,
        ) else {
            refused += 1;
            continue;
        };
        let expected = support_sets(
            &after,
            &ground_after,
            &[],
            &|f| graphs_in(&placed, f),
            usize::MAX,
        );
        for &fact in &after {
            let got = update.get(&fact).cloned().unwrap_or_else(|| previous(fact));
            let want = expected.get(&fact).cloned().unwrap_or_default();
            if got != want {
                let text = |t: &Triple| {
                    let [s, p, o] = t.map(|id| vocabulary.text(id).to_owned());
                    format!("{s} {p} {o}")
                };
                let shown: Vec<String> = placed
                    .iter()
                    .map(|(f, g)| format!("{} @{g}", text(f)))
                    .collect();
                panic!(
                    "case {case}: {} has {got:?}, recomputed {want:?}; before {:?}; touched {:?}\nplaced:\n{}",
                    text(&fact),
                    previous(fact),
                    touched.iter().map(text).collect::<Vec<_>>(),
                    shown.join("\n")
                );
            }
        }
        updated += 1;
    }
    assert!(updated > cases / 4, "updated {updated}, refused {refused}");
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
        // A third of the cases run the other rulesets in turn.
        let ruleset = if case % 3 == 0 {
            super::rulesets::ALL[(case as usize / 3) % 5]
        } else {
            Ruleset::Owl2Rl
        };
        let rules = ruleset.rules(&mut vocabulary).unwrap();
        let lists = ListVocabulary::new(&mut vocabulary);
        let lists = ruleset.has_list_rules().then_some(&lists);
        let schema = Schema::owl(&mut vocabulary);
        let closure = |asserted: &[Triple]| batch::materialise(asserted, &rules, lists, &schema);
        let mut asserted: Vec<Triple> = pool.iter().copied().filter(|_| next(10) < 7).collect();
        asserted.sort_unstable();
        asserted.dedup();
        let mut before = closure(&asserted);
        let mut inferred = before.derived.clone();
        // Odd cases carry the ground program from change to change, even ones rebuild it.
        let mut cache: Option<super::eval::GroundProgram> = None;
        let compiled = Rules {
            rules: &rules,
            lists,
            schema: &schema,
        };
        for step in 0..4 {
            // Insert some facts of the pool, delete some asserted ones.
            let insert: Vec<Triple> = pool
                .iter()
                .copied()
                .filter(|f| asserted.binary_search(f).is_err() && next(4) == 0)
                .collect();
            let delete: Vec<Triple> = asserted.iter().copied().filter(|_| next(6) == 0).collect();
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
                    "case {case} ({}) step {step}\ninsert {:#?}\ndelete {:#?}\nasserted before {:#?}",
                    ruleset.name(),
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

/// The batch store's in-place drivers ([`Source::scan_range`]) visit exactly what a scan
/// of the same pattern and segment visits, in the same order, however the positions are
/// split into morsels: every shape of bound positions, every segment, an open predicate.
#[test]
fn in_place_drivers_visit_what_a_scan_visits() {
    use super::eval::{Seg, Source};
    let mut next = rng(0x5eed_d21e);
    for _ in 0..40 {
        let facts = |next: &mut dyn FnMut(usize) -> usize, n: usize| -> Vec<Triple> {
            (0..n)
                .map(|_| [next(12) as u64, 100 + next(3) as u64, next(12) as u64])
                .collect()
        };
        let mut store = batch::Store::new(facts(&mut next, 60));
        store.advance(facts(&mut next, 40));
        store.advance(facts(&mut next, 30));
        let term = |next: &mut dyn FnMut(usize) -> usize, base: u64, n: usize| {
            (next(2) == 0).then(|| base + next(n) as u64)
        };
        for _ in 0..30 {
            let pattern = [
                term(&mut next, 0, 12),
                term(&mut next, 100, 3),
                term(&mut next, 0, 12),
            ];
            for seg in [Seg::Delta, Seg::Old, Seg::All] {
                let mut scanned = Vec::new();
                store.scan(pattern, seg, &mut |t| scanned.push(t));
                let len = store
                    .matches_len(pattern, seg)
                    .expect("the store indexes its matches");
                assert!(len >= scanned.len());
                let morsel = 1 + next(7);
                let mut ranged = Vec::new();
                for start in (0..len).step_by(morsel) {
                    store.scan_range(pattern, seg, start..(start + morsel).min(len), &mut |t| {
                        ranged.push(t)
                    });
                }
                assert_eq!(ranged, scanned, "{pattern:?} {seg:?} morsel {morsel}");
            }
        }
    }
}
