"""The endpoint's classification as a case (fast.py `endpoint-classify`): GET /dataset/classification
timed alone, its answer in one form for every side's server, and an answer the server didn't give
(refused, incomplete, undecided, not 200) failing as not answered, never as wrong and never as a pass
(the master, 14 Nov 2026 18:24).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(ROOT / "benches/fast"))

import fast  # noqa: E402
import release_gate as rg  # noqa: E402

E = "http://example.org/"
THING, NOTHING = "http://www.w3.org/2002/07/owl#Thing", "http://www.w3.org/2002/07/owl#Nothing"
CASE = dict(name="ep", data="none", metric="whole_ms", cap_gb=2, tool="endpoint-classify", phases=["request"],
            check={"taxonomy_equal": "true"}, reference="/fast/ref/ep.tax")
# The reference by hand in the DL kit's format (canonical.py): A below B, B below owl:Thing.
REFERENCE = "\n".join(sorted([f"= {THING} {THING}", f"= {NOTHING} {NOTHING}", f"= {E}A {E}A", f"= {E}B {E}B",
                              f"< {E}A {E}B", f"< {E}B {THING}"])) + "\n"
# engine-v2's answer to el-classify-irregular under `dl.outside_profile = dl-part`, in its fields.
REFUSED = {"engine": "tableau", "complete": False, "incomplete": ["axioms left out: an irregular role hierarchy"],
           "consistent": None, "subsumptions": [[f"{E}A", f"{E}B"]], "unsatisfiable": [], "equivalent_to_thing": []}


def measured(answer, http: int = 200, seconds: str = "0.250", runs: list | None = None) -> dict:
    """The case measured with the server replaced: per process (`runs`: answer, HTTP status, exit,
    anon + kernel peak) the request line curl prints and the answer it saved."""
    runs = runs or [(answer, http, 0, 1000)]
    with tempfile.TemporaryDirectory() as temporary:
        scratch = Path(temporary)
        calls = iter(runs)

        def run(name, _cap, _command, **_):
            body, status, code, peak = next(calls)
            (scratch / f"{name}.json").write_text(json.dumps(body), encoding="utf-8")
            line = "" if status is None else f"fast-endpoint-request {status} {seconds}\n"
            return code, "", f"{line}fast-anon-kernel-peak {peak}\n", 9.0

        with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", side_effect=run), \
                patch.object(fast, "seed_from_inputs", return_value=True), \
                patch.object(fast, "files", return_value=subprocess.CompletedProcess([], 0, REFERENCE, "")):
            return fast.measure({**CASE, "processes": len(runs)}, "contract", 1, {}, {}, "none")


def side(record: dict) -> dict:
    return rg.evidence([{"n": 1, "report": {"records": [record]}}], record["case"])


class NotAnswered(unittest.TestCase):
    def test_an_answer_marked_incomplete_is_not_answered_with_status_200(self):
        # Spec (master 18:24, point 2): engine-v2's sound-only answer, status 200, is refused.
        record = measured(REFUSED)
        self.assertEqual(record["status"], fast.NOT_ANSWERED)
        self.assertEqual([c["ok"] for c in record["checks"]], [None])
        self.assertEqual(fast.not_as_expected([record]), [record])
        self.assertEqual(rg.state(side(record)), "failed")
        self.assertNotIn("wrong", rg.why_not(side(record), "main"))

    def test_main_s_skipped_axioms_a_server_error_and_an_undecided_answer_are_not_answered(self):
        # Spec: main marks what its EL classifier left out as `skipped`; not 200 and `consistent`
        # null say no answer either.
        complete = {"subsumptions": [[f"{E}A", f"{E}B"]], "unsatisfiable": []}
        for answer, http in (({**complete, "skipped": {"ObjectHasSelf": 1}}, 200), (complete, 500),
                             ({**complete, "consistent": None}, 200)):
            with self.subTest(http=http, answer=answer):
                self.assertEqual(measured(answer, http)["status"], fast.NOT_ANSWERED)

    def test_a_failed_request_or_a_payload_that_is_no_answer_is_never_called_wrong(self):
        # Spec (review r1 F2): curl's own failure (a timeout: exit 28, HTTP 000; a partial transfer
        # after a 200: exit 18), the case's limit (exit 124) and readable JSON that is no
        # classification ({} or null) give no answer to judge.
        good = {"subsumptions": [[f"{E}A", f"{E}B"]]}
        for runs, status in (([(good, "000", 28, 1)], fast.NOT_ANSWERED), ([(good, 200, 18, 1)], fast.NOT_ANSWERED),
                             ([(good, None, 124, 1)], "timeout"),
                             ([({}, 200, 0, 1)], fast.NOT_ANSWERED), ([(None, 200, 0, 1)], fast.NOT_ANSWERED)):
            with self.subTest(runs=runs):
                record = measured(None, runs=runs)
                self.assertEqual(record["status"], status)
                self.assertEqual([c["ok"] for c in record["checks"]], [None])
                self.assertEqual(rg.state(side(record)), "failed")
                self.assertNotIn("wrong", rg.why_not(side(record), "main"))


class Answered(unittest.TestCase):
    def test_the_series_is_the_request_and_the_answer_is_judged_against_the_reference(self):
        # Spec (point 1): the time is curl's time of the request (250 ms), not the scope's 9 s.
        record = measured({"subsumptions": [[f"{E}A", f"{E}B"]], "unsatisfiable": [], "skipped": {}})
        self.assertEqual(record["status"], "ok", record["notes"])
        self.assertEqual(record["observations"][0]["samples"], {"request": [250.0]})
        self.assertEqual(record["result"]["taxonomy_equal"], "true")
        # B below A instead: a wrong answer, judged so.
        self.assertEqual(measured({"subsumptions": [[f"{E}B", f"{E}A"]], "complete": True})["status"], "wrong")

    def test_every_process_is_judged(self):
        # Spec (review r1 F1): a wrong first answer isn't hidden by a right last one.
        right, wrong = {"subsumptions": [[f"{E}A", f"{E}B"]]}, {"subsumptions": [[f"{E}B", f"{E}A"]]}
        record = measured(None, runs=[(wrong, 200, 0, 1), (right, 200, 0, 1)])
        self.assertEqual(record["status"], "wrong")
        self.assertEqual(rg.state(side(record)), "failed")

    def test_every_process_keeps_its_peak_and_its_request(self):
        # Spec (review r1 F3): each process's peak reaches the gate, the larger first one included;
        # one sample per request, none twice.
        right = {"subsumptions": [[f"{E}A", f"{E}B"]]}
        both = measured(None, runs=[(right, 200, 0, 900), (right, 200, 0, 100)])
        self.assertEqual(both["status"], "ok", both["notes"])
        self.assertEqual(side(both)["anon_kernel"], [900])
        self.assertEqual(side(both)["process"], [{"request": [250.0, 250.0]}])

    def test_one_form_for_both_servers(self):
        # Spec (point 3): unsatisfiable classes are below owl:Nothing and engine-v2's classes
        # equivalent to owl:Thing above it; main's answer has no such field and states none.
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "a.json"
            path.write_text(json.dumps({"subsumptions": [["a", "b"]], "unsatisfiable": ["u"],
                                        "equivalent_to_thing": ["t"]}), encoding="utf-8")
            self.assertEqual(fast.endpoint_answer(200, path)[0], [("a", "b"), ("u", NOTHING), (THING, "t")])
            path.write_text(json.dumps({"subsumptions": [["a", "b"]], "unsatisfiable": []}), encoding="utf-8")
            self.assertEqual(fast.endpoint_answer(200, path)[0], [("a", "b")])

    def test_the_pairs_are_closed(self):
        # Spec: neither server says its pairs are closed; a chain gives its ends, a cycle both ways.
        self.assertEqual(sorted(fast.closed([("a", "b"), ("b", "c")])), [("a", "b"), ("a", "c"), ("b", "c")])
        self.assertEqual(sorted(fast.closed([("a", "b"), ("b", "a")])), [("a", "b"), ("b", "a")])


# A stand-in server: ready, takes the load, answers with the NRESE settings it was started with.
FAKE = """import http.server, json, os
host, port = os.environ["NRESE_BIND_ADDR"].rsplit(":", 1)
seen = {k: v for k, v in os.environ.items() if k.startswith("NRESE_")}
class H(http.server.BaseHTTPRequestHandler):
    def reply(self, code, body=b""):
        self.send_response(code); self.send_header("Content-Length", str(len(body))); self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        self.reply(200, b"" if self.path == "/readyz" else json.dumps({"subsumptions": [], "env": seen}).encode())
    def do_PUT(self):
        self.rfile.read(int(self.headers["Content-Length"])); self.reply(204)
    def log_message(self, *_):
        pass
http.server.HTTPServer((host, int(port)), H).serve_forever()
"""


@unittest.skipUnless(sys.platform.startswith("linux"), "the case's shell, curl and a server on Linux")
class Shell(unittest.TestCase):
    def test_every_side_s_server_gets_the_one_environment_and_one_timed_request(self):
        # Spec (point 4): the server sees ENDPOINT_ENV and its address, no other NRESE setting of
        # the harness's; the shell prints one request line, status 200, and saves the answer.
        with tempfile.TemporaryDirectory() as temporary:
            where = Path(temporary)
            (where / "server.py").write_text(FAKE, encoding="utf-8")
            (where / "data.nt").write_text("<a> <b> <c> .\n", encoding="utf-8")
            port = fast.free_port()
            script = fast.endpoint_script(str(where / "data.nt"), port, str(where / "ep")).replace(
                "/target/release/nrese-server", f"{sys.executable} {where / 'server.py'}")
            done = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=60,
                                  env={"PATH": "/usr/bin:/bin", "NRESE_HUGE_PAGES": "off", "NRESE_LAYOUT_PAD": "x"})
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(len(fast.ENDPOINT_REQUEST.findall(done.stderr)), 1, done.stderr)
            self.assertEqual(fast.ENDPOINT_REQUEST.findall(done.stderr)[0][0], "200")
            seen = json.loads((where / "ep.json").read_text(encoding="utf-8"))["env"]
            self.assertEqual(seen, {**fast.ENDPOINT_ENV, "NRESE_BIND_ADDR": f"127.0.0.1:{port}", "NRESE_LAYOUT_PAD": "x"})


if __name__ == "__main__":
    unittest.main()
