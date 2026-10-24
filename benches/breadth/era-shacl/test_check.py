"""One consumer table; authored transport contract, not an engine/RDF oracle."""
import ast
import csv
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


def owning_functions(path):
    source = ast.parse(path.read_text(encoding="utf-8"))
    names = {"structured_check", "structured_attempt"}
    body = [x for x in source.body if isinstance(x, ast.FunctionDef) and x.name in names]
    scope = dict(Path=Path, csv=csv, hashlib=hashlib, json=json, subprocess=subprocess)
    for node in source.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id in {"ENGINES", "SYSTEM"}:
                    scope[target.id] = ast.literal_eval(node.value)
    exec(compile(ast.Module(body=body, type_ignores=[]), str(path), "exec"), scope)
    return scope["structured_check"]


class StructuredConsumerContract(unittest.TestCase):
    def test_execution_and_capture_binding_table(self):
        # Spec: accepted R1-F2 requires both actual successful owning rows/calls,
        # not old report presence, before attaching artifact equality to a row.
        source = Path(os.environ.get("ERA_PYTHON_SOURCE", Path(__file__).with_name("run.py")))
        check = owning_functions(source)
        fixture = Path(os.environ["ERA_FIXTURE"])
        retained = Path(os.environ["ERA_CONSUMER_OUT"])
        retained.mkdir(parents=True, exist_ok=True)
        cases = [("complete-control", "MATCH"), ("failed-candidate", "UNQUALIFIED"),
                 ("timeout-candidate", "UNQUALIFIED"), ("cancelled-candidate", "UNQUALIFIED"),
                 ("stale-candidate", "UNQUALIFIED"), ("failed-reference", "UNQUALIFIED"),
                 ("unbound-reference", "UNQUALIFIED"), ("ambiguous-reference", "UNQUALIFIED")]
        for name, expected in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory(prefix="era-consumer-") as tmp:
                root = Path(tmp); (root / "reports").mkdir()
                rows = [dict(task="validate", status="ok", system=engine, tier="ES", item="tds", run="1",
                             date="authored-contract-20261024", note="authored consumer unit; no engine executed")
                        for engine in ("nrese", "topbraid")]
                for row in rows:
                    engine = row["system"]; stem = f"{engine}-ES-tds-r1"
                    attempt = root / "attempts" / ("authored-" + engine); attempt.mkdir(parents=True)
                    for item in fixture.iterdir():
                        if item.is_file(): shutil.copyfile(item, attempt / item.name)
                    observation = json.loads((attempt / "observation.json").read_bytes())
                    # Explicit authored call, never a fake runtime receipt/certificate.
                    call = observation["call_id"]
                    invocation = dict(fixture_origin="independently-authored-consumer-contract-not-engine-execution",
                        call_id=call, measured=dict(ms=0.0, peak_mib=None, rc=0, timed_out=False),
                        case=dict(name=stem, engine=engine, subset="ES", shape="tds", rep=1, phase="engine"),
                        report_sha256=hashlib.sha256((attempt / "report.nq").read_bytes()).hexdigest())
                    (attempt / "invocation.json").write_text(json.dumps(invocation), encoding="utf-8")
                    with open(attempt / "results.csv", "w", encoding="utf-8", newline="") as stream:
                        writer = csv.DictWriter(stream, fieldnames=list(row)); writer.writeheader(); writer.writerow(row)
                    # Historical old consumer locates this stale-capable filename.
                    (root / "reports" / (stem + ".observation.json")).write_text(json.dumps(observation), encoding="utf-8")
                if name in {"failed-candidate", "timeout-candidate", "cancelled-candidate"}:
                    rows[0]["status"] = name.split("-")[0]
                    rows[0]["note"] = "actual runner diagnostic retained: " + name
                elif name == "stale-candidate":
                    observation = json.loads((root / "attempts/authored-nrese/observation.json").read_bytes())
                    observation["call_id"] = "older-authored-call"
                    (root / "attempts/authored-nrese/observation.json").write_text(json.dumps(observation), encoding="utf-8")
                elif name == "failed-reference": rows[1]["status"] = "failed"
                elif name == "unbound-reference":
                    invocation_path = root / "attempts/authored-topbraid/invocation.json"
                    invocation = json.loads(invocation_path.read_bytes()); del invocation["call_id"]
                    invocation_path.write_text(json.dumps(invocation), encoding="utf-8")
                elif name == "ambiguous-reference": rows.append(dict(rows[1]))
                with open(root / "results.csv", "w", encoding="utf-8", newline="") as stream:
                    writer = csv.DictWriter(stream, fieldnames=list(rows[0])); writer.writeheader(); writer.writerows(rows)

                def bridge_delivery_contract(command, *, stdout, stderr, timeout, check):
                    # Only the subprocess delivery boundary is doubled. No RDF
                    # semantics/canonicalization is implemented or claimed here.
                    target = Path(command[command.index("--out") + 1])
                    target.write_text(json.dumps(dict(comparison_kind="authored-consumer-delivery-contract-double",
                        completeness="authored-unit-only", projection="none", verdict="MATCH",
                        full_graph_certificate=False)), encoding="utf-8")
                    stdout.write(b"authored bridge delivery contract\n"); stderr.write(b"")
                    return SimpleNamespace(returncode=0)

                with patch.object(subprocess, "run", side_effect=bridge_delivery_contract):
                    check(SimpleNamespace(results=str(root), reference="topbraid", bridge_program="/authored/contract-double"))
                saved = json.loads((root / "structured-check/comparisons.json").read_bytes())
                if isinstance(saved, list):
                    candidate = next(x for x in saved if x["row_index"] == 0)
                    verdict = candidate["comparison"]["verdict"]
                else: candidate = saved["nrese-ES-tds-r1"]; verdict = candidate["verdict"]
                (retained / (name + ".json")).write_text(json.dumps(saved, indent=2), encoding="utf-8")
                shutil.copytree(root / "structured-check", retained / (name + "-evidence"))
                self.assertEqual(verdict, expected, "owning execution/capture contract: " + name)
                if isinstance(saved, list):
                    self.assertEqual(candidate["candidate_row"], rows[0])
                    self.assertEqual(candidate["reference_rows"], rows[1:])
                    if name == "complete-control":
                        self.assertEqual(candidate["bridge"]["exit"], 0)
                        self.assertEqual(Path(candidate["bridge"]["stdout"]).read_bytes(), b"authored bridge delivery contract\n")
                        self.assertEqual(Path(candidate["bridge"]["stderr"]).read_bytes(), b"")
                        self.assertEqual(candidate["candidate_binding"]["call_id"], "authored-unit-not-engine-validation")
                print("ERA consumer contract " + name + ": " + verdict, flush=True)


if __name__ == "__main__":
    unittest.main()
