//! Keys bind named individuals only. Under the OWL 2 Direct Semantics a `HasKey` axiom
//! equates two named instances of its class that have, for each key property, the same
//! named individual (object properties) or the same literal (data properties) as value.
//! An anonymous individual, as an instance or as a value, binds nothing.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{MutationPipeline, StoreConfig, StoreService};

use super::queries::query;
use super::{ask, insert, pipeline, update};

/// A union on the right of a subclass axiom: outside OWL 2 RL, so beside the data it takes
/// the ontology off the RL route, to the bounds.
const OUTSIDE_RL: &str = ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :tib a :Pet .";

/// The fingerprint of `owl2-rl`'s program, as its stores record it. Another value rebuilds
/// every `owl2-rl` store's closure at its next start.
const OWL2_RL_FINGERPRINT: &str = "e4d77523ded2917d";

/// An object key over `z`, `:a` and `:b` sharing it as value, and `more`.
fn object_key(z: &str, more: &str) -> String {
    format!(
        ":p a owl:ObjectProperty . :K owl:hasKey ( :p ) . :a a :K ; :p {z} . \
         :b a owl:NamedIndividual , :K , :D ; :p {z} . {more}"
    )
}

/// Each `(data, answer, complete)` committed and `read` asked: the rows must be `[answer]`,
/// and the answer complete or not as `complete` says (either, if `None`). The wrong ones,
/// as lines; every answer is printed with its route and status.
fn answered(cases: &[(String, &str, Option<bool>)], read: &str) -> Vec<String> {
    let mut wrong = Vec::new();
    for (data, answer, complete) in cases {
        let dl = pipeline();
        insert(&dl, data).expect("consistent");
        let route = dl.store().dl_bounds().last;
        let (rows, status) = query(&dl, read);
        let line = format!(
            "{data} / route {route}: {read} answers {rows:?}, complete {}",
            status.is_complete()
        );
        eprintln!("{line}");
        if rows != [*answer] || complete.is_some_and(|c| c != status.is_complete()) {
            wrong.push(line);
        }
    }
    wrong
}

/// An object key whose shared value is an anonymous individual (`_:z`) doesn't equate `:a`
/// and `:b`, so `:a a :D` isn't entailed; with the value named (`:z`) it is, and so it is
/// when the anonymous value is the named `:n` (`_:z owl:sameAs :n`): the key binds the
/// individual, whatever names it. The rules decide it alone. The first two cases are OWL 2
/// RL ontologies; the third asserts `owl:sameAs` on a blank node, so it is outside OWL 2 DL
/// (*OWL 2 Structural Specification* §11.2: no anonymous individual in SameIndividual) and
/// so outside OWL 2 RL too. That case tests how the store handles the equality, not the
/// Direct Semantics.
#[test]
fn a_key_binds_no_anonymous_value() {
    let cases = [
        ("_:z", "", "false"),
        (":z", "", "true"),
        ("_:z", "_:z owl:sameAs :n .", "true"),
    ]
    .map(|(z, more, answer)| (object_key(z, more), answer, None));
    let wrong = answered(&cases, "ASK { :a a :D }");
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A data key shared by an anonymous instance (`_:d1`) and a named one doesn't equate them,
/// so `:d2 a :E` isn't entailed; with the instance named (`:d1`) it is. Under the Direct
/// Semantics a key equates only elements in NAMED (*OWL 2 Direct Semantics* §2.3.5,
/// Table 9). Outside OWL 2 RL: the bounds decide it. Their upper bound applies the key to
/// every individual and so holds `:d2 a :E`; the exact check of that one candidate refutes
/// it, so the `false` is complete.
#[test]
fn a_key_binds_no_anonymous_instance() {
    let cases =
        [("_:d1", "false", Some(true)), (":d1", "true", None)].map(|(d1, answer, complete)| {
            let data = format!(
                ":k a owl:DatatypeProperty . :D owl:hasKey ( :k ) . \
             {d1} a :D , :E ; :k 7 . :d2 a :D ; :k 7 . {OUTSIDE_RL}"
            );
            (data, answer, complete)
        });
    let wrong = answered(&cases, "ASK { :d2 a :E }");
    assert!(wrong.is_empty(), "{wrong:#?}");
    // The anonymous instance's `false`: the upper bound's one candidate, refuted exactly.
    let dl = pipeline();
    insert(&dl, &cases[0].0).expect("consistent");
    let (_, status) = query(&dl, "ASK { :d2 a :E }");
    let refuted = status
        .bounds
        .map(|b| (b.lower, b.upper, b.refuted, b.unresolved));
    assert_eq!(
        (status.paths.as_slice(), refuted),
        (&["exact-ground-entailment"][..], Some((0, Some(1), 1, 0))),
        "{status:?}"
    );
}

/// An object key whose value's `sameAs` class is all anonymous (`_:z1`, `_:z2`) binds
/// nothing; a later commit that adds the named `:n` to the class makes it bind, and the
/// commit that deletes that statement unbinds it again. `owl:sameAs` on a blank node is
/// outside OWL 2 DL (*OWL 2 Structural Specification* §11.2), so no status is required:
/// each answer and its status is the one a store given the same statements in one commit
/// gives; in both equality modes, every identity stored and representatives stored.
#[test]
fn a_key_binds_from_the_commit_that_names_its_value() {
    let anonymous = object_key("_:z1", "_:z1 owl:sameAs _:z2 .");
    let named = object_key("_:z1", "_:z1 owl:sameAs _:z2 . _:z2 owl:sameAs :n .");
    let wrong = commits_answer_as_one_load(
        &[
            (
                format!("INSERT DATA {{ {anonymous} }}"),
                anonymous.clone(),
                "false",
            ),
            (
                "INSERT { ?z owl:sameAs :n } WHERE { :a :p ?z }".to_owned(),
                named,
                "true",
            ),
            (
                "DELETE { ?z owl:sameAs :n } WHERE { ?z owl:sameAs :n }".to_owned(),
                anonymous,
                "false",
            ),
        ],
        None,
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// [`a_key_binds_from_the_commit_that_names_its_value`] inside OWL 2 DL: the values `_:z1`
/// and `_:z2` are equal through a functional property of the anonymous `_:w`, and a commit
/// names them by giving `_:w` the value `:n`. The anonymous individuals form a tree with
/// `_:w` at most once next to a named one (§11.2), and no `owl:sameAs` is asserted: an OWL 2
/// RL ontology, so each answer must also be complete.
#[test]
fn a_key_binds_from_the_commit_that_names_its_value_by_a_functional_property() {
    let data = |more: &str| {
        format!(
            ":p a owl:ObjectProperty . :K owl:hasKey ( :p ) . :a a :K ; :p _:z1 . \
             :b a owl:NamedIndividual , :K , :D ; :p _:z2 . \
             :f a owl:ObjectProperty , owl:FunctionalProperty . _:w :f _:z1 , _:z2 {more} ."
        )
    };
    let (anonymous, named) = (data(""), data(", :n"));
    let wrong = commits_answer_as_one_load(
        &[
            (
                format!("INSERT DATA {{ {anonymous} }}"),
                anonymous.clone(),
                "false",
            ),
            (
                "INSERT { ?w :f :n } WHERE { ?w :f ?z }".to_owned(),
                named,
                "true",
            ),
            (
                "DELETE { ?w :f :n } WHERE { ?w :f :n }".to_owned(),
                anonymous,
                "false",
            ),
        ],
        Some(true),
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Each `(change, the same statements as data, answer)` committed in turn, `ASK { :a a :D }`
/// asked after each: the answer must be `[answer]`, and it and its status the ones of a
/// store given the data in one commit; complete or not as `complete` says (either, if
/// `None`). In both equality modes. The wrong ones, as lines; every answer is printed.
fn commits_answer_as_one_load(
    steps: &[(String, String, &str)],
    complete: Option<bool>,
) -> Vec<String> {
    let read = "ASK { :a a :D }";
    let mut wrong = Vec::new();
    for compact in [false, true] {
        let committed = store(compact);
        for (change, data, answer) in steps {
            update(&committed, change).expect("commit");
            let (rows, status) = query(&committed, read);
            let once = store(compact);
            insert(&once, data).expect("consistent");
            let (once_rows, once_status) = query(&once, read);
            let line = format!(
                "compact {compact}, {change}: {rows:?}, complete {}; in one commit {once_rows:?}, \
                 complete {}",
                status.is_complete(),
                once_status.is_complete()
            );
            eprintln!("{line}");
            if rows != [*answer]
                || once_rows != [*answer]
                || status.is_complete() != once_status.is_complete()
                || complete.is_some_and(|c| c != status.is_complete())
            {
                wrong.push(line);
            }
        }
    }
    wrong
}

/// An `owl2-dl` store, equality stored compactly or not.
fn store(compact: bool) -> MutationPipeline {
    let config = StoreConfig {
        equality_compact: compact,
        ..crate::support::in_memory_store_config()
    };
    let dl = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl);
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(dl)),
    )
}

/// A key binds an anonymous individual that the data makes equal to a named one: as its
/// instance (`_:b`, by an inverse-functional `:p`, is `:a`) and as its value (`_:w`, by an
/// inverse-functional `:q` through a second anonymous individual, is `:n`). The key's
/// condition is on domain elements in NAMED (*OWL 2 Direct Semantics* §2.3.5, Table 9),
/// "a subset of ΔI such that (a)I ∈ NAMED for each named individual a" (§2.2): an
/// anonymous individual equal to a named one in every model is that one's element, so the
/// key applies, and `:a` is `:c`. Each form is inside OWL 2 DL: *OWL 2 Structural
/// Specification* §11.2 restricts the axioms ("No anonymous individual occurs in Ax in an
/// axiom of type … SameIndividual"), not an equality they entail, and each tree of
/// anonymous individuals has one with at most one named neighbour. Each form has a named
/// control and an unmerged one (the key doesn't apply). The answers are HermiT's and
/// Openllet's, which agree on every case; in both equality modes, `:a a :D` complete.
#[test]
fn a_key_binds_an_anonymous_individual_equal_to_a_named_one() {
    let instance = |b: &str, merged: bool| {
        format!(
            ":p a owl:ObjectProperty , owl:InverseFunctionalProperty . :k a owl:DatatypeProperty . \
             :C a owl:Class ; owl:hasKey ( :k ) . :D a owl:Class . :v a owl:NamedIndividual . \
             :a a owl:NamedIndividual , :C {} . \
             :c a owl:NamedIndividual , :C , :D ; :k \"1\"^^xsd:integer . \
             {b} a :C ; :p :v ; :k \"1\"^^xsd:integer .",
            if merged { "; :p :v" } else { "" }
        )
    };
    let value = |w: &str, x: &str, merged: bool| {
        format!(
            ":q a owl:ObjectProperty , owl:InverseFunctionalProperty . :k a owl:ObjectProperty . \
             :C a owl:Class ; owl:hasKey ( :k ) . :D a owl:Class . \
             :a a owl:NamedIndividual , :C ; :k {w} . {w} :q {x} . \
             :n a owl:NamedIndividual {} . \
             :c a owl:NamedIndividual , :C , :D ; :k :n .",
            if merged {
                format!("; :q {x}")
            } else {
                String::new()
            }
        )
    };
    let cases = [
        (instance("_:b", true), "true"),
        (instance(":b", true), "true"),
        (instance("_:b", false), "false"),
        (value("_:w", "_:x", true), "true"),
        (value(":w", ":x", true), "true"),
        (value("_:w", "_:x", false), "false"),
    ];
    let mut wrong = Vec::new();
    for (data, answer) in &cases {
        for compact in [false, true] {
            let dl = store(compact);
            insert(&dl, data).expect("consistent");
            for read in ["ASK { :a owl:sameAs :c }", "ASK { :a a :D }"] {
                let (rows, status) = query(&dl, read);
                let line = format!(
                    "{data} / compact {compact}: {read} answers {rows:?}, complete {}",
                    status.is_complete()
                );
                eprintln!("{line}");
                if rows != [*answer] || (read.contains(":D") && !status.is_complete()) {
                    wrong.push(line);
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A key's property need not be simple: *OWL 2 Structural Specification* §11.2 (the
/// restriction on simple roles) lists the axioms that may use only simple properties,
/// "ObjectMinCardinality, ObjectMaxCardinality, ObjectExactCardinality, and ObjectHasSelf.
/// FunctionalObjectProperty, InverseFunctionalObjectProperty, IrreflexiveObjectProperty,
/// AsymmetricObjectProperty, and DisjointObjectProperties", and `HasKey` is not among them.
/// So a key reads the values a property chain implies. `:x :p1 :b` and `:b :p2 :z` give
/// `:x :p3 :z` by the chain `:p1 o :p2`; `:y :p3 :z` shares the value, so the key on `:C`
/// makes `:x` and `:y` the same, and `:x` is in `:D` as `:y` is, complete. HermiT answers
/// "not entailed" here: the reference misses it, and the store must not be changed to
/// agree. On the rules route and off it (with `OUTSIDE_RL`), in both equality modes. The
/// `owl:sameAs` read answers `true` on both routes; it is complete off the rules route
/// only, because on it a read of the `owl:` vocabulary is never complete (OWL 2 entails
/// statements of it that no closure holds in full).
#[test]
fn a_key_reads_a_value_a_property_chain_implies() {
    let data = ":p1 a owl:ObjectProperty . :p2 a owl:ObjectProperty . \
         :p3 a owl:ObjectProperty ; owl:propertyChainAxiom ( :p1 :p2 ) . \
         :C a owl:Class ; owl:hasKey ( :p3 ) . :D a owl:Class . \
         :x a owl:NamedIndividual , :C ; :p1 :b . :b a owl:NamedIndividual ; :p2 :z . \
         :y a owl:NamedIndividual , :C , :D ; :p3 :z . :z a owl:NamedIndividual .";
    let mut wrong = Vec::new();
    for (more, rules) in [("", true), (OUTSIDE_RL, false)] {
        for compact in [false, true] {
            let dl = store(compact);
            insert(&dl, &format!("{data} {more}")).expect("consistent");
            let route = dl.store().dl_bounds().last;
            for read in ["ASK { :x a :D }", "ASK { :x owl:sameAs :y }"] {
                let (rows, status) = query(&dl, read);
                let line = format!(
                    "{more} / compact {compact}, route {route}: {read} answers {rows:?}, complete {}",
                    status.is_complete()
                );
                eprintln!("{line}");
                let complete = !(rules && read.contains("sameAs"));
                if rows != ["true"]
                    || (complete && !status.is_complete())
                    || (route == "rules") != rules
                {
                    wrong.push(line);
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A generated case, shrunk, in its two commits: the second is refused as inconsistent.
/// `:C3` is at most one `:d1` value in `{ 1 }`, which no individual can exceed, so every
/// individual is in `:C3`. `_:k7` is `:a1` (two values of the functional `:pk`), so the key
/// on `:C3` over `:p3` makes `:a1` and `:a2` the same (both reach `:a3`). The second commit's
/// chain `:p1 o :p2` with `:a0 :p1 :a1` and `:a1 :p2 :a3` (`:a2 :p2 :a3`, `:a2` being
/// `:a1`) gives `:a0 :p3 :a3`, and the key makes `:a0` the same as `:a1`, against
/// `:a1 owl:differentFrom :a0`. The key over the non-simple `:p3` is inside OWL 2 DL, as
/// the row above says. HermiT and Openllet both call the case consistent until the chain's
/// conclusion is asserted: the references miss it, and the store must not be changed to
/// agree. The restriction on `:C3` is outside OWL 2 RL, so only the route off the rules
/// reaches it; in both equality modes.
#[test]
fn a_key_over_a_chain_implied_value_refuses_an_inconsistent_write() {
    let prefixes = "PREFIX f: <http://example.org/fuzz#> \
         PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";
    let first = "f:C0 rdf:type owl:Class . f:C1 rdf:type owl:Class . \
         f:C2 rdf:type owl:Class . f:C3 rdf:type owl:Class . \
         f:p0 rdf:type owl:ObjectProperty . f:p1 rdf:type owl:ObjectProperty . \
         f:p2 rdf:type owl:ObjectProperty . f:p3 rdf:type owl:ObjectProperty . \
         f:d0 rdf:type owl:DatatypeProperty . f:d1 rdf:type owl:DatatypeProperty . \
         f:a0 rdf:type owl:NamedIndividual . f:a1 rdf:type owl:NamedIndividual . \
         f:a2 rdf:type owl:NamedIndividual . f:a3 rdf:type owl:NamedIndividual . \
         _:w120 rdf:type owl:Restriction . _:w120 owl:onProperty f:d1 . \
         _:w122 rdf:first \"1\"^^xsd:integer . _:w122 rdf:rest rdf:nil . \
         _:w123 rdf:type rdfs:Datatype . _:w123 owl:oneOf _:w122 . \
         _:w120 owl:maxQualifiedCardinality \"1\"^^xsd:nonNegativeInteger . \
         _:w120 owl:onDataRange _:w123 . _:w120 owl:equivalentClass f:C3 . \
         f:a1 owl:differentFrom f:a0 . \
         f:C3 owl:hasKey ( f:p3 ) . f:pk a owl:ObjectProperty , owl:FunctionalProperty . \
         _:k7 a f:C3 ; f:p3 f:a3 . [] f:pk _:k7 , f:a1 . f:a2 a f:C3 ; f:p3 f:a3 .";
    let second = "f:p3 owl:propertyChainAxiom ( f:p1 f:p2 ) . \
         f:a0 f:p1 f:a1 . f:a1 f:p2 f:a2 . \
         f:C2 owl:hasKey ( f:p2 ) . f:pk a owl:ObjectProperty , owl:FunctionalProperty . \
         _:k8 a f:C2 ; f:p2 f:a3 . _:k9 a f:C1 . [] f:pk _:k8 , _:k9 . \
         f:a2 a f:C2 ; f:p2 f:a3 .";
    let mut wrong = Vec::new();
    for compact in [false, true] {
        let dl = store(compact);
        let one = update(&dl, &format!("{prefixes}INSERT DATA {{ {first} }}"));
        let route = dl.store().dl_bounds().last;
        let two = update(&dl, &format!("{prefixes}INSERT DATA {{ {second} }}"));
        let line = format!("compact {compact}: first {one:?}, route {route}; second {two:?}");
        eprintln!("{line}");
        let refused = matches!(&two, Err(nrese_store::MutationError::Rejected(r))
            if r.kind == nrese_store::RejectKind::Inconsistent);
        if one.is_err() || route == "rules" || !refused {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Under `owl2-rl` the key rule (prp-key, OWL 2 Profiles §4.3, Table 5) has no condition on
/// the value: `_:z` binds `:a` and `:b`, and `:a a :D` holds. The named-only keys are
/// `owl2-dl`'s; `owl2-rl`'s program keeps the fingerprint its stores were written with.
#[test]
fn owl2_rl_keeps_its_key_rule_and_its_fingerprint() {
    let rl = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl);
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    let pipeline =
        MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(rl.clone())));
    insert(&pipeline, &object_key("_:z", "")).expect("consistent");
    assert!(ask(&pipeline, ":a a :D"));
    let program = rl.materialised_program().expect("rules");
    assert_eq!(
        format!("{:016x}", program.fingerprint()),
        OWL2_RL_FINGERPRINT
    );
}
