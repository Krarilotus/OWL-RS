//! The datatype checks (dt-diff, dt-not-type) under each `reasoner.equality` setting
//! (`representatives`, `replicate`, `compact`), under `owl2-rl` and `owl2-dl`, by every way
//! data reach a full closure and by commits.
//!
//! Spec: OWL 2 RL/RDF rules (OWL 2 Profiles §4.3): prp-fp (table 5) makes two values of a
//! functional property `owl:sameAs`; eq-trans and eq-sym (table 4) make every member of an
//! `owl:sameAs` class equal to every other; dt-diff (table 8) makes `lt1 owl:sameAs lt2` a
//! clash where the two literals' data values differ, and dt-not-type makes `lt rdf:type dt`
//! a clash where `lt` is not in `dt`'s value space. Data values are compared by value, not
//! by lexical form (OWL 2 Structural Specification §4: the value space of `xsd:integer` is a
//! subset of that of `xsd:decimal`, so `42` and `42.0` are one value).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    ConsistencyStatus, MutationCommand, MutationPipeline, MutationTicket, SparqlUpdateRequest,
    StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

const SCHEMA: &str = ":p a owl:DatatypeProperty, owl:FunctionalProperty .";

const SETTINGS: [&str; 3] = ["representatives", "replicate", "compact"];

const MODES: [ReasoningMode; 2] = [ReasoningMode::Owl2Rl, ReasoningMode::Owl2Dl];

const WAYS: [&str; 2] = ["loaded", "written with reasoning off"];

/// An integer past the range the datatype checks represent (2^127): a literal no check
/// judges, neither a value nor ill-typed.
const UNJUDGED: &str = "\"170141183460469231731687303715884105728\"^^xsd:integer";

/// A clashing data set: its name, what is written (one write after another, so a literal of
/// an earlier write has the smaller id), and the clashing pairs among its literals.
struct Clash {
    name: &'static str,
    writes: Vec<String>,
    pairs: usize,
}

/// Every clashing data set the tests read.
///
/// The values of a functional property, written after its schema one value a write: two
/// integers, two strings, and three literals of which two are one value, in each order (so
/// the pair that differs is sometimes a member against the class's representative,
/// sometimes two members, whichever literal that is).
///
/// Then classes whose representative holds no value: an IRI (`a`), a literal no check
/// judges written first (`b`), an ill-typed literal written first (`b-ill-typed`), and a
/// range the class's representative fits and another member doesn't (`c`).
fn clashes() -> Vec<Clash> {
    let values = |name, values: &[&str], pairs| Clash {
        name,
        writes: std::iter::once(SCHEMA.to_owned())
            .chain(values.iter().map(|v| format!(":x :p {v} .")))
            .collect(),
        pairs,
    };
    let first = |name, seed: &str, rest: &str| Clash {
        name,
        writes: vec![
            format!(":seed :q {seed} ."),
            format!("{SCHEMA} :x :p {seed}, {rest} ."),
        ],
        pairs: 1,
    };
    let plus = "\"+42\"^^xsd:integer, \"+43\"^^xsd:integer";
    vec![
        values("ints", &["42", "43"], 1),
        values("strings", &["\"a\"", "\"b\""], 1),
        values("three", &["42", "\"42.0\"^^xsd:decimal", "43"], 2),
        values("three again", &["43", "42", "\"42.0\"^^xsd:decimal"], 2),
        values("three once more", &["\"42.0\"^^xsd:decimal", "43", "42"], 2),
        Clash {
            name: "a",
            writes: vec![":a owl:sameAs 42, 43 .".to_owned()],
            pairs: 1,
        },
        first("b", UNJUDGED, plus),
        first("b-ill-typed", "\"x\"^^xsd:integer", plus),
        Clash {
            name: "c",
            writes: vec![format!(
                "{SCHEMA} :p rdfs:range xsd:integer . :x :p 42, \"bad\"^^xsd:integer ."
            )],
            pairs: 1,
        },
    ]
}

/// One data value in two lexical forms.
const EQUAL: &[&str] = &["\"42\"^^xsd:integer", "\"42.0\"^^xsd:decimal"];

fn store(setting: &str) -> Arc<StoreService> {
    Arc::new(
        StoreService::new(StoreConfig {
            equality_by_representatives: setting != "replicate",
            equality_compact: setting == "compact",
            ..crate::support::in_memory_store_config()
        })
        .expect("store"),
    )
}

fn pipeline(store: &Arc<StoreService>, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn commit(pipeline: &MutationPipeline, data: &str) -> bool {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{PREFIXES}INSERT DATA {{ {data} }}"
            ))),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .is_ok()
}

fn holds(store: &StoreService, data: &str) -> bool {
    let answer = store
        .execute_query_str(&format!("{PREFIXES}ASK {{ {data} }}"))
        .expect("asked");
    String::from_utf8(answer.payload)
        .expect("utf8")
        .contains("true")
}

fn data(values: &[&str]) -> String {
    format!("{SCHEMA} :x :p {} .", values.join(", "))
}

/// The ways data reach a full closure: loaded past the commit gates (the bulk loader, as
/// `nrese-server load` and a start-up rebuild see them), or written with reasoning off, then
/// rebuilt with `mode`'s rules. Whether every write arrived, the rebuild's violations, then
/// the store's verdict.
fn rebuilt(setting: &str, mode: ReasoningMode, way: &str, writes: &[String]) -> String {
    let store = store(setting);
    let _reasoning = pipeline(&store, mode);
    let off = pipeline(&store, ReasoningMode::Disabled);
    for written in writes {
        match way {
            "loaded" => {
                crate::support::load_past_the_pipeline(&store, &format!("{PREFIXES}{written}"))
            }
            _ => assert!(commit(&off, written)),
        }
    }
    if let Some(missing) = writes.iter().find(|w| !holds(&store, w)) {
        return format!("not in the store: {missing}");
    }
    let program = ReasonerConfig::for_mode(mode)
        .materialised_program()
        .expect("a materialised program");
    let report = store.rematerialise(program).expect("rebuilt");
    let consistency = match store.consistency() {
        ConsistencyStatus::Consistent => "consistent",
        ConsistencyStatus::Inconsistent { .. } => "inconsistent",
        _ => "other",
    };
    format!(
        "violations {}, {consistency}, quarantined {}",
        report.violations,
        store.is_quarantined()
    )
}

/// Spec: a functional data property with two different values, an `owl:sameAs` class
/// holding two different values or an ill-typed member under a range, is a clash whichever
/// member of the class is its representative (an IRI, a literal no check judges, an
/// ill-typed literal): every full closure finds it, under every equality setting, and the
/// store is inconsistent and in quarantine. Each pair is reported once: a closure listing
/// every equality reports every clashing pair; a compact one at least one per class, and
/// never more than there are.
#[test]
fn a_class_holding_two_different_values_clashes_in_every_full_closure() {
    let mut wrong = Vec::new();
    for setting in SETTINGS {
        for mode in MODES {
            for way in WAYS {
                for clash in clashes() {
                    let found = rebuilt(setting, mode, way, &clash.writes);
                    let expected: Vec<String> = if setting == "compact" {
                        (1..=clash.pairs).collect()
                    } else {
                        vec![clash.pairs]
                    }
                    .into_iter()
                    .map(|n| format!("violations {n}, inconsistent, quarantined true"))
                    .collect();
                    if !expected.contains(&found) {
                        wrong.push(format!(
                            "{setting}, {mode:?}, {way}, {}: {found}",
                            clash.name
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec: one data value in two lexical forms (`42` as `xsd:integer` and as `xsd:decimal`) is
/// no clash: no full closure reports one, under any equality setting.
#[test]
fn one_value_in_two_lexical_forms_is_no_clash_in_any_full_closure() {
    let mut wrong = Vec::new();
    for setting in SETTINGS {
        for mode in MODES {
            for way in WAYS {
                let found = rebuilt(setting, mode, way, &[data(EQUAL)]);
                if found != "violations 0, consistent, quarantined false" {
                    wrong.push(format!("{setting}, {mode:?}, {way}: {found}"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (the commits' side of the same rules): a commit that makes a clash is refused, as
/// one commit or as its writes one after another (each functional value its own commit),
/// under every equality setting; a second lexical form of the same value is accepted.
#[test]
fn a_commit_making_a_datatype_clash_is_refused() {
    let mut wrong = Vec::new();
    for setting in SETTINGS {
        for mode in MODES {
            for clash in clashes() {
                let one = store(setting);
                if commit(&pipeline(&one, mode), &clash.writes.join(" ")) {
                    wrong.push(format!(
                        "{setting}, {mode:?}, {}: one commit accepted",
                        clash.name
                    ));
                }
                let each = store(setting);
                let writer = pipeline(&each, mode);
                if clash.writes.iter().all(|w| commit(&writer, w)) {
                    wrong.push(format!(
                        "{setting}, {mode:?}, {}: every commit accepted",
                        clash.name
                    ));
                }
            }
            let equal = store(setting);
            if !commit(&pipeline(&equal, mode), &data(EQUAL)) {
                wrong.push(format!(
                    "{setting}, {mode:?}: one value in two forms refused"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
