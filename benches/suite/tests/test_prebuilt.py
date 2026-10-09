"""Preflight and adapter contracts; no builds, containers or real license reads."""
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit.driver import Suite
from suitekit.adapters import Context, Graphdb
from suitekit.adapters import Endpoint, Step
from suitekit.manifest import Manifest
from suitekit.runtime import Measured, Mount
from suitekit.workloads import write_scaling


class PrebuiltHarness(unittest.TestCase):
    def test_skip_build_never_launches_a_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'harness'
            binary.write_bytes(b'prebuilt fixture')
            for path, expected in [(root / 'missing', False), (root, False), (binary, True)]:
                with self.subTest(path=path):
                    suite = Suite.__new__(Suite)
                    suite.args = SimpleNamespace(skip_build=True)
                    suite.harness = lambda: str(path)
                    suite.say = Mock()
                    suite.host_command = Mock(side_effect=AssertionError('unexpected build'))
                    self.assertEqual(suite.build_harness(), expected)
                    suite.host_command.assert_not_called()
                    if not expected:
                        self.assertIn('--skip-build requires an existing harness', suite.say.call_args.args[0])


class GraphdbContract(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        work = Path(self.scratch.name)
        logs = work / 'logs'
        logs.mkdir()
        runtime = Mock(kind='docker', dry=True)
        runtime.run.return_value = Measured(0, None, 0)
        self.ctx = Context(runtime, Path(__file__).resolve().parents[3], work, logs,
                           Mount('/fixture-data', '/data'), [], 30,
                           {'GRAPHDB_LICENSE': '/private/key-that-must-not-be-read',
                            'DOCKER_MEMORY': '4g', 'JAVA_HEAP': '2g'}, 'contract')
        self.adapter = Graphdb()

    def test_native_equality_follows_entailment_regime(self):
        for regime in self.adapter.regimes:
            with self.subTest(regime=regime):
                config = self.adapter.configuration(self.ctx, regime)
                disabled = 'true' if regime in ('none', 'rdfs') else 'false'
                self.assertIn(f'graphdb:disable-sameAs "{disabled}"', config)
                self.assertIn(f'graphdb:ruleset "{self.adapter.regimes[regime]}"', config)
                self.assertIn('graphdb:enablePredicateList "true"', config)
                self.assertIn('graphdb:enable-literal-index "true"', config)
                self.assertIn('graphdb:query-limit-results "0"', config)
                self.assertIn('graphdb:throw-QueryEvaluationException-on-timeout "true"', config)

    def test_enterprise_does_not_force_wide_ids(self):
        self.ctx.settings['GRAPHDB_EDITION'] = 'enterprise'
        self.assertEqual(self.adapter.repository_settings(self.ctx, 'owl2-rl')['entity-id-size'], '32')
        self.ctx.settings.update(GRAPHDB_ENTITY_ID_SIZE='40', GRAPHDB_ENTITY_INDEX_SIZE='2147483647',
                                 GRAPHDB_CONTEXT_INDEX='true', GRAPHDB_CHECK_INCONSISTENCIES='true')
        config = self.adapter.configuration(self.ctx, 'owl2-rl')
        for text in ('entity-id-size "40"', 'entity-index-size "2147483647"',
                     'enable-context-index "true"', 'check-for-inconsistencies "true"'):
            self.assertIn(text, config)
        self.ctx.settings['GRAPHDB_EDITION'] = 'free'
        self.assertIn('Enterprise license', self.adapter.supports(self.ctx, [], 'owl2-rl'))

    def test_preflight_returns_reasons_without_starting_or_reading_license(self):
        for setting, value in [('GRAPHDB_EDITION', 'unknown'), ('GRAPHDB_ENTITY_ID_SIZE', '64'),
                               ('GRAPHDB_ENTITY_INDEX_SIZE', '0'), ('GRAPHDB_ENTITY_INDEX_SIZE', '-1'),
                               ('GRAPHDB_ENTITY_INDEX_SIZE', '2147483648'),
                               ('GRAPHDB_CONTEXT_INDEX', 'true; injected'),
                               ('GRAPHDB_PAGE_CACHE_SIZE', '1g -Dunsafe=yes'),
                               ('GRAPHDB_QUERY_MEMORY_THRESHOLD', '0'),
                               ('GRAPHDB_INFERENCE_CONCURRENCY', '2m')]:
            with self.subTest(setting=setting, value=value):
                self.ctx.settings[setting] = value
                self.assertIn(setting, self.adapter.supports(self.ctx, [], 'owl2-rl'))
                del self.ctx.settings[setting]
        self.assertIn('ruleset', self.adapter.supports(self.ctx, [], 'owl2-dl'))
        self.ctx.runtime.kind = 'process'
        self.assertIn('no process path', self.adapter.supports(self.ctx, [], 'owl2-dl'))
        self.ctx.runtime.run.assert_not_called()
        self.ctx.runtime.start.assert_not_called()

    def test_documented_sizes_and_inference_options(self):
        self.ctx.settings.update(GRAPHDB_PAGE_CACHE_SIZE='3G', GRAPHDB_QUERY_MEMORY_THRESHOLD='250m',
                                 GRAPHDB_MAX_DIRECT_MEMORY='256M', GRAPHDB_INFERENCE_CONCURRENCY='1',
                                 GRAPHDB_INFERENCE_BUFFER='200000')
        options = self.adapter.env(self.ctx, '-Dgraphdb.home=/work/home')['GDB_JAVA_OPTS']
        for text in ('-Dgraphdb.home=/work/home', '-Dgraphdb.page.cache.size=3221225472',
                     '-Dgraphdb.query.memory.threshold=262144000', '-XX:MaxDirectMemorySize=256M',
                     '-Dgraphdb.inference.concurrency=1', '-Dgraphdb.inference.buffer=200000'):
            self.assertIn(text, options)
        for value, expected in [('5g', '5368709120'), ('1024K', '1048576'), ('12345678901', '12345678901')]:
            self.ctx.settings['GRAPHDB_PAGE_CACHE_SIZE'] = value
            self.assertIn('page.cache.size=' + expected, self.adapter.env(self.ctx, '')['GDB_JAVA_OPTS'])
        self.ctx.settings['GRAPHDB_INFERENCE_BUFFER'] = '12345678901234567890'
        self.assertIn('inference.buffer=12345678901234567890', self.adapter.env(self.ctx, '')['GDB_JAVA_OPTS'])

    @patch('suitekit.adapters.urllib.request.urlopen')
    def test_version_is_runtime_metadata_not_a_digest_tag(self, request):
        self.ctx.runtime.dry = False
        request.return_value.__enter__.return_value.read.return_value = json.dumps(
            dict(productVersion='11.5.1', productType='free', unrelated='do not retain')).encode()
        self.assertEqual(self.adapter.version(self.ctx, Endpoint('http://fixture/repositories/bench')), '11.5.1')
        self.assertEqual(json.loads((self.ctx.logs / 'graphdb-runtime.json').read_text()),
                         dict(productVersion='11.5.1', productType='free'))

    def test_import_and_server_preserve_reproducible_configuration(self):
        for regime in ('none', 'owl2-rl'):
            with self.subTest(regime=regime):
                self.adapter.load(self.ctx, '/own-store', ['/data/tiny.nt'], regime)
                spec = self.ctx.runtime.run.call_args.args[0]
                self.assertIn('preload' if regime == 'none' else 'load', spec.command)
                self.assertEqual('-s' in spec.command, regime != 'none')
                self.assertIn('-Dgraphdb.license.file=/license/graphdb.license', spec.env['GDB_JAVA_OPTS'])
                self.assertEqual(spec.memory, '4g')
                self.assertTrue(next(m for m in spec.mounts if m.target == '/license/graphdb.license').readonly)
                self.assertEqual((self.ctx.work / 'repo.ttl').read_bytes(),
                                 (self.ctx.logs / 'graphdb-repo.ttl').read_bytes())
        self.adapter.start = Mock()
        self.ctx.runtime.url.return_value = 'http://fixture'
        self.adapter.serve(self.ctx, '/own-store', 'owl2-rl')
        for phase in ('load', 'serve'):
            raw = (self.ctx.logs / f'graphdb-{phase}-config.json').read_text()
            record = json.loads(raw)
            self.assertEqual(record['image'], 'ontotext/graphdb:11.5.1')
            self.assertEqual(record['requested_edition'], 'free')
            self.assertEqual(record['env']['GDB_HEAP_SIZE'], '2g')
            self.assertIn('graphdb.license.file=<set>', record['env']['GDB_JAVA_OPTS'])
            self.assertNotIn('/private/', raw)

    @patch('suitekit.manifest.machine', return_value={})
    @patch('suitekit.manifest.git', return_value={'commit': 'fixture'})
    def test_manifest_records_explicit_tuning_and_redacts_license(self, _git, _machine):
        settings = {**self.ctx.settings, **self.adapter.defaults,
                    'GRAPHDB_PAGE_CACHE_SIZE': '3G', 'GRAPHDB_QUERY_MEMORY_THRESHOLD': '250m',
                    'GRAPHDB_MAX_DIRECT_MEMORY': '256M', 'GRAPHDB_INFERENCE_CONCURRENCY': '1',
                    'GRAPHDB_INFERENCE_BUFFER': '200000'}
        args = SimpleNamespace(runs=1, query_runs=1, cache='off,on', order='shuffled', seed=1,
                               timeout_s=30, query_timeout_s=10)
        manifest = Manifest(self.ctx.logs, self.ctx.root, [], args, 'docker', settings)
        recorded = manifest.entry['settings']
        for key, value in settings.items():
            self.assertEqual(recorded[key], 'set' if key == 'GRAPHDB_LICENSE' else value)
        self.assertNotIn('/private/', manifest.path.read_text())
        defaults = Manifest(self.ctx.logs, self.ctx.root, [], args, 'docker', {})
        self.assertEqual(defaults.entry['settings'], {})


class WriteScalingChecks(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.path = Path(self.scratch.name)
        self.suite = Suite.__new__(Suite)
        self.suite.harness = lambda: 'prebuilt-harness'
        self.suite.emit = Mock()
        self.ctx = SimpleNamespace(logs=self.path)
        self.base = dict(system='graphdb', tier='5,7,8')
        self.endpoint = Endpoint('http://fixture/repositories/bench')

    def report(self):
        records = []
        for requested, actual, loaded, persons, probes in [(5, 4, 4, 1, 1), (7, 4, 0, 1, 2), (8, 8, 4, 2, 3)]:
            records.append(dict(triples=requested, asserted_entity_triples=actual, loaded_triples=loaded,
                                expected_persons=persons, observed_persons=persons,
                                expected_probes=probes, observed_probes=probes))
        return dict(mode='write-scaling', samples_per_step=1,
                    services=[dict(label='GraphDB', reset=True, steps=records)])

    def run_report(self, report):
        def execute(command, log):
            if report is not None:
                (self.path / 'write-scaling.json').write_text(json.dumps(report), encoding='utf-8')
            return 0
        self.suite.host_command = Mock(side_effect=execute)
        self.suite.writes(self.ctx, self.endpoint, self.base)
        return self.suite.emit.call_args.kwargs['status']

    def test_graphdb_runs_alone_and_checks_rounded_entity_counts(self):
        self.assertEqual(self.run_report(self.report()), 'ok')
        command = self.suite.host_command.call_args.args[0]
        self.assertNotIn('--nrese-base-url', command)
        self.assertEqual(command[command.index('--reference-base-url') + 1], self.endpoint.query)
        self.assertEqual(command[command.index('--reset') + 1], 'true')

    def test_http_success_cannot_promote_bad_or_legacy_reports(self):
        self.assertEqual(self.run_report(None), 'failed')
        for fault in ['missing', 'persons', 'probes', 'floor', 'repeat', 'service', 'reset', 'boolean']:
            with self.subTest(fault=fault):
                report = self.report()
                service = report['services'][0]
                row = service['steps'][1]
                if fault == 'missing': del row['observed_probes']
                elif fault == 'persons': row['observed_persons'] = 0
                elif fault == 'probes': row['observed_probes'] = 1
                elif fault == 'floor': row['loaded_triples'] = 2
                elif fault == 'repeat': service['steps'].pop()
                elif fault == 'service': service['label'] = 'NRESE'
                elif fault == 'reset': service['reset'] = False
                elif fault == 'boolean': row['observed_persons'] = True
                self.assertEqual(self.run_report(report), 'failed')

    def test_graphdb_setup_failure_stops_before_serving_or_writing(self):
        suite = self.suite
        suite.stamp, suite.date, suite.host = 'test', 'test', 'fixture'
        suite.scratch, suite.results, suite.root = self.path / 'scratch', self.path / 'results', self.path
        suite.data, suite.settings = Mount('/data', '/data'), {}
        suite.runtime = Mock(kind='docker')
        suite.runtime.new_store.return_value = 'owned-store'
        suite.args = SimpleNamespace(timeout_s=30, query_timeout_s=10)
        suite.say, suite.writes, suite.publish = Mock(), Mock(), lambda key: 'permission'
        system = Mock()
        system.load.return_value = Step(Measured(1, None, 1), note='failed empty import')
        suite.cycle(write_scaling(self.path, '1m', {}), 'graphdb', system, 'none', 1)
        self.assertEqual(system.load.call_args.args[2], ['/work/empty.nt'])
        system.serve.assert_not_called()
        suite.writes.assert_not_called()
        suite.runtime.remove_store.assert_called_once_with('owned-store')
        self.assertEqual(suite.emit.call_args.kwargs['status'], 'failed')
