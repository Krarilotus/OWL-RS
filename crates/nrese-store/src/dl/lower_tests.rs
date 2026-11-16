//! L's memberships, read by one scan per class and a sorted difference (D16-712), are exactly
//! those of their definition read literally: for each subsumption `(C, D)` and each member `a`
//! of `C` in L, `(a rdf:type D)` where a point read of it in L finds nothing.

use std::sync::Arc;

use nrese_reasoner::rulesets::Ruleset;

use super::*;
use crate::StoreConfig;

const PREFIXES: &str = "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// A union on the right: `B ⊑ E` holds under OWL 2 DL, and the RL rules miss it.
const UNION: &str = ":B rdfs:subClassOf [ owl:unionOf ( :C :D ) ] . :C rdfs:subClassOf :E .
     :D rdfs:subClassOf :E . :x a :B .";

/// The store `data` gives, in the equality mode `config` has, with L the OWL 2 RL closure.
fn store_with(config: StoreConfig, data: &str) -> StoreService {
    let store = StoreService::new(config).unwrap();
    store
        .update_str_ungated(&format!("{PREFIXES} INSERT DATA {{ {data} }}"))
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store
}

fn id(snapshot: &Snapshot, name: &str) -> u64 {
    let iri = if name.starts_with("http") {
        name.to_owned()
    } else {
        format!("urn:test:{name}")
    };
    snapshot
        .lookup(nrese_rdf::NamedNodeRef::new_unchecked(&iri).into())
        .unwrap_or_else(|| panic!("{iri} has an id"))
        .raw()
}

/// L's quads `(s rdf:type class)` in any graph; every subject where `s` is `None`.
fn typed(snapshot: &Snapshot, rdf_type: u64, s: Option<u64>, class: u64) -> Vec<u64> {
    let pattern = QuadPattern {
        subject: s.map(TermId::from_raw),
        predicate: Some(TermId::from_raw(rdf_type)),
        object: Some(TermId::from_raw(class)),
        graph: GraphSelector::Any,
    };
    snapshot
        .quads_for_pattern_in(ReadModel::Materialised, &pattern)
        .map(|q| q.subject.raw())
        .collect()
}

/// The definition read literally: one point read per member and superclass.
fn by_point_reads(snapshot: &Snapshot, taxonomy: &Taxonomy, rdf_type: u64) -> Vec<Triple> {
    let mut out = Vec::new();
    for &(c, d) in &taxonomy.classification.subsumptions {
        for a in typed(snapshot, rdf_type, None, c) {
            if typed(snapshot, rdf_type, Some(a), d).is_empty() {
                out.push([a, rdf_type, d]);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// `memberships` of `store` beside the point reads, with what the check needs of the store.
fn both(store: &StoreService) -> (Snapshot, u64, Vec<Triple>, Vec<Triple>) {
    let snapshot = store.engine().snapshot();
    let budget = Arc::new(nrese_exec::Budget::unlimited());
    let ontology = super::super::source::read_snapshot(&snapshot, Arc::clone(&budget)).unwrap();
    let taxonomy = tbox_taxonomy(store, &ontology);
    let rdf_type = id(&snapshot, "http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let scanned = memberships(&snapshot, &taxonomy, rdf_type, &budget)
        .unwrap()
        .to_vec();
    let expected = by_point_reads(&snapshot, &taxonomy, rdf_type);
    (snapshot, rdf_type, scanned, expected)
}

#[test]
fn memberships_by_scan_equal_the_point_reads() {
    let replicate = StoreConfig::in_memory;
    let representatives = || StoreConfig {
        equality_by_representatives: true,
        ..StoreConfig::in_memory()
    };
    let compact = || StoreConfig {
        equality_by_representatives: true,
        equality_compact: true,
        ..StoreConfig::in_memory()
    };
    let equality = format!("{UNION} :x owl:sameAs :y .");
    let stores: [(&str, StoreConfig, &str); 8] = [
        (
            "the chain",
            replicate(),
            ":C0 rdfs:subClassOf :C1 . :C1 rdfs:subClassOf :C2 . :a a :C0 . :b a :C1 .",
        ),
        ("union on the right", replicate(), UNION),
        ("equality, replicated", replicate(), &equality),
        ("equality, by representatives", representatives(), &equality),
        ("equality, compact", compact(), &equality),
        // `C1`'s members from both stacks (`v` inferred, `u` and `w` asserted; one quad is never
        // in both, the inferred stack being disjoint from the asserted one), `w` in two graphs.
        (
            "both stacks and two graphs",
            replicate(),
            ":C0 rdfs:subClassOf :C1 . :C1 rdfs:subClassOf [ owl:unionOf ( :K :L ) ] .
             :K rdfs:subClassOf :M . :L rdfs:subClassOf :M .
             :v a :C0 . :u a :C1 . :w a :C1 . GRAPH :g { :w a :C1 . }",
        ),
        (
            "an empty class",
            replicate(),
            ":F rdfs:subClassOf :G . :G rdfs:subClassOf [ owl:unionOf ( :K :L ) ] .",
        ),
        (
            "a class with no subsumption",
            replicate(),
            ":C0 rdfs:subClassOf :C1 . :a a :C0 . :h a :H .",
        ),
    ];
    for (name, config, data) in stores {
        let store = store_with(config, data);
        let (snapshot, rdf_type, scanned, expected) = both(&store);
        let id = |n: &str| id(&snapshot, n);
        // Each store reaches its case first, so that no row passes vacuously.
        let reached = match name {
            "the chain" => expected.is_empty(),
            // `E` has no member in L: an empty class as the superclass.
            "union on the right" => {
                expected == [[id("x"), rdf_type, id("E")]]
                    && typed(&snapshot, rdf_type, None, id("E")).is_empty()
            }
            n if n.starts_with("equality") => {
                expected.contains(&[id("x"), rdf_type, id("E")])
                    && expected.contains(&[id("y"), rdf_type, id("E")])
            }
            "both stacks and two graphs" => {
                let c1 = typed(&snapshot, rdf_type, None, id("C1"));
                let asserted = |s: &str| {
                    let pattern = QuadPattern {
                        subject: Some(TermId::from_raw(id(s))),
                        predicate: Some(TermId::from_raw(rdf_type)),
                        object: Some(TermId::from_raw(id("C1"))),
                        graph: GraphSelector::Any,
                    };
                    snapshot
                        .quads_for_pattern_in(ReadModel::Asserted, &pattern)
                        .count()
                };
                c1.iter().filter(|&&s| s == id("w")).count() == 2
                    && c1.contains(&id("v"))
                    && asserted("v") == 0
                    && asserted("u") == 1
                    && ["u", "v", "w"]
                        .iter()
                        .all(|s| expected.contains(&[id(s), rdf_type, id("M")]))
            }
            "an empty class" => typed(&snapshot, rdf_type, None, id("F")).is_empty(),
            _ => !typed(&snapshot, rdf_type, None, id("H")).is_empty() && expected.is_empty(),
        };
        assert!(
            reached,
            "{name}: the store doesn't reach its case: {expected:?}"
        );
        assert_eq!(scanned, expected, "{name}");
    }
}

/// The member lists are charged as they grow (SYN-61): on a chain, where the output needs no
/// room, a budget with none refuses the memberships at the first member read.
#[test]
fn member_lists_past_the_budget_refuse_the_memberships() {
    let store = store_with(
        StoreConfig::in_memory(),
        ":C0 rdfs:subClassOf :C1 . :a a :C0 .",
    );
    let (snapshot, rdf_type, scanned, _) = both(&store);
    let c0 = typed(&snapshot, rdf_type, None, id(&snapshot, "C0"));
    assert!(scanned.is_empty() && !c0.is_empty(), "{scanned:?}, {c0:?}");
    let unlimited = Arc::new(nrese_exec::Budget::unlimited());
    let ontology = super::super::source::read_snapshot(&snapshot, unlimited).unwrap();
    let taxonomy = tbox_taxonomy(&store, &ontology);
    let none = Arc::new(nrese_exec::Budget::new(0));
    assert!(memberships(&snapshot, &taxonomy, rdf_type, &none).is_err());
    assert!(none.refused());
}
