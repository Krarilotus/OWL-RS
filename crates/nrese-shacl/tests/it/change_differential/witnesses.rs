use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use nrese_rdf::Quad;
use nrese_rdf_io::{RdfFormat, RdfParser};
use serde::Deserialize;

use super::case::{Case, Step};
use super::gaps::Gap;
use super::observe::{Deviation, compare};
use super::stops::Outcome;

#[derive(Deserialize)]
struct Registry {
    gap: Vec<Registered>,
}

#[derive(Deserialize)]
struct Registered {
    #[serde(flatten)]
    entry: Gap,
    says: String,
    limits: String,
    witness: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Witness {
    case: String,
    selection: String,
    equality: bool,
    shapes: Option<String>,
    #[serde(default)]
    asserted: String,
    #[serde(default)]
    inferred: String,
    steps: Vec<Edit>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Edit {
    add: String,
    remove: String,
    infer: String,
    retract: String,
    shapes: Option<String>,
}

fn parse(format: RdfFormat, text: &str) -> Vec<Quad> {
    RdfParser::from_format(format)
        .for_slice(text.as_bytes())
        .map(|quad| quad.expect("witness RDF"))
        .collect()
}

impl Witness {
    fn case(self) -> Case {
        assert_eq!(self.selection, "any-excluding-shapes");
        Case {
            asserted: parse(RdfFormat::NQuads, &self.asserted),
            inferred: parse(RdfFormat::NTriples, &self.inferred),
            shapes: self.shapes,
            steps: self
                .steps
                .into_iter()
                .map(|step| Step {
                    add: parse(RdfFormat::NQuads, &step.add),
                    remove: parse(RdfFormat::NQuads, &step.remove),
                    infer: parse(RdfFormat::NTriples, &step.infer),
                    retract: parse(RdfFormat::NTriples, &step.retract),
                    shapes: step.shapes,
                })
                .collect(),
            fresh_value: false,
            equality: self.equality,
            stop: None,
        }
    }
}

pub(super) fn verified_gaps() -> Vec<Gap> {
    let registry: Registry = toml::from_str(include_str!("known-gaps.toml")).expect("gap registry");
    let mut ids = BTreeSet::new();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/it/change_differential/cases");
    for record in &registry.gap {
        let gap = &record.entry;
        assert!(ids.insert(&gap.id), "duplicate gap {}", gap.id);
        assert!(!record.says.is_empty() && record.limits.starts_with("docs/limits.md#"));
        assert!(gap.case.starts_with("row:") || gap.case.starts_with("saved:"));
        let path = Path::new(&record.witness);
        assert_eq!(path.file_name(), Some(path.as_os_str()), "witness filename");
        let text = fs::read_to_string(dir.join(path)).expect("read gap witness");
        let witness: Witness = toml::from_str(&text).expect("parse gap witness");
        assert_eq!(witness.case, gap.case);
        let mut deviation = None;
        for outcome in witness.case().run().expect("run gap witness") {
            let Outcome::Report(actual) = &outcome.actual else {
                panic!("{}: witness stopped: {:?}", gap.id, outcome.actual);
            };
            let raw = compare(&outcome.before, &outcome.after, actual);
            if raw != Deviation::default() {
                assert_eq!(
                    (outcome.step, outcome.entry),
                    (gap.step, gap.entry),
                    "{}: changed",
                    gap.id
                );
                assert!(deviation.replace(raw).is_none(), "{}: changed", gap.id);
            }
        }
        gap.verify_witness(&deviation.unwrap_or_default())
            .unwrap_or_else(|error| panic!("{}: {error}", gap.id));
    }
    registry
        .gap
        .into_iter()
        .map(|record| record.entry)
        .collect()
}
