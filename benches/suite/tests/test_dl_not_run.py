"""The DL kit's steps that did not run (D16-582). A task whose input is missing, whose program does
not start, or whose N-Triples the conversion did not make gets a `not-run` row with its reason, and
the driver exits 2; `ore.py compare` and `stats` never count such a row as an answer. The EL track's
conversion container starts as every suite container does (ns.run_args: in a fleet job its
`fleet.job` label and the declared memory; fleet Spec 9 stops a step at any other container), by the
image's pinned ID. The EL classifier killed at its timeout is a `timeout` row, and the next task runs."""
import contextlib
import csv
import importlib.util
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

DL = Path(__file__).resolve().parents[2] / "reasoning" / "dl"
SUITE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(DL))
sys.path.insert(0, str(SUITE))
from suitekit import hosts, ns  # noqa: E402

# `reference` resolves its image through Docker on import; a stand-in names it here.
sys.modules.setdefault("reference", SimpleNamespace(IMAGE="unused"))


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


nrese = load("nrese_not_run_contract", DL / "nrese.py")
ore = load("ore_not_run_contract", DL / "ore.py")

THING = "http://www.w3.org/2002/07/owl#Thing"
NOTHING = "http://www.w3.org/2002/07/owl#Nothing"
# A reference's canonical taxonomy of A ⊑ B (docs/design/owl2-dl.md §11), written by hand.
REFERENCE = (f"< urn:a urn:b\n< urn:b {THING}\n= {NOTHING} {NOTHING}\n= {THING} {THING}\n"
             "= urn:a urn:a\n= urn:b urn:b\n")
EL1, EL2 = "ore_ont_1-el-classification", "ore_ont_2-el-classification"
PINNED = "sha256:" + "ab" * 32
# A fleet job declaring one container of 2 GB and 2 CPUs: its id, and the limits the suite gives each container
# (a job sharing its host must declare CPUs: the fleet refuses a container without a limit there).
JOB = {"FLEET_JOB": "ore80-job-1", "NRESE_SUITE_MEMORY_GB": "2", "NRESE_SUITE_CPUS": "2"}


def ok(stdout="", stderr=""):
    return SimpleNamespace(returncode=0, stdout=stdout, stderr=stderr)


def drive(function, args):
    """The driver's exit status (or what it raised) and the rows of CASES/results.tsv after it."""
    try:
        status = function(args)
    except (Exception, SystemExit) as error:  # the red runs: the step's failure escaped the driver
        status = error
    rows = [l.split("\t") for l in (Path(args.cases) / "results.tsv").read_text(encoding="utf-8").splitlines()]
    return status, rows


def ours(rows, system):
    """The latest row of each task by `system`: {task: (status, detail)}."""
    return {r[0]: (r[3], r[5] if len(r) > 5 else "") for r in rows if r[1] == system}


def status_of(rows, system, task):
    return ours(rows, system).get(task, ("no row", ""))


def converting(command):
    """The runner's `batch` on the conversion manifest, every task converted: its N-Triples, then its
    `converted` row (Runner.java: the file is written before the task's row)."""
    work = Path(command[command.index("-v") + 1].rsplit(":", 1)[0])
    rows = ""
    for line in (work / "manifest.nt.tsv").read_text(encoding="utf-8").splitlines():
        task = line.split("\t")[0]
        (work / "tax" / f"{task}.nt").write_text("<urn:a> <urn:p> <urn:b> .\n", encoding="utf-8")
        rows += f"{task}\tconvert\tntriples\tconverted\t5\t{task}.nt\n"
    (work / "results.nt.tsv").write_text(rows, encoding="utf-8")
    return ok()


class ElTrack(unittest.TestCase):
    def cases(self, tmp):
        """Two EL tasks, each with a reference taxonomy, no N-Triples yet."""
        cases = Path(tmp)
        (cases / "tax").mkdir()
        rows = ""
        for task in (EL1, EL2):
            (cases / "tax" / f"{task}.hermit.tax").write_text(REFERENCE, encoding="utf-8")
            rows += f"{task}\thermit\tclassify\tclassified\t9\t\n"
        (cases / "results.tsv").write_text(rows, encoding="utf-8")
        return cases

    def classify(self, command):
        Path(command[command.index("--out") + 1]).write_text("urn:a\turn:b\n", encoding="utf-8")
        return ok(stderr="profile total=1\n")

    def run_el(self, cases, conversion, classifier=None, environ=None):
        """nrese.el on CASES, the conversion container answering `conversion(command)`, the classifier
        `classifier(command)` (default: A ⊑ B); its status, rows and the commands it gave."""
        commands = []

        def run(command, **kw):
            commands.append(command)
            reply = conversion(command) if "batch" in command else (classifier or self.classify)(command)
            if kw.get("check") and reply.returncode:  # subprocess.run(check=True)'s answer
                raise subprocess.CalledProcessError(reply.returncode, command)
            return reply

        args = SimpleNamespace(cases=str(cases), binary="classify", threads=1, timeout=60)
        with patch.object(nrese.subprocess, "run", side_effect=run), \
                patch.object(ns, "pin_images", return_value={}), \
                patch.object(ns, "image_id", return_value=PINNED), \
                patch.dict(os.environ, {k: v for k, v in os.environ.items() if k not in JOB} | (environ or {}),
                           clear=True):
            status, rows = drive(nrese.el, args)
        return status, rows, commands

    def test_a_conversion_the_fleet_refuses_leaves_not_run_rows_and_exit_2(self):
        # Spec: the master, 14 Nov 2026, D16-582: the conversion container stopped by the fleet (exit 125, as
        # ore80-nrese-5009ce9b-r3 showed) is no answer: every EL task is `not-run` with the reason.
        with tempfile.TemporaryDirectory() as tmp:
            cases = self.cases(tmp)
            status, rows, commands = self.run_el(
                cases, lambda c: SimpleNamespace(returncode=125, stdout="", stderr="refused by the fleet"))
        self.assertEqual(ours(rows, "nrese-el").keys(), {EL1, EL2})
        self.assertEqual({s for s, _ in ours(rows, "nrese-el").values()}, {"not-run"})
        self.assertEqual(status, 2)
        self.assertIn("exited 125", ours(rows, "nrese-el")[EL1][1])
        self.assertEqual([c for c in commands if "batch" not in c], [])

    def test_a_task_the_conversion_fails_is_not_run_and_the_others_classify(self):
        # Spec: the master, 14 Nov 2026: a task without its N-Triples is `not-run` with its reason; one the
        # runner converted is classified. The runner's row for a failed conversion (Runner.java:
        # status, then its message) names the reason.
        def conversion(command):
            work = Path(command[command.index("-v") + 1].rsplit(":", 1)[0])
            (work / "tax" / f"{EL2}.nt").write_text("<urn:a> <urn:p> <urn:b> .\n", encoding="utf-8")
            (work / "results.nt.tsv").write_text(f"{EL1}\tconvert\tntriples\terror\t5\tparse error at line 3\n"
                                                 f"{EL2}\tconvert\tntriples\tconverted\t5\t{EL2}.nt\n",
                                                 encoding="utf-8")
            return ok()

        with tempfile.TemporaryDirectory() as tmp:
            status, rows, _ = self.run_el(self.cases(tmp), conversion)
        found = ours(rows, "nrese-el")
        self.assertEqual({t: s for t, (s, _) in found.items()}, {EL1: "not-run", EL2: "classified"})
        self.assertEqual(status, 2)
        self.assertIn("parse error at line 3", found[EL1][1])

    def test_the_conversion_container_carries_the_job_label_the_declared_limits_and_the_pinned_id(self):
        # Spec: fleet Spec 9 (a container in a declared job's step needs `fleet.job=<id>`, a declared memory and,
        # in a job sharing its host, the declared CPUs: ore80-nrese-06886882-r1 stopped at a container without a
        # CPU limit); the master, 14 Nov 2026: through ns.run_args and the pinned image ID, never the tag.
        with tempfile.TemporaryDirectory() as tmp:
            _, _, commands = self.run_el(self.cases(tmp), converting, environ=JOB)
        start = next(c for c in commands if "batch" in c)
        self.assertEqual(start[1], "run")
        self.assertIn("fleet.job=ore80-job-1", [start[i + 1] for i, a in enumerate(start) if a == "--label"])
        self.assertEqual(start[start.index("--memory") + 1], str(2 * 2**30) + "b")
        self.assertEqual(start[start.index("--memory-swap") + 1], str(2 * 2**30) + "b")
        self.assertEqual(cpu_limits(start), ["2"])
        self.assertEqual(start[start.index(PINNED) + 1], "batch")
        self.assertNotIn(nrese.reference.IMAGE, start)

    def test_the_classifier_killed_at_its_timeout_is_a_timeout_and_the_next_task_runs(self):
        # Spec: the master, 14 Nov 2026 (the EL timeout fixed in the same place): `classify` has no budget of its
        # own; killed at --timeout, the row says `timeout`, and the run goes on.
        def classifier(command):
            if command[-1].endswith(f"{EL1}.nt"):
                raise subprocess.TimeoutExpired(command, 60)
            return self.classify(command)

        with tempfile.TemporaryDirectory() as tmp:
            status, rows, _ = self.run_el(self.cases(tmp), converting, classifier)
        self.assertEqual({t: s for t, (s, _) in ours(rows, "nrese-el").items()}, {EL1: "timeout", EL2: "classified"})
        self.assertEqual(status, 0)

    def test_a_file_a_failed_conversion_left_is_converted_again_never_classified(self):
        # Spec: nrese.py's docstring (review round 1, R1): only a completed conversion's file is an input.
        # The runner writes into the final file before its row, so a timeout (its exit 3, Runner.java) or a
        # kill leaves a partial file; the next run converts that task again instead of classifying it.
        manifests = []

        def interrupted(command):  # EL1's file half-written, then the runner's timeout row and exit 3
            work = Path(command[command.index("-v") + 1].rsplit(":", 1)[0])
            manifests.append((work / "manifest.nt.tsv").read_text(encoding="utf-8"))
            (work / "tax" / f"{EL1}.nt").write_text("<urn:a> <urn:p>", encoding="utf-8")
            (work / "results.nt.tsv").write_text(f"{EL1}\tconvert\tntriples\ttimeout\t600000\t\n", encoding="utf-8")
            return SimpleNamespace(returncode=3, stdout="", stderr="")

        def complete(command):
            work = Path(command[command.index("-v") + 1].rsplit(":", 1)[0])
            manifests.append((work / "manifest.nt.tsv").read_text(encoding="utf-8"))
            return converting(command)

        with tempfile.TemporaryDirectory() as tmp:
            cases = self.cases(tmp)
            first, _, _ = self.run_el(cases, interrupted)
            second, rows, _ = self.run_el(cases, complete)
        self.assertEqual([line.split("\t")[0] for line in manifests[-1].splitlines()], [EL1, EL2])
        self.assertEqual({t: s for t, (s, _) in ours(rows, "nrese-el").items()}, {EL1: "classified", EL2: "classified"})
        self.assertEqual((first, second), (2, 0))


class DlTracks(unittest.TestCase):
    def cases(self, tmp, kind, with_source=True, earlier=""):
        cases = Path(tmp)
        (cases / "files").mkdir()
        (cases / "tax").mkdir()
        task = f"ore_ont_7-dl-{kind}"
        if with_source:
            (cases / "files" / "ore_ont_7.owl").write_text("Ontology()\n", encoding="utf-8")
        reference_kind = "classify" if kind == "classification" else kind
        (cases / "results.tsv").write_text(f"{task}\thermit\t{reference_kind}\tconsistent\t9\t\n{earlier}",
                                           encoding="utf-8")
        return cases, task

    def test_a_dl_step_that_does_not_run_is_a_not_run_row_and_the_driver_exits_2(self):
        # Spec: the master, 14 Nov 2026, D16-582: a missing input or a program that does not start is no
        # outcome of the reasoner's (before: `error`, or the driver stopped), and the job sees exit 2. Exit 127
        # is the loader's when a library is missing (review round 1, R2): the program never ran either.
        def not_found(command, **_):
            raise FileNotFoundError(2, "No such file or directory")

        def not_loaded(command, **_):
            return SimpleNamespace(returncode=127, stdout="",
                                   stderr="dl_classify: error while loading shared libraries: libz.so.1\n")

        for name, with_source, program in (("its input missing", False, lambda command, **_: ok()),
                                           ("its program not found", True, not_found),
                                           ("its program not loaded (exit 127)", True, not_loaded)):
            with self.subTest(name), tempfile.TemporaryDirectory() as tmp:
                cases, task = self.cases(tmp, "classification", with_source)
                calls = []

                def fake(command, **kw):
                    calls.append(command)
                    return program(command, **kw)

                argv = ["nrese.py", "dl", str(cases), "--binary", str(Path(tmp) / "dl_classify")]
                with patch.object(nrese.subprocess, "run", side_effect=fake), patch.object(sys, "argv", argv):
                    status, rows = drive(lambda _: nrese.main(), SimpleNamespace(cases=str(cases)))
                self.assertEqual(status_of(rows, "nrese-dl", task)[0], "not-run")
                self.assertIsInstance(status, SystemExit)
                self.assertEqual(status.code, 2)
                self.assertEqual(len(calls), 1 if with_source else 0)

    def test_the_beyond_path_not_starting_is_not_run_naming_it(self):
        # Spec: the master, 14 Nov 2026: one launch path for the three drivers; the consistency track's second
        # program not starting is `not-run` with the path named, never the tableau's `unsupported`.
        refused = ok(stdout="ore_ont_7.owl\tunsupported\t-\t13 reader diagnostics (not OWL 2 DL, left out)\n")

        def run(command, **_):
            if command[0] == "dl_beyond":
                raise FileNotFoundError(2, "No such file or directory")
            return refused

        with tempfile.TemporaryDirectory() as tmp:
            cases, task = self.cases(tmp, "consistency")
            args = SimpleNamespace(cases=str(cases), binary="tableau_consistency", beyond="dl_beyond", only=None,
                                   timeout=300, again=False)
            with patch.object(nrese.subprocess, "run", side_effect=run):
                status, rows = drive(nrese.consistency, args)
        found = status_of(rows, "nrese-dl", task)
        self.assertEqual(found[0], "not-run")
        self.assertEqual(status, 2)
        self.assertTrue(found[1].startswith("beyond: dl_beyond did not start"), found[1])

    def test_a_task_not_run_before_is_run_again(self):
        # Spec: nrese.py's docstring: a `not-run` row is no answer, so a later run takes the task again
        # (a task with an answer is skipped unless --again); the latest row decides, as in ore.py (review
        # round 1, R3: an older answer, then a `--again` run that did not run).
        task_id = "ore_ont_7-dl-classification"
        not_run = f"{task_id}\tnrese-dl\tclassify\tnot-run\t0\tinput missing\n"
        answered = f"{task_id}\tnrese-dl\tclassify\tclassified\t5\th\n"
        for name, earlier in (("not run before", not_run), ("answered, then not run", answered + not_run)):
            with self.subTest(name), tempfile.TemporaryDirectory() as tmp:
                cases, task = self.cases(tmp, "classification", earlier=earlier)

                def run(command, **_):
                    Path(command[command.index("--tax") + 1]).write_text("= a a\n", encoding="utf-8")
                    return ok(stderr="1 triples; 1 classes, 0 subsumptions, 0 unsatisfiable, consistent\n")

                args = SimpleNamespace(cases=str(cases), binary="dl_classify", only=None, threads=1, timeout=300,
                                       again=False, extra=[])
                with patch.object(nrese.subprocess, "run", side_effect=run):
                    status, rows = drive(nrese.dl, args)
                self.assertEqual(status_of(rows, "nrese-dl", task)[0], "classified")
                self.assertEqual(status, 0)


class Readers(unittest.TestCase):
    def cases(self, tmp, results):
        cases = Path(tmp)
        (cases / "index.tsv").write_text("id\ttrack\tsplit\tstratum\n"
                                         "ore_ont_1\tel/classification\tdev\ts-el\n"
                                         "ore_ont_2\tel/classification\tdev\ts-el\n", encoding="utf-8")
        (cases / "results.tsv").write_text(results, encoding="utf-8")
        return cases

    def test_compare_counts_a_not_run_row_as_no_answer_and_exits_2(self):
        # Spec: the master, 14 Nov 2026 (`compare` counts them; impossible to read as an answer): the task's
        # verdict is the references', the not-run row's own verdict column says `not-run`; a task no reasoner
        # ran (EL2) is no `undecided` task either (review round 1, R5).
        h = "c" * 64
        results = (f"{EL1}\thermit\tclassify\tclassified\t9\t{h}\n{EL1}\telk\tclassify\tclassified\t9\t{h}\n"
                   f"{EL1}\tnrese-el\tclassify\tnot-run\t0\tno N-Triples: the conversion exited 125\n"
                   f"{EL2}\tnrese-el\tclassify\tnot-run\t0\tno N-Triples: the conversion exited 125\n")
        with tempfile.TemporaryDirectory() as tmp:
            cases = self.cases(tmp, results)
            table = Path(tmp) / "compare.tsv"
            printed = io.StringIO()
            with contextlib.redirect_stdout(printed):
                status = ore.compare(SimpleNamespace(cases=str(cases), out=str(table)))
            written = {(r["task"], r["reasoner"]): r["verdict"]
                       for r in csv.DictReader(table.read_text(encoding="utf-8").splitlines(), delimiter="\t")}
        self.assertEqual(written[(EL1, "nrese-el")], "not-run")
        self.assertEqual(written[(EL1, "hermit")], "agree(2)")
        self.assertEqual(written[(EL2, "nrese-el")], "not-run")
        self.assertIn("2 tasks: agree 1, not-run 1\n", printed.getvalue())
        self.assertEqual(status, 2)
        self.assertIn("solved: elk 1, hermit 1\n", printed.getvalue())
        self.assertIn("NOT RUN (no answer; the step did not run): nrese-el 2", printed.getvalue())

    def test_stats_leaves_not_run_tasks_out_of_solved_and_par2(self):
        # Spec: the master, 14 Nov 2026: a not-run task is no outcome, so neither unsolved at the limit nor in
        # PAR-2; its own column counts it, a reasoner's latest row of each task. By hand: nrese-el
        # ran one task (EL2, retried after a not-run row) in 2 s: solved 1/1, PAR-2 2.0 s, not run 1.
        results = (f"{EL1}\thermit\tclassify\tclassified\t1000\th\n{EL2}\thermit\tclassify\tclassified\t1000\th\n"
                   f"{EL1}\tnrese-el\tclassify\tnot-run\t0\tinput missing\n"
                   f"{EL2}\tnrese-el\tclassify\tnot-run\t0\tinput missing\n"
                   f"{EL2}\tnrese-el\tclassify\tclassified\t2000\th\n")
        with tempfile.TemporaryDirectory() as tmp:
            printed = io.StringIO()
            with contextlib.redirect_stdout(printed):
                ore.stats(SimpleNamespace(cases=str(self.cases(tmp, results)), limit_s=60, split="all", by="all"))
        lines = [[c.strip() for c in l.strip("|").split("|")] for l in printed.getvalue().splitlines()]
        header = lines[0]
        row = dict(zip(header, next(l for l in lines[2:] if l[1] == "nrese-el")))
        self.assertEqual((row["solved"], row["PAR-2 s"]), ("1/1", "2.0"))
        self.assertEqual(row["not run"], "1")


class ReferenceRunner(unittest.TestCase):
    def test_its_container_takes_the_declared_limits_in_a_job_and_12g_8_cpus_elsewhere(self):
        # Spec: fleet Spec 9 and ns.run_args: in a job declaring its containers, every suite container
        # has the declared memory and CPUs (any other stops the step); elsewhere the runner's 12g without swap
        # and 8 CPUs. The declared size asked in another unit is the same size; a different size or CPU count
        # starts nothing (review round 1, R6).
        with patch.object(hosts, "image", return_value="nrese-bench/dl-reference"):
            reference = load("reference_not_run_contract", DL / "reference.py")
        declared = str(2 * 2**30) + "b"
        # CPU counts as Docker reads them (docker/cli opts.ParseCPUs: an exact rational number of whole
        # nano-CPUs in a signed 64-bit integer). A fractional declaration is the same count asked again and is
        # given as declared (review round 3, F1); a spelling finer than Docker takes is given at its nearest
        # nano-CPU (round 4, F1) and compared there (round 5, F1); another spelling of the same number (`1/2`,
        # `0x1`) is the same limit (round 6, F1); the exact number is rounded, not its float (round 6, F2).
        # Refused, nothing started: below one nano-CPU (Docker refuses it as too precise; rounded it could be
        # 0, no limit; round 5, F2, round 6, F2) and past the 64-bit count (Docker would wrap it; round 6, F3).
        fraction = {**JOB, "NRESE_SUITE_CPUS": "1.234567"}
        precise = {**JOB, "NRESE_SUITE_CPUS": "0.10000000000000001"}
        for environ, asked, memory, cpus in ((JOB, [], declared, "2"), ({}, [], "12g", "8"),
                                             (JOB, ["--memory", "2g"], declared, "2"),
                                             (JOB, ["--memory", "3g"], None, None),
                                             (JOB, ["--cpus", "4"], None, None),
                                             (fraction, ["--cpus", "1.234567"], declared, "1.234567"),
                                             (fraction, [], declared, "1.234567"),
                                             (fraction, ["--cpus", "1.5"], None, None),
                                             (precise, ["--cpus", "0.10000000000000001"], declared, "0.1"),
                                             ({**JOB, "NRESE_SUITE_CPUS": "1.0000000004"}, ["--cpus", "1.0000000004"],
                                              declared, "1"),
                                             ({**JOB, "NRESE_SUITE_CPUS": "6e-10"}, [], None, None),
                                             ({**JOB, "NRESE_SUITE_CPUS": "0.5"}, ["--cpus", "1/2"], declared, "0.5"),
                                             (JOB, ["--cpus", "0x2"], declared, "2"),
                                             ({**JOB, "NRESE_SUITE_CPUS": "1.0000000005000000001"}, [], declared,
                                              "1.000000001"),
                                             ({**JOB, "NRESE_SUITE_CPUS": "0.00000000099999999999999999"}, [], None,
                                              None),
                                             ({**JOB, "NRESE_SUITE_CPUS": "18446744073.709551616"}, [], None, None)):
            with self.subTest(environ=environ, asked=asked), tempfile.TemporaryDirectory() as tmp:
                cases = Path(tmp)
                (cases / "manifest.tsv").write_text("t1\thermit\tclassify\t/work/files/x.owl\n", encoding="utf-8")
                starts = []

                def run(command, **_):
                    if command[1] == "run":
                        starts.append(command)
                        (cases / "results.part.tsv").write_text("t1\thermit\tclassify\tclassified\t5\th\n",
                                                                encoding="utf-8")
                    if command[1] == "inspect":
                        return ok(stdout="false\n" if "{{.State.Running}}" in command else "0\n")
                    return ok()

                env = {k: v for k, v in os.environ.items() if k not in JOB} | environ
                with patch.object(reference.subprocess, "run", side_effect=run), \
                        patch.object(reference.time, "sleep"), \
                        patch.object(ns, "pin_images", return_value={}), \
                        patch.object(ns, "image_id", return_value=PINNED), \
                        patch.dict(os.environ, env, clear=True), \
                        patch.object(sys, "argv", ["reference.py", "run", str(cases), *asked]), \
                        contextlib.suppress(Exception):  # a container refused before it starts: none started
                    reference.main()
                if memory is None:
                    self.assertEqual(starts, [])
                    continue
                self.assertEqual(len(starts), 1)
                self.assertEqual(starts[0][starts[0].index("--memory") + 1] if "--memory" in starts[0] else None,
                                 memory)
                self.assertEqual(starts[0][starts[0].index("--memory-swap") + 1], memory)
                self.assertEqual(cpu_limits(starts[0]), [cpus])


def cpu_limits(argv):
    """Every CPU limit a `docker run` argv gives, in either of Docker's spellings (`--cpus N`, `--cpus=N`)."""
    return [b for a, b in zip(argv, argv[1:]) if a == "--cpus"] + [a[len("--cpus="):] for a in argv
                                                                   if a.startswith("--cpus=")]


class W3cReaders(unittest.TestCase):
    """The other reader of the kit's rows (review round 1, R4): w3c.py's `compare` and `against`."""

    def setUp(self):
        try:
            import rdflib  # noqa: F401  (the reader needs only the rows here; the suite's Python has no rdflib)
            self.w3c = load("w3c_not_run_contract", DL / "w3c.py")
        except ImportError:
            class Namespace(str):  # rdflib's: an attribute is the base plus its name
                def __getattr__(self, name):
                    return self + name

            stub = SimpleNamespace(Graph=object, Literal=object, URIRef=str, RDF=Namespace("rdf:"), Namespace=Namespace)
            with patch.dict(sys.modules, {"rdflib": stub}):
                self.w3c = load("w3c_not_run_contract", DL / "w3c.py")

    def test_a_not_run_row_is_no_result_in_compare_or_against(self):
        # Spec: the master, 14 Nov 2026 (impossible to read as an answer anywhere downstream): a Rejected test
        # with a not-run row is neither `wrong-result` nor a published pass lost; both readers exit 2.
        with tempfile.TemporaryDirectory() as tmp:
            cases = Path(tmp)
            (cases / "index.tsv").write_text("id\tname\tstatus\ttask\texpected\tpage\n"
                                             "w1\tTest-1\tRejected\tconsistency\tconsistent\tTest-1\n", encoding="utf-8")
            (cases / "results.tsv").write_text("w1\tnrese-dl\tconsistency\tnot-run\t0\tinput missing\n",
                                               encoding="utf-8")
            (cases / "disputed.tsv").write_text("", encoding="utf-8")
            (cases / "published.tsv").write_text("reasoner\ttest\ttask\texpected\tresult\n"
                                                 "nrese-dl\tTest-1\tconsistency\tconsistent\tPass\n", encoding="utf-8")
            printed = io.StringIO()
            with contextlib.redirect_stdout(printed):
                compared = self.w3c.compare(SimpleNamespace(cases=str(cases), disputed=str(cases / "disputed.tsv"),
                                                            out=str(cases / "out.tsv")))
                against = self.w3c.against(SimpleNamespace(cases=str(cases), published=str(cases / "published.tsv"),
                                                           reasoner="nrese-dl", published_as=None))
            verdicts = [r["verdict"] for r in csv.DictReader((cases / "out.tsv").read_text(encoding="utf-8").splitlines(),
                                                             delimiter="\t")]
        self.assertEqual(verdicts, ["not-run"])
        self.assertNotIn("published Pass, here not", printed.getvalue())
        self.assertIn("nrese-dl: not run here: 1", printed.getvalue())
        self.assertEqual((compared, against), (2, 2))


if __name__ == "__main__":
    unittest.main()
