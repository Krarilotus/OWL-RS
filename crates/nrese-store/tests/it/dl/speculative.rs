//! Dictionary lifecycle V1 (`notes/2026-11-09-dict-v1-speculative-private-design.md` §4): a
//! request's speculative work never appends its own terms to the shared dictionary. Items 1
//! and 2 for the DL routes (entailment, explanation, the exact query services).
//!
//! The oracle is a store with the same history but without the speculative request: after a
//! later commit and a reopen, the two must agree, in their dictionary's size and in the
//! global search relevance that counts every literal the dictionary holds
//! (`search_scope_tests`).

use nrese_rdf::{Literal, NamedNode, NamedOrBlankNode, Term, TermRef, Triple};
use nrese_store::{ReadScope, StoreConfig, StoreMode, StoreService};

use crate::support::Writes;

const EX: &str = "http://example.com/";

fn named(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn triple(subject: &str, predicate: &str, object: Term) -> Triple {
    Triple::new(
        NamedOrBlankNode::NamedNode(named(subject)),
        named(predicate),
        object,
    )
}

fn terms(store: &StoreService) -> u64 {
    store.engine_stats().dictionary.terms
}

/// A store on disk in `dir`, the cache off.
fn on_disk(dir: &std::path::Path) -> StoreService {
    StoreService::new(StoreConfig {
        mode: StoreMode::OnDisk,
        data_dir: dir.to_path_buf(),
        query_cache_bytes: 0,
        ..StoreConfig::default()
    })
    .expect("store")
}

const PREMISE: &str = "INSERT DATA { <http://example.com/A> \
     <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example.com/B> . \
     <http://example.com/w> a <http://example.com/A> . }";

/// A conclusion naming terms only this request has: a class and an individual, and a
/// literal.
fn own_conclusion(i: usize) -> Vec<Triple> {
    vec![
        triple(
            &format!("only{i}"),
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            named(&format!("Only{i}")).into(),
        ),
        triple(
            &format!("only{i}"),
            "label",
            Literal::new_simple_literal(format!("only here {i}")).into(),
        ),
    ]
}

/// Item 1, the DL entailment route (red on the parent, which interns the conclusion's terms).
///
/// - Input: two stores on disk with the same premise and a warm-up entailment over its own
///   terms; one of them answers an entailment whose conclusion names a class, an individual
///   and a literal no statement has. Then a later, unrelated commit in both, and a reopen.
/// - Expected: the dictionary is the same size right after the request as before it; after
///   the commit and after the reopen, both stores' dictionaries are the same size (the later
///   commit's record carries none of the request's terms).
#[test]
fn a_dl_entailment_appends_none_of_its_terms() {
    let (asked, oracle) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut sizes = Vec::new();
    for (dir, speculate) in [(&asked, true), (&oracle, false)] {
        let store = on_disk(dir.path());
        store.execute_update_str(PREMISE).expect("premise");
        let warm = [triple(
            "w",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            named("B").into(),
        )];
        store.entails_dl(&warm).expect("warm-up");
        let before = terms(&store);
        if speculate {
            store.entails_dl(&own_conclusion(0)).expect("answered");
            assert_eq!(terms(&store), before, "the request appended terms");
        }
        store
            .execute_update_str("INSERT DATA { <urn:later> <urn:p> \"later\" . }")
            .expect("later commit");
        let committed = terms(&store);
        drop(store);
        let reopened = terms(&on_disk(dir.path()));
        sizes.push((committed, reopened));
    }
    assert_eq!(
        sizes[0], sizes[1],
        "(after the commit, after the reopen): asked vs. oracle"
    );
}

/// Item 1, the DL explanation route (red on the parent, which interns the tests' fresh names
/// into the dictionary on a store whose data came past the pipeline).
///
/// - Input: a store whose premise was loaded past the pipeline (so no commit interned the
///   DL vocabulary); an explanation of `w a B`.
/// - Expected: the explanation, and the dictionary the same size as before it.
#[test]
fn a_dl_explanation_appends_no_terms() {
    let pipeline = super::pipeline();
    crate::support::load_past_the_pipeline(
        pipeline.store(),
        "<http://example.com/A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> \
         <http://example.com/B> . <http://example.com/w> a <http://example.com/A> .",
    );
    let store = pipeline.store();
    let before = terms(store);
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let (w, b) = (named("w"), named("B"));
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        b.as_ref().into(),
    ];
    let explained = store
        .explain_dl(&ReadScope::All, statement)
        .expect("decided")
        .expect("w a B holds");
    assert!(explained.verified, "{explained:?}");
    assert_eq!(terms(store), before, "the explanation appended terms");
}

/// Item 7, a store route: the explanation under a memory limit too small for it, measured
/// here rather than picked. Its private transaction's names are charged to the request's
/// envelope; a refusal there is the route's memory error.
///
/// - Input: the explanation route of `a_dl_explanation_appends_no_terms`, on stores whose
///   server-wide query memory (the request's envelope) is 1 KiB, doubled until the route
///   answers, then the limits between the last refusal and that one, halved down to 64 bytes.
/// - Expected: below the route's need, every answer is the memory limit's error
///   (`is_memory_limit`) or the route's existing "undecided" (`Incomplete`, a test of the
///   explanation stopped at its budget), never an explanation; at every limit the dictionary
///   stays as it was (no fallback to shared names); at the need, the verified explanation.
#[test]
fn an_explanation_refused_for_memory_is_the_memory_limit() {
    let explain = |limit: usize| -> (Result<bool, nrese_store::StoreError>, bool) {
        let store = StoreService::new(StoreConfig {
            total_query_memory_bytes: limit,
            ..StoreConfig::in_memory()
        })
        .expect("store");
        crate::support::load_past_the_pipeline(
            &store,
            "<http://example.com/A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> \
             <http://example.com/B> . <http://example.com/w> a <http://example.com/A> .",
        );
        let before = terms(&store);
        let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let (w, b) = (named("w"), named("B"));
        let statement: [TermRef<'_>; 3] = [
            w.as_ref().into(),
            rdf_type.as_ref().into(),
            b.as_ref().into(),
        ];
        let answer = store
            .explain_dl(&ReadScope::All, statement)
            .map(|explained| explained.is_some_and(|e| e.verified));
        (answer, terms(&store) == before)
    };
    let mut failed = Vec::new();
    let mut refused_at = Vec::new();
    let mut limit = 1024usize;
    let need = loop {
        let (answer, kept) = explain(limit);
        if !kept {
            failed.push(format!("{limit} bytes: the dictionary grew"));
        }
        match answer {
            Ok(true) => break limit,
            Ok(false) => failed.push(format!("{limit} bytes: an unverified or no explanation")),
            Err(error) if error.is_memory_limit() => refused_at.push(limit),
            Err(nrese_store::StoreError::Incomplete(_)) => {}
            Err(error) => failed.push(format!("{limit} bytes: {error}")),
        }
        assert!(
            limit < 1 << 34,
            "no limit up to 16 GiB answers: {failed:#?}"
        );
        limit *= 2;
    };
    // Between the last refusal and the need, halving the gap: the same rules hold.
    let (mut low, mut high) = (need / 2, need);
    while high - low > 64 {
        let mid = low + (high - low) / 2;
        let (answer, kept) = explain(mid);
        if !kept {
            failed.push(format!("{mid} bytes: the dictionary grew"));
        }
        match answer {
            Ok(true) => high = mid,
            // Neither bound moves on these: recorded, and the search ends.
            Ok(false) => {
                failed.push(format!("{mid} bytes: an unverified or no explanation"));
                break;
            }
            Err(error) if error.is_memory_limit() => {
                refused_at.push(mid);
                low = mid;
            }
            Err(nrese_store::StoreError::Incomplete(_)) => low = mid,
            Err(error) => {
                failed.push(format!("{mid} bytes: {error}"));
                break;
            }
        }
    }
    assert!(
        !refused_at.is_empty(),
        "no limit was refused as memory: {failed:#?}"
    );
    assert!(failed.is_empty(), "need {high} bytes: {failed:#?}");
}

/// Item 1, the exact query services (a guard: their only speculative terms are the tests'
/// fresh names, system vocabulary the bounds' first read appends on the parent and here
/// alike).
///
/// - Input: the union data of `queries.rs`; a query whose `w` only ground entailment proves,
///   once to read the bounds, then again.
/// - Expected: the second answer is `queries.rs`'s, through the exact services, and appended
///   no term.
#[test]
fn an_exact_query_appends_no_terms_once_the_bounds_are_read() {
    const UNION: &str = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
         :C rdfs:subClassOf :D . :x a :A . :y a :B . :z :knows :y . \
         :w a [ owl:unionOf ( :B :C ) ] .";
    let dl = super::pipeline();
    super::insert(&dl, UNION).expect("data");
    let _ = super::queries::query(&dl, "SELECT ?x { ?x a :D }");
    let before = terms(dl.store());
    // Another text, so no cached answer stands in for the services.
    let (rows, status) = super::queries::query(&dl, "SELECT DISTINCT ?x { ?x a :D }");
    assert_eq!(rows, ["w", "x", "y"]);
    assert!(
        status.paths.contains(&"exact-ground-entailment"),
        "{:?}",
        status.paths
    );
    assert_eq!(terms(dl.store()), before, "the query appended terms");
}

/// What U1 approximates: existentials (Skolem constants), a union, disjointness, a
/// transitive property, an inverse (`bounds.rs`'s schema).
const SCHEMA: &str = ":Person rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :parent ; \
       owl:someValuesFrom :Person ] . \
     :Person owl:equivalentClass [ owl:unionOf ( :Man :Woman ) ] . \
     :Man owl:disjointWith :Woman . \
     :ancestor a owl:TransitiveProperty . :parent rdfs:subPropertyOf :ancestor . \
     :child owl:inverseOf :parent . \
     :i0 a :Person . :i1 :parent :i0 .";

/// A made name: one of the DL's own terms, apart from every IRI (D16-051).
fn own(term: &str) -> bool {
    term.strip_prefix('<')
        .is_some_and(nrese_dl::internal::is_made)
}

/// U1's facts, its own terms ([`own`]) as one name, with their counts (`bounds.rs`'s
/// `canonical`).
fn canonical(facts: &[[String; 3]]) -> Vec<([String; 3], usize)> {
    let mut out = std::collections::BTreeMap::new();
    for fact in facts {
        let key = fact.clone().map(|t| match own(&t) {
            true => "<U1's own>".to_owned(),
            false => t,
        });
        *out.entry(key).or_insert(0) += 1;
    }
    out.into_iter().collect()
}

/// Caller 10's report decodes U1's names (a guard, DICT2 04:03): they are shared ids appended
/// past the published snapshot's length, with no commit since.
///
/// - Input: the schema of the guard below, loaded past the pipeline (no commit interned U1's
///   names); the upper-facts report evaluated afresh, cold.
/// - Expected: U1's own names among its facts, and every term decoded: none is `#id`, the
///   report's text for an id its decoding vocabulary lacks.
#[test]
fn a_cold_upper_facts_report_decodes_every_name() {
    let dl = super::pipeline();
    let turtle = format!(
        "@prefix : <{EX}> . @prefix owl: <http://www.w3.org/2002/07/owl#> . \
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . {SCHEMA}"
    );
    crate::support::load_past_the_pipeline(dl.store(), &turtle);
    let facts = dl.store().dl_upper_facts(true).expect("U1 afresh");
    let terms: Vec<&String> = facts.iter().flatten().collect();
    assert!(terms.iter().any(|t| own(t)), "U1's own names: {facts:?}");
    let undecoded: Vec<&&String> = terms.iter().filter(|t| t.starts_with('#')).collect();
    assert!(undecoded.is_empty(), "undecoded: {undecoded:?}");
}

/// Item 5's commit path (a guard of the delta from shared names; the window before the first
/// commit is `dl::source::tests::the_kept_cold_upper_bound_survives_an_aborted_writers_terms`).
///
/// - Input: a schema U1 approximates with Skolem constants, loaded past the pipeline (no
///   commit interned U1's names); a cold DL read, which builds U1 afresh and keeps it; then
///   commits that allocate new terms (individuals and their links), each followed by a read.
/// - Expected: after each commit, the U1 the store keeps and maintains equals U1 evaluated
///   afresh, term by term, and `?x a :Person` answers as in a store that got everything
///   through the pipeline.
#[test]
fn the_bounds_keep_their_names_valid_across_later_allocations() {
    let dl = super::pipeline();
    let turtle = format!(
        "@prefix : <{EX}> . @prefix owl: <http://www.w3.org/2002/07/owl#> . \
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . {SCHEMA}"
    );
    crate::support::load_past_the_pipeline(dl.store(), &turtle);
    let oracle = super::pipeline();
    super::insert(&oracle, SCHEMA).expect("schema");
    assert!(
        dl.store().dl_upper_facts(false).is_some(),
        "the cold read keeps U1"
    );
    let mut failed = Vec::new();
    for step in 0..4 {
        let facts =
            format!(":n{step} a :Person . :n{step} :parent :i{step} . :n{step} :child :m{step} .");
        super::insert(&dl, &facts).expect("insert");
        super::insert(&oracle, &facts).expect("insert");
        let maintained = dl.store().dl_upper_facts(false).expect("U1");
        let afresh = dl.store().dl_upper_facts(true).expect("U1 afresh");
        if canonical(&maintained) != canonical(&afresh) {
            failed.push(format!("step {step}: the kept U1 differs from U1 afresh"));
        }
        let asked = super::queries::query(&dl, "SELECT ?x { ?x a :Person }").0;
        let expected = super::queries::query(&oracle, "SELECT ?x { ?x a :Person }").0;
        if asked != expected {
            failed.push(format!("step {step}: {asked:?}, the oracle {expected:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// `alpha` and `beta` once each, and fillers: under the global scope `beta` ranks first.
const FILL: &str = "INSERT DATA { <urn:a> <urn:label> \"alpha one two\" . \
     <urn:b> <urn:label> \"beta\" . <urn:f0> <urn:label> \"filler 0\" . \
     <urn:f1> <urn:label> \"filler 1\" . <urn:f2> <urn:label> \"filler 2\" . \
     <urn:f3> <urn:label> \"filler 3\" . <urn:f4> <urn:label> \"filler 4\" . \
     <urn:f5> <urn:label> \"filler 5\" . <urn:f6> <urn:label> \"filler 6\" . \
     <urn:f7> <urn:label> \"filler 7\" . }";

const TOP: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                   SELECT ?o WHERE { ?o bds:search \"alpha beta\" ; bds:maxRank 1 }";

fn top(store: &StoreService) -> &'static str {
    let result = store.execute_query_str(TOP).expect("answered");
    let payload = String::from_utf8(result.payload).expect("utf-8");
    match (
        payload.contains("alpha one two"),
        payload.contains("\"beta\""),
    ) {
        (true, false) => "alpha one two",
        (false, true) => "beta",
        _ => panic!("one match expected: {payload}"),
    }
}

/// Item 2 (LC A12's shape; red on the parent, whose dictionary keeps the requests' literals,
/// which global relevance counts).
///
/// - Input: two stores on disk filled alike; one answers sixty DL entailments, each with a
///   conclusion naming the literal `beta extra i`. Then a later, unrelated commit in both, and
///   a reopen.
/// - Expected: the global search's best match for `alpha beta` is `beta` in both, as in a
///   store that never saw those literals, after the commit and after the reopen.
#[test]
fn speculative_literals_leave_global_search_relevance_unchanged() {
    let (asked, oracle) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for (dir, speculate) in [(&asked, true), (&oracle, false)] {
        let which = if speculate { "asked" } else { "oracle" };
        let store = on_disk(dir.path());
        store.execute_update_str(FILL).expect("written");
        if speculate {
            for i in 0..60 {
                let conclusion = [triple(
                    &format!("x{i}"),
                    "label",
                    Literal::new_simple_literal(format!("beta extra {i}")).into(),
                )];
                store.entails_dl(&conclusion).expect("answered");
            }
        }
        store
            .execute_update_str("INSERT DATA { <urn:later> <urn:p> <urn:o> . }")
            .expect("later commit");
        assert_eq!(top(&store), "beta", "{which}, after the later commit");
        drop(store);
        assert_eq!(
            top(&on_disk(dir.path())),
            "beta",
            "{which}, after the reopen"
        );
    }
}
