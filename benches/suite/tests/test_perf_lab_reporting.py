"""Opt-in native contract: report the existing legacy observations without adding work."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('reporting', ROOT / 'benches/fast/patches/perf_lab_reporting.py')
reporting = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reporting)

FIXTURE = r'''
use std::{cell::Cell, path::PathBuf, time::{Duration, Instant}};
mod nrese_store { pub const DEFAULT_SHAPES_GRAPH: &str = "shapes"; pub enum ReadScope { All } }
#[derive(Clone, Copy)] struct Ruleset;
impl Ruleset { fn name(&self) -> &str { "owl2-rl" } }
struct MaterialisationReport { inferred: u64, rounds: usize, elapsed: Duration }
struct ValidationReport { results: Vec<()> }
struct Validation { report: ValidationReport }
enum GraphTarget { NamedGraph(String) }
struct BulkLoadRequest { files: Vec<String>, replace: bool, graph: GraphTarget, skip_errors: bool }
#[derive(Default)] struct ShaclValidationRequest;
struct Store { reason: Cell<usize>, validate: Cell<usize> }
impl Store {
 fn rematerialise(&self, _: Ruleset) -> Result<MaterialisationReport, Box<dyn std::error::Error>> {
  self.reason.set(self.reason.get() + 1);
  Ok(MaterialisationReport { inferred: 7, rounds: 2, elapsed: Duration::from_millis(42) })
 }
 fn bulk_load(&self, _: &BulkLoadRequest) -> Result<(), Box<dyn std::error::Error>> { Ok(()) }
 fn validate_shacl(&self, _: &nrese_store::ReadScope, _: &ShaclValidationRequest) -> Result<Validation, Box<dyn std::error::Error>> {
  self.validate.set(self.validate.get() + 1);
  Ok(Validation { report: ValidationReport { results: vec![(); 3] } })
 }
}
struct Args { reason: Option<Ruleset>, shapes: Option<String>, runs: usize, json: Option<PathBuf>, label: String }
fn ms(d: Duration) -> f64 { d.as_secs_f64() * 1e3 }
fn main() -> Result<(), Box<dyn std::error::Error>> {
 let args = Args { reason: Some(Ruleset), shapes: Some("shapes.ttl".into()), runs: 2,
  json: Some(std::env::args().nth(1).unwrap().into()), label: "original".into() };
 let store = Store { reason: Cell::new(0), validate: Cell::new(0) };
 let open_s = 0.125; let load_s = 0.25; let sum_p50 = 99.0;
 let memory: Option<(u64, u64)> = None;
 let report = vec![r#"{"name":"sentinel","rows":5,"p50_ms":99.0}"#.to_owned()];
__PHASES__
__WRITE__
 println!("reason_calls={} validation_calls={}", store.reason.get(), store.validate.get());
 Ok(())
}
'''


class LegacyReporting(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('NRESE_REPORTING_NATIVE_PROOF') == '1', 'official native proof only')
    def test_existing_observations_reach_json_without_extra_work(self):
        text = subprocess.check_output(['git', '-C', str(ROOT), 'show',
            'f9b515e81f013ecd8c3811f3e9c65e91450ee44c:crates/nrese-store/examples/perf_lab.rs'], text=True)
        if os.environ.get('NRESE_REPORTING_APPLY', '1') == '1':
            text, outcome = reporting.patch(text)
            self.assertEqual(outcome, 'applied')
        begin = text.index('    if let Some(ruleset) = args.reason {')
        marker = text.rfind('    // Legacy phase observations', 0, begin)
        begin = marker if marker >= 0 else begin
        end = text.index('    let memory_after_load', begin)
        write = text[text.index('    if let Some(path) = &args.json {'):]
        write = write[:write.index('\n    Ok(())')]
        source = FIXTURE.replace('__PHASES__', text[begin:end]).replace('__WRITE__', write)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            rust = directory / 'contract.rs'
            binary = directory / 'contract'
            output = directory / 'report.json'
            rust.write_text(source)
            subprocess.run(['rustc', '--edition=2024', '-Awarnings', str(rust), '-o', str(binary)], check=True)
            ran = subprocess.run([str(binary), str(output)], capture_output=True, text=True, check=True)
            self.assertIn('reason_calls=1 validation_calls=2', ran.stdout)
            result = json.loads(output.read_text())
            self.assertIn('reason', result, 'legacy phase observations missing from JSON')
            self.assertEqual(result['reason'], {'inferred': 7, 'rounds': 2, 'runs': 1,
                'p50_ms': 42.0, 'max_ms': 42.0, 'samples_ms': [42.0]})
            shacl = result['shacl']
            self.assertEqual((shacl['results'], shacl['runs'], len(shacl['samples_ms'])), (3, 2, 2))
            self.assertEqual(shacl['p50_ms'], max(shacl['samples_ms']))
            self.assertEqual({k: v for k, v in result.items() if k not in ('reason', 'shacl')},
                {'label': 'original', 'open_s': 0.125, 'load_s': 0.25, 'peak_mib': 0,
                 'sum_p50_ms': 99.0, 'queries': [{'name': 'sentinel', 'rows': 5, 'p50_ms': 99.0}]})


if __name__ == '__main__':
    unittest.main()
