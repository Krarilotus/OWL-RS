"""The SPARQL group's workloads (benches/cases/sparql.toml): the competitors' own benchmarks,
each tier named after its case, so that a run's `versus` entries find the case's home turf.

Their data comes from benches/sparql/prepare.sh into one directory (`suite.py run --data
DIR`), frozen in benches/datasets.toml; their queries are in benches/competitors/queries;
their known answers, where a source publishes them, are the answer recorder's oracle
(Plan.answers): Sparqloscope's answers that every engine of its paper published alike, and
SP2Bench's published result sizes.
"""
from __future__ import annotations

import re
from pathlib import Path

PREPARE = "benches/sparql/prepare.sh"


def Plan(*args, **kwargs):
    """workloads.Plan, imported when a plan is made: workloads.py registers this module's
    definitions as it loads."""
    from .workloads import Plan as _Plan
    return _Plan(*args, **kwargs)


def known_from_checks(path: Path) -> dict[str, dict]:
    """`"q.STEM.value" = V` and `"q.STEM.rows" = N` lines of a check table as known answers."""
    out: dict[str, dict] = {}
    if not path.exists():
        return out
    for line in path.read_text(encoding="utf-8").splitlines():
        m = re.fullmatch(r'"q\.(.+)\.(value|rows)"\s*=\s*(.+)', line.strip())
        if m:
            value = m.group(3).strip()
            value = int(value) if re.fullmatch(r"-?\d+", value) else value.strip("'\"")
            out.setdefault(m.group(1), {})[m.group(2)] = value
    return out


def known_from_table(path: Path, column: str) -> dict[str, dict]:
    """A published table's `rows=N` cells for one column (tier) as known answers."""
    out: dict[str, dict] = {}
    header: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        cells = line.split("\t")
        if not header:
            header = cells
            continue
        cell = dict(zip(header, cells)).get(column, "-")
        if cell.startswith("rows="):
            out[cells[0]] = {"rows": int(cell[len("rows="):])}
    return out


def sparqloscope(root: Path, tier: str, settings: dict) -> Plan:
    """Sparqloscope (ISWC 2025) on DBLP's knowledge graph as its Qleverfile downloads it
    (dblp's release of 11 Oct 2026): its 105 queries, QLever's home turf. The paper's answers
    are of the April 2025 release, so the reference systems decide the answers here
    (`known_from_checks` reads them where the data is the paper's)."""
    plan = Plan("sparqloscope", tier, inputs=["/data/dblp-kg-20261011.nt"],
                queries=root / "benches/competitors/queries/sparqloscope-dblp",
                prepare=["bash", PREPARE, "dblp-kg"],
                note="answers against the reference systems; the paper's are of another release")
    if tier != "sparqloscope-dblp":
        plan.unavailable = f"no tier {tier}: sparqloscope-dblp (Wikidata truthy waits for Draco)"
    return plan


def sp2bench(root: Path, tier: str, settings: dict) -> Plan:
    """SP2Bench v1.01 at 1 M and 25 M triples: its 17 queries, answers by its paper's table."""
    size = tier.removeprefix("sp2bench-")
    plan = Plan("sp2bench", tier, inputs=[f"/data/{tier}.nt"],
                queries=root / "benches/competitors/queries/sp2bench",
                prepare=["bash", PREPARE, tier],
                note="known answers: the result sizes of the SP2Bench paper (Table V)")
    if size not in ("1m", "25m"):
        plan.unavailable = f"no tier {tier}: sp2bench-1m, sp2bench-25m"
    else:
        plan.answers = known_from_table(root / "benches/sparql/sp2bench/published.tsv", size)
    return plan


def generated_queries(root: Path, settings: dict, name: str) -> tuple[Path, str | None]:
    """A generated query set (WatDiv's instances, G-CARE's converted queries): it lives with the
    data, SPARQL_DATA/queries/NAME, never in the repository; its files must be the ones
    benches/sparql/queries.sha256 pins. The directory, and why it can't be used (or None)."""
    import hashlib
    directory = Path(settings.get("SPARQL_DATA") or Path.home() / "owlrs" / "sparql-data") / "queries" / name
    pinned = {}
    for line in (root / "benches/sparql/queries.sha256").read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            digest, path = line.split("  ", 1)
            if path.startswith(name + "/"):
                pinned[path[len(name) + 1:]] = digest
    if not directory.is_dir():
        return directory, f"{directory} missing: the pinned set (benches/sparql/queries.sha256) goes there"
    found = {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in directory.iterdir() if f.is_file()}
    if found != pinned:
        differ = sorted(set(found) ^ set(pinned) | {n for n in found if n in pinned and found[n] != pinned[n]})
        return directory, f"{directory} isn't the pinned set ({len(differ)} files differ, e.g. {differ[:2]})"
    return directory, None


def watdiv(root: Path, tier: str, settings: dict) -> Plan:
    """WatDiv's basic testing workload (20 templates, linear, star, snowflake, complex) on its
    authors' datasets, instantiated from the generator's saved state of that dataset (the
    instances pinned by hash, read from SPARQL_DATA/queries)."""
    queries, why = generated_queries(root, settings, tier)
    plan = Plan("watdiv", tier, inputs=[f"/data/{tier}.nt"], queries=queries,
                prepare=["bash", PREPARE, tier],
                note="answers checked against the reference systems (no published answers)")
    if tier not in ("watdiv-10m", "watdiv-100m"):
        plan.unavailable = f"no tier {tier}: watdiv-10m, watdiv-100m"
    elif why:
        plan.unavailable = why
    return plan


def bsbm(root: Path, tier: str, settings: dict) -> Plan:
    """BSBM V3.1 (tiers bsbm-explore-SIZE, bsbm-bi-SIZE): BIBM 0.7.8's test driver with the
    published procedure, 1, 4, 8 and 64 clients (benches/sparql/bsbm/drive.py), Virtuoso's
    home turf. SPARQL_DATA names the host directory given as --data (the driver reads the
    test driver data bsbm-SIZE.td/ and BIBM under src/ there)."""
    m = re.fullmatch(r"bsbm-(explore|bi)-(10m|100m)", tier)
    plan = Plan("bsbm", tier, prepare=["bash", PREPARE, f"bsbm-{m.group(2)}"] if m else None,
                note="BSBM V3.1's procedure through BIBM 0.7.8; answers by its qualification against a reference")
    if not m:
        plan.unavailable = f"no tier {tier}: bsbm-explore-100m, bsbm-bi-100m (or -10m)"
        return plan
    mix, size = m.groups()
    plan.inputs = [f"/data/bsbm-{size}.nt"]
    data = settings.get("SPARQL_DATA")
    if not data:
        plan.unavailable = "set SPARQL_DATA to the host directory given as --data (BIBM and bsbm-SIZE.td/ are read there)"
        return plan
    data = Path(data)
    plan.external = ["python3", str(root / "benches/sparql/bsbm/drive.py"), "--bibm", str(data / "src/bibm/bibm"),
                     "--td", str(data / f"bsbm-{size}.td"), "--mix", mix, "--endpoint", "{endpoint}",
                     "--out", "{out}", "--system", "{system}", "--qualify", str(data / f"qualification/bsbm-{size}"),
                     "--clients", settings.get("BSBM_CLIENTS", "1,4,8,64"),
                     "--timeout-ms", str(int(settings.get("QUERY_TIMEOUT_S", "300")) * 1000)]
    return plan


DEFINITIONS = {
    "bsbm": (bsbm, ["bsbm-explore-100m", "bsbm-bi-100m"]),
    "sparqloscope": (sparqloscope, ["sparqloscope-dblp"]),
    "sp2bench": (sp2bench, ["sp2bench-1m"]),
    "watdiv": (watdiv, ["watdiv-10m"]),
}
