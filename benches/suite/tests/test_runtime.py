"""The runtime's helpers (suitekit/runtime.py).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from suitekit.runtime import run_quiet  # noqa: E402


class Scopes(unittest.TestCase):
    """The kit rule of 14 Oct 2026 (a whole-host OOM): a scope the kit starts joins the
    fleet's step slice where FLEET_SLICE is set, and the user's default slice where not."""

    def test_a_scope_joins_the_fleet_slice(self):
        import os
        from unittest.mock import patch
        from suitekit.runtime import scope_argv
        with patch.dict(os.environ, {"FLEET_SLICE": "fleet-step-7.slice"}):
            argv = scope_argv("case-1", "6g")
        self.assertIn("--slice=fleet-step-7.slice", argv)
        self.assertLess(argv.index("--slice=fleet-step-7.slice"), argv.index("--"))
        self.assertIn("MemoryMax=6G", argv)
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("FLEET_SLICE", None)
            argv = scope_argv("case-1", "6g")
        self.assertFalse(any(a.startswith("--slice") for a in argv))


class RunQuiet(unittest.TestCase):
    def test_output_is_read_as_utf8_whatever_the_console_code_page(self):
        # 0x9d has no character in Windows' cp1252: read with the console's code page, the
        # reader thread failed and stdout was None (a container log of Jena's, 12 Oct 2026).
        script = "import sys; sys.stdout.buffer.write('– ok'.encode() + bytes([0x9d]))"
        done = run_quiet([sys.executable, "-c", script])
        self.assertIsInstance(done.stdout, str)
        self.assertTrue(done.stdout.startswith("– ok"))


class EraProducer(unittest.TestCase):
    """Spec: Root's typed StepNeeds decision binds the immutable create argv to
    one actual engine/converter, 16/12/4 GiB and one host-resolved CPU; no fake launch.
    """

    def test_canonical_producer_renders_consumable_steps(self):
        import importlib.util
        import json
        from types import SimpleNamespace
        path = Path(__file__).resolve().parents[2] / 'breadth/era-shacl/run.py'
        spec = importlib.util.spec_from_file_location('era_contract', path)
        era = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(era)
        self.assertTrue(callable(getattr(era, 'fleet_job', None)),
                        'ERA needs its actual canonical prepare/consume producer, absent on baseline')
        args = SimpleNamespace(data='/cache/era-shacl', results='/results/diagnostic',
            job_id='era-diagnostic', owner='t3:fixture', source_git='source.git', source_rev='a'*40,
            engines='jena,topbraid', subsets='ES', shapes='tds,core', runs=3,
            converter_image='sha256:'+'b'*64, cpu_pool='8-15,24-31', host='office-pc')
        job = era.fleet_job(args)
        self.assertEqual(job['spec'], 7)
        self.assertEqual(len(job['steps']), 24)
        cases = []
        for step, need in zip(job['steps'], job['step_needs']):
            self.assertEqual(step[:6], ['python3', '-B', 'benches/breadth/era-shacl/run.py', 'consume', '--case', step[5]])
            case = json.loads(step[5])
            self.assertEqual(len(step), 6, 'ordinary argv contains only the driver')
            self.assertNotIn('argv', case, 'the agent request is the single resolved plan owner')
            actual = need['docker']['argv']
            self.assertEqual(actual[:2], ['docker', 'create'])
            self.assertIn('--pull=never', actual, 'missing cache is a refusal, never an uncapped pull')
            self.assertEqual(need, dict(cores=1, ram_gb=16, docker=dict(ram_gb=12, argv=actual)))
            for flag, value in (('--memory','12g'), ('--memory-swap','12g'), ('--cpus','1'),
                                ('--cpuset-cpus','@FLEET_CPU@'), ('--name','@FLEET_CONTAINER_NAME@')):
                self.assertEqual(actual[actual.index(flag)+1], value)
            self.assertRegex(case['image'], r'^sha256:[0-9a-f]{64}$')
            self.assertIn(case['image'], actual)
            self.assertEqual(case['timeout_s'], 2700)
            cases.append(case)
        self.assertEqual([c['phase'] for c in cases], ['engine','convert']*12)
        self.assertEqual([(c['rep'], c['shape'], c['engine']) for c in cases[::2]],
            [(1,'tds','jena'),(1,'tds','topbraid'),(1,'core','jena'),(1,'core','topbraid'),
             (2,'tds','topbraid'),(2,'core','jena'),(2,'core','topbraid'),(2,'tds','jena'),
             (3,'core','jena'),(3,'core','topbraid'),(3,'tds','jena'),(3,'tds','topbraid')])
        args.converter_image = None
        no_converter = era.fleet_job(args)
        self.assertEqual(len(no_converter['steps']), 12)
        self.assertIn('unavailable', json.loads(no_converter['steps'][0][5])['report_count'])

    def test_native_prebuilt_plan_retains_source_and_readonly_artifacts(self):
        # Spec: Root's image decision binds a native prebuilt perf_lab and the
        # canonical adapter to an immutable Debian runtime; no hidden image build.
        import importlib.util
        import json
        import tempfile
        from types import SimpleNamespace
        path = Path(__file__).resolve().parents[2] / 'breadth/era-shacl/run.py'
        spec = importlib.util.spec_from_file_location('era_native_contract', path)
        era = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(era)
        self.assertTrue(callable(getattr(era, 'fleet_job', None)),
                        'native prebuilt ERA execution needs an admitted-plan producer')
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / 'native.json'
            manifest.write_text(json.dumps(dict(source='a'*40, host='office-pc', image='sha256:'+'c'*64,
                binary=dict(path='/cache/bin/release/examples/perf_lab', sha256='d'*64),
                identity=dict(path='/cache/bin/IDENTITY', sha256='e'*64))))
            args = SimpleNamespace(data='/cache/era-shacl', results='/results/native',
                job_id='era-native', owner='t3:fixture', source_git='source.git', source_rev='b'*40,
                engines='nrese', subsets='ES', shapes='tds', runs=1, converter_image=None,
                cpu_pool='8-15,24-31', host='office-pc', native_manifest=str(manifest))
            try:
                job = era.fleet_job(args)
            except ValueError as error:
                self.fail(f'authorized native prebuilt route must render an actual bound plan: {error}')
            case = json.loads(job['steps'][0][5])
            argv = job['step_needs'][0]['docker']['argv']
            self.assertEqual(case['native']['source'], 'a'*40)
            self.assertEqual(case['source'], 'b'*40, 'harness source and binary source remain distinct')
            self.assertIn('/cache/bin/release/examples/perf_lab:/usr/local/bin/perf_lab:ro', argv)
            self.assertIn('/results/native/native/nrese-era.sh:/usr/local/bin/nrese-era.sh:ro', argv)
            self.assertEqual(argv[argv.index('--entrypoint')+1], '/bin/sh')
            self.assertIn('ldd /usr/local/bin/perf_lab', ' '.join(argv))
            self.assertRegex(case['native']['script_sha256'], r'^[0-9a-f]{64}$')


if __name__ == "__main__":
    unittest.main()
