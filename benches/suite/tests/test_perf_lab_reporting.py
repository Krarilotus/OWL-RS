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
enum GraphTarget { NamedGraph(String), DefaultGraph }
struct BulkLoadReport { inserted: u64 }
struct BulkLoadRequest { files: Vec<String>, replace: bool, graph: GraphTarget, skip_errors: bool }
#[derive(Default)] struct ShaclValidationRequest;
struct Store { reason: Cell<usize>, validate: Cell<usize>, loads: Cell<usize> }
impl Store {
 fn rematerialise(&self, _: Ruleset) -> Result<MaterialisationReport, Box<dyn std::error::Error>> {
  self.reason.set(self.reason.get() + 1);
  Ok(MaterialisationReport { inferred: 7, rounds: 2, elapsed: Duration::from_millis(42) })
 }
 fn bulk_load(&self, _: &BulkLoadRequest) -> Result<BulkLoadReport, Box<dyn std::error::Error>> {
  self.loads.set(self.loads.get() + 1); Ok(BulkLoadReport { inserted: 17 })
 }
 fn validate_shacl(&self, _: &nrese_store::ReadScope, _: &ShaclValidationRequest) -> Result<Validation, Box<dyn std::error::Error>> {
  self.validate.set(self.validate.get() + 1);
  Ok(Validation { report: ValidationReport { results: vec![(); 3] } })
 }
}
struct Args { load: Vec<String>, reason: Option<Ruleset>, shapes: Option<String>, runs: usize, json: Option<PathBuf>, label: String }
fn ms(d: Duration) -> f64 { d.as_secs_f64() * 1e3 }
fn main() -> Result<(), Box<dyn std::error::Error>> {
 let args = Args { load: vec!["input.nt".into()], reason: Some(Ruleset), shapes: Some("shapes.ttl".into()), runs: 2,
  json: Some(std::env::args().nth(1).unwrap().into()), label: "original".into() };
 let store = Store { reason: Cell::new(0), validate: Cell::new(0), loads: Cell::new(0) };
 let open_s = 0.125; let sum_p50 = 99.0;
 let memory: Option<(u64, u64)> = None;
 let report = vec![r#"{"name":"sentinel","rows":5,"p50_ms":99.0}"#.to_owned()];
__PHASES__
__WRITE__
 println!("reason_calls={} validation_calls={}", store.reason.get(), store.validate.get());
 println!("load_calls={}", store.loads.get());
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
        begin = text.index('    let mut load_s = 0.0;')
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
            self.assertIn('load_calls=2', ran.stdout)
            result = json.loads(output.read_text())
            self.assertEqual(result.get('loaded'), 17, 'actual inserted load count missing from JSON')
            self.assertIn('reason', result, 'legacy phase observations missing from JSON')
            self.assertEqual(result['reason'], {'inferred': 7, 'rounds': 2, 'runs': 1,
                'p50_ms': 42.0, 'max_ms': 42.0, 'samples_ms': [42.0]})
            shacl = result['shacl']
            self.assertEqual((shacl['results'], shacl['runs'], len(shacl['samples_ms'])), (3, 2, 2))
            self.assertEqual(shacl['p50_ms'], max(shacl['samples_ms']))
            self.assertGreaterEqual(result.pop('load_s'), 0)
            self.assertEqual({k: v for k, v in result.items() if k not in ('reason', 'shacl', 'loaded')},
                {'label': 'original', 'open_s': 0.125, 'peak_mib': 0,
                 'sum_p50_ms': 99.0, 'queries': [{'name': 'sentinel', 'rows': 5, 'p50_ms': 99.0}]})

    @unittest.skipUnless(os.environ.get('NRESE_REPORTING_COMMON201_PROOF') == '1', 'official common201 proof only')
    # Spec: R1-F1/Johannes ordinary default JSON SELECT+ASK; scalar TSV, unchanged samples/calls/errors.
    def test_common201_actual_output_and_execution_contract(self):
        _common201_contract(self)


COMMON201_FIXTURE = r'''
use std::{cell::Cell, io::Write, time::{Duration, Instant}};
#[derive(Clone, Copy, PartialEq, Eq)] enum QueryResultKind { Solutions, Boolean, Graph }
#[derive(Clone, Copy)] enum SolutionsResultFormat { Tsv, Json }
struct SparqlQueryRequest { text: String, solutions_format: SolutionsResultFormat }
impl SparqlQueryRequest { fn all(text: &str) -> Self { Self { text: text.into(), solutions_format: SolutionsResultFormat::Json } } }
struct PreparedQuery { text: String, solutions_format: SolutionsResultFormat }
impl PreparedQuery {
 fn parse(r: &SparqlQueryRequest) -> Result<Self, String> { Ok(Self { text: r.text.clone(), solutions_format:r.solutions_format }) }
 fn kind(&self) -> QueryResultKind { if self.text == "ask" { QueryResultKind::Boolean } else { QueryResultKind::Solutions } }
}
#[derive(Clone)] struct CancellationToken;
impl CancellationToken { fn new() -> Self { Self } fn cancel(&self) {} }
struct Step { operator: String }
struct Explanation { steps: Vec<Step> }
struct StoreService { runs: Cell<usize>, explains: Cell<usize>, serialized:std::cell::RefCell<Vec<serde_json::Value>> }
impl StoreService {
 fn run_query(&self, q: &PreparedQuery, _: &CancellationToken, mut out: impl Write) -> Result<(), String> {
  use nrese_sparql_results::{QueryResultsFormat, QueryResultsSerializer};
  use nrese_rdf::{Literal, Variable};
  let n=self.runs.get()+1; self.runs.set(n);
  let (format,label)=match q.solutions_format {
   SolutionsResultFormat::Tsv => (QueryResultsFormat::Tsv,"tsv"),
   SolutionsResultFormat::Json => (QueryResultsFormat::Json,"json"),
  };
  if q.text=="diagnostic_error" && n==5 {
   self.serialized.borrow_mut().push(serde_json::json!({"format":label,"output":null,"bytes":0,"error":"actual scalar refused"}));
   return Err("actual scalar refused".into());
  }
  let mut bytes=Vec::new();
  let serializer=QueryResultsSerializer::from_format(format);
  if q.text=="ask" {
   serializer.serialize_boolean_to_writer(&mut bytes,true).map_err(|e|e.to_string())?;
  } else {
   let mut writer=serializer.serialize_solutions_to_writer(&mut bytes,vec![Variable::new("x").unwrap()]).map_err(|e|e.to_string())?;
   let values=match q.text.as_str() {
    "escaped" => vec![Literal::new_simple_literal("a\"b\tline\nend")],
    "many" => vec![Literal::from(37_i64),Literal::from(38_i64)],
    _ => vec![Literal::from(37_i64)],
   };
   for value in &values { writer.serialize([("x",value)]).map_err(|e|e.to_string())?; }
   writer.finish().map_err(|e|e.to_string())?;
  }
  // Both scalar Vec and ordinary CountingSink receive actual frozen serializer bytes.
  for chunk in bytes.chunks(3) { out.write_all(chunk).map_err(|e|e.to_string())?; }
  self.serialized.borrow_mut().push(serde_json::json!({"format":label,"output":String::from_utf8(bytes.clone()).unwrap(),"bytes":bytes.len(),"error":null}));
  Ok(())
 }
 fn explain_query(&self, q: &PreparedQuery, _: &CancellationToken) -> Result<Explanation,String> {
  let n=self.explains.get()+1;self.explains.set(n);
  if q.text=="diagnostic_error" && n==2 { return Err("actual operator refused".into()); }
  let names=if n==1 { vec!["actual-first-plan"] } else { vec!["actual-second-plan", "group", "actual-second-plan"] };
  Ok(Explanation { steps:names.into_iter().map(|s|Step { operator:s.into() }).collect() })
 }
}
struct Args { runs: usize, warmup: usize, timeout: Duration, format: SolutionsResultFormat }
__ACTUAL_COUNTING_MEASURE__
__ACTUAL_COMMON_HELPER__
fn ms(d: Duration) -> f64 { d.as_secs_f64()*1e3 }
fn main() {
 let args=Args { runs:3,warmup:1,timeout:Duration::from_secs(2),format:SolutionsResultFormat::Tsv };
 let mut records=Vec::new();
 for text in ["typed","escaped","many","ask","diagnostic_error"] {
  let text=text.to_owned(); let name=text.clone();
  let store=StoreService { runs:Cell::new(0),explains:Cell::new(0),serialized:std::cell::RefCell::new(Vec::new()) };
  let m=measure(&store,&text,&args);
  assert!(m.error.is_none());
  let p50=ms(m.times[1]);let min=ms(m.times[0]);let max=ms(m.times[2]);
  let mut report=Vec::new();
__ACTUAL_QUERY_REPORT__
  let query:serde_json::Value=serde_json::from_str(&report[0]).expect("actual query report is complete JSON");
  records.push(serde_json::json!({"query":query,"run_calls":store.runs.get(),
   "explain_calls":store.explains.get(),"timed_samples":m.times.len(),"serializations":store.serialized.borrow().clone()}));
 }
 println!("{}",serde_json::to_string(&records).unwrap());
}
'''


def _common201_contract(test):
    """One table: actual scalar/escapes, Solutions gate, real routes/counts, failures/default."""
    import hashlib
    source = subprocess.check_output(['git', '-C', str(ROOT), 'show',
        'f9b515e81f013ecd8c3811f3e9c65e91450ee44c:crates/nrese-store/examples/perf_lab.rs'], text=True)
    owner = Path(reporting.__file__).read_text()
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        root = Path(os.environ['NRESE_COMMON201_DEP_SOURCE'])
        cargo = ('[package]\nname="reporting-common201-contract"\nversion="0.0.0"\nedition="2024"\n'
                 '[workspace]\n[dependencies]\nserde_json="1.0.145"\n'
                 f'nrese-rdf={{path="{root.as_posix()}/crates/rdf/nrese-rdf",default-features=false}}\n'
                 f'nrese-sparql-results={{path="{root.as_posix()}/crates/rdf/nrese-sparql-results"}}\n')
        (directory/'Cargo.toml').write_text(cargo)
        (directory/'src').mkdir()
        for profile in ('loaded','common201'):
            namespace={'__name__':'reporting_contract'}
            exec(owner.replace("PROFILE = 'loaded'",f"PROFILE = '{profile}'"),namespace)
            patched,outcome=namespace['patch'](source)
            test.assertIn(outcome,('applied','present'))
            measurement=patched[patched.index('/// Counts bytes and lines written, discarding the data.'):patched.index('\nfn ms(')]
            helper=patched[patched.index('// COMMON21 Option B only:'):patched.index('/// Counts bytes and lines written, discarding the data.')] if '// COMMON21 Option B only:' in patched else ''
            begin=patched.index('        let diagnostics = common201_reporting::observe(') if helper else patched.index('        report.push(format!(',patched.index('        let p50 = ms(m.times'))
            end=patched.index('\n        ));',begin)+len('\n        ));')
            fixture=COMMON201_FIXTURE.replace('__ACTUAL_COUNTING_MEASURE__',measurement).replace('__ACTUAL_COMMON_HELPER__',helper).replace('__ACTUAL_QUERY_REPORT__',patched[begin:end])
            (directory/'src/main.rs').write_text(fixture)
            build_env={**os.environ,'RUSTFLAGS':'','CARGO_ENCODED_RUSTFLAGS':''}
            stage=os.environ['NRESE_REPORTING_PROOF_STAGE']
            evidence=Path(os.environ['FLEET_OUT'])/'fixtures'/stage/profile
            evidence.mkdir(parents=True,exist_ok=True)
            locked=subprocess.run(['cargo','generate-lockfile','--manifest-path',str(directory/'Cargo.toml')],cwd=directory,env=build_env,capture_output=True,text=True)
            (evidence/'lock-stderr.log').write_text(locked.stderr)
            test.assertEqual(locked.returncode,0,locked.stderr)
            lock=(directory/'Cargo.lock').read_bytes()
            rustc=subprocess.check_output(['rustc','-vV'],text=True)
            parser_tree=subprocess.check_output(['git','-C',str(ROOT),'rev-parse','f9b515e81f013ecd8c3811f3e9c65e91450ee44c:crates/rdf'],text=True).strip()
            identity=hashlib.sha256(fixture.encode()+cargo.encode()+lock+rustc.encode()+parser_tree.encode()).hexdigest()
            target=Path(os.environ['NRESE_COMMON201_TARGET_ROOT'])/stage/profile/identity
            ran=subprocess.run(['cargo','run','--quiet','--manifest-path',str(directory/'Cargo.toml'),'--target-dir',str(target)],cwd=directory,env=build_env,capture_output=True,text=True)
            (evidence/'main.rs').write_text(fixture);(evidence/'Cargo.toml').write_text(cargo)
            (evidence/'Cargo.lock').write_bytes(lock);(evidence/'stdout.json').write_text(ran.stdout)
            (evidence/'stderr.log').write_text(ran.stderr)
            test.assertEqual(ran.returncode,0,ran.stderr)
            binary=target/'debug/reporting-common201-contract'
            proof={'profile':profile,'stage':stage,'identity':identity,
                'source_sha256':hashlib.sha256(fixture.encode()).hexdigest(),
                'cargo_lock_sha256':hashlib.sha256(lock).hexdigest(),
                'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
                'binary_path':str(binary),'rustc':rustc,'rustflags':'','cargo_encoded_rustflags':'',
                'dependency_parser_tree':parser_tree,
                'dependency_source':'f9b515e81f013ecd8c3811f3e9c65e91450ee44c:crates/rdf; source-only workspace member narrowing'}
            (evidence/'identity.json').write_text(json.dumps(proof,indent=2))
            records=json.loads(ran.stdout)
            test.assertIsInstance(records,list)
            test.assertEqual([r['query']['name'] for r in records],['typed','escaped','many','ask','diagnostic_error'])
            for record in records:
                test.assertEqual(set(record),{'query','run_calls','explain_calls','timed_samples','serializations'})
                name=record['query']['name']
                q=record['query']; test.assertEqual(record['timed_samples'],3)
                test.assertEqual(q['rows'],2 if name=='many' else 1)

                # Spec: R1-F1/Johannes: ordinary SELECT+ASK use default JSON; scalar capture stays TSV.
                serial=record['serializations']
                test.assertEqual(len(serial),record['run_calls'])
                measured_format='json' if name=='ask' else 'tsv'
                test.assertEqual([s['format'] for s in serial[:4]],[measured_format]*4)
                expected_tsv={'typed':'?x\n37\n','escaped':'?x\n"a\\"b\\tline\\nend"\n',
                              'many':'?x\n37\n38\n','diagnostic_error':'?x\n37\n'}
                expected_json={
                    'typed':r'{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"literal","value":"37","datatype":"http://www.w3.org/2001/XMLSchema#integer"}}]}}',
                    'escaped':r'{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"literal","value":"a\"b\tline\nend"}}]}}',
                    'many':r'{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"literal","value":"37","datatype":"http://www.w3.org/2001/XMLSchema#integer"}},{"x":{"type":"literal","value":"38","datatype":"http://www.w3.org/2001/XMLSchema#integer"}}]}}',
                    'ask':r'{"head":{},"boolean":true}'}
                expected_json['diagnostic_error']=expected_json['typed']
                if profile=='common201':
                    test.assertEqual(serial[-1]['format'],'json','R1-F1 ordinary diagnostic must serialize default JSON')
                    test.assertEqual(serial[-1]['output'],expected_json[name],'R1-F1 ordinary JSON bytes mismatch')
                    if name not in ('many','ask'):
                        test.assertEqual(serial[4]['format'],'tsv','scalar capture must remain TSV')
                for call in serial:
                    if call['error'] is not None:
                        test.assertEqual((name,call['output'],call['bytes']),('diagnostic_error',None,0))
                        continue
                    expected_bytes=expected_json[name] if call['format']=='json' else expected_tsv[name]
                    test.assertEqual(call['output'],expected_bytes)
                    test.assertEqual(call['bytes'],len(expected_bytes.encode('utf-8')))

                if profile=='loaded':
                    test.assertEqual((record['run_calls'],record['explain_calls']),(4,0))
                    test.assertNotIn('value',q);test.assertNotIn('common201',q)
                    continue
                test.assertIn('common201',q,'common201 diagnostics missing from actual query report')
                c=q['common201'];scalar=name not in ('many','ask')
                test.assertEqual((record['run_calls'],record['explain_calls']),(6 if scalar else 5,2))
                test.assertEqual((c['scalar_attempts'],c['explain_attempts'],c['ordinary_attempts']),(int(scalar),2,1))
                test.assertEqual(c['executions_attempted'],4 if scalar else 3)
                test.assertEqual(c['scalar_solutions_kind'],scalar)
                test.assertEqual(c['new_timed_samples'],0)
                test.assertFalse(c['status_available']);test.assertFalse(c['rewrites_available'])
                test.assertNotIn('status',q);test.assertNotIn('rewrites',q)
                if name=='diagnostic_error':
                    test.assertEqual(c['executions_completed'],2)
                    test.assertEqual(len(c['errors']),2);test.assertNotIn('value',q)
                    test.assertEqual(q['operators'],[])
                else:
                    test.assertEqual(c['executions_completed'],c['executions_attempted'])
                    test.assertEqual(c['errors'],[])
                    test.assertEqual(q['operators'],['actual-second-plan','group'])
                    if scalar: test.assertEqual(q['value'],'a"b\tline\nend' if name=='escaped' else '37')
                    else: test.assertNotIn('value',q)



if __name__ == '__main__':
    unittest.main()
