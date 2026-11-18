//! A read of a named class's members, `SELECT ?x { ?x a :C }`, whose candidates one model of
//! the premise decides. By *OWL 2 Direct Semantics* §2.5 an ontology entails `C(x)` exactly
//! when every model of it puts `x` in `C`, so one model with `x` outside `C` refutes the
//! candidate, and no model can prove one. Each case holds the read to the answers each
//! candidate's own entailment gives, and to the counts its status tells.

use nrese_store::dl::entailment::Entailed;
use nrese_store::{DlConfig, MutationPipeline, SparqlQueryRequest};

use super::{Counts, PREFIXES, Status, insert, pipeline_with};

/// A functional `val` with values `v0` to `v3`, `Cv_i` the class of what has the value `v_i`,
/// and the unions `U0 = Cv0 ⊔ Cv1`, `U1 = Cv1 ⊔ Cv2`, `U2 = Cv2 ⊔ Cv3`.
const UNIONS: &str = include_str!("model_labels_unions.ttl");

/// [`UNIONS`] with `n` individuals, the `k`-th with the value `v(k mod 4)`.
fn unions(n: usize) -> String {
    let mut text = UNIONS.to_owned();
    for k in 0..n {
        text.push_str(&format!(":i{k} :val :v{} .\n", k % 4));
    }
    text
}

/// The rows of `SELECT ?x { pattern }` (local names, sorted) and their status.
fn read(dl: &MutationPipeline, pattern: &str) -> (Vec<String>, Status) {
    let store = dl.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!(
            "{PREFIXES}SELECT ?x {{ {pattern} }}"
        )))
        .expect("prepare");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out)
        .expect("run")
        .expect("a status under owl2-dl");
    let json: serde_json::Value = serde_json::from_slice(&out).expect("json results");
    let mut rows: Vec<String> = json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| {
            let x = row["x"]["value"].as_str().unwrap_or("-");
            x.trim_start_matches("http://example.com/").to_owned()
        })
        .collect();
    rows.sort();
    (rows, status)
}

fn counts(status: &Status) -> (u64, Option<u64>, u64, u64, u64) {
    let Counts {
        lower,
        upper,
        proved,
        refuted,
        unresolved,
    } = status.bounds.expect("the bounds' counts");
    (lower, upper, proved, refuted, unresolved)
}

fn names(prefix: &str, ks: impl IntoIterator<Item = usize>) -> Vec<String> {
    let mut names: Vec<String> = ks.into_iter().map(|k| format!("{prefix}{k}")).collect();
    names.sort();
    names
}

/// Each of `members` by its own entailment test over `premise`, `C(m)` for the class `class`.
fn each(premise: &str, class: &str, members: &[String]) -> Vec<(String, Entailed)> {
    members
        .iter()
        .map(|m| {
            let conclusion = format!(":{m} a :{class} .");
            (m.clone(), super::entailment::entailed(premise, &conclusion))
        })
        .collect()
}

/// Spec: *Direct Semantics* §2.5. Of the 24 individuals, those with `v0` or `v1` are `U0`s
/// (`val hasValue v_i` makes them `Cv_i`, a disjunct of `U0`). One with `v2` or `v3` is not:
/// nothing makes its value `v0` or `v1`, so a model keeps the values apart and it outside
/// `U0`. Each such candidate's own test says no. The candidates are the upper bound's
/// (the rules read each union as all its disjuncts, and `val` being functional then makes
/// every value one, so every individual is a candidate of every union). The argument for
/// the status: a candidate one finished model of the premise refutes needs no test of its
/// own, so a candidate limit (4 here, the gap 12) meets only those the model leaves; here
/// it leaves none, so the read is `complete`: the 12 `U0`s, 12 refuted, none unresolved.
#[test]
fn one_model_refutes_every_candidate_of_a_union_over_functional_values() {
    let premise = unions(24);
    let dl = pipeline_with(DlConfig {
        max_candidates: Some(4),
        model_labels: true,
        ..DlConfig::default()
    });
    insert(&dl, &premise).expect("a consistent premise is committed");
    let (rows, status) = read(&dl, "?x a :U0");
    let members = names("i", (0..24).filter(|k| k % 4 < 2));
    let gap = names("i", (0..24).filter(|k| k % 4 >= 2));
    let tested = each(&premise, "U0", &gap);
    println!(
        "rows {rows:?}; told complete {}, paths {:?}, reasons {:?}; counts {:?}; the gap's \
         own tests {tested:?}",
        status.is_complete(),
        status.paths,
        status.reasons(),
        counts(&status)
    );
    assert_eq!(rows, members, "the read's rows");
    assert!(tested.iter().all(|(_, e)| *e == Entailed::No), "{tested:?}");
    assert_eq!(
        counts(&status),
        (12, Some(24), 0, 12, 0),
        "lower, upper, proved, refuted, unresolved"
    );
    assert!(status.is_complete(), "{:?}", status.reasons());
}

/// The individuals by cases (the measured hand store): `A ⊔ B` and `N ⊔ B` beside `W`
/// disjoint with `N`, `A` and `B` below `U0` and below `∀s.U0`. Spec: *Direct Semantics*
/// §2.5 with Table 4's `ObjectUnionOf`: a member of `A ⊔ B` is a `U0` in every model, by
/// whichever disjunct holds; so is one of `N ⊔ B` that is a `W` (the `B` branch); so is an
/// `s` successor of a member of `A ⊔ B` (`∀s.U0` either way). A member of `A ⊔ N` is not:
/// a model with `N` keeps it outside `U0`, and its successor too.
const CASES: &str = ":A a owl:Class . :B a owl:Class . :N a owl:Class . :W a owl:Class . \
     :U0 a owl:Class . :s a owl:ObjectProperty . :W owl:disjointWith :N . \
     :A rdfs:subClassOf :U0 , _:all . :B rdfs:subClassOf :U0 , _:all . \
     _:all a owl:Restriction ; owl:onProperty :s ; owl:allValuesFrom :U0 . \
     _:ab a owl:Class ; owl:unionOf ( :A :B ) . _:nb a owl:Class ; owl:unionOf ( :N :B ) . \
     _:an a owl:Class ; owl:unionOf ( :A :N ) . _:na a owl:Class ; owl:unionOf ( :N :A ) . \
     :d1 a :A . :c1 a _:ab . :c2 a _:ab . :c3 a _:ab . :c4 a _:ab . \
     :c5 a _:nb , :W . :c6 a _:nb , :W . \
     :y1 a _:ab ; :s :g1 . :y2 a _:ab ; :s :g2 . :y3 a _:ab ; :s :g3 . \
     :n1 a _:an . :n2 a _:an . :n3 a _:an . :n4 a _:na . :n5 a _:na . :n6 a _:na . \
     :m1 a _:an ; :s :h1 . :m2 a _:na ; :s :h2 .";

/// By equality (the measured hand store): `x_i` has `f` values `p_i` (a `K`, below `U0`) and
/// `q_i`, and is `A ⊔ B` (both at most one `f`) or `A ⊔ N`; `z1` is `{p1}`, `z2` `{p2}`.
/// Spec: *Direct Semantics* §2.5 with Table 4's `ObjectMaxCardinality` and `ObjectOneOf`: by
/// cases `q_i = p_i` where `x_i` is `A ⊔ B`, so `q_i` is a `U0`; `z_i = p_i` by the nominal. A
/// model with `N` for `x4` to `x6` keeps `q_i` apart from `p_i`, outside `U0`.
const EQUAL: &str = ":A a owl:Class . :B a owl:Class . :N a owl:Class . :K a owl:Class . \
     :U0 a owl:Class . :f a owl:ObjectProperty . :K rdfs:subClassOf :U0 . \
     _:one a owl:Restriction ; owl:onProperty :f ; owl:maxCardinality \"1\"^^xsd:nonNegativeInteger . \
     :A rdfs:subClassOf _:one . :B rdfs:subClassOf _:one . \
     _:ab a owl:Class ; owl:unionOf ( :A :B ) . _:an a owl:Class ; owl:unionOf ( :A :N ) . \
     _:na a owl:Class ; owl:unionOf ( :N :A ) . \
     :x1 a _:ab ; :f :p1 , :q1 . :x2 a _:ab ; :f :p2 , :q2 . :x3 a _:ab ; :f :p3 , :q3 . \
     :x4 a _:an ; :f :p4 , :q4 . :x5 a _:an ; :f :p5 , :q5 . :x6 a _:na ; :f :p6 , :q6 . \
     :p1 a :K . :p2 a :K . :p3 a :K . :p4 a :K . :p5 a :K . :p6 a :K . \
     :z1 a [ a owl:Class ; owl:oneOf ( :p1 ) ] . :z2 a [ a owl:Class ; owl:oneOf ( :p2 ) ] .";

/// Commits `premise` and holds the read of `U0` to `members` (the entailed, sorted), each of
/// `gap` to its own test (`Yes` for a member), `complete`, with `counts`.
fn keeps(
    premise: &str,
    members: &[&str],
    gap: &[&str],
    expected: (u64, Option<u64>, u64, u64, u64),
) {
    let dl = pipeline_with(DlConfig {
        model_labels: true,
        ..DlConfig::default()
    });
    insert(&dl, premise).expect("a consistent premise is committed");
    let (rows, status) = read(&dl, "?x a :U0");
    let gap: Vec<String> = gap.iter().map(|m| (*m).to_owned()).collect();
    let tested = each(premise, "U0", &gap);
    println!(
        "rows {rows:?}; told complete {}, paths {:?}, reasons {:?}; counts {:?}; the gap's \
         own tests {tested:?}",
        status.is_complete(),
        status.paths,
        status.reasons(),
        counts(&status)
    );
    assert_eq!(rows, members, "the read's rows");
    for (m, answer) in &tested {
        let entailed = members.contains(&m.as_str());
        let expected = if entailed {
            Entailed::Yes
        } else {
            Entailed::No
        };
        assert_eq!(*answer, expected, "{m} a U0, by its own test");
    }
    assert_eq!(
        counts(&status),
        expected,
        "lower, upper, proved, refuted, unresolved"
    );
    assert!(status.is_complete(), "{:?}", status.reasons());
}

/// Spec: [`CASES`]. Every true answer is returned and the read `complete`: `d1` (the rules'
/// lower bound) and the 12 by cases; the 10 others refuted.
#[test]
fn every_answer_by_cases_is_kept_and_told_complete() {
    let members = [
        "c1", "c2", "c3", "c4", "c5", "c6", "d1", "g1", "g2", "g3", "y1", "y2", "y3",
    ];
    let gap = [
        "c1", "c2", "c3", "c4", "c5", "c6", "g1", "g2", "g3", "y1", "y2", "y3", "h1", "h2", "m1",
        "m2", "n1", "n2", "n3", "n4", "n5", "n6",
    ];
    keeps(CASES, &members, &gap, (1, Some(23), 12, 10, 0));
}

/// Spec: [`EQUAL`]. Every true answer is returned and the read `complete`: the six `K`s, `q1`
/// to `q3` merged by cases and `z1`, `z2` by their nominals; `q4` to `q6` refuted.
#[test]
fn every_answer_by_equality_is_kept_and_told_complete() {
    let members = [
        "p1", "p2", "p3", "p4", "p5", "p6", "q1", "q2", "q3", "z1", "z2",
    ];
    let gap = ["q1", "q2", "q3", "q4", "q5", "q6", "z1", "z2"];
    keeps(EQUAL, &members, &gap, (6, Some(14), 5, 3, 0));
}

/// Spec: *Structural Specification* §9.5 ("m or n (or both) MUST be larger than zero"): a key
/// with no property is outside OWL 2 DL, and the reader leaves it out. A model of what it
/// kept is no model of the premise, so it refutes nothing: the candidates of [`unions`] stay
/// with their own tests, which can't say no either (the left-out axiom might entail them):
/// none refuted, all 12 unresolved, the read not `complete`.
#[test]
fn a_premise_the_reader_left_part_of_refutes_no_candidate_by_its_model() {
    let premise = format!("{} :U0 owl:hasKey () .", unions(24));
    let dl = pipeline_with(DlConfig {
        model_labels: true,
        ..DlConfig::default()
    });
    insert(&dl, &premise).expect("the premise is committed");
    let (rows, status) = read(&dl, "?x a :U0");
    println!(
        "rows {rows:?}; told complete {}, reasons {:?}; counts {:?}",
        status.is_complete(),
        status.reasons(),
        counts(&status)
    );
    assert_eq!(
        rows,
        names("i", (0..24).filter(|k| k % 4 < 2)),
        "the read's rows"
    );
    assert_eq!(
        counts(&status),
        (12, Some(24), 0, 0, 12),
        "lower, upper, proved, refuted, unresolved"
    );
    assert!(!status.is_complete());
}

/// Spec: the argument of the module: a model's label speaks for one class assertion. A
/// candidate of `?x a :U0 . ?x a :U1` is two, so no label decides it, and the candidate limit
/// meets the gap as it is: of the 24, the 6 with `v1` are both (the lower bound), 4 of the
/// 18 others are tested (each refuted: a model with `x`'s value apart from `v1` keeps it out
/// of one of the two) and 14 are unresolved, the read not `complete`.
#[test]
fn a_candidate_of_two_class_atoms_is_left_to_its_tests() {
    let dl = pipeline_with(DlConfig {
        max_candidates: Some(4),
        model_labels: true,
        ..DlConfig::default()
    });
    insert(&dl, &unions(24)).expect("a consistent premise is committed");
    let (rows, status) = read(&dl, "?x a :U0 . ?x a :U1");
    println!(
        "rows {rows:?}; told complete {}, reasons {:?}; counts {:?}",
        status.is_complete(),
        status.reasons(),
        counts(&status)
    );
    assert_eq!(
        rows,
        names("i", (0..24).filter(|k| k % 4 == 1)),
        "the read's rows"
    );
    assert_eq!(
        counts(&status),
        (6, Some(24), 0, 4, 14),
        "lower, upper, proved, refuted, unresolved"
    );
    assert!(!status.is_complete());
}
