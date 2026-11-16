//! An axiom the ontology reader leaves out (`nrese_owl::Diagnostic`'s `Reserved`, `NotOwl`,
//! `Malformed`, `BrokenList`, `Unsupported`) whose triples the OWL 2 RL rules may still read:
//! the closure L may then hold what OWL 2 DL doesn't entail over the ontology the reader
//! reads (docs/spec/reasoning-semantics.md, owl2-dl), so no read over it is `complete` or
//! `sound-only`. Every case's rows are expected by hand: the axiom is outside the mapping's
//! image (*OWL 2 Mapping to RDF Graphs* §3) or uses the reserved vocabulary (*Structural
//! Specification* §2.4), and its read is not entailed by what is read.

use nrese_store::{CancellationToken, SparqlQueryRequest, StatementOp};

use super::queries::query;
use super::versions::{in_transaction, ntriples};
use super::{PREFIXES, insert, pipeline, update};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// Outside OWL 2 RL beside each case, so it is read off the rules route too.
const OUTSIDE_RL: &str = ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :tib a :Pet .";

const NAMES: &str = ":C a owl:Class . :D a owl:Class . :x a owl:NamedIndividual . \
                     :v a owl:NamedIndividual .";

/// Whether a reason says the reader left an axiom out whose statements the rules may still
/// read: the mark's, not the consistency verdict's, which says only that it isn't known.
fn says_left_out(reasons: &[String]) -> bool {
    reasons
        .iter()
        .any(|r| r.contains("left out") && r.contains("may still read"))
}

/// (case, data, read): every read not entailed, the last case's answer `false` (a broken
/// list no rule reads), each told `unsound` all the same: the mark asks whether the reader
/// left anything out, not whether a rule read it.
fn cases() -> Vec<(&'static str, String, &'static str)> {
    let value = format!("<{RDF}value>");
    let (first, rest, nil) = (
        format!("<{RDF}first>"),
        format!("<{RDF}rest>"),
        format!("<{RDF}nil>"),
    );
    let q = ":q a owl:ObjectProperty .";
    let p = ":p a owl:ObjectProperty .";
    vec![
        (
            "member-domain",
            "rdfs:member rdfs:domain :C . :x rdfs:member :v .".into(),
            "ASK { :x a :C }",
        ),
        (
            "subpropertyof-domain",
            format!("rdfs:subPropertyOf rdfs:domain :C . :p rdfs:subPropertyOf :q . {p} {q}"),
            "ASK { :p a :C }",
        ),
        (
            "value-domain",
            format!("{value} rdfs:domain :C . :x {value} :v ."),
            "ASK { :x a :C }",
        ),
        (
            "label-equivalent-literal",
            "rdfs:label owl:equivalentProperty :d . :x rdfs:label 2 .".into(),
            "ASK { :x :d 2 }",
        ),
        (
            "value-equivalent-literal",
            format!("{value} owl:equivalentProperty :d . :x {value} 2 ."),
            "ASK { :x :d 2 }",
        ),
        (
            "label-equivalent-iri",
            format!("rdfs:label owl:equivalentProperty :p . :x rdfs:label :v . {p}"),
            "ASK { :x :p :v }",
        ),
        (
            "value-equivalent-iri",
            format!("{value} owl:equivalentProperty :p . :x {value} :v . {p}"),
            "ASK { :x :p :v }",
        ),
        (
            "class-resource",
            "rdfs:Resource rdfs:subClassOf :C . :x a rdfs:Resource .".into(),
            "ASK { :x a :C }",
        ),
        (
            "notowl-iri-restriction",
            format!(
                ":R owl:onProperty :q ; owl:someValuesFrom :D . :R rdfs:subClassOf :C . :x :q :v . :v a :D . {q}"
            ),
            "ASK { :x a :C }",
        ),
        (
            "malformed-two-fillers",
            format!(
                "[ owl:onProperty :q ; owl:someValuesFrom :D, :F ] rdfs:subClassOf :C . :F a owl:Class . :x :q :v . :v a :D . {q}"
            ),
            "ASK { :x a :C }",
        ),
        (
            "malformed-two-properties",
            format!(
                "[ owl:onProperty :q, :p ; owl:someValuesFrom :D ] rdfs:subClassOf :C . :x :q :v . :v a :D . {q} {p}"
            ),
            "ASK { :x a :C }",
        ),
        (
            "broken-list-two-firsts",
            format!(
                ":A a owl:Class ; owl:intersectionOf _:l . _:l {first} :C, :D ; {rest} {nil} . :x a :A ."
            ),
            "ASK { :x a :D }",
        ),
        (
            "broken-list-no-nil",
            format!(
                ":A a owl:Class ; owl:intersectionOf _:l . _:l {first} :D ; {rest} :tail . :x a :A ."
            ),
            "ASK { :x a :D }",
        ),
    ]
}

/// Each case on the RL route and off it: told `unsound`, a reason saying what the reader
/// left out; the control (a kept domain) `true` and `complete`.
#[test]
fn every_read_over_an_axiom_the_reader_leaves_out_is_unsound() {
    let cases = cases();
    let mut wrong = Vec::new();
    for (name, data, read) in &cases {
        for extra in ["", OUTSIDE_RL] {
            let dl = pipeline();
            insert(&dl, &format!("{data} {NAMES} {extra}")).expect("committed");
            let (rows, status) = query(&dl, read);
            let (label, reasons) = (status.shared.as_str(), status.reasons());
            if !(label == "unsound" && says_left_out(&reasons)) {
                wrong.push(format!("{name} {extra}: {rows:?} {label} {reasons:?}"));
            }
        }
    }
    for extra in ["", OUTSIDE_RL] {
        let dl = pipeline();
        let control = ":q a owl:ObjectProperty ; rdfs:domain :C . :x :q :v .";
        insert(&dl, &format!("{control} {NAMES} {extra}")).expect("committed");
        let (rows, status) = query(&dl, "ASK { :x a :C }");
        if rows != ["true"] || !status.is_complete() {
            wrong.push(format!("control {extra}: {rows:?} {:?}", status.reasons()));
        }
    }
    assert!(wrong.is_empty(), "{} reads: {wrong:#?}", wrong.len());
    assert_eq!(cases.len(), 13, "the table's rows");
}

/// The store's consistency verdict over each case: `unknown`, never `consistent`, though U1,
/// built from what the reader kept, may prove that part consistent: consistent only if
/// nothing of the source was left out (`dl::consistency::check`).
#[test]
fn a_store_with_a_left_out_axiom_is_not_told_consistent() {
    let mut wrong = Vec::new();
    for (name, data, _) in cases() {
        for extra in ["", OUTSIDE_RL] {
            let dl = pipeline();
            insert(&dl, &format!("{data} {NAMES} {extra}")).expect("committed");
            let verdict = dl.store().dl().status().map(|s| s.consistency.verdict);
            if verdict.as_ref().map(|v| v.as_str()) != Some("unknown") {
                wrong.push(format!("{name} {extra}: {verdict:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The other paths that report a status read the same record: EXPLAIN, the plan, a read of
/// the inferred statements alone, the store's status of them (`status_without_dl`), and a
/// read with an addition pending.
#[test]
fn every_path_that_reports_a_status_tells_the_left_out_axiom() {
    let dl = pipeline();
    insert(
        &dl,
        &format!("<{RDF}value> rdfs:domain :C . :x <{RDF}value> :v . {NAMES}"),
    )
    .expect("committed");
    let store = dl.store();
    let prepare = |q: &str| {
        store
            .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{q}")))
            .expect("prepared")
    };
    let q = "SELECT ?x { ?x a :C }";
    let implicit = "SELECT ?x FROM <http://www.ontotext.com/implicit> { ?x a :C }";
    let token = CancellationToken::new();
    let add = StatementOp::Add {
        data: ntriples("<http://example.com/y> <http://example.com/r> <http://example.com/w> ."),
        contexts: Vec::new(),
    };
    let told = [
        (
            "explain",
            store
                .explain_query(&prepare(q), &token)
                .expect("explained")
                .completeness,
        ),
        (
            "plan",
            store
                .plan_query(&prepare(q), &token)
                .expect("planned")
                .completeness,
        ),
        (
            "status without dl",
            store.status_without_dl(&prepare(implicit)),
        ),
        ("pending", in_transaction(&dl, vec![add], q).1),
    ];
    let mut wrong = Vec::new();
    for (path, status) in told {
        let left_out = status.as_ref().is_some_and(|s| {
            let reasons: Vec<String> = s.reasons.iter().map(|r| r.text.clone()).collect();
            s.as_str() == "unsound" && says_left_out(&reasons)
        });
        if !left_out {
            wrong.push(format!("{path}: {status:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A pending addition of such an axiom over a store without one: the read inside the
/// transaction is told from its base, and its own additions aren't reasoned over before the
/// commit, which the transaction's reason says: `sound-only`, not `unsound`, a limit of a
/// read in a transaction (the commit's gate judges the addition). Committed, the store's
/// reads are `unsound`.
#[test]
fn a_pending_addition_of_a_left_out_axiom_is_told_at_its_commit() {
    let dl = pipeline();
    insert(&dl, NAMES).expect("committed");
    let axiom = format!("<{RDF}value> rdfs:domain :C . :x <{RDF}value> :v .");
    let add = StatementOp::Add {
        data: ntriples(&format!(
            "<{RDF}value> <http://www.w3.org/2000/01/rdf-schema#domain> <http://example.com/C> .\n\
             <http://example.com/x> <{RDF}value> <http://example.com/v> ."
        )),
        contexts: Vec::new(),
    };
    let (rows, status) = in_transaction(&dl, vec![add], "SELECT ?x { ?x a :C }");
    let status = status.expect("a status");
    let reasons: Vec<String> = status.reasons.iter().map(|r| r.text.clone()).collect();
    assert!(
        status.as_str() == "sound-only" && !says_left_out(&reasons),
        "pending: {rows} {status:?}"
    );
    insert(&dl, &axiom).expect("committed");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :C }");
    let (label, reasons) = (status.shared.as_str(), status.reasons());
    assert!(
        label == "unsound" && says_left_out(&reasons),
        "committed: {rows:?} {label} {reasons:?}"
    );
}

/// Without the consistency check (`dl.consistency = off`) no verdict says what the reader
/// left out: the mark reads the ontology itself, and the read is `unsound` all the same.
#[test]
fn without_the_consistency_check_a_left_out_axiom_is_still_told() {
    let dl = super::pipeline_with(nrese_store::DlConfig {
        consistency: nrese_store::DlConsistency::Off,
        ..nrese_store::DlConfig::default()
    });
    insert(
        &dl,
        &format!("<{RDF}value> rdfs:domain :C . :x <{RDF}value> :v . {NAMES}"),
    )
    .expect("committed");
    let (rows, status) = query(&dl, "ASK { :x a :C }");
    let (label, reasons) = (status.shared.as_str(), status.reasons());
    assert!(
        label == "unsound" && says_left_out(&reasons),
        "{rows:?} {label} {reasons:?}"
    );
}

/// What a commit makes or ends is what the next read is told: each sequence read after
/// every commit, expected `complete` before the axiom the reader leaves out, `unsound`
/// with its reason while it is there (also after a commit of a plain assertion beside it,
/// off the rules route a delta of U1, which then may not prove the store consistent), and
/// `complete` again once it is gone. The changes
/// are a reserved name first used, a list's last `rdf:rest` deleted, a restriction's filler
/// deleted, an `owl:onProperty` added on an IRI, and a reserved name as an assertion's value
/// (off the rules route an assertion over U1's kinds, which U1 takes as a delta), each
/// undone, each on the rules route and off it.
#[test]
fn a_commit_that_makes_or_ends_a_left_out_axiom_changes_the_status() {
    let rest = format!("<{RDF}rest>");
    let nil = format!("<{RDF}nil>");
    // (sequence, the store, the change, its undoing, the read)
    let sequences = [
        (
            "reserved name first used",
            format!("{NAMES} :q a owl:ObjectProperty . :x :q :v ."),
            format!("INSERT DATA {{ <{RDF}value> rdfs:domain :C . :x <{RDF}value> :v . }}"),
            format!("DELETE DATA {{ <{RDF}value> rdfs:domain :C . :x <{RDF}value> :v . }}"),
            "ASK { :x a :C }",
        ),
        (
            "a list's last rdf:rest deleted",
            format!("{NAMES} :A a owl:Class ; owl:intersectionOf ( :C :D ) . :x a :A ."),
            format!("DELETE {{ ?c {rest} {nil} }} WHERE {{ :A owl:intersectionOf ?l . ?l {rest} ?c . ?c {rest} {nil} }}"),
            format!("INSERT {{ ?c {rest} {nil} }} WHERE {{ :A owl:intersectionOf ?l . ?l {rest} ?c }}"),
            "ASK { :x a :C }",
        ),
        (
            "a restriction's filler deleted",
            format!("{NAMES} :q a owl:ObjectProperty . [ owl:onProperty :q ; owl:someValuesFrom :D ] rdfs:subClassOf :C . :x :q :v . :v a :D ."),
            "DELETE { ?r owl:someValuesFrom :D } WHERE { ?r owl:onProperty :q ; owl:someValuesFrom :D }".to_owned(),
            "INSERT { ?r owl:someValuesFrom :D } WHERE { ?r owl:onProperty :q }".to_owned(),
            "ASK { :x a :C }",
        ),
        (
            "owl:onProperty added on an IRI",
            format!("{NAMES} :q a owl:ObjectProperty . :R a owl:Class ; rdfs:subClassOf :C . :x :q :v . :v a :D ."),
            "INSERT DATA { :R owl:onProperty :q ; owl:someValuesFrom :D . }".to_owned(),
            "DELETE DATA { :R owl:onProperty :q ; owl:someValuesFrom :D . }".to_owned(),
            "ASK { :x a :C }",
        ),
        (
            // An assertion only, off the rules route a delta of U1 by its kinds.
            "a reserved name as an assertion's value",
            format!("{NAMES} :q a owl:ObjectProperty ; rdfs:domain :C . :v :q :x ."),
            "INSERT DATA { :x :q rdfs:Resource . }".to_owned(),
            "DELETE DATA { :x :q rdfs:Resource . }".to_owned(),
            "ASK { :x a :C }",
        ),
    ];
    let assertion = "INSERT DATA { :w a :D . }".to_owned();
    let mut wrong = Vec::new();
    for ((name, data, change, undo, read), extra) in sequences
        .iter()
        .flat_map(|s| ["", OUTSIDE_RL].map(|extra| (s, extra)))
    {
        let dl = pipeline();
        insert(&dl, &format!("{data} {extra}")).expect("committed");
        for (step, update_text, left_out) in [
            ("before", None, false),
            ("changed", Some(change), true),
            ("an assertion beside it", Some(&assertion), true),
            ("undone", Some(undo), false),
        ] {
            if let Some(text) = update_text {
                update(&dl, text).expect("committed");
            }
            let (rows, status) = query(&dl, read);
            let (label, reasons) = (status.shared.as_str(), status.reasons());
            let told = label == "unsound" && says_left_out(&reasons);
            let ok = match left_out {
                true => told,
                false => status.is_complete() && !says_left_out(&reasons),
            };
            if !ok {
                wrong.push(format!(
                    "{name} {extra}, {step}: {rows:?} {label} {reasons:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
