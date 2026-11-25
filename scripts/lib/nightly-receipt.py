"""The nightly tier's receipt (scripts/check.sh nightly): one outcome for each test of its inventory.

    python scripts/lib/nightly-receipt.py DIR STEP=SELECTION...

DIR holds inventory.json, the tree's every workspace test, #[ignore]d ones marked (cargo nextest list
--message-format json, written before any test ran, no filter applied), and STEP.junit.xml, nextest's
JUnit report of each step (none: that step's tests have no outcome). SELECTION is what the step was told to run: `not-ignored`, every test not
#[ignore]d, `exact:BINARY_ID/NAME`, the one test so named, or `known`, the known open faults of
scripts/lib/known-faults.tsv (each an #[ignore]d test of the inventory).

Writes DIR/receipt.json: the fields of the gate's CheckReceipt (benches/fast/release_gate.py: kind,
commit, tree, dirty, identity, host, job, attempt; from RECEIPT_* in the environment), each step as a
unit (its selection and its report's SHA-256), and one outcome per test id `<binary id> <name>`:
passed, failed, or skipped with its reason (an #[ignore]d test no step selected: the tier doesn't run
it). It is complete only when every test a step selected has exactly one outcome, run (passed or failed:
a selected test reported skipped never ran), every test not #[ignore]d has one, and every outcome names
a test of the inventory. A test a step reports skipped without selecting it is no outcome of that step.
A failed test is an outcome, which its own step fails on. A known open fault that fails is
`known-open-fault`, counted (`known_open_faults`) and named (`known_open_fault_list`) apart; one that
passes leaves the receipt not complete: the fault no longer reproduces, and its line goes.

Exit: 0 complete, 1 not (each reason on stderr, `receipt: ...`), 2 a usage error.
"""
from collections import Counter
import hashlib
import json
import os
import sys
import xml.etree.ElementTree as ET

if len(sys.argv) < 3 or not all("=" in a for a in sys.argv[2:]):
    sys.exit(__doc__.split("\n\n")[1])
out, e = sys.argv[1], os.environ
errors, cases, units, selected = [], {}, [], set()
with open(os.path.join(out, "inventory.json"), encoding="utf-8") as f:
    suites = json.load(f)["rust-suites"]
# (binary id, name) -> #[ignore]d
inventory = {(binary, name): bool(case.get("ignored"))
             for binary, suite in suites.items() for name, case in suite["testcases"].items()}
if not inventory:
    errors.append("the inventory lists no test")
# The known open faults, id ("<binary id> <name>") -> what is wrong (known-faults.tsv).
with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "known-faults.tsv"), encoding="utf-8") as f:
    known = dict(line.rstrip("\r\n").split("\t", 1) for line in f if line.strip() and not line.startswith("#"))

for arg in sys.argv[2:]:
    step, selection = arg.split("=", 1)
    if selection == "not-ignored":
        want = {k for k, ignored in inventory.items() if not ignored}
    elif selection.startswith("exact:") and "/" in selection:
        want = {tuple(selection[len("exact:"):].split("/", 1))}
        errors += [f"{step}: selects {' '.join(k)}, which the inventory doesn't list" for k in want - inventory.keys()]
    elif selection == "known":
        want = {tuple(k.split(" ", 1)) for k in known}
        errors += [f"{step}: the known open fault {' '.join(k)} is no #[ignore]d test of the inventory"
                   for k in sorted(want) if not inventory.get(k)]
    else:
        sys.exit(f"receipt: {step}: no such selection {selection!r}")
    selected |= want
    report = os.path.join(out, f"{step}.junit.xml")
    try:
        with open(report, "rb") as f:
            data = f.read()
        root = ET.fromstring(data)
    except (OSError, ET.ParseError) as err:
        errors.append(f"{step}: no readable JUnit report ({err}), so none of its {len(want)} tests has an outcome")
        continue
    units.append({"unit": step, "selection": selection, "log_sha256": hashlib.sha256(data).hexdigest()})
    seen = Counter()
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            key = (suite.get("name"), case.get("name"))
            seen[key] += 1
            skipped = case.find("skipped")
            if key not in inventory:
                errors.append(f"{step}: an outcome for {' '.join(key)}, which the inventory doesn't list")
                continue
            if skipped is not None and key not in want:
                continue  # a test the step didn't select (an #[ignore]d one): no outcome of this step
            if case.find("failure") is not None or case.find("error") is not None:
                outcome = {"outcome": "failed"}
            elif skipped is not None:
                # Selected but not run: no outcome a test earns, so the receipt isn't complete.
                outcome = {"outcome": "skipped", "reason": skipped.get("message") or "skipped by nextest"}
                errors.append(f"{step}: {' '.join(key)} was selected, but reported skipped: it never ran")
            else:
                outcome = {"outcome": "passed"}
            if selection == "known" and key in want and outcome["outcome"] == "failed":
                outcome = {"outcome": "known-open-fault", "about": known[" ".join(key)]}
            elif selection == "known" and key in want and outcome["outcome"] == "passed":
                errors.append(f"{step}: known fault no longer reproduces: {' '.join(key)} passed; "
                              "remove its line from scripts/lib/known-faults.tsv")
            if key in cases:
                errors.append(f"{step}: a second outcome for {' '.join(key)} (first in {cases[key]['step']})")
            elif key not in want:
                errors.append(f"{step}: ran {' '.join(key)}, which it didn't select")
            cases.setdefault(key, {"step": step, **outcome})
    errors += [f"{step}: {n} outcomes for {' '.join(k)}" for k, n in seen.items() if n > 1]
    lost = sorted((want - seen.keys()) & inventory.keys())
    errors += [f"{step}: no outcome for {' '.join(k)}" for k in lost]

# The tier's skipped outcomes: the #[ignore]d tests outside every step's selection, and only those.
for key, ignored in sorted(inventory.items()):
    if key in cases or key in selected:
        continue
    if ignored:
        cases[key] = {"step": None, "outcome": "skipped", "reason": "#[ignore]d: no step of the tier runs it"}
    else:
        errors.append(f"no step selected {' '.join(key)}, which isn't #[ignore]d")

receipt = {
    "kind": "nightly", "commit": e.get("RECEIPT_COMMIT", ""), "tree": e.get("RECEIPT_TREE", ""),
    "dirty": bool(e.get("RECEIPT_DIRTY")), "identity": e.get("RECEIPT_IDENTITY", ""),
    "host": e.get("RECEIPT_HOST", ""), "job": e.get("RECEIPT_JOB", ""), "attempt": e.get("RECEIPT_ATTEMPT", ""),
    "units": units, "complete": not errors, "errors": errors,
    "counts": dict(Counter(c["outcome"] for c in cases.values())),
    "known_open_faults": sum(c["outcome"] == "known-open-fault" for c in cases.values()),
    "known_open_fault_list": [{"id": f"{b} {n}", "about": c["about"]} for (b, n), c in sorted(cases.items())
                              if c["outcome"] == "known-open-fault"],
    "cases": [{"id": f"{b} {n}", **cases[(b, n)]} for b, n in sorted(cases)],
}
with open(os.path.join(out, "receipt.json"), "w", encoding="utf-8") as f:
    json.dump(receipt, f, indent=1)
for message in errors[:40]:
    print(f"receipt: {message}", file=sys.stderr)
if len(errors) > 40:
    print(f"receipt: ... and {len(errors) - 40} more (receipt.json)", file=sys.stderr)
print(f"receipt: known open faults: {receipt['known_open_faults']}")
for fault in receipt["known_open_fault_list"]:
    print(f"receipt: known open fault: {fault['id']}: {fault['about']}")
print(f"receipt: {len(inventory)} tests listed; {receipt['counts']}; {'complete' if not errors else 'NOT complete'}")
sys.exit(1 if errors else 0)
