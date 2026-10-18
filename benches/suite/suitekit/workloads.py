"""What each workload of workloads.toml runs, tier by tier: its inputs (as the systems see
them: datasets under /data, a workload's own checkout under /rggs), its queries and their
expected answer counts, the regimes it prefers, and how its data is made.

A workload is one of three kinds:
- a cycle (load, reason, serve, queries) on every system that can run it;
- a kit command (a conformance suite of NRESE's, run on the host);
- a write benchmark (the harness's write-scaling, against NRESE or GraphDB).
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
    kind: str = "cycle"  # cycle, kit, writes, driver
    inputs: list[str] = field(default_factory=list)
    # A system that reads the data in another form (system key -> its inputs): the same
    # content, in the syntax that system's best configuration loads (StarBench: RDF-star for
    # GraphDB and Stardog, RDF 1.2 for the others). Answers stay comparable across forms.
    inputs_for: dict[str, list[str]] = field(default_factory=dict)
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
    clients: list[int] = field(default_factory=list)  # the queries again from this many concurrent clients each (throughput, p99)
    duration_s: int = 20
    replay: bool = False  # the queries are a log: each run once, in name order, no warm-up
    # A benchmark's own driver instead of the queries (BSBM's test driver): a host command run
    # against the served endpoint in each cache mode, `{endpoint}`, `{update}`, `{out}` (a
    # file to write), `{system}` and `{run}` substituted. It writes to {out} a JSON list of
    # result rows (task, item, repeat, status, ms, rows, note), which the suite emits as its own.
    external: list[str] | None = None
    # driver: a kit's own driver runs the systems the suite doesn't serve (Soufflé, VLog, GLog,
    # Nemo's programs, the maintenance kit), once per tier for all the selected systems it can
    # run (`systems`): its steps in order, from the repository root, with `{results}` (a
    # directory of the run to write), `{systems}` (comma-separated keys), `{tier}` and `{runs}`
    # substituted. Every results.csv the steps write below {results} joins the run's own. It is
    # SPARQL's `external` turned inside out: there a driver runs inside the suite's cycle
    # against a served endpoint. A timed step needs the quiet slot around `suite.py run`.
    driver: list[list[str]] | None = None
    # Known answers per query (its file's stem): {"value": the one value} or {"rows": n}, from
    # an independent source (a fast case's generator or a pinned check); the answer recorder's oracle.
    answers: dict[str, dict] = field(default_factory=dict)
    services: list[str] = field(default_factory=list)  # geosparql, fulltext: Context.services

    def inputs_of(self, system: str) -> list[str]:
        return self.inputs_for.get(system, self.inputs)

    def all_inputs(self) -> list[str]:
        return list(dict.fromkeys([*self.inputs, *(i for form in self.inputs_for.values() for i in form)]))


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


def lubm_materialised(root: Path, tier: str, settings: dict) -> Plan:
    queries = root / "benches/reasoning/queries/lubm"
    return Plan("lubm-materialised", tier, inputs=[f"/data/lubm-{tier}-materialised.nt"], queries=queries,
                expected=expected_counts(queries / f"expected-lubm-{tier}.tsv"),
                prepare=["bash", "benches/reasoning/prepare-lubm-materialised.sh", tier],
                note="the OWL 2 RL closure precomputed by NRESE; every system loads it without inference")


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
                prepare=["bash", "benches/competitors/prepare-entities.sh", tier.removeprefix("entities-")]
                if tier.startswith("entities-") else
                ["bash", "benches/competitors/prepare-datasets.sh", tier],
                note="answer counts cross-checked between systems")


def starbench(root: Path, tier: str, settings: dict) -> Plan:
    """StarBench (QuWeDa 2023): 56 SPARQL-star queries on BKR, 61 M statements as RDF-star.
    GraphDB and Stardog load the published RDF-star, as the paper ran them; the RDF 1.2
    systems load it with one reifier per distinct quoted triple, which keeps every answer
    (benches/breadth/starbench)."""
    queries = root / "benches/breadth/starbench/queries"
    star = ["/data/starbench/bkr-star.ttls"]
    return Plan("starbench", tier, inputs=["/data/starbench/bkr-rdf12.nt"],
                inputs_for={"graphdb": star, "stardog": star}, queries=queries,
                expected=expected_counts(queries / "expected.tsv"),
                prepare=["bash", "benches/breadth/starbench/prepare.sh"],
                note="answer counts cross-checked between systems; RDF-star (GraphDB, Stardog) and RDF 1.2 forms")


def geographica2(root: Path, tier: str, settings: dict) -> Plan:
    """Geographica 2: GeoSPARQL on the real-world sets in their named graphs (the micro and
    most macro queries), New York's streets (geocoding), the generated set and a 10 M
    slice of the scalability set (benches/breadth/geographica2)."""
    files = {"real": "real.nq", "census": "census.nt", "synthetic": "synthetic.nt",
             "scalability": "scalability-10m.nt"}
    plan = Plan("geographica2", tier, inputs=[f"/data/geographica2/{files.get(tier, tier)}"],
                queries=root / "benches/breadth/geographica2/queries" / tier, services=["geosparql"],
                prepare=["bash", "benches/breadth/geographica2/prepare.sh"],
                note="answer counts cross-checked between systems; queries as published (Strabon's dialect included)")
    if tier not in files:
        plan.unavailable = f"no tier {tier}: {', '.join(files)}"
    return plan


# The W3C suites' kits: their test data fetched first at its pinned commits, and required (the
# driver sets NRESE_W3C_REQUIRED: a suite without its data fails, never passes by skipping, as
# every suite of the release gate's first W3C part did on 13 Oct 2026); --nocapture puts each
# suite's counts into its log, from where the row's note takes them.
W3C_DATA = ["bash", "scripts/fetch-w3c-tests.sh"]
SHOW = ["--", "--nocapture"]


def w3c_sparql(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("w3c-sparql11", tier, kind="kit", systems={"nrese"}, prepare=W3C_DATA,
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-sparql",
                         "--test", "w3c_sparql11", *SHOW],
                note="the other systems' results are cited from their reports")


def w3c_shacl(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("w3c-shacl", tier, kind="kit", systems={"nrese"}, prepare=W3C_DATA,
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-shacl",
                         "--test", "w3c_shacl", *SHOW])


def w3c_owl2_rl(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("w3c-owl2-rl", tier, kind="kit", systems={"nrese"}, prepare=W3C_DATA,
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-store",
                         "--test", "w3c_owl2_rl", *SHOW],
                note="the test cases come from scripts/fetch-w3c-tests.sh")


def geosparql(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("geosparql", tier, kind="kit", systems={"nrese"}, prepare=W3C_DATA,
                command=["bash", "scripts/cargo-guarded.sh", "test", "--locked", "-p", "nrese-sparql",
                         "--test", "geosparql_compliance", *SHOW],
                note="the compliance benchmark; the data comes from scripts/fetch-w3c-tests.sh")


def write_scaling(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("write-scaling", tier, kind="writes", systems={"nrese", "graphdb"},
                note="Graph Store uploads and SPARQL inserts with checked Person/probe counts")


def clients(root: Path, tier: str, settings: dict) -> Plan:
    return Plan("clients", tier, kind="kit", systems={"nrese"},
                unavailable="needs the ResearchSpace stack (ops/researchspace) and a DMW instance: "
                            "run scripts/smoke-researchspace.sh and scripts/smoke-dmw-export.sh by hand")


def fast_data_name(spec: str) -> str:
    """The file a case's data spec names (benches/cases/README.md): `volume:FILE` is the
    dataset volume's FILE; `KIND key=value ...` the generator's output, by its parameters."""
    if spec.startswith("volume:"):
        return spec[len("volume:"):]
    kind, *params = spec.split()
    return "-".join([kind, *[p.replace("=", "") for p in params]]) + ".nt"


# A fast case's semantics as the suite's regime; the others have no runner in the suite.
FAST_REGIMES = {"plain": "none", "closure": "none", "none": "none", "rdfs": "rdfs",
                "owl-horst": "owl-horst", "owl2-rl": "owl2-rl", "owl2-ql": "owl2-ql"}


def fast(root: Path, tier: str, settings: dict) -> Plan:
    """A case of the fast suite (benches/fast) on its comparators. The tier is the case's
    name, with `~SEMANTICS` for a comparator's semantics: `~closure` reads NRESE's exported
    closure, `~plain` the case's data without reasoning, `~owl-horst` and the like the data
    under the system's own reasoner. benches/fast/compete.py prepares the files in the
    volume nrese-fast-data, which it passes as --data."""
    from . import cases as registry
    name, _, semantics = tier.partition("~")
    cases = {c["name"]: c for c in registry.load(root / "benches/cases")}
    case = cases.get(name)
    plan = Plan("fast", tier, note="a fast-suite case on its comparators (benches/fast/compete.py)")
    if case is None:
        plan.unavailable = f"no case {name} in benches/cases/"
        return plan
    semantics = semantics or case["semantics"]
    if semantics not in FAST_REGIMES:
        plan.unavailable = f"semantics {semantics} has no runner in the suite"
        return plan
    plan.regimes = [FAST_REGIMES[semantics]]
    if semantics == "closure":
        plan.inputs = [f"/data/{name}.closure.nt"]
    else:
        plan.inputs = [f"/data/{fast_data_name(f)}" for f in [case["data"], *case.get("extra", [])]]
    queries = case.get("queries", "")
    if queries in ("lubm", "owl2bench"):
        plan.queries = root / "benches/reasoning/queries" / queries
    elif queries and not queries.startswith("@"):
        plan.queries = root / "benches/fast/queries" / queries
    if queries == "@cache-log":
        plan.replay = True
    if settings.get("FAST_QUERIES"):
        # Several cases on the same data in one run: their queries merged (compete.py).
        plan.queries = Path(settings["FAST_QUERIES"])
    plan.answers = fast_answers(case, semantics, settings)
    args = case.get("args", [])
    if "--clients" in args:
        plan.clients = [int(n) for n in args[args.index("--clients") + 1].split(",")]
        plan.duration_s = int(float(args[args.index("--duration-s") + 1])) if "--duration-s" in args else 20
    return plan


def fast_answers(case: dict, semantics: str, settings: dict) -> dict[str, dict]:
    """A fast case's checks of single query answers (`q.NAME.value`, `q.NAME.rows`) as known
    answers, where they hold for the comparator: under the case's own semantics, or on NRESE's
    closure of it. `expect.KEY` is the generator's answer, read from the file FAST_EXPECT names
    (compete.py writes it); without that file those checks are left out."""
    import json
    if semantics != "closure" and FAST_REGIMES.get(semantics) != FAST_REGIMES.get(case["semantics"], case["semantics"]):
        return {}
    expect = {}
    if settings.get("FAST_EXPECT"):
        try:
            expect = json.loads(Path(settings["FAST_EXPECT"]).read_text(encoding="utf-8"))
        except (OSError, ValueError):
            expect = {}
    out: dict[str, dict] = {}
    for path, want in case.get("check", {}).items():
        parts = path.split(".")
        if len(parts) != 3 or parts[0] != "q" or parts[2] not in ("value", "rows"):
            continue
        if isinstance(want, str) and want.startswith("expect."):
            want = expect.get(want[len("expect."):])
        elif isinstance(want, str) and not re.fullmatch(r"-?\d+(\.\d+)?", want.strip()):
            continue  # a comparison (">= 0.9"), not an answer
        if want is not None:
            out.setdefault(parts[1], {})[parts[2]] = want
    return out


# workload -> (the plan of a tier, the default tiers)
DEFINITIONS = {
    "lubm": (lubm, ["1"]),
    "lubm-materialised": (lubm_materialised, ["1"]),
    "owl2bench": (owl2bench, ["rl-1"]),
    "integration-rg-gs-gnd": (integration, ["example"]),
    "basics-mix": (basics, ["olympics"]),
    "starbench": (starbench, ["bkr"]),
    "geographica2": (geographica2, ["real"]),
    "w3c-sparql11": (w3c_sparql, ["-"]),
    "w3c-shacl": (w3c_shacl, ["-"]),
    "w3c-owl2-rl": (w3c_owl2_rl, ["-"]),
    "geosparql": (geosparql, ["compliance"]),
    "write-scaling": (write_scaling, ["1m"]),
    "clients": (clients, ["-"]),
    "fast": (fast, []),
}


def _groups():
    """The groups' workloads, each in a module of its own (benches/cases/README.md)."""
    from . import reasoning_plans, sparql_plans
    DEFINITIONS.update(sparql_plans.DEFINITIONS)
    DEFINITIONS.update(reasoning_plans.definitions(DEFINITIONS))


_groups()


def plans(root: Path, workload: str, tiers: list[str] | None, settings: dict) -> list[Plan] | None:
    """The plans of a workload's tiers; None if the workload has no definition yet."""
    definition = DEFINITIONS.get(workload)
    if definition is None:
        return None
    make, default = definition
    return [make(root, tier, settings) for tier in (tiers or default)]
