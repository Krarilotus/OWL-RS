//! Step 5: the query path. Every answer under `owl2-dl` carries its status; each test
//! names the path that must decide (closed predicates without a second evaluation, the
//! bounds, ground entailment, a rolled-up tree), and the fuzzed differential checks class
//! queries against the hypertableau's realisation of the same ontology.

use std::collections::BTreeSet;

use nrese_sparql::Completeness;
use nrese_store::{
    CancellationToken, DlAnswers, DlConfig, MutationPipeline, SparqlQueryRequest, StoreError,
};

use super::{PREFIXES, insert, pipeline, pipeline_with};

/// The answers of `query` (with [`PREFIXES`]) as `?x` values (local names, or `true` /
/// `false` for ASK), and their status.
fn query_with(
    pipeline: &MutationPipeline,
    query: &str,
    mode: Option<DlAnswers>,
) -> Result<(Vec<String>, Completeness), StoreError> {
    let store = pipeline.store();
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES}{query}"));
    request.dl_answers = mode;
    let prepared = store.prepare_query(&request)?;
    let mut out = Vec::new();
    let status = store
        .run_query_reporting(&prepared, &CancellationToken::new(), &mut out)?
        .expect("a status under owl2-dl");
    let json: serde_json::Value = serde_json::from_slice(&out).expect("json results");
    if let Some(b) = json.get("boolean") {
        return Ok((vec![b.to_string()], status));
    }
    let mut values: Vec<String> = json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| {
            row["x"]["value"]
                .as_str()
                .unwrap_or("-")
                .trim_start_matches("http://example.com/")
                .to_owned()
        })
        .collect();
    values.sort();
    Ok((values, status))
}

fn query(pipeline: &MutationPipeline, query: &str) -> (Vec<String>, Completeness) {
    query_with(pipeline, query, None).expect("query")
}

const UNION: &str = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
     :C rdfs:subClassOf :D . :x a :A . :y a :B . :z :knows :y .";

const EMPLOYEE: &str = ":Employee rdfs:subClassOf [ a owl:Restriction ; \
       owl:onProperty :worksFor ; owl:someValuesFrom :Org ] . \
     :ann a :Employee . :bob :worksFor :acme .";

#[test]
fn closed_predicates_answer_from_the_lower_bound_alone() {
    let dl = pipeline();
    insert(&dl, UNION).expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?z :knows ?x }");
    assert_eq!(rows, ["y"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert_eq!(status.paths, ["closed-predicates"]);
    // Non-monotone operators over closed predicates are exact too.
    let (rows, status) = query(&dl, "SELECT ?x { ?x :knows ?y OPTIONAL { ?x :hates ?y } }");
    assert_eq!(rows, ["z"]);
    assert!(status.is_complete());
}

#[test]
fn a_membership_through_a_union_is_proved_by_ground_entailment() {
    let dl = pipeline();
    insert(&dl, UNION).expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(rows, ["x", "y"], "x is a D through the union, y by RL");
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert!(status.paths.contains(&"exact-ground-entailment"));
    let b = status.bounds.expect("bounds");
    assert_eq!((b.lower, b.upper, b.proved), (1, Some(2), 1));
}

#[test]
fn a_candidate_the_union_splits_into_is_refuted() {
    let dl = pipeline();
    insert(&dl, UNION).expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :C }");
    assert!(rows.is_empty(), "{rows:?}");
    assert!(status.is_complete());
    let b = status.bounds.expect("bounds");
    assert_eq!((b.refuted, b.unresolved), (1, 0));
}

#[test]
fn an_existential_answer_is_proved_by_rolling_up_the_query() {
    let dl = pipeline();
    insert(&dl, EMPLOYEE).expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x :worksFor ?y }");
    assert_eq!(rows, ["ann", "bob"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert!(status.paths.contains(&"exact-internalisable-cq"));
    let (rows, _) = query(&dl, "SELECT ?x { ?x :worksFor ?y . ?y a :Org }");
    assert_eq!(rows, ["ann"], "acme isn't known to be an Org");
    // With ?y an answer variable, ann's employer is anonymous: no named answer.
    let (rows, status) = query(&dl, "SELECT ?x ?y { ?x :worksFor ?y }");
    assert_eq!(rows, ["bob"]);
    assert!(status.is_complete());
    // ASK: some employer exists.
    let (rows, status) = query(&dl, "ASK { ?x :worksFor ?y . ?y a :Org }");
    assert_eq!(
        rows,
        ["true"],
        "{:?} {:?} {:?}",
        status.reasons(),
        status.paths,
        status.bounds
    );
    assert!(status.is_complete());
}

#[test]
fn non_monotone_operators_over_an_open_gap_are_sound_only() {
    let dl = pipeline();
    insert(&dl, UNION).expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :A FILTER NOT EXISTS { ?x a :D } }");
    assert_eq!(rows, ["x"], "over the lower bound");
    assert!(!status.is_complete());
    assert!(
        status.reasons()[0].contains("EXISTS"),
        "{:?}",
        status.reasons()
    );
}

#[test]
fn the_sound_and_exact_modes() {
    let dl = pipeline_with(DlConfig {
        answers: DlAnswers::Sound,
        ..DlConfig::default()
    });
    insert(&dl, UNION).expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(rows, ["y"], "the lower bound alone");
    assert!(!status.is_complete());
    // Asked per query: exact answers, which a union query can't get.
    let (rows, status) =
        query_with(&dl, "SELECT ?x { ?x a :D }", Some(DlAnswers::Exact)).expect("exact");
    assert_eq!(rows, ["x", "y"]);
    assert!(status.is_complete());
    let failed = query_with(
        &dl,
        "SELECT ?x { { ?x a :D } UNION { ?x a :C } }",
        Some(DlAnswers::Exact),
    );
    assert!(
        matches!(failed, Err(StoreError::Incomplete(_))),
        "{failed:?}"
    );
    let (rows, status) = query_with(
        &dl,
        "SELECT ?x { { ?x a :D } UNION { ?x a :C } }",
        Some(DlAnswers::CertainWhereComplete),
    )
    .expect("certain where complete");
    assert_eq!(rows, ["y"]);
    assert!(status.reasons()[0].contains("neither proved nor refuted"));
}

#[test]
fn explain_and_plan_carry_the_status() {
    let dl = pipeline();
    insert(&dl, UNION).expect("data");
    let store = dl.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!(
            "{PREFIXES}SELECT ?x {{ ?x a :D }}"
        )))
        .expect("prepared");
    let explained = store
        .explain_query(&prepared, &CancellationToken::new())
        .expect("explain");
    let status = explained.completeness.expect("a status");
    assert!(status.is_complete());
    assert!(status.paths.contains(&"exact-ground-entailment"));
    let planned = store.plan_query(&prepared).expect("plan");
    assert!(!planned.completeness.expect("a status").is_complete());
}

/// Term table for generated ontologies.
#[derive(Default)]
struct Table {
    text: Vec<String>,
    ids: std::collections::HashMap<String, u64>,
}

impl Table {
    fn id(&mut self, text: String) -> u64 {
        if let Some(&id) = self.ids.get(&text) {
            return id;
        }
        let id = self.text.len() as u64;
        self.ids.insert(text.clone(), id);
        self.text.push(text);
        id
    }
}

impl nrese_owl::Make for Table {
    fn blank(&mut self) -> u64 {
        let n = self.text.len();
        self.id(format!("_:w{n}"))
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        self.id(format!("\"{lexical}\"^^<{datatype}>"))
    }
}

/// Class queries on random ontologies with assertions: every complete answer is the
/// hypertableau's realisation of that class, and incomplete ones are within it.
#[test]
fn class_queries_answer_as_the_realisation_on_random_ontologies() {
    use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
    let (mut compared, mut complete) = (0, 0);
    for seed in 0..25u64 {
        let mut table = Table::default();
        for (_, iri) in nrese_owl::Vocabulary::iris() {
            table.id(format!("<{iri}>"));
        }
        let sig = Signature::new(Sizes::default(), &mut |name| match name {
            Name::Iri(iri) => table.id(format!("<{iri}>")),
            Name::Integer(i) => table.id(format!(
                "\"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
            )),
        });
        let profile = Profile {
            data: false,
            nominals: false,
            numbers: false,
            chains: false,
            ..Profile::sroiq()
        };
        let mut ontology = fuzz::ontology(&mut Rng::new(seed), &sig, profile);
        ontology.axioms.extend(sig.declarations());
        ontology.axioms.sort();
        ontology.axioms.dedup();
        ontology.sources = vec![Vec::new(); ontology.axioms.len()];
        let reference = nrese_dl::classify::realise(&ontology, &Default::default());
        if !reference.incomplete.is_empty() || !reference.taxonomy.complete() {
            continue;
        }
        if !reference.taxonomy.classification.consistent {
            continue;
        }
        let vocabulary =
            nrese_owl::Vocabulary::new(&|iri| table.ids.get(&format!("<{iri}>")).copied());
        let triples = nrese_owl::write(&ontology, &vocabulary, &mut table);
        let data: String = triples
            .iter()
            .map(|t| t.map(|x| table.text[x as usize].clone()).join(" ") + " .\n")
            .collect();
        let dl = pipeline();
        insert(&dl, &data).expect("a consistent ontology");
        compared += 1;
        let name = |t: u64| table.text[t as usize].trim_matches(['<', '>']).to_owned();
        for &class in &sig.classes {
            let expected: BTreeSet<String> = reference
                .individuals
                .iter()
                .zip(&reference.types)
                .filter(|(_, types)| types.contains(&class))
                .map(|(&a, _)| name(a))
                .collect();
            let q = format!("SELECT ?x {{ ?x a <{}> }}", name(class));
            let (rows, status) = query(&dl, &q);
            let got: BTreeSet<String> = rows
                .into_iter()
                .map(|r| format!("http://example.com/{r}"))
                .map(|r| r.replace("http://example.com/http", "http"))
                .collect();
            assert!(
                got.is_subset(&expected),
                "seed {seed}, {q}: unsound {:?}",
                got.difference(&expected).collect::<Vec<_>>()
            );
            if status.is_complete() {
                complete += 1;
                assert_eq!(
                    got, expected,
                    "seed {seed}, {q}: complete but missing answers"
                );
            }
        }
    }
    assert!(compared >= 15, "only {compared} ontologies compared");
    assert!(complete > 0, "no complete answer");
    eprintln!("{compared} ontologies, {complete} complete class queries");
}
