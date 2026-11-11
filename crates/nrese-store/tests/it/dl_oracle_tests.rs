//! The DL-oracle differential (design note `2026-11-08-dl-oracle-differential-design.md`).
//! What a `complete` status under `owl2-ql` or `owl2-rl` claims, since a regime may claim it
//! only where it answers every axiom the query can meet (I24c; master, 8 Nov 10:31): every
//! certain answer owl2-dl gives (`DlAnswers::Exact`). One that claims `sound` too must hold
//! nothing else.
//!
//! The comparison is direct ([`Relation::Direct`], master 12:33). The saturated one
//! ([`Relation::Claim`]) only attributes a miss:
//! - the case is loaded with owl2-dl's facts of its named individuals first;
//! - a miss that remains comes through anonymous individuals (the I24b ledger);
//! - one that goes away is a named individual's (the I24c classification).
//!
//! When owl2-dl can't decide (an error under `Exact`, a fact it can't settle, a deadline),
//! the case has no verdict. That is never agreement.

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
    // Declared data properties: neither classes nor saturated.
    let mut data = BTreeSet::new();
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
            // A member of the case's own class, or a declared entity (generated cases).
            if let (Some(s), Some(o)) = (&si, &oi) {
                let declared = o.strip_prefix(OWL).unwrap_or_default();
                if own(o) || declared == "NamedIndividual" {
                    parsed.individuals.insert(s.clone());
                } else if declared == "ObjectProperty" {
                    parsed.properties.insert(s.clone());
                } else if declared == "DatatypeProperty" {
                    data.insert(s.clone());
                }
            }
        } else if pi == format!("{OWL}onProperty") {
            parsed
                .properties
                .extend(oi.iter().filter(|t| own(t)).cloned());
        }
    }
    parsed.properties.retain(|p| !data.contains(p));
    parsed.classes = named
        .into_iter()
        .filter(|t| {
            !parsed.properties.contains(t) && !parsed.individuals.contains(t) && !data.contains(t)
        })
        .collect();
    parsed
}

/// A run's deadline, and the token that stops owl2-dl's work at it (review G1 r2, F4): a
/// watcher fires it at the deadline, so a DL call begun before it stops there, whatever
/// its own timeout left. The watcher ends with the run.
pub(crate) struct Expiry {
    deadline: std::time::Instant,
    cancel: nrese_store::CancellationToken,
    /// Dropped with the run, which wakes the watcher before the deadline.
    _ended: std::sync::mpsc::Sender<()>,
}

impl Expiry {
    pub(crate) fn at(deadline: std::time::Instant) -> Self {
        let cancel = nrese_store::CancellationToken::new();
        let (ended, run) = std::sync::mpsc::channel::<()>();
        let fire = cancel.clone();
        std::thread::spawn(move || {
            let wait = deadline.saturating_duration_since(std::time::Instant::now());
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = run.recv_timeout(wait) {
                fire.cancel();
            }
        });
        Self {
            deadline,
            cancel,
            _ended: ended,
        }
    }
}

/// owl2-dl's certain answers to `query`, as a set of rows (`true` for an ASK that holds);
/// `Err` with the reason where it can't decide, `expiry` has fired among them.
fn certain(
    dl: &nrese_store::MutationPipeline,
    query: &str,
    expiry: &Expiry,
) -> Result<BTreeSet<String>, String> {
    let store = dl.store();
    let mut request = SparqlQueryRequest::all(query);
    request.dl_answers = Some(DlAnswers::Exact);
    let prepared = store.prepare_query(&request).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    match store.run_query_dl(&prepared, &expiry.cancel, &mut out) {
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

/// What a QL/RL `complete` answer is compared with (design note, decision 1, as the master
/// confirmed it on 8 Nov 12:33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Relation {
    /// Attribution: the named individuals are saturated under owl2-dl first, so only answers
    /// through anonymous individuals can be missed.
    Claim,
    /// The contract: owl2-dl's certain answers directly.
    Direct,
}

/// The test's deadline (review G1 r1, F4): past it, owl2-dl gets no time, a store no query,
/// and a case no verdict.
fn left(deadline: std::time::Instant) -> Option<std::time::Duration> {
    deadline
        .checked_duration_since(std::time::Instant::now())
        .filter(|d| !d.is_zero())
}

/// The facts owl2-dl entails of the case's named individuals:
/// - their types among its classes;
/// - its properties between them;
/// - equality between them.
///
/// `Err` where one can't be decided, or the deadline has passed.
fn saturation(
    dl: &nrese_store::MutationPipeline,
    parsed: &Parsed,
    expiry: &Expiry,
) -> Result<Vec<String>, String> {
    let mut facts = Vec::new();
    let mut ask = |triple: String| -> Result<(), String> {
        left(expiry.deadline).ok_or("the deadline passed")?;
        if !certain(dl, &format!("ASK {{ {triple} }}"), expiry)?.is_empty() {
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
/// `relation`, within `expiry`.
pub(crate) fn verdicts(
    case: &Case,
    relation: Relation,
    expiry: &Expiry,
) -> Vec<(Ruleset, Verdict)> {
    let deadline = expiry.deadline;
    let both = |v: Verdict| vec![(Ruleset::Owl2Ql, v.clone()), (Ruleset::Owl2Rl, v)];
    let Some(time) = left(deadline) else {
        return both(Verdict::Undecided("the deadline passed".to_owned()));
    };
    let parsed = parse(case.turtle);
    // Each owl2-dl task within 5 s, or what is left of the deadline, and the tableau's node
    // and branch budgets (design note, "The generator"): past them it can't decide, never
    // agrees.
    let dl = crate::dl::pipeline_with(nrese_store::DlConfig {
        timeout: Some(time.min(std::time::Duration::from_secs(5))),
        max_nodes: Some(100_000),
        max_branch_points: Some(100_000),
        ..nrese_store::DlConfig::default()
    });
    if let Err(e) = crate::dl::insert(&dl, &parsed.lines.join("\n")) {
        // The commit gate names what it found ("violates … consistency check(s)").
        let text = e.to_string();
        let inconsistent = text.contains("inconsisten") || text.contains("consistency check");
        return both(match inconsistent {
            true => Verdict::Inconsistent,
            false => Verdict::Undecided(format!("load: {e}")),
        });
    }
    let expected = match certain(&dl, case.query, expiry) {
        Ok(rows) => rows,
        Err(e) => return both(Verdict::Undecided(format!("query: {e}"))),
    };
    let facts = match relation {
        Relation::Direct => Vec::new(),
        Relation::Claim => match saturation(&dl, &parsed, expiry) {
            Ok(facts) => facts,
            Err(e) => return both(Verdict::Undecided(format!("saturation: {e}"))),
        },
    };
    let saturated = format!("{}\n{}", parsed.lines.join("\n"), facts.join("\n"));
    let mut out = Vec::new();
    for ruleset in [Ruleset::Owl2Ql, Ruleset::Owl2Rl] {
        if left(deadline).is_none() {
            out.push((
                ruleset,
                Verdict::Undecided("the deadline passed".to_owned()),
            ));
            continue;
        }
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

/// The fixed cases. No QL or RL answer labelled `complete` misses a certain answer, and
/// none claimed `sound` holds an uncertain one, under the direct comparison (the contract)
/// and the saturated one (review G1 r1, F6: the direct verdict was printed, never asserted).
/// Every fixture is decidable, so a fixture without a verdict fails too.
///
/// On engine-v2 `7fa950ef`, each of the I24b, F1–F4 fixtures should be `missed`: the
/// harness's own check (design note, "First campaign").
#[test]
fn the_review_fixtures_answer_as_the_dl_oracle() {
    let expiry = Expiry::at(std::time::Instant::now() + std::time::Duration::from_secs(300));
    let mut wrong = Vec::new();
    for (name, body, query) in review_fixtures() {
        let turtle = format!("{HEAD}{body}");
        let case = Case {
            name,
            turtle: &turtle,
            query,
        };
        let direct = verdicts(&case, Relation::Direct, &expiry);
        for ((ruleset, saturated), (_, verdict)) in verdicts(&case, Relation::Claim, &expiry)
            .into_iter()
            .zip(direct)
        {
            eprintln!(
                "VERDICT {name}, {}: direct {verdict:?}; saturated {saturated:?}",
                ruleset.name()
            );
            for (relation, v) in [("direct", &verdict), ("saturated", &saturated)] {
                if !matches!(v, Verdict::Agree | Verdict::Reported) {
                    wrong.push(format!("{name}, {}, {relation}: {v:?}", ruleset.name()));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// owl2-dl's work stops at the run's deadline, whatever its own timeout leaves (review G1
/// r2, F4: each DL call had a fresh timeout, so one begun before the deadline could run
/// past it). With the run's expiry fired and its deadline still ahead, a decidable fixture
/// gets no verdict under either relation: owl2-dl decides nothing, and nothing agrees.
#[test]
fn a_fired_expiry_stops_owl2_dl() {
    let (name, body, query) = review_fixtures().swap_remove(0);
    let turtle = format!("{HEAD}{body}");
    let case = Case {
        name,
        turtle: &turtle,
        query,
    };
    let expiry = Expiry::at(std::time::Instant::now() + std::time::Duration::from_secs(300));
    expiry.cancel.cancel();
    for relation in [Relation::Direct, Relation::Claim] {
        for (ruleset, verdict) in verdicts(&case, relation, &expiry) {
            assert!(
                matches!(verdict, Verdict::Undecided(_)),
                "{name}, {}: {verdict:?}",
                ruleset.name()
            );
        }
    }
}

/// A number setting of the generated test, from the environment's `value` (review G1 r1,
/// F2: a malformed value fell back to its default, and `NRESE_FUZZ_CASES=0` passed while
/// comparing nothing): `default` where it is unset; an error where it is set but isn't a
/// whole number of at least `min` (overflow included).
fn setting(value: Option<&str>, default: u64, min: u64) -> Result<u64, String> {
    match value {
        None => Ok(default),
        Some(v) => match v.trim().parse::<u64>() {
            Ok(n) if n >= min => Ok(n),
            Ok(n) => Err(format!("{n} is below {min}")),
            Err(e) => Err(format!("{v:?}: {e}")),
        },
    }
}

/// [`setting`] of the variable `name`, failing the test before any work where it is invalid.
fn env_setting(name: &str, default: u64, min: u64) -> u64 {
    let value = match std::env::var(name) {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => panic!("{name}: {e}"),
    };
    setting(value.as_deref(), default, min).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Unset settings take their defaults; zero cases and malformed or overflowing values are
/// refused (review G1 r1, F2).
#[test]
fn generated_test_settings_refuse_zero_and_invalid_values() {
    assert_eq!(setting(None, 100, 1), Ok(100));
    assert_eq!(setting(Some(" 7 "), 100, 1), Ok(7));
    assert_eq!(setting(Some("0"), 0, 0), Ok(0), "seed 0 is a seed");
    assert_eq!(setting(Some("18446744073709551615"), 0, 0), Ok(u64::MAX));
    for refused in ["0", "", "x", "-1", "1.5", "18446744073709551616"] {
        assert!(setting(Some(refused), 100, 1).is_err(), "{refused:?} taken");
    }
}

/// A generated case (`nrese_owl::fuzz::templates::case`, all of SROIQ(D)) as N-Triples,
/// with its family and its queries.
fn generated(seed: u64) -> (String, &'static str, Vec<String>) {
    use nrese_owl::fuzz::{Name, Profile, Rng, Signature, Sizes, templates};
    let mut table = crate::dl::classification::Table::default();
    for (_, iri) in nrese_owl::Vocabulary::iris() {
        table.id(format!("<{iri}>"));
    }
    let sizes = Sizes {
        object_properties: 3,
        simple: 2,
        individuals: 4,
        ..Sizes::default()
    };
    let sig = Signature::new(sizes, &mut |name| match name {
        Name::Iri(iri) => table.id(format!("<{iri}>")),
        Name::Integer(i) => table.id(format!(
            "\"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        )),
    });
    let mut rng = Rng::new(seed);
    // Depth 3 for a share of cases (design note, "Reuse"). The ABox is the case's own
    // (named types and edges): the generator's negative and equality assertions made
    // over a quarter of the first 40 cases inconsistent (`dlorc-g1-measure-442a326d`).
    let profile = Profile {
        axioms: 6,
        depth: 2 + u32::from(rng.one_in(3)),
        abox: false,
        ..Profile::sroiq()
    };
    let case = templates::case(&mut rng, &sig, profile);
    let vocabulary = nrese_owl::Vocabulary::new(&|iri| table.ids.get(&format!("<{iri}>")).copied());
    let triples = nrese_owl::write(&case.ontology, &vocabulary, &mut table);
    let text: String = triples
        .iter()
        .map(|t| t.map(|x| table.text[x as usize].clone()).join(" ") + " .\n")
        .collect();
    let iri = |t: nrese_owl::Term| table.text[t as usize].clone();
    let queries = (0..2 + rng.below(3))
        .map(|i| query(&mut rng, &sig, &case.focus[..], i % 2 == 0, &iri))
        .collect();
    (text, case.family, queries)
}

/// A tree-shaped conjunctive query of one to three atoms over `sig`, its classes and
/// properties from `focus` where `prefer` (and `focus` has one of the kind). Rooted at `?x`
/// (a SELECT) or at an individual (an ASK). Cyclic ones leave owl2-dl undecided
/// (fixed-case run 2), so none is drawn.
fn query(
    rng: &mut nrese_owl::fuzz::Rng,
    sig: &nrese_owl::fuzz::Signature,
    focus: &[nrese_owl::Term],
    prefer: bool,
    iri: &dyn Fn(nrese_owl::Term) -> String,
) -> String {
    let from = |rng: &mut nrese_owl::fuzz::Rng, all: &[nrese_owl::Term]| {
        let preferred: Vec<nrese_owl::Term> =
            focus.iter().copied().filter(|t| all.contains(t)).collect();
        let pool = match prefer && !preferred.is_empty() {
            true => &preferred[..],
            false => all,
        };
        iri(pool[rng.below(pool.len() as u64) as usize])
    };
    let ask = rng.one_in(2);
    let root = match ask {
        true => from(rng, &sig.individuals),
        false => "?x".to_owned(),
    };
    let mut nodes = vec![root];
    let mut atoms = Vec::new();
    for i in 0..1 + rng.below(3) {
        let at = nodes[rng.below(nodes.len() as u64) as usize].clone();
        if rng.one_in(2) {
            atoms.push(format!("{at} a {}", from(rng, &sig.classes)));
        } else {
            let (p, next) = (from(rng, &sig.object_properties), format!("?y{i}"));
            atoms.push(match rng.one_in(3) {
                true => format!("{next} {p} {at}"),
                false => format!("{at} {p} {next}"),
            });
            nodes.push(next);
        }
    }
    let body = atoms.join(" . ");
    match ask {
        true => format!("ASK {{ {body} }}"),
        false => format!("SELECT ?x WHERE {{ {body} }}"),
    }
}

/// The generator's cases (design note, "The generator", confirmed by the master 12:33).
/// Under the direct relation, no QL or RL answer labelled `complete` misses a certain
/// answer of owl2-dl, and none claimed `sound` holds an uncertain one. Each failure names
/// its attribution, the saturated relation's verdict: missed there too, through anonymous
/// individuals (the I24b ledger); agreed there, a named individual's (the I24c
/// classification). Undecided verdicts are at most a fifth, in total and in each family
/// with ten verdicts or more, and at least one verdict is decided (review G1 r1, F3). A case
/// still inconsistent after its redraws was never compared, so it counts as undecided.
/// Every family's counts are printed (`FAMILY` lines), and each failure its replay.
///
/// Settings, each refused before any work where invalid ([`setting`]):
/// - `NRESE_FUZZ_SEED` (default 0) varies the seeds;
/// - `NRESE_FUZZ_CASES` (default 100, at least 1) sets their number;
/// - `NRESE_FUZZ_DEADLINE_S` (default 240) bounds the whole test. Past it, the run stops
///   and is inconclusive (review G1 r1, F4).
#[test]
fn generated_cases_answer_as_the_dl_oracle() {
    let base = env_setting("NRESE_FUZZ_SEED", 0, 0);
    // 100 cases, about 20 s on one test thread (`dlorc-g1-measure5-51a049c1`: 120 cases in
    // 24 s, redraws included).
    let cases = env_setting("NRESE_FUZZ_CASES", 100, 1);
    let deadline_s = env_setting("NRESE_FUZZ_DEADLINE_S", 240, 1);
    let expiry = Expiry::at(std::time::Instant::now() + std::time::Duration::from_secs(deadline_s));
    let deadline = expiry.deadline;
    // Every setting, so a custom deadline replays too (review G1 r2, F5).
    let replay = |i: u64| {
        format!(
            "NRESE_FUZZ_SEED={base} NRESE_FUZZ_CASES={} NRESE_FUZZ_DEADLINE_S={deadline_s} \
             cargo test --locked -p nrese-store --test it -- \
             generated_cases_answer_as_the_dl_oracle --nocapture --test-threads 1",
            i + 1
        )
    };
    let mut counts = Counts::default();
    let mut failures = Vec::new();
    let mut inconclusive = Vec::new();
    let mut redrawn = 0;
    for i in 0..cases {
        if left(deadline).is_none() {
            inconclusive.push(format!("the deadline passed after {i} of {cases} cases"));
            break;
        }
        let Drawn {
            seed,
            tries,
            turtle,
            family,
            queries,
            first,
        } = drawn(i, base, &expiry);
        redrawn += tries;
        let at = format!("base {base}, case {i}, redraw {tries}, seed {seed} ({family})");
        for (n, query) in queries.iter().enumerate() {
            let case = Case {
                name: family,
                turtle: &turtle,
                query,
            };
            let found = match n {
                0 => first.clone(),
                _ => verdicts(&case, Relation::Direct, &expiry),
            };
            for (ruleset, verdict) in found {
                if let Verdict::Undecided(why) = &verdict {
                    let why: String = why.chars().take(120).collect();
                    eprintln!("UNDECIDED {at}, {}: {why}", ruleset.name());
                }
                let class = match verdict {
                    Verdict::Agree => 0,
                    Verdict::Reported => 1,
                    Verdict::Missed(_) => 2,
                    Verdict::Extra(_) => 3,
                    Verdict::Undecided(_) => 4,
                    Verdict::Inconsistent => 5,
                };
                counts.entry(family).or_default()[class] += 1;
                if matches!(class, 2 | 3) {
                    // Attribution within the deadline: past it, `Undecided`.
                    let saturated = verdicts(&case, Relation::Claim, &expiry)
                        .into_iter()
                        .find(|(r, _)| *r == ruleset)
                        .map(|(_, v)| v);
                    failures.push(format!(
                        "{at}, {}: {verdict:?}; saturated: {saturated:?}\n\
                         replay: {}\n{query}\n{turtle}",
                        ruleset.name(),
                        replay(i)
                    ));
                }
            }
        }
    }
    eprintln!("REDRAWN {redrawn} inconsistent cases");
    inconclusive.extend(judge(&counts, deadline));
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(inconclusive.is_empty(), "{}", inconclusive.join("\n"));
}

/// How often a case is drawn again while owl2-dl finds it inconsistent. About one draw in
/// six is (`NRESE_FUZZ_SEED=158`: 19 redraws for 100 cases). With three, a case still
/// inconsistent came about once in fifteen runs, and in a family of a few cases it was over
/// the family's fifth, a run inconclusive on a draw (D16-460); with ten, about once in
/// three million runs, where the family's draws are as often inconsistent.
const REDRAWS: u64 = 10;

/// A case of the generated run as drawn: its seed, how often it was drawn again, its
/// N-Triples, family and queries, and the verdicts of its first query.
struct Drawn {
    seed: u64,
    tries: u64,
    turtle: String,
    family: &'static str,
    queries: Vec<String>,
    first: Vec<(Ruleset, Verdict)>,
}

/// Case `i` of the generated run with seed `base`. An inconsistent case says nothing of
/// `complete` here, so it is drawn again, up to [`REDRAWS`] times, in the family of its
/// first draw: the families keep their share (nearly half the verdicts of
/// `dlorc-g1-measure2-6cc10f32` were inconsistent), and a family owl2-dl finds inconsistent
/// throughout stays counted, over its fifth (D16-460).
fn drawn(i: u64, base: u64, expiry: &Expiry) -> Drawn {
    let (mut k, mut tries) = (0u64, 0u64);
    let mut first_family = None;
    loop {
        let seed = seed(i, base, k);
        k += 1;
        let (turtle, family, queries) = generated(seed);
        if *first_family.get_or_insert(family) != family {
            continue;
        }
        let case = Case {
            name: family,
            turtle: &turtle,
            query: &queries[0],
        };
        let first = verdicts(&case, Relation::Direct, expiry);
        if tries == REDRAWS || !first.iter().any(|(_, v)| *v == Verdict::Inconsistent) {
            return Drawn {
                seed,
                tries,
                turtle,
                family,
                queries,
                first,
            };
        }
        tries += 1;
    }
}

/// The seed of draw `k` of case `i` of the generated run with seed `base`.
fn seed(i: u64, base: u64, k: u64) -> u64 {
    (i ^ base.wrapping_mul(0x9e37_79b9_7f4a_7c15)).wrapping_add(k << 40)
}

/// Verdicts per family, as the generated test counts them: agree, reported, missed,
/// extra, undecided, inconsistent.
type Counts<'a> = std::collections::BTreeMap<&'a str, [usize; 6]>;

/// Why a generated run that ended by `deadline` is inconclusive, printing each family's
/// counts (`FAMILY` lines). Undecided verdicts are at most a fifth, in total and in each
/// family with ten verdicts or more, an inconsistent case never compared among them, and at
/// least one verdict is decided.
fn judge(counts: &Counts, deadline: std::time::Instant) -> Vec<String> {
    let mut inconclusive = Vec::new();
    let undecided = |c: &[usize; 6]| (c[4] + c[5], c.iter().sum::<usize>());
    let mut total = [0usize; 6];
    for (family, c) in counts {
        eprintln!(
            "FAMILY {family}: agree {}, reported {}, missed {}, extra {}, undecided {}, \
             inconsistent {}",
            c[0], c[1], c[2], c[3], c[4], c[5]
        );
        let (u, n) = undecided(c);
        if n >= 10 && 5 * u > n {
            inconclusive.push(format!("{family}: {u} of {n} undecided"));
        }
        for (t, x) in total.iter_mut().zip(c) {
            *t += x;
        }
    }
    let (u, n) = undecided(&total);
    if 5 * u > n {
        inconclusive.push(format!("total: {u} of {n} undecided"));
    }
    if u == n {
        inconclusive.push(format!("no decided verdict of {n}"));
    }
    // Time only moves on: a deadline that passed at any point in the run has passed now.
    if left(deadline).is_none() {
        inconclusive.push("the deadline passed during the run".to_owned());
    }
    inconclusive
}

/// A run whose deadline passed at any point is inconclusive, even where its counts would
/// pass: the case it was in ended undecided for want of time (review G1 r2, F4: a deadline
/// in the last case passed the gate). Here one family of ten verdicts, one of them
/// undecided, under a deadline already past.
#[test]
fn a_run_past_its_deadline_is_inconclusive() {
    let counts: Counts = [("uniform", [9, 0, 0, 0, 1, 0])].into_iter().collect();
    let now = std::time::Instant::now();
    assert!(judge(&counts, now + std::time::Duration::from_secs(60)).is_empty());
    let past = now.checked_sub(std::time::Duration::from_secs(1)).unwrap();
    assert!(!judge(&counts, past).is_empty(), "passed past its deadline");
}

/// - Input: case 95 of the generated run with `NRESE_FUZZ_SEED=158`, inconsistent in
///   its first four draws (D16-460): kept so, it was 8 of its family's 34 verdicts, over
///   the family's fifth, and the run was inconclusive on a draw. Then a family whose every
///   verdict is inconsistent, beside a family that agrees.
/// - Expected: the case drawn again, in the family of its first draw, until its first
///   query's verdicts are compared (neither inconsistent nor undecided). A family owl2-dl
///   finds inconsistent throughout, which redraws in it keep counted, is over its fifth.
#[test]
fn seed_158s_inconsistent_case_is_drawn_again_in_its_family_until_compared() {
    let expiry = Expiry::at(std::time::Instant::now() + std::time::Duration::from_secs(120));
    let case = drawn(95, 158, &expiry);
    let compared = (case.first.iter())
        .all(|(_, v)| !matches!(v, Verdict::Inconsistent | Verdict::Undecided(_)));
    assert!(compared, "{:?} after {} redraws", case.first, case.tries);
    let family = generated(seed(95, 158, 0)).1;
    assert_eq!(case.family, family, "redrawn in another family");
    assert!(case.tries > 0, "its first draw is consistent");
    let later = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let rejected = [
        ("asserted", [0, 0, 0, 0, 0, 10]),
        ("uniform", [90, 0, 0, 0, 0, 0]),
    ];
    let rejected: Counts = rejected.into_iter().collect();
    assert_eq!(judge(&rejected, later), ["asserted: 10 of 10 undecided"]);
}
