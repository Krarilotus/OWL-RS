//! The long chain of the 5,000-seed outside run (seeds 1942, 2982, 3507, 4669 and 4733, each
//! shrunk under HermiT): `f:p3` is the property chain of 260 properties `f:c0 … f:c259`,
//! more than one rule holds, and the data joins `f:a0` to `f:a1` along it through 259 blank
//! nodes. By the chain's semantics (OWL 2 Direct Semantics, Table 6: the composition of the
//! chain's extensions is in `f:p3`'s; a blank node is an anonymous individual)
//! `f:a0 f:p3 f:a1` is entailed. The expected rows are written from that; the status must be
//! honest about them: `complete` only with exactly these rows, `sound` only with none outside
//! them (the outside check's FAIL rule).

use nrese_sparql::Completeness;
use nrese_store::{MutationPipeline, SparqlQueryRequest};

use super::{pipeline, update};

const F: &str = "PREFIX f: <http://example.org/fuzz#> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

/// Every case's first commit, as the shrunk cases have it.
const DECLARATIONS: &str = "f:C0 a owl:Class . f:C1 a owl:Class . f:C2 a owl:Class . \
     f:C3 a owl:Class . f:p0 a owl:ObjectProperty . f:p1 a owl:ObjectProperty . \
     f:p2 a owl:ObjectProperty . f:p3 a owl:ObjectProperty . f:d0 a owl:DatatypeProperty . \
     f:d1 a owl:DatatypeProperty . f:a0 a owl:NamedIndividual . f:a1 a owl:NamedIndividual . \
     f:a2 a owl:NamedIndividual . f:a3 a owl:NamedIndividual .";

/// Seed 4733's second group: the short chain `f:p1 f:p2` with its data.
const SHORT_CHAIN: &str =
    "f:p3 owl:propertyChainAxiom ( f:p1 f:p2 ) . f:a0 f:p1 f:a1 . f:a1 f:p2 f:a2 .";

/// `f:p3`'s chain of 260 properties, `f:a0` joined to `f:a1` along it.
fn long_chain() -> String {
    const N: usize = 260;
    let names: Vec<String> = (0..N).map(|i| format!("f:c{i}")).collect();
    let links: String = names
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let from = if i == 0 {
                "f:a0".into()
            } else {
                format!("_:l{i}")
            };
            let to = if i + 1 == N {
                "f:a1".into()
            } else {
                format!("_:l{}", i + 1)
            };
            format!(" {c} a owl:ObjectProperty . {from} {c} {to} .")
        })
        .collect();
    format!(
        "f:p3 owl:propertyChainAxiom ( {} ) .{links}",
        names.join(" ")
    )
}

const EXISTS: &str = "SELECT ?v0 { ?v0 f:p3 ?e1 . }";
const NOT_PROP_2: &str = "SELECT ?v0 ?v1 { ?v0 f:p3 ?v1 . FILTER NOT EXISTS { ?v0 f:p1 ?v1 . } }";
const NOT_PROP: &str = "SELECT ?v0 ?v1 { ?v0 f:p1 ?v1 . FILTER NOT EXISTS { ?v0 f:p3 ?v1 . } }";

/// A query's rows (its variables' local names, in order, sorted) and status.
fn answers(dl: &MutationPipeline, select: &str) -> (Vec<Vec<String>>, Completeness) {
    let store = dl.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!("{F}{select}")))
        .expect("prepare");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out)
        .expect("run")
        .expect("a status under owl2-dl");
    let json: serde_json::Value = serde_json::from_slice(&out).expect("json results");
    let vars = json["head"]["vars"].as_array().expect("vars").clone();
    let mut rows: Vec<Vec<String>> = json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| {
            vars.iter()
                .map(|v| {
                    let value = row[v.as_str().expect("name")]["value"].as_str();
                    value
                        .unwrap_or("-")
                        .trim_start_matches("http://example.org/fuzz#")
                        .to_owned()
                })
                .collect()
        })
        .collect();
    rows.sort();
    (rows, status.shared)
}

#[test]
fn a_chain_longer_than_a_rule_is_answered_honestly_on_the_closed_predicates_path() {
    let chain = long_chain();
    let one_commit = format!("{DECLARATIONS} {chain}");
    let first_4733 = format!("{DECLARATIONS} {SHORT_CHAIN}");
    // (seed, commits, query, the entailed rows, by hand)
    let cases: [(u32, Vec<&str>, &str, Vec<Vec<&str>>); 5] = [
        // f:a0 f:p3 f:a1 holds, so ?v0 = f:a0 has a p3-successor.
        (1942, vec![one_commit.as_str()], EXISTS, vec![vec!["a0"]]),
        (
            2982,
            vec![DECLARATIONS, chain.as_str()],
            EXISTS,
            vec![vec!["a0"]],
        ),
        (
            4669,
            vec![DECLARATIONS, chain.as_str()],
            EXISTS,
            vec![vec!["a0"]],
        ),
        // (f:a0, f:a1) is in p3; nothing states or entails a p1 pair, so it stays.
        (
            3507,
            vec![DECLARATIONS, chain.as_str()],
            NOT_PROP_2,
            vec![vec!["a0", "a1"]],
        ),
        // The one p1 pair, (f:a0, f:a1), is also in p3 by the long chain, so it is removed
        // (the short chain adds (f:a0, f:a2) to p3, which is no p1 pair).
        (
            4733,
            vec![first_4733.as_str(), chain.as_str()],
            NOT_PROP,
            vec![],
        ),
    ];
    let mut wrong = Vec::new();
    for (seed, commits, select, entailed) in cases {
        let dl = pipeline();
        let refused: Vec<String> = commits
            .iter()
            .filter_map(|data| update(&dl, &format!("{F}INSERT DATA {{ {data} }}")).err())
            .map(|e| format!("{e}"))
            .collect();
        let (rows, status) = answers(&dl, select);
        let route = dl.store().dl_bounds().last;
        let line = format!(
            "seed {seed}: rows {rows:?}, entailed {entailed:?}, complete {}, sound {}, \
             route {route}, reasons {:?}, refused {refused:?}",
            status.complete,
            status.sound,
            status.reasons.iter().map(|r| &r.text).collect::<Vec<_>>()
        );
        println!("{line}");
        let missing = entailed.iter().any(|e| !rows.iter().any(|r| r == e));
        let extra = rows.iter().any(|r| !entailed.iter().any(|e| r == e));
        if !refused.is_empty() || (status.complete && missing) || (status.sound && extra) {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "dishonest cases:\n{}", wrong.join("\n"));
}
