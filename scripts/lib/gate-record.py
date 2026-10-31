"""A gate job's record (scripts/lib/gate-job.sh on Linux, scripts/lib/windows-job.sh on Windows),
for scripts/lib/fleet-gate.sh: OUT/gate.json, from check.sh's log (OUT/check.log) and what the job
says in its environment. It holds the tier, the tree, the file list's hash, the toolchain and flags,
the CPU `native` resolved to, the host (its name and the fleet's), the job and attempt, each step's
exit code and seconds from check.sh's summary, check.sh's exit code, whether the run left tracked
files changed, its seconds, the memory the job's slice held at most (GATE_PEAK, bytes; Linux) and
the stdin the fleet gave (GATE_STDIN; Windows).

    python3 scripts/lib/gate-record.py OUT
"""
import json
import os
import re
import sys

e, out = os.environ, sys.argv[1]
# The steps as check.sh's summary lists them: `  <code>  <name> (<seconds> s)` after `=== summary`.
steps, summary = [], False
for line in open(os.path.join(out, "check.log"), encoding="utf-8", errors="replace"):
    if line.startswith("=== summary"):
        summary, steps = True, []
        continue
    m = re.match(r"^  (\d+)  (.*) \((\d+) s\)$", line.rstrip("\n")) if summary else None
    if m:
        steps.append({"name": m.group(2), "code": int(m.group(1)), "seconds": int(m.group(3))})
json.dump({
    "tier": e["GATE_TIER"], "tree": e["GATE_TREE"], "list_hash": e["GATE_LIST"],
    "identity": e["GATE_IDENTITY"], "w3c_optional": bool(e.get("NRESE_W3C_OPTIONAL")),
    "cpu": e["GATE_CPU"], "host": e["GATE_HOST"], "nextest": e["GATE_NEXTEST"],
    "job": e["GATE_JOB"], "fleet_host": e["GATE_FLEET_HOST"], "attempt": e["GATE_ATTEMPT"],
    "summary": summary, "steps": steps, "code": int(e["GATE_CODE"]), "tracked_changes": e["GATE_CHANGED"],
    "seconds": int(e["GATE_SECONDS"]),
    "memory_peak_gb": round(int(e["GATE_PEAK"]) / 2**30, 2) if e.get("GATE_PEAK") else None,
    "stdin": e.get("GATE_STDIN") or None,
}, open(os.path.join(out, "gate.json"), "w", encoding="utf-8"), indent=1)
