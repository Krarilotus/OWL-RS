use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use nrese_rdf::Quad;
use nrese_rdf_io::{RdfFormat, RdfParser};
use serde::{Deserialize, Serialize};

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
    /// Only for a deviation documented as a limit.
    #[serde(default)]
    limits: Option<String>,
    witness: String,
}

/// A saved case: a gap's witness, or a failing draw's fixture with its signature and the
/// full command that replays it.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Witness {
    case: String,
    selection: String,
    equality: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    shapes: Option<String>,
    #[serde(default)]
    asserted: String,
    #[serde(default)]
    inferred: String,
    steps: Vec<Edit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replay: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Edit {
    add: String,
    remove: String,
    infer: String,
    retract: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    shapes: Option<String>,
}

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/it/change_differential/cases")
}

/// The saved case `file`: a path, or a file name under `cases/`.
pub(super) fn saved_path(file: &str) -> PathBuf {
    cases_dir().join(file)
}

/// The saved case `file` and the signature it recorded, if any.
pub(super) fn load(file: &str) -> (Case, Option<String>) {
    let path = saved_path(file);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read saved case {}: {error}", path.display()));
    let mut witness: Witness = toml::from_str(&text).expect("parse saved case");
    let signature = witness.signature.take();
    (witness.case(), signature)
}

/// `case` as a fixture named `id`, with the signature it shows and its replay command.
pub(super) fn fixture(id: &str, case: &Case, signature: &str, replay: &str) -> String {
    let lines = |quads: &[Quad]| {
        quads
            .iter()
            .map(|quad| format!("{quad} .\n"))
            .collect::<String>()
    };
    let witness = Witness {
        case: id.into(),
        selection: "any-excluding-shapes".into(),
        equality: case.equality,
        shapes: case.shapes.clone(),
        asserted: lines(&case.asserted),
        inferred: lines(&case.inferred),
        steps: case
            .steps
            .iter()
            .map(|step| Edit {
                add: lines(&step.add),
                remove: lines(&step.remove),
                infer: lines(&step.infer),
                retract: lines(&step.retract),
                shapes: step.shapes.clone(),
            })
            .collect(),
        signature: Some(signature.into()),
        replay: Some(replay.into()),
    };
    toml::to_string(&witness).expect("fixture TOML")
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
        assert!(
            !self.steps.is_empty(),
            "{}: a saved case has steps",
            self.case
        );
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
            cancellation: None,
        }
    }
}

pub(super) fn verified_gaps() -> Vec<Gap> {
    let registry: Registry = toml::from_str(include_str!("known-gaps.toml")).expect("gap registry");
    let mut ids = BTreeSet::new();
    let dir = cases_dir();
    for record in &registry.gap {
        let gap = &record.entry;
        assert!(ids.insert(&gap.id), "duplicate gap {}", gap.id);
        assert!(!record.says.is_empty(), "{}: says nothing", gap.id);
        assert!(
            record
                .limits
                .as_ref()
                .is_none_or(|anchor| anchor.starts_with("docs/limits.md#")),
            "{}: limits anchor outside docs/limits.md",
            gap.id
        );
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
