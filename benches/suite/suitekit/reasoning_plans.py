"""The reasoning group's workloads whose kits run their own drivers (Plan kind `driver`):
the rule engines' programs (benches/reasoning/datalog: Soufflé, Nemo, VLog, GLog and NRESE on
the same program and facts), view maintenance under readers (benches/reasoning/maintenance)
and LDBC SPB (benches/reasoning/spb). `suite.py run --workloads claros` runs them as it runs
any workload, their rows in the run's results.csv, regime `custom` (a named rule program).

Answers before time: the rule kits qualify first, untimed (every derived relation counted,
`count:REL` rows, checked against the published counts), and their timed step runs only
against the qualified totals; it needs the quiet slot around `suite.py run`.
"""
from __future__ import annotations

import sys
import tomllib
from pathlib import Path

DATALOG = "benches/reasoning/datalog/datalog.py"
MAINTENANCE = "benches/reasoning/maintenance/maintenance.py"
SPB = "benches/reasoning/spb/spb.py"
RULE_ENGINES = {"nrese", "souffle", "nemo", "vlog", "glog"}


def Plan(*args, **kwargs):
    """workloads.Plan, imported when a plan is made (workloads.py registers this module)."""
    from .workloads import Plan as _Plan
    return _Plan(*args, **kwargs)


def datalog_case(root: Path, workload: str, tier: str) -> str | None:
    """The datalog kit's case that is `tier` of `workload` (cases.toml)."""
    cases = tomllib.loads((root / "benches/reasoning/datalog/cases.toml").read_text(encoding="utf-8"))["case"]
    for case in cases:
        if case.get("workload", "datalog-graphs") == workload and case.get("tier", case["name"]) == tier:
            return case["name"]
    return None


def rules(workload: str):
    """A workload of the datalog kit: prepare, qualify (untimed), then the timed repetitions."""
    def make(root: Path, tier: str, settings: dict):
        case = datalog_case(root, workload, tier)
        plan = Plan(workload, tier, kind="driver", systems=set(RULE_ENGINES), regimes=["custom"],
                    note="the datalog kit's driver: qualified counts first, then timed")
        if case is None:
            plan.unavailable = f"no case of benches/reasoning/datalog/cases.toml is {workload} {tier}"
            return plan
        run = [sys.executable, DATALOG, "run", "--cases", case, "--systems", "{systems}"]
        plan.driver = [[sys.executable, DATALOG, "prepare", "--cases", case],
                       [*run, "--qualify", "--runs", "1", "--results", "{results}/qualify"],
                       [*run, "--runs", "{runs}", "--qualified", "{results}/qualify/qualified.json",
                        "--results", "{results}/timed"]]
        return plan
    return make


def commit_latency(root: Path, tier: str, settings: dict):
    """Commits under readers over the SPARQL protocol (the maintenance kit); each change undone,
    so the closure must hash back to the load's (its `count` rows)."""
    return Plan("commit-latency", tier, kind="driver", systems={"nrese", "graphdb"}, regimes=["owl2-rl"],
                driver=[[sys.executable, MAINTENANCE, "run", "--systems", "{systems}", "--tier", tier,
                         "--runs", "{runs}", "--results", "{results}"]])


def spb(root: Path, tier: str, settings: dict):
    """LDBC SPB 2.0's basic mix with the benchmark's own ruleset (generated first, untimed)."""
    plan = Plan("ldbc-spb", tier, kind="driver", systems={"nrese", "graphdb"}, regimes=["custom"])
    size = {"50m": 50_000_000, "256m": 256_000_000}.get(tier)
    if size is None:
        plan.unavailable = f"no tier {tier}: 50m, 256m"
        return plan
    plan.driver = [[sys.executable, SPB, "generate", "--size", str(size)],
                   [sys.executable, SPB, "run", "--systems", "{systems}", "--size", str(size), "--runs", "{runs}",
                    "--results", "{results}"]]
    return plan


def definitions(existing: dict) -> dict:
    """These workloads; `lubm` keeps its suite cycle, and its tiers rdfox-* (RDFox's L and LE
    programs) go to the datalog kit."""
    lubm, lubm_default = existing["lubm"]
    rdfox_lubm = rules("lubm")

    def lubm_or_rdfox(root: Path, tier: str, settings: dict):
        return (rdfox_lubm if tier.startswith("rdfox-") else lubm)(root, tier, settings)

    return {
        "datalog-graphs": (rules("datalog-graphs"), ["tc-G10K", "sg-G10K", "cspa-httpd", "cspa-linux", "cspa-postgresql"]),
        "claros": (rules("claros"), ["l", "le"]),
        "lubm": (lubm_or_rdfox, lubm_default),
        "commit-latency": (commit_latency, ["lubm-10"]),
        "ldbc-spb": (spb, ["50m"]),
    }
