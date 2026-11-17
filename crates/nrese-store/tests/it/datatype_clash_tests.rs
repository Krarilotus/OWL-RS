//! A functional data property's values under each `reasoner.equality` setting
//! (`representatives`, `replicate`, `compact`), under `owl2-rl` and `owl2-dl`, by every way
//! data reach a full closure and by commits.
//!
//! Spec: OWL 2 RL/RDF rules (OWL 2 Profiles §4.3): prp-fp (table 5) makes two values of a
//! functional property `owl:sameAs`; dt-diff (table 8) makes `lt1 owl:sameAs lt2` a clash
//! where the two literals' data values differ. Data values are compared by value, not by
//! lexical form (OWL 2 Structural Specification §4: the value space of `xsd:integer` is a
//! subset of that of `xsd:decimal`, so `42` and `42.0` are one value).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    ConsistencyStatus, MutationCommand, MutationPipeline, MutationTicket, SparqlUpdateRequest,
    StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

const SCHEMA: &str = ":p a owl:DatatypeProperty, owl:FunctionalProperty .";

const SETTINGS: [&str; 3] = ["representatives", "replicate", "compact"];

const MODES: [ReasoningMode; 2] = [ReasoningMode::Owl2Rl, ReasoningMode::Owl2Dl];

/// Value sets with two different data values: two integers, two strings, and three literals
/// of which two are one value, in each order (so the pair that differs is sometimes a member
/// against the class's representative only, whichever literal that is).
const DIFFERENT: [&[&str]; 5] = [
    &["42", "43"],
    &["\"a\"", "\"b\""],
    &["42", "\"42.0\"^^xsd:decimal", "43"],
    &["43", "42", "\"42.0\"^^xsd:decimal"],
    &["\"42.0\"^^xsd:decimal", "43", "42"],
];

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

fn data(values: &[&str]) -> String {
    format!("{SCHEMA} :x :p {} .", values.join(", "))
}

/// The ways data reach a full closure: loaded past the commit gates (the bulk loader, as
/// `nrese-server load` and a start-up rebuild see them), or written with reasoning off, then
/// rebuilt with `mode`'s rules. The rebuild's violations, then the store's verdict.
fn rebuilt(setting: &str, mode: ReasoningMode, way: &str, values: &[&str]) -> String {
    let store = store(setting);
    let written = data(values);
    let _reasoning = match way {
        "loaded" => {
            let reasoning = pipeline(&store, mode);
            crate::support::load_past_the_pipeline(&store, &format!("{PREFIXES}{written}"));
            reasoning
        }
        _ => {
            assert!(commit(&pipeline(&store, ReasoningMode::Disabled), &written));
            pipeline(&store, mode)
        }
    };
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
        report.violations.min(1),
        store.is_quarantined()
    )
}

/// Spec: two different values of a functional data property are a clash (prp-fp, then
/// dt-diff), whichever literal of the class is its representative: every full closure finds
/// it, under every equality setting, and the store is inconsistent and in quarantine.
#[test]
fn two_different_values_of_a_functional_data_property_clash_in_every_full_closure() {
    let mut wrong = Vec::new();
    for setting in SETTINGS {
        for mode in MODES {
            for way in ["loaded", "written with reasoning off"] {
                for values in DIFFERENT {
                    let found = rebuilt(setting, mode, way, values);
                    if found != "violations 1, inconsistent, quarantined true" {
                        wrong.push(format!("{setting}, {mode:?}, {way}, {values:?}: {found}"));
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
            for way in ["loaded", "written with reasoning off"] {
                let found = rebuilt(setting, mode, way, EQUAL);
                if found != "violations 0, consistent, quarantined false" {
                    wrong.push(format!("{setting}, {mode:?}, {way}: {found}"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (the commits' side of the same rules): a commit that gives a functional data property
/// a second, different value is refused, in one commit or one value per commit, under every
/// equality setting; a second lexical form of the same value is accepted.
#[test]
fn a_commit_giving_a_functional_data_property_a_different_value_is_refused() {
    let mut wrong = Vec::new();
    for setting in SETTINGS {
        for mode in MODES {
            for values in DIFFERENT {
                let one = store(setting);
                if commit(&pipeline(&one, mode), &data(values)) {
                    wrong.push(format!(
                        "{setting}, {mode:?}, {values:?}: one commit accepted"
                    ));
                }
                let each = store(setting);
                let writer = pipeline(&each, mode);
                let accepted: Vec<bool> = std::iter::once(commit(&writer, SCHEMA))
                    .chain(
                        values
                            .iter()
                            .map(|v| commit(&writer, &format!(":x :p {v} ."))),
                    )
                    .collect();
                if accepted.iter().all(|a| *a) {
                    wrong.push(format!(
                        "{setting}, {mode:?}, {values:?}: every commit accepted"
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
