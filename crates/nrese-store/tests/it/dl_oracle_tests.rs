//! The DL-oracle differential (design note `2026-11-08-dl-oracle-differential-design.md`,
//! decision 1 (a)). What a `complete` status under `owl2-ql` or `owl2-rl` claims
//! (`nrese-sparql`'s `Regime`): every answer the closure's rules and the QL rewriting's
//! existentials entail. The rewriting reports not `complete` where an axiom it doesn't follow
//! meets anonymous individuals.
//!
//! The test checks exactly that claim, in four steps:
//! 1. The owl2-dl mode saturates the named individuals: their entailed types and the property
//!    assertions between them.
//! 2. Those facts are loaded with the case into a QL store and an RL store, rewriting on.
//! 3. Each store answers the query. The named facts are DL-complete, so any certain answer
//!    still missing comes through anonymous individuals.
//! 4. A `complete` answer must contain owl2-dl's certain answers (`DlAnswers::Exact`). One that
//!    claims `sound` too must hold nothing else.
//!
//! When owl2-dl can't decide (an error under `Exact`, a fact it can't settle), the case has no
//! verdict. That is never agreement.

use std::collections::BTreeSet;

use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::rulesets::Ruleset;
use nrese_store::{DlAnswers, SparqlQueryRequest, StoreService};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const VOCABULARY: [&str; 4] = [
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://www.w3.org/2000/01/rdf-schema#",
    OWL,
    "http://www.w3.org/2001/XMLSchema#",
];

/// One case: a Turtle document (ontology and data) and a query over it.
pub(crate) struct Case<'a> {
    pub name: &'a str,
    pub turtle: &'a str,
    pub query: &'a str,
}

/// What the differential found for one case under one regime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// `complete`, with every certain answer (and nothing else where `sound` is claimed).
    Agree,
    /// No `complete` claim: allowed, and counted as the precision cost.
    Reported,
    /// `complete`, with these certain answers missing: a silent incompleteness.
    Missed(Vec<String>),
    /// `sound`, with these answers that aren't certain: a wrong answer.
    Extra(Vec<String>),
    /// owl2-dl couldn't decide, for this reason: no verdict, never agreement.
    Undecided(String),
    /// owl2-dl finds the case inconsistent.
    Inconsistent,
}

/// The terms of a Turtle document as N-Triples lines, with the case's signature: its IRIs
/// outside the RDF, RDFS, OWL and XSD vocabularies, sorted into classes, properties and
/// individuals by where they stand.
struct Parsed {
    lines: Vec<String>,
    classes: BTreeSet<String>,
    properties: BTreeSet<String>,
    individuals: BTreeSet<String>,
}

fn parse(turtle: &str) -> Parsed {
    let own = |iri: &str| !VOCABULARY.iter().any(|ns| iri.starts_with(ns));
    let iri = |t: &str| {
        t.strip_prefix('<')
            .and_then(|t| t.strip_suffix('>'))
            .map(str::to_owned)
    };
    let mut parsed = Parsed {
        lines: Vec::new(),
        classes: BTreeSet::new(),
        properties: BTreeSet::new(),
        individuals: BTreeSet::new(),
    };
    let mut named = BTreeSet::new();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(turtle.as_bytes()) {
        let quad = quad.expect("a case parses");
        let (s, p, o) = (
            quad.subject.to_string(),
            quad.predicate.to_string(),
            quad.object.to_string(),
        );
        parsed.lines.push(format!("{s} {p} {o} ."));
        let (si, pi, oi) = (iri(&s), iri(&p).unwrap_or_default(), iri(&o));
        named.extend(si.iter().chain(&oi).filter(|t| own(t)).cloned());
        if own(&pi) {
            // An edge of the case's own property: its ends are individuals.
            parsed.properties.insert(pi.clone());
            parsed.individuals.extend(si.iter().chain(&oi).cloned());
        } else if pi == RDF_TYPE {
            if let (Some(s), Some(o)) = (&si, &oi)
                && own(o)
            {
                parsed.individuals.insert(s.clone());
            }
        } else if pi == format!("{OWL}onProperty") {
            parsed
                .properties
                .extend(oi.iter().filter(|t| own(t)).cloned());
        }
    }
    parsed.classes = named
        .into_iter()
        .filter(|t| !parsed.properties.contains(t) && !parsed.individuals.contains(t))
        .collect();
    parsed
}

/// owl2-dl's certain answers to `query`, as a set of rows (`true` for an ASK that holds);
/// `Err` with the reason where it can't decide.
fn certain(dl: &nrese_store::MutationPipeline, query: &str) -> Result<BTreeSet<String>, String> {
    let store = dl.store();
    let mut request = SparqlQueryRequest::all(query);
    request.dl_answers = Some(DlAnswers::Exact);
    let prepared = store.prepare_query(&request).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    match crate::dl::run_dl(store, &prepared, &mut out) {
        Ok(_) => Ok(rows(&out)),
        Err(e) => Err(e.to_string()),
    }
}

/// A store's answers as rows: `true` for an ASK that holds, nothing for one that doesn't,
/// and per solution its bindings, sorted by variable.
fn rows(json: &[u8]) -> BTreeSet<String> {
    let json: serde_json::Value = serde_json::from_slice(json).expect("json results");
    if let Some(b) = json.get("boolean") {
        return match b.as_bool() == Some(true) {
            true => BTreeSet::from(["true".to_owned()]),
            false => BTreeSet::new(),
        };
    }
    json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| {
            let mut cells: Vec<String> = row
                .as_object()
                .expect("a row")
                .iter()
                .map(|(var, value)| format!("{var}={}", value["value"]))
                .collect();
            cells.sort();
            cells.join(" ")
        })
        .collect()
}

/// What a QL/RL `complete` answer is compared with (design note, decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Relation {
    /// (a), the contract: the named individuals are saturated under owl2-dl first, so only
    /// answers through anonymous individuals can be missed.
    Claim,
    /// (b), a diagnostic: owl2-dl's certain answers directly. That also counts the RL
    /// closure's unclaimed gaps for named individuals.
    Direct,
}

/// The facts owl2-dl entails of the case's named individuals:
/// - their types among its classes;
/// - its properties between them;
/// - equality between them.
///
/// `Err` where one can't be decided.
fn saturation(dl: &nrese_store::MutationPipeline, parsed: &Parsed) -> Result<Vec<String>, String> {
    let mut facts = Vec::new();
    let mut ask = |triple: String| -> Result<(), String> {
        if !certain(dl, &format!("ASK {{ {triple} }}"))?.is_empty() {
            facts.push(format!("{triple} ."));
        }
        Ok(())
    };
    for a in &parsed.individuals {
        for c in &parsed.classes {
            ask(format!("<{a}> <{RDF_TYPE}> <{c}>"))?;
        }
        for p in &parsed.properties {
            for b in &parsed.individuals {
                ask(format!("<{a}> <{p}> <{b}>"))?;
            }
        }
        for b in parsed.individuals.iter().filter(|b| *b > a) {
            ask(format!("<{a}> <{OWL}sameAs> <{b}>"))?;
        }
    }
    Ok(facts)
}

/// The verdict for `case` under each of owl2-ql and owl2-rl, rewriting on, compared by
/// `relation`.
pub(crate) fn verdicts(case: &Case, relation: Relation) -> Vec<(Ruleset, Verdict)> {
    let both = |v: Verdict| vec![(Ruleset::Owl2Ql, v.clone()), (Ruleset::Owl2Rl, v)];
    let parsed = parse(case.turtle);
    let dl = crate::dl::pipeline();
    if let Err(e) = crate::dl::insert(&dl, &parsed.lines.join("\n")) {
        return both(match e.to_string().contains("inconsisten") {
            true => Verdict::Inconsistent,
            false => Verdict::Undecided(format!("load: {e}")),
        });
    }
    let expected = match certain(&dl, case.query) {
        Ok(rows) => rows,
        Err(e) => return both(Verdict::Undecided(format!("query: {e}"))),
    };
    let facts = match relation {
        Relation::Direct => Vec::new(),
        Relation::Claim => match saturation(&dl, &parsed) {
            Ok(facts) => facts,
            Err(e) => return both(Verdict::Undecided(format!("saturation: {e}"))),
        },
    };
    let saturated = format!("{}\n{}", parsed.lines.join("\n"), facts.join("\n"));
    let mut out = Vec::new();
    for ruleset in [Ruleset::Owl2Ql, Ruleset::Owl2Rl] {
        let dir = tempfile::tempdir().unwrap();
        let store: StoreService =
            crate::ql_rewriting_tests::store(&saturated, dir.path(), ruleset, true);
        let result = store
            .execute_query(&SparqlQueryRequest::all(case.query))
            .unwrap_or_else(|e| panic!("{}, {}: {e}", case.name, ruleset.name()));
        let got = rows(&result.payload);
        // No status: nothing could leave answers out, which claims both. A withheld
        // certification claims neither.
        let (complete, sound) = result.completeness.as_ref().map_or((true, true), |c| {
            let claimed = !c.certification_withheld;
            (claimed && c.complete, claimed && c.sound)
        });
        let missing: Vec<String> = expected.difference(&got).cloned().collect();
        let extra: Vec<String> = got.difference(&expected).cloned().collect();
        let verdict = if sound && !extra.is_empty() {
            Verdict::Extra(extra)
        } else if !complete {
            Verdict::Reported
        } else if !missing.is_empty() {
            Verdict::Missed(missing)
        } else {
            Verdict::Agree
        };
        out.push((ruleset, verdict));
    }
    out
}

const HEAD: &str = "@prefix : <http://e/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
    @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
    @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n";

/// The review fixtures of I24b (review r1 F1, F2; fix checks F3, F4), each with an
/// entailed answer under the OWL 2 Direct Semantics that the rewriting missed on
/// engine-v2 `7fa950ef`. Kept as the differential's fixed cases (design note, "First
/// campaign").
fn review_fixtures() -> Vec<(&'static str, String, &'static str)> {
    let some = "[ a owl:Restriction ; owl:onProperty :S ; owl:someValuesFrom :D ]";
    let none = "[ a owl:Restriction ; owl:onProperty :S ; owl:onClass :D ;
                  owl:maxQualifiedCardinality \"0\"^^xsd:nonNegativeInteger ]";
    let not_some = format!("[ a owl:Class ; owl:complementOf {some} ]");
    let x_succ = "ASK { <http://e/x> <http://e/S> ?y . ?y a <http://e/D> }";
    let c_succ = "ASK { <http://e/c> <http://e/S> ?y . ?y a <http://e/D> }";
    vec![
        (
            "I24b, C ⊑ ∀R.∃S.D",
            format!(
                ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :R ; owl:allValuesFrom {some} ] .
                 :c a :C ; :R :x ."
            ),
            x_succ,
        ),
        (
            "F1, A ⊓ B ⊑ ∃S.D",
            format!(
                "[ owl:intersectionOf ( :A :B ) ] rdfs:subClassOf {some} .
                 :c a :A, :B ."
            ),
            c_succ,
        ),
        (
            "F1, A ⊓ B ⊑ ∀R.∃S.D",
            format!(
                "[ owl:intersectionOf ( :A :B ) ] rdfs:subClassOf
                     [ a owl:Restriction ; owl:onProperty :R ; owl:allValuesFrom {some} ] .
                 :c a :A, :B ; :R :x ."
            ),
            x_succ,
        ),
        (
            "F2, DisjointUnion(C, ∃R.A, ∃R.B)",
            ":C a owl:Class ; owl:disjointUnionOf (
                 [ a owl:Restriction ; owl:onProperty :R ; owl:someValuesFrom :A ]
                 [ a owl:Restriction ; owl:onProperty :R ; owl:someValuesFrom :B ] ) .
             :c a :C ."
                .to_owned(),
            "ASK { <http://e/c> <http://e/R> ?y }",
        ),
        (
            "F3, DisjointUnion(C, A, ≤0 S.D)",
            format!(":C a owl:Class ; owl:disjointUnionOf ( :A {none} ) . :c a :A ."),
            c_succ,
        ),
        (
            "F3, A disjointWith ≤0 S.D",
            format!(":A owl:disjointWith {none} . :c a :A ."),
            c_succ,
        ),
        (
            "F4, C ⊑ ¬≥1 R.¬∃S.D",
            format!(
                ":C rdfs:subClassOf [ a owl:Class ; owl:complementOf
                     [ a owl:Restriction ; owl:onProperty :R ;
                       owl:minQualifiedCardinality \"1\"^^xsd:nonNegativeInteger ;
                       owl:onClass {not_some} ] ] .
                 :c a :C ; :R :x ."
            ),
            x_succ,
        ),
        (
            "F4, C ⊑ =0 R.¬∃S.D",
            format!(
                ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :R ;
                     owl:qualifiedCardinality \"0\"^^xsd:nonNegativeInteger ;
                     owl:onClass {not_some} ] .
                 :c a :C ; :R :x ."
            ),
            x_succ,
        ),
        // Two shapes that force a fact on an existing successor, no existential (the I24b
        // second lead, master 10:22). Each comes with a named successor and with an
        // anonymous one. Under (a) the named successor's forced fact is in the
        // saturation, so only the anonymous form tests the claim; (b) shows both.
        (
            "Self, named successor: C ⊑ ∀R.Self(S)",
            ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :R ; owl:allValuesFrom
                 [ a owl:Restriction ; owl:onProperty :S ; owl:hasSelf true ] ] .
             :c a :C ; :R :x ."
                .to_owned(),
            "ASK { <http://e/x> <http://e/S> <http://e/x> }",
        ),
        (
            "Self, anonymous successor: C ⊑ ∃R.⊤ ⊓ ∀R.Self(S)",
            // As a tree query: `?y :S ?y` is a cycle owl2-dl's exact path leaves undecided
            // (fixed-case run 2), so a class `E ≡ ∃S.Self` names the loop.
            ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :R ; owl:someValuesFrom owl:Thing ] ,
                 [ a owl:Restriction ; owl:onProperty :R ; owl:allValuesFrom
                   [ a owl:Restriction ; owl:onProperty :S ; owl:hasSelf true ] ] .
             [ a owl:Restriction ; owl:onProperty :S ; owl:hasSelf true ] rdfs:subClassOf :E .
             :c a :C ."
                .to_owned(),
            "ASK { <http://e/c> <http://e/R> ?y . ?y a <http://e/E> }",
        ),
        (
            "Nominal, named successor: C ⊑ ∀R.{a}",
            ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :R ;
                 owl:allValuesFrom [ a owl:Class ; owl:oneOf ( :a ) ] ] .
             :c a :C ; :R :x . :a a :D ."
                .to_owned(),
            "ASK { <http://e/x> a <http://e/D> }",
        ),
        (
            "Nominal, anonymous successor: C ⊑ ∃R.⊤ ⊓ ∀R.{a}",
            ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :R ; owl:someValuesFrom owl:Thing ] ,
                 [ a owl:Restriction ; owl:onProperty :R ;
                   owl:allValuesFrom [ a owl:Class ; owl:oneOf ( :a ) ] ] .
             :c a :C . :a a :D ."
                .to_owned(),
            "ASK { <http://e/c> <http://e/R> ?y . ?y a <http://e/D> }",
        ),
    ]
}

/// The fixed cases. Under the contract (decision 1 (a)), no QL or RL answer labelled
/// `complete` misses a certain answer, and none claimed `sound` holds an uncertain one.
/// Every fixture is decidable, so a fixture without a verdict fails too. The direct
/// comparison (b) is printed beside each one, never asserted.
///
/// On engine-v2 `7fa950ef`, each of the I24b, F1–F4 fixtures should be `missed`: the
/// harness's own check (design note, "First campaign").
#[test]
fn the_review_fixtures_answer_as_the_dl_oracle() {
    let mut wrong = Vec::new();
    for (name, body, query) in review_fixtures() {
        let turtle = format!("{HEAD}{body}");
        let case = Case {
            name,
            turtle: &turtle,
            query,
        };
        let direct = verdicts(&case, Relation::Direct);
        for ((ruleset, verdict), (_, plain)) in
            verdicts(&case, Relation::Claim).into_iter().zip(direct)
        {
            eprintln!(
                "VERDICT {name}, {}: (a) {verdict:?}; (b) {plain:?}",
                ruleset.name()
            );
            if !matches!(verdict, Verdict::Agree | Verdict::Reported) {
                wrong.push(format!("{name}, {}: {verdict:?}", ruleset.name()));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
