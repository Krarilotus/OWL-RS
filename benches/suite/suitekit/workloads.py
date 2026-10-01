"""What each workload of workloads.toml runs, tier by tier: its inputs (as the systems see
them: datasets under /data, a workload's own checkout under /rggs), its queries and their
expected answer counts, the regimes it prefers, and how its data is made.

A workload is one of three kinds:
- a cycle (load, reason, serve, queries) on every system that can run it;
- a kit command (a conformance suite of NRESE's, run on the host);
- a write benchmark (the harness's write-scaling, against NRESE).
Workloads without a definition here are planned: the driver skips them and says so.
"""
from __future__ import annotations

import os
import re
from dataclasses import dataclass, field
from pathlib import Path

from .runtime import Mount


@dataclass
class Plan:
    workload: str
    tier: str
    kind: str = "cycle"  # cycle, kit, writes
    inputs: list[str] = field(default_factory=list)
    mounts: list[Mount] = field(default_factory=list)
    queries: Path | None = None
    expected: dict[str, int] | None = None
    regimes: list[str] = field(default_factory=lambda: ["none"])
    small: bool = False  # the reference closure (owlrl) runs on it
    prepare: list[str] | None = None  # the command that makes missing /data inputs (Docker)
    command: list[str] | None = None  # kit: the command, from the repository root
    systems: set[str] | None = None  # kit and writes: the systems it runs on
    unavailable: str | None = None  # why it can't run here at all
    note: str = ""


def expected_counts(path: Path) -> dict[str, int] | None:
    if not path.exists():
        return None
    out = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        query, count = line.split("\t")[:2]
        out[query] = int(count)
    return out


def lubm(root: Path, tier: str, settings: dict) -> Plan:
    queries = root / "benches/reasoning/queries/lubm"
    return Plan("lubm", tier, inputs=["/data/univ-bench.nt", f"/data/lubm-{tier}.nt"], queries=queries,
                expected=expected_counts(queries / f"expected-lubm-{tier}.tsv"),
                regimes=["owl2-rl", "owl-horst"], small=tier == "1",
                prepare=["bash", "benches/reasoning/prepare-lubm.sh", tier])


def owl2bench(root: Path, tier: str, settings: dict) -> Plan:
    profile, _, n = tier.partition("-")
    regimes = {"rl": ["owl2-rl", "owl-horst"], "ql": ["owl2-ql", "owl2-rl"]}.get(profile)
    plan = Plan("owl2bench", tier, inputs=[f"/data/owl2bench-{tier}.nt"],
                queries=root / "benches/reasoning/queries/owl2bench", regimes=regimes or [],
                small=n == "1", prepare=["bash", "benches/reasoning/prepare-owl2bench.sh", f"{profile.upper()}:{n}"],
                note="answers outside the profile may differ legitimately (queries/owl2bench/profiles.tsv)")
    if regimes is None:
        plan.unavailable = f"profile {profile.upper()} needs classification: the ore-2015 workload"
    return plan


def integration(root: Path, tier: str, settings: dict) -> Plan:
    repo = settings.get("RGGS_REPO")
    plan = Plan("integration-rg-gs-gnd", tier, regimes=["owl2-rl", "owl-horst"], small=tier == "example",
                note="the workload repository has no licence: results stay unpublished until its authors agree")
    if not repo or not (Path(repo) / "queries/cq").is_dir():
        plan.unavailable = "set RGGS_REPO to a checkout of HisQu/rgonline-gs-data-integration"
        return plan
    repo = Path(repo).resolve()
    plan.mounts = [Mount(str(repo), "/rggs")]
    plan.queries = repo / "queries/cq"
    if tier == "example":
        files = sorted(p for p in (repo / "data/examples/harmonized").glob("*.ttl")
                       if not p.name.endswith(".reasoned.ttl"))
        files += [repo / f"data/raw/{s}/example_min.ttl" for s in ("dnb", "gs", "rgo")]
        files.append(repo / "mappings/harmonize.ttl")
    else:
        # cohort and full: the statements the project's pipeline last built
        # (`just use-cohort` or `just use-full`, then `just harmonize`).
        files = [repo / "data/harmonized/statements.ttl"]
        plan.note += f"; tier {tier} = data/harmonized/statements.ttl as last built"
    if settings.get("ONTOLOGY") == "gndo":
        files += [repo / "mappings/gndo.ttl", repo / "mappings/rgo/tbox.ttl"]
    missing = [str(f) for f in files if not f.is_file() or f.stat().st_size == 0]
    if missing:
        plan.unavailable = f"missing inputs: {', '.join(missing)}"
    plan.inputs = ["/rggs/" + f.relative_to(repo).as_posix() for f in files]
    return plan


def basics(root: Path, tier: str, settings: dict) -> Plan:
    # The query set of a sized slice is its family's: wikidata-lexemes-60m reads
    # queries/wikidata-lexemes.
    family = "entities" if tier.startswith("entities-") else re.sub(r"-\d+m$", "", tier)
    return Plan("basics-mix", tier, inputs=[f"/data/{tier}.nt"],
                queries=root / f"benches/competitors/queries/{family}",
                prepare=None if tier.startswith("entities-") else
                ["bash", "benches/competitors/prepare-datasets.sh", tier],
                note="answer counts cross-checked between systems")


def w3c_sparql(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("w3c-sparql11", tier, kind="kit", systems={"nrese"},
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-sparql",
                         "--test", "w3c_sparql11"],
                note="the other systems' results are cited from their reports")


def w3c_shacl(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("w3c-shacl", tier, kind="kit", systems={"nrese"},
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-shacl",
                         "--test", "w3c_shacl"])


def w3c_owl2_rl(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("w3c-owl2-rl", tier, kind="kit", systems={"nrese"},
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-store",
                         "--test", "w3c_owl2_rl"],
                note="the test cases come from scripts/fetch-w3c-tests.sh")


def geosparql(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("geosparql", tier, kind="kit", systems={"nrese"},
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-sparql",
                         "--test", "geosparql_compliance"],
                note="the compliance benchmark; the data comes from scripts/fetch-w3c-tests.sh")


def write_scaling(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("write-scaling", tier, kind="writes", systems={"nrese"},
                note="the harness drives NRESE's graph store API")


def clients(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("clients", tier, kind="kit", systems={"nrese"},
                unavailable="needs the ResearchSpace stack (ops/researchspace) and a DMW instance: "
                            "run scripts/smoke-researchspace.sh and scripts/smoke-dmw-export.sh by hand")


# workload -> (the plan of a tier, the default tiers)
DEFINITIONS = {
    "lubm": (lubm, ["1"]),
    "owl2bench": (owl2bench, ["rl-1"]),
    "integration-rg-gs-gnd": (integration, ["example"]),
    "basics-mix": (basics, ["olympics"]),
    "w3c-sparql11": (w3c_sparql, ["-"]),
    "w3c-shacl": (w3c_shacl, ["-"]),
    "w3c-owl2-rl": (w3c_owl2_rl, ["-"]),
    "geosparql": (geosparql, ["compliance"]),
    "write-scaling": (write_scaling, ["1m"]),
    "clients": (clients, ["-"]),
}


def plans(root: Path, workload: str, tiers: list[str] | None, settings: dict) -> list[Plan] | None:
    """The plans of a workload's tiers; None if the workload has no definition yet."""
    definition = DEFINITIONS.get(workload)
    if definition is None:
        return None
    make, default = definition
    return [make(root, tier, settings) for tier in (tiers or default)]
