//! The upper bound U1 (docs/design/owl2-dl.md §8, package 3.2), evaluated by the rule
//! reasoner and checked:
//!
//! - on PAGOdA's running example (Zhou et al., JAIR 2015, Table 2), against the bound the
//!   paper computes for it (Example 5.6);
//! - through its Notation3 form, which must give the same closure;
//! - with large at-least restrictions collapsed, against the full encoding;
//! - on fuzzed ontologies with ABoxes ([`fuzz`]): a clash-free U1 must be a model of the
//!   ontology (so it contains every certain answer), L ⊆ U1, and an L violation means a
//!   clash in U1;
//! - on the W3C OWL 2 test cases ([`w3c`]).

mod fuzz;
mod support;
mod w3c;

use std::collections::BTreeSet;

use nrese_dl::bounds::{Answer, AtomicQuery, Bounds, Origin};
use nrese_rdf_io::RdfFormat;
use support::{Table, Triple};

const EX: &str = "http://example.org/kex#";

/// PAGOdA's running example `Kex` (Table 2) in OWL: rules (R1)–(R9) as axioms, facts
/// (D1)–(D15) as assertions.
const KEX: &str = r#"
@prefix : <http://example.org/kex#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
:Carnivore a owl:Class . :Mammal a owl:Class . :Herbivore a owl:Class .
:Folivore a owl:Class . :MeatEater a owl:Class . :Plant a owl:Class . :Leaf a owl:Class .
:eats a owl:ObjectProperty .
:Carnivore rdfs:subClassOf :Mammal .
:Herbivore rdfs:subClassOf :Mammal .
:Folivore owl:disjointWith :MeatEater .
:Herbivore rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :eats ; owl:allValuesFrom :Plant ] .
:Mammal rdfs:subClassOf [ a owl:Class ; owl:unionOf ( :Herbivore :MeatEater ) ] .
:MeatEater rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :eats ; owl:someValuesFrom :Herbivore ] .
:Mammal rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :eats ; owl:someValuesFrom owl:Thing ] .
:Folivore rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :eats ; owl:someValuesFrom :Leaf ] .
:Leaf rdfs:subClassOf :Plant .
:tiger a :Mammal . :lion a :Mammal . :python a :MeatEater . :python :eats :rabbit .
:rabbit a :Herbivore . :wolf a :Mammal . :wolf a :MeatEater . :wolf :eats :sheep .
:sheep a :Herbivore . :sheep :eats :grass . :howler a :Mammal . :howler a :Folivore .
:a_hare a :Mammal . :a_hare a :Folivore . :a_hare :eats :willow .
"#;

struct Example {
    table: Table,
    input: Vec<Triple>,
    program: nrese_dl::bounds::Program,
    lower: support::Closure,
    upper: support::Closure,
}

fn example(text: &str) -> Example {
    let mut table = Table::default();
    let input = table.parse(RdfFormat::Turtle, text.as_bytes());
    let (ontology, normalised) = support::read(&mut table, &input);
    let program = support::compile(&mut table, &ontology, &normalised);
    let lower = support::lower(&mut table, &input);
    let upper = support::upper(&mut table, &program, &input);
    Example {
        table,
        input,
        program,
        lower,
        upper,
    }
}

fn ex(table: &mut Table, local: &str) -> u64 {
    table.named(&format!("{EX}{local}"))
}

#[test]
fn pagoda_running_example_gives_the_papers_bound() {
    let mut e = example(KEX);
    assert!(e.program.incomplete.is_empty());
    let has = |e: &mut Example, s: &str, p: &str, o: &str| {
        let rdf_type = e.program.names.rdf_type;
        let s = ex(&mut e.table, s);
        let p = if p == "a" {
            rdf_type
        } else {
            ex(&mut e.table, p)
        };
        let o = ex(&mut e.table, o);
        e.upper.facts.binary_search(&[s, p, o]).is_ok()
    };
    // Certain answers (by hand, as in the paper's §3): all in U1.
    for (s, p, o) in [
        ("rabbit", "a", "Mammal"),
        ("howler", "a", "Herbivore"),
        ("a_hare", "a", "Herbivore"),
        ("grass", "a", "Plant"),
        ("willow", "a", "Plant"),
    ] {
        assert!(has(&mut e, s, p, o), "{s} {p} {o}");
    }
    // The clash of (R3) fires on howler and a_hare (Example 5.3), and U1 still answers.
    let clash = e.program.names.clash;
    let clashes: BTreeSet<String> = e
        .upper
        .facts
        .iter()
        .filter(|t| t[1] == clash)
        .map(|t| e.table.text(t[0]))
        .collect();
    assert_eq!(
        clashes,
        BTreeSet::from([format!("<{EX}a_hare>"), format!("<{EX}howler>")])
    );
    // q(x) = ∃y eats(x, y) ∧ Plant(y) over U1: Example 5.6's answers over named terms.
    let (eats, plant) = (ex(&mut e.table, "eats"), ex(&mut e.table, "Plant"));
    let rdf_type = e.program.names.rdf_type;
    let plants: BTreeSet<u64> = e
        .upper
        .facts
        .iter()
        .filter(|t| t[1] == rdf_type && t[2] == plant)
        .map(|t| t[0])
        .collect();
    let answers: BTreeSet<String> = e
        .upper
        .facts
        .iter()
        .filter(|t| t[1] == eats && plants.contains(&t[2]) && !e.program.is_internal(t[0]))
        .map(|t| e.table.text(t[0]).replace(EX, "").replace(['<', '>'], ""))
        .collect();
    let paper: BTreeSet<String> = [
        "tiger", "lion", "python", "rabbit", "wolf", "sheep", "howler", "a_hare",
    ]
    .map(str::to_owned)
    .into();
    assert_eq!(answers, paper);
    // L ⊆ U1, and the gap: Herbivore(howler) needs reasoning by cases.
    let bounds = Bounds::new(
        &e.program,
        &e.table,
        e.lower.facts.clone(),
        e.upper.facts.clone(),
    );
    assert_eq!(bounds.lower_not_in_upper(), Vec::<Triple>::new());
    let herbivore = ex(&mut e.table, "Herbivore");
    let howler = ex(&mut e.table, "howler");
    match bounds.answer(AtomicQuery::Instances(herbivore)) {
        Answer::Gap { candidates, .. } => {
            assert!(candidates.contains(&[howler, rdf_type, herbivore]))
        }
        Answer::Exact(_) => panic!("Herbivore(howler) is not in L"),
    }
    let grass = ex(&mut e.table, "grass");
    assert_eq!(
        bounds.answer(AtomicQuery::Fact([grass, rdf_type, plant])),
        Answer::Exact(vec![[grass, rdf_type, plant]])
    );
    let report = bounds.report();
    assert_eq!(report.lower_only, 0);
    assert!(report.gap > 0 && report.clashes == 2);
    // Provenance: every clause rule names its source axioms.
    assert!(e.program.rules.iter().all(|r| match r.provenance.origin {
        Origin::Clause(_) | Origin::Chain | Origin::Key | Origin::Skolem | Origin::Assertion => {
            !r.provenance.sources.is_empty()
        }
        Origin::Thing | Origin::Equality | Origin::Nominal | Origin::Builtin => true,
    }));
}

#[test]
fn the_n3_form_gives_the_same_closure() {
    let mut e = example(KEX);
    let text = {
        let table = &e.table;
        e.program.to_n3(&|t| table.text(t)).expect("no blank nodes")
    };
    let n3 = nrese_reasoner::n3::compile("u1", &text, &mut e.table).expect("U1 is valid N3");
    let mut input = e.input.clone();
    input.extend(n3.facts);
    input.sort_unstable();
    input.dedup();
    assert_eq!(input, support::upper_input(&e.program, &e.input));
    let from_n3 = support::upper_copied(&mut e.table, &n3.rules, &input);
    let rules = support::reasoner_rules(&e.program);
    let copied = support::upper_copied(&mut e.table, &rules, &input);
    assert_eq!(from_n3, copied);
    // support::upper (what the other tests read) gives the same answers.
    let program = &e.program;
    let answers = |facts: &[Triple]| -> Vec<Triple> {
        facts
            .iter()
            .filter(|t| !program.is_internal(t[0]) && !program.is_internal(t[2]))
            .copied()
            .collect()
    };
    assert_eq!(answers(&copied), answers(&e.upper.facts));
}

/// `≥ n` collapsed to two constants keeps every answer and every clash of the full
/// encoding (the homomorphism and symmetry argument of `bounds::upper`'s docs).
#[test]
fn collapsed_at_least_keeps_answers_and_clashes() {
    let prefix = r#"
@prefix : <http://example.org/kex#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
:A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class .
:r a owl:ObjectProperty . :s a owl:ObjectProperty .
:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ;
    owl:minQualifiedCardinality "5"^^xsd:nonNegativeInteger ; owl:onClass :B ] .
:B rdfs:subClassOf :C , [ a owl:Restriction ; owl:onProperty :s ; owl:hasValue :o ] .
:r rdfs:range :D .
:a a :A . :a :r :b .
"#;
    let cases = [
        ("consistent", ""),
        (
            "at-most atom",
            ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; \
             owl:maxQualifiedCardinality \"3\"^^xsd:nonNegativeInteger ; owl:onClass :B ] .",
        ),
        ("functional", ":r a owl:FunctionalProperty ."),
        (
            "at-most above n",
            ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; \
             owl:maxQualifiedCardinality \"7\"^^xsd:nonNegativeInteger ; owl:onClass :B ] .",
        ),
        ("inverse functional", ":s a owl:InverseFunctionalProperty ."),
    ];
    for (name, extra) in cases {
        let text = format!("{prefix}{extra}\n");
        let mut table = Table::default();
        let input = table.parse(RdfFormat::Turtle, text.as_bytes());
        let (ontology, normalised) = support::read(&mut table, &input);
        let full = support::compile(&mut table, &ontology, &normalised);
        let options = nrese_dl::bounds::Options { max_skolems: 2 };
        let collapsed = support::compile_with(&mut table, &ontology, &normalised, options);
        assert!(
            !full
                .rules
                .iter()
                .any(|r| r.provenance.approximations.collapsed)
        );
        assert!(
            collapsed
                .rules
                .iter()
                .any(|r| r.provenance.approximations.collapsed)
        );
        let answers = |program: &nrese_dl::bounds::Program, table: &mut Table| {
            let closure = support::upper(table, program, &input);
            let clash = closure.facts.iter().any(|t| t[1] == program.names.clash);
            let named: BTreeSet<Triple> = closure
                .facts
                .into_iter()
                .filter(|t| !t.iter().any(|&x| program.is_internal(x)))
                .collect();
            (named, clash)
        };
        let (full_answers, full_clash) = answers(&full, &mut table);
        let (collapsed_answers, collapsed_clash) = answers(&collapsed, &mut table);
        let lost: Vec<_> = full_answers.difference(&collapsed_answers).collect();
        assert!(lost.is_empty(), "{name}: the collapse lost {lost:?}");
        assert!(
            !full_clash || collapsed_clash,
            "{name}: the collapse lost the clash"
        );
        let expect_clash = name != "consistent";
        assert_eq!(full_clash, expect_clash, "{name}");
    }
}

#[test]
fn chains_nominals_cardinalities_and_keys() {
    let text = r#"
@prefix : <http://example.org/kex#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
:A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class .
:r a owl:ObjectProperty . :s a owl:ObjectProperty . :t a owl:ObjectProperty .
:f a owl:ObjectProperty , owl:FunctionalProperty . :k a owl:DatatypeProperty .
:t a owl:TransitiveProperty .
:t owl:propertyChainAxiom ( :r :s ) .
:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:hasValue :o ] .
:B rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :f ; owl:maxCardinality "1"^^xsd:nonNegativeInteger ] .
:C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :s ; owl:minCardinality "2"^^xsd:nonNegativeInteger ] .
:D owl:hasKey ( :k ) .
:a a :A . :o :s :p . :p :t :q .
:b :f :c1 . :b :f :c2 .
:d1 a :D . :d2 a :D . :d1 :k "7"^^xsd:integer . :d2 :k "7"^^xsd:integer .
:e a :C .
"#;
    let mut e = example(text);
    let rdf_type = e.program.names.rdf_type;
    let same = e.program.names.same_as;
    let (r, t) = (ex(&mut e.table, "r"), ex(&mut e.table, "t"));
    let mut has = |s: &str, p: u64, o: &str| {
        let (s, o) = (ex(&mut e.table, s), ex(&mut e.table, o));
        e.upper.facts.binary_search(&[s, p, o]).is_ok()
    };
    // A ⊑ ∃r.{o} with the nominal; the chain r∘s ⊑ t; t transitive.
    assert!(has("a", r, "o"));
    assert!(has("a", t, "p"));
    assert!(has("a", t, "q"));
    // Functional: the two successors are equal; the key equates d1 and d2.
    assert!(has("c1", same, "c2"));
    assert!(has("d1", same, "d2"));
    // ≥ 2 s at e: two Skolem constants, distinct, no clash.
    let _ = rdf_type;
    let s = ex(&mut e.table, "s");
    let e_ = ex(&mut e.table, "e");
    let successors = e
        .upper
        .facts
        .iter()
        .filter(|f| f[0] == e_ && f[1] == s)
        .count();
    assert_eq!(successors, 2);
    assert!(!e.upper.facts.iter().any(|f| f[1] == e.program.names.clash));
}
