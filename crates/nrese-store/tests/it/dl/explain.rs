//! Step 8: explanations of DL answers, as minimal sets of the ontology's axioms with the
//! triples each was read from (the black-box justification of design §10).

use std::collections::HashSet;
use std::sync::Arc;

use nrese_rdf::{NamedNode, TermRef};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::GraphAccess;
use nrese_store::{DatasetBackupFormat, DlConfig, MutationError, ReadScope, Refusal, StoreError};

use super::{insert, pipeline, pipeline_with};

const EX: &str = "http://example.com/";

/// w is a D through its assertion of a union; the rest is noise a justification must
/// leave out.
const DATA: &str = ":B rdfs:subClassOf :D . :C rdfs:subClassOf :D . \
     :w a [ owl:unionOf ( :B :C ) ] . \
     :Q rdfs:subClassOf :R . :x a :Q . :B rdfs:subClassOf :E . :x :knows :w .";

fn named(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

#[test]
fn a_dl_answer_is_explained_by_a_minimal_set_of_axioms() {
    let dl = pipeline();
    insert(&dl, DATA).expect("data");
    let (w, d) = (named("w"), named("D"));
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        d.as_ref().into(),
    ];
    let explained = dl
        .store()
        .explain_dl(&ReadScope::All, statement)
        .expect("decided")
        .expect("w a D holds under OWL 2 DL");
    assert!(explained.minimal && explained.verified, "{explained:?}");
    let axioms: Vec<&str> = explained.axioms.iter().map(|a| a.axiom.as_str()).collect();
    assert_eq!(axioms.len(), 3, "{axioms:#?}");
    assert!(
        axioms.iter().any(|a| a.contains("ObjectUnionOf")),
        "{axioms:#?}"
    );
    for noise in ["example.com/Q", "example.com/E", "example.com/knows"] {
        assert!(
            !axioms.iter().any(|a| a.contains(noise)),
            "{noise} in {axioms:#?}"
        );
    }
    // Each axiom with the triples it was read from.
    assert!(
        explained
            .axioms
            .iter()
            .all(|a| !a.sources.is_empty() && !a.sources[0].1.is_empty())
    );
    // A statement that doesn't hold has no explanation: a model puts w in C, outside E.
    let e = named("E");
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        e.as_ref().into(),
    ];
    assert!(matches!(
        dl.store().explain_dl(&ReadScope::All, statement),
        Ok(None)
    ));
    // A term the store doesn't hold is still in entailed statements (every individual is
    // an owl:Thing): undecided, never "doesn't hold".
    let (nowhere, thing) = (
        named("nowhere"),
        NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#Thing"),
    );
    let statement: [TermRef<'_>; 3] = [
        nowhere.as_ref().into(),
        rdf_type.as_ref().into(),
        thing.as_ref().into(),
    ];
    let answer = dl.store().explain_dl(&ReadScope::All, statement);
    assert!(
        matches!(answer, Err(StoreError::Incomplete(_))),
        "{answer:?}"
    );
    // DL answers, and so their explanations, are for readers of every graph only: the
    // others are refused.
    let restricted = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        ..GraphAccess::default()
    }));
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        d.as_ref().into(),
    ];
    let answer = dl.store().explain_dl(&restricted, statement);
    assert!(
        matches!(answer, Err(StoreError::Forbidden(Refusal::ReadAll(_)))),
        "{answer:?}"
    );
}

/// Spec (C-501): a budget that stops the first entailment test leaves the explanation
/// undecided, an incomplete answer naming the budget, never "doesn't hold" (w a D holds:
/// the test above).
#[test]
fn a_dl_explanation_stopped_before_its_first_test_is_undecided() {
    let dl = pipeline_with(DlConfig {
        explanation_tests: Some(0),
        ..DlConfig::default()
    });
    insert(&dl, DATA).expect("data");
    let (w, d) = (named("w"), named("D"));
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let answer = dl.store().explain_dl(
        &ReadScope::All,
        [
            w.as_ref().into(),
            rdf_type.as_ref().into(),
            d.as_ref().into(),
        ],
    );
    let Err(StoreError::Incomplete(answer)) = answer else {
        panic!("undecided, got {answer:?}");
    };
    assert!(
        answer
            .status
            .reasons
            .iter()
            .any(|r| r.source == "dl" && r.text.contains("dl.explanation_tests")),
        "{answer:?}"
    );
}

/// `w a D`'s explanation over `B ⊑ D`, `C ⊑ D` (default graph) and `data`, which asserts `w`
/// a union of `B` and `C` in named graphs: per axiom, per graph (the default graph `None`,
/// a named one by its last segment) the predicates of its triples by local name, sorted.
/// Every triple it lists under a graph is checked first to be asserted in that graph.
fn graphs_explained(data: &str) -> Vec<Vec<(Option<String>, Vec<String>)>> {
    let dl = pipeline();
    let subclasses = ":B rdfs:subClassOf :D . :C rdfs:subClassOf :D .";
    insert(&dl, &format!("{subclasses} {data}")).expect("data");
    let (w, d) = (named("w"), named("D"));
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let statement: [TermRef<'_>; 3] = [
        w.as_ref().into(),
        rdf_type.as_ref().into(),
        d.as_ref().into(),
    ];
    // Only a reader of every graph gets an explanation; a reader of some is refused.
    let some = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        ..GraphAccess::default()
    }));
    let answer = dl.store().explain_dl(&some, statement);
    assert!(
        matches!(answer, Err(StoreError::Forbidden(Refusal::ReadAll(_)))),
        "{answer:?}"
    );
    let local = |t: &str| {
        let iri = t.trim_end_matches('>');
        iri.rsplit(['/', '#']).next().unwrap_or(iri).to_owned()
    };
    let explained = dl
        .store()
        .explain_dl(&ReadScope::All, statement)
        .expect("decided")
        .expect("w a D holds under OWL 2 DL");
    // Every triple listed under a graph is asserted in that graph: the store's asserted
    // quads, as N-Triples terms, by graph (`None`: the default graph).
    let export = dl
        .store()
        .export_dataset(&ReadScope::All, DatasetBackupFormat::NQuads)
        .expect("an export");
    let parser = RdfParser::from_format(RdfFormat::NQuads).for_reader(&export.payload[..]);
    let asserted: HashSet<(Option<String>, [String; 3])> = parser
        .map(|quad| {
            let q = quad.expect("the export's quads");
            let graph = (!q.graph_name.is_default_graph()).then(|| q.graph_name.to_string());
            let (s, p, o) = (q.subject, q.predicate, q.object);
            (graph, [s.to_string(), p.to_string(), o.to_string()])
        })
        .collect();
    for (graph, triples) in explained.axioms.iter().flat_map(|a| &a.sources) {
        for triple in triples {
            let listed = (graph.clone(), triple.clone());
            assert!(asserted.contains(&listed), "{listed:?} isn't asserted");
        }
    }
    let mut axioms: Vec<_> = explained
        .axioms
        .iter()
        .map(|a| {
            let sources = a.sources.iter().map(|(graph, triples)| {
                let mut predicates: Vec<String> = triples.iter().map(|t| local(&t[1])).collect();
                predicates.sort();
                (graph.as_deref().map(local), predicates)
            });
            sources.collect::<Vec<_>>()
        })
        .collect();
    axioms.sort();
    axioms
}

/// Spec (`ExplainedAxiom::sources`: per graph, the triples of the axiom's RDF form that
/// graph asserts; the default graph first, then named graphs by IRI): every triple is listed
/// under each graph that holds it and under no other.
#[test]
fn an_explanation_lists_each_triple_under_the_graph_that_holds_it() {
    let g = |graph: Option<&str>, predicates: &[&str]| {
        let predicates = predicates.iter().map(|p| (*p).to_owned()).collect();
        (graph.map(str::to_owned), predicates)
    };
    let sub = vec![g(None, &["subClassOf"])];
    let list = ["first", "first", "rest", "rest"];
    let union = [&list[..], &["unionOf"]].concat();
    let whole = [&list[..], &["type", "unionOf"]].concat();
    // The class assertion's own triple in g1, the union it names in g2.
    let split = vec![g(Some("g1"), &["type"]), g(Some("g2"), &union)];
    // The same, and the class assertion's own triple in g4 as well (written first).
    let twice = vec![
        g(Some("g1"), &["type"]),
        g(Some("g2"), &union),
        g(Some("g4"), &["type"]),
    ];
    // All of the axiom in g1: one graph, every triple under it.
    let plain = vec![g(Some("g1"), &whole)];
    let cases = [
        (
            "GRAPH :g1 { :w a _:u } GRAPH :g2 { _:u owl:unionOf ( :B :C ) }",
            split,
        ),
        (
            "GRAPH :g4 { :w a _:u } GRAPH :g1 { :w a _:u } GRAPH :g2 { _:u owl:unionOf ( :B :C ) }",
            twice,
        ),
        ("GRAPH :g1 { :w a _:u . _:u owl:unionOf ( :B :C ) }", plain),
    ];
    for (data, assertion) in cases {
        let mut want = vec![assertion, sub.clone(), sub.clone()];
        want.sort();
        assert_eq!(graphs_explained(data), want, "{data}");
    }
}

#[test]
fn a_dl_rejection_names_the_axioms_with_no_model() {
    let dl = pipeline();
    insert(
        &dl,
        ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . :B owl:disjointWith :D . \
         :C owl:disjointWith :D . :Q rdfs:subClassOf :R . :x a :Q .",
    )
    .expect("schema");
    let rejected = insert(&dl, ":x a :A , :D .");
    let Err(MutationError::Rejected(reject)) = rejected else {
        panic!("expected a rejection, got {rejected:?}");
    };
    assert!(
        reject.detail.contains("have no model together"),
        "{}",
        reject.detail
    );
    let evidence = reject.explanation.expect("an explanation").evidence;
    let mentions = |local: &str| {
        evidence.iter().any(|e| {
            [&e.subject, &e.predicate, &e.object]
                .iter()
                .any(|t| t.contains(local))
        })
    };
    assert!(
        mentions("example.com/A") && mentions("example.com/D"),
        "{evidence:#?}"
    );
    assert!(!mentions("example.com/R"), "noise in {evidence:#?}");
    assert!(evidence.iter().all(|e| e.role == "axiom"));
}
