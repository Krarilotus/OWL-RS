"""The fleet jobs of a /verify run: a debug build of nrese-server at one commit, then the scenarios
against it, both on gamergrill (WSL, cgroup v2: each step capped at its ram_gb), never on a timed
host.

The build runs there too: fleet stages only git inputs onto a WSL host (fleet-hosts copy.rs
refuses artifacts there), so a binary built elsewhere can't reach it (the orchestrator's choice,
20 Oct 2026). The build job's step is build-on-host.sh, sent inline, so the commit under test
needn't have it; it leaves the binary at ~/fleet/cache/owlrs-verify/bin-<sha12>/ for the run
jobs (and returns it with build.json), and a second build of the same commit only checks it.
The run job takes the harness (runner and scenarios) from its own checkout, which may be another
commit than the server's: scenarios a fleet job runs must be committed there."""
from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
HOST = "gamergrill"
# A cold debug build of nrese-server at -j2 (the commit tier's ask there: about 3 GB); a reused
# build is a checksum.
BUILD = {"cores": 2, "ram_gb": 3, "est_min": 45}
# The runner, and one debug server at a time on a small store (the step's cgroup peak is in each
# summary: read it before changing this).
RUN = {"cores": 2, "ram_gb": 1.5, "est_min": 10}


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True,
                          timeout=60).stdout.strip()


def toml_job(job: dict) -> str:
    def value(v):
        if isinstance(v, bool):
            return "true" if v else "false"
        if isinstance(v, (int, float)):
            return repr(v)
        if isinstance(v, list):
            return "[" + ", ".join(value(x) for x in v) + "]"
        if isinstance(v, dict):
            return "{ " + ", ".join(f"{json.dumps(k)} = {value(x)}" for k, x in v.items()) + " }"
        return json.dumps(v)
    lines = [f"{k} = {value(v)}" for k, v in job.items() if k not in ("requires", "inputs")]
    lines += ["", "[requires]"] + [f"{k} = {value(v)}" for k, v in job["requires"].items()]
    for item in job.get("inputs", []):
        lines += ["", "[[inputs]]"] + [f"{k} = {value(v)}" for k, v in item.items()]
    return "\n".join(lines) + "\n"


def jobs(repo: Path, rev: str, harness: str, scenarios: list[str], settings: list[str], priority: int,
         reason: str, verifies: str | None = None, selftest: bool = True, stamp: str | None = None) -> dict[str, dict]:
    """The build job and the run job (after it), by id. `rev` is the server's commit and `harness`
    the runner's; the scenarios are paths in the harness commit."""
    sha, harness_sha = git(repo, "rev-parse", f"{rev}^{{commit}}"), git(repo, "rev-parse", f"{harness}^{{commit}}")
    missing = [s for s in scenarios
               if subprocess.run(["git", "-C", str(repo), "cat-file", "-e", f"{harness_sha}:{s}"],
                                 capture_output=True, timeout=60).returncode != 0]
    if missing:
        raise SystemExit(f"not committed at {harness_sha[:12]} (a fleet job sees git only): {', '.join(missing)}")
    stamp = stamp or time.strftime("%m%d%H%M")
    build_id, run_id = f"verify-build-{sha[:8]}-{stamp}", f"verify-run-{sha[:8]}-{stamp}"
    script = (HERE / "build-on-host.sh").read_text(encoding="utf-8").replace("\r\n", "\n")  # bash -c, on Linux
    source = str(repo.resolve().as_posix())
    build = {
        "id": build_id, "group": "verify", "kind": "owlrs.any", "priority": priority,
        "priority_reason": f"{reason} (debug nrese-server {sha[:12]} for /verify)", "est_min": BUILD["est_min"],
        "env": {"CARGO_BUILD_JOBS": str(BUILD["cores"])},
        "steps": [["bash", "-c", script, "build-on-host"]],
        "requires": {"hosts": [HOST], "cores": BUILD["cores"], "ram_gb": BUILD["ram_gb"]},
        "inputs": [{"name": "checkout", "source": {"git": source, "rev": sha}}]}
    run_args = [a for s in settings for a in ("--set", s)]
    # The self-test first: a runner whose checks don't fail when they should proves nothing after.
    steps = [["python3", "scripts/verify/verify.py", "selftest", "--server-rev", sha]] if selftest else []
    steps.append(["python3", "scripts/verify/verify.py", "run", "--server-rev", sha, *run_args, *scenarios])
    run = {
        "id": run_id, "group": "verify", "kind": "owlrs.any", "priority": priority, "priority_reason": reason,
        "after": [build_id], "est_min": RUN["est_min"], "steps": steps,
        **({"verifies": verifies} if verifies else {}),
        "requires": {"hosts": [HOST], "cores": RUN["cores"], "ram_gb": RUN["ram_gb"]},
        "inputs": [{"name": "checkout", "source": {"git": source, "rev": harness_sha}}]}
    return {build_id: build, run_id: run}
