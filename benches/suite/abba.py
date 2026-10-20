#!/usr/bin/env python3
"""A targeted A-B-B-A: one change (B) against its base (A), on the release gate's machinery, with
an interleaved A-A' arm. Agents declare a family file; this checks it, writes its fleet jobs and
gives the verdict. Nothing here re-implements timing or statistics: the blocks are
release-gate.sh's (a palindrome A A' B B A' A per case, every side a fresh copy of its build,
seeded orientation, the quiet watcher, at most one repeat of a disturbed block), the tests
release_gate.py's (gate(), Holm, rounds by decide(), the floor).

    abba.py check FAMILY                     the family, valid or not (exit 2); in a job, its manifest
    abba.py jobs FAMILY [--host H] [--round K] [--harness REV] [--out DIR]
                                             the fleet jobs: its builds, then its blocks and report
    abba.py report FAMILY RESULTS...          the verdict from each round's results (in round order)

A family (benches/abba/NAME.toml):

    name = "lcp"                  # the jobs' names: abba-lcp-...
    host = "phuoc-yu"             # one exclusive timing host for every block (--host overrides)
    blocks = 6                    # a round's blocks, at least 6
    [sides]
    a = "REV"                     # the base
    b = "REV"                     # the change; A' is A's build again, a copy of its own
    [[case]]
    catalog = "op-group"          # a case of benches/cases/, by name
    dimensions = ["time/tag-counts"]   # optional: the hypotheses, else all the case's
    min_samples = 20              # optional: samples every series needs on every pass (default 1)
    [[case]]
    name = "..."                  # or a case of its own, in the catalogue's schema (any perf_lab mode)

Every case needs a content check (`check`): a run whose check fails, on any side, has no verdict.

Exit codes of report: 0 no loss shown; 1 a loss shown, a confirmation round due (jobs --round 2);
2 the family is invalid; 3 no verdict (evidence missing, incomplete or failed, or the A-A' arm
itself differs); 4 a confirmed loss (Holm over the family, the rounds combined).
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
from suitekit import cases as cases_registry, gatestats  # noqa: E402

spec = importlib.util.spec_from_file_location("release_gate", ROOT / "benches" / "fast" / "release_gate.py")
rg = importlib.util.module_from_spec(spec)
sys.path.insert(0, str(ROOT / "benches" / "fast"))
spec.loader.exec_module(rg)

SIDES = ("a", "a2", "b")  # A, A' (A's build, another copy), B: the palindrome's order
MIN_BLOCKS = 6
NO_LOSS, LOSS_SHOWN, INVALID, NO_VERDICT, CONFIRMED = 0, 1, 2, 3, 4
NAME = re.compile(r"^[a-z0-9][a-z0-9-]*$")
REV = re.compile(r"^[0-9a-f]{7,40}$")


class Invalid(Exception):
    """A family that can't be run: its problems, one line each."""


def family_path(path: str | Path) -> str:
    """The family's path as the jobs and NRESE_FAMILY name it: relative to the repository."""
    resolved = Path(path).resolve()
    try:
        return resolved.relative_to(ROOT).as_posix()
    except ValueError:
        raise Invalid([f"{path}: a family lives in the repository (benches/abba/), not outside it"])


def load_family(path: str | Path) -> dict:
    """The family, resolved and validated: its cases (catalogue or its own, each with a content
    check), its hypotheses (b vs a, per case and declared dimension). Raises Invalid."""
    rel = family_path(path)
    raw = (ROOT / rel).read_bytes()
    data = tomllib.loads(raw.decode("utf-8"))
    problems = []
    name, host, blocks = data.get("name", ""), data.get("host", ""), data.get("blocks", MIN_BLOCKS)
    sides = data.get("sides", {})
    if not NAME.match(str(name)):
        problems.append(f"name {name!r}: lower-case letters, digits and -")
    if not isinstance(blocks, int) or blocks < MIN_BLOCKS:
        problems.append(f"blocks {blocks!r}: at least {MIN_BLOCKS}")
    if set(sides) != {"a", "b"} or not all(isinstance(v, str) and REV.match(v) for v in sides.values()):
        problems.append("sides: exactly a and b, each a commit id (7 to 40 hex digits)")
    entries = data.get("case", [])
    if not entries:
        problems.append("no [[case]]")
    catalog = cases_registry.load(family=rel)
    by_name = {c["name"]: c for c in catalog}
    cases, hypotheses = [], []
    for i, entry in enumerate(entries):
        case = by_name.get(entry.get("catalog") or entry.get("name", ""))
        where = f"case {i + 1} ({entry.get('catalog') or entry.get('name', '?')})"
        if case is None:
            problems.append(f"{where}: no such case in benches/cases/, and not one of its own")
            continue
        if case.get("group") == "family":
            problems += [f"{where}: {p}" for p in cases_registry.problems([case], {})]
        if not case.get("check"):
            problems.append(f"{where}: no content check (`check`): its times would count whatever it answered")
        if case.get("outcome", "ok") != "ok":
            problems.append(f"{where}: a case with a declared outcome ({case['outcome']}) isn't timed")
        offered = [h.split("|", 2)[2] for h in rg.family_of(catalog, [case["name"]], baselines=("a",))]
        wanted = entry.get("dimensions") or offered
        unknown = sorted(set(wanted) - set(offered))
        if unknown:
            problems.append(f"{where}: dimensions {unknown} aren't the case's ({', '.join(offered)})")
        cases.append({"name": case["name"], "cap_gb": case.get("cap_gb", 4), "min_samples": int(entry.get("min_samples", 1))})
        hypotheses += [rg.hypothesis("a", case["name"], d) for d in wanted if d in offered]
    if len({c["name"] for c in cases}) != len(cases):
        problems.append("a case is declared twice")
    if problems:
        raise Invalid(problems)
    return {"path": rel, "sha256": hashlib.sha256(raw).hexdigest(), "name": name, "host": host, "blocks": blocks,
            "sides": dict(sides), "cases": cases, "hypotheses": hypotheses, "catalog": catalog}


def manifest_of(family: dict, harness: str, round_: int, host: str) -> dict:
    """What a round is held to; `host` is the one its blocks ran on (jobs --host may override the
    family's), whose floor the report takes."""
    return {k: family[k] for k in ("path", "sha256", "name", "blocks", "sides", "cases", "hypotheses")} | {
        "harness": harness, "round": round_, "host": host}


def git(*args: str, cwd: Path = ROOT) -> str:
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=True).stdout.strip()


# --- jobs --------------------------------------------------------------------------------------

def jobs(family: dict, harness: str, checkout: str, host: str, round_: int = 1) -> dict[str, dict]:
    """The family's fleet jobs: its check and builds (A, B and the harness), then on one host,
    alone, its check again (the manifest the report holds the results to), its blocks and its
    report (the verdict in abba-verdict.json; the step itself fails only for an invalid family)."""
    rg.linux_host(host)
    a, b = family["sides"]["a"], family["sides"]["b"]
    prefix = f"abba-{family['name']}-{b[:8]}" + (f"-r{round_}" if round_ > 1 else "")
    gate_ = ["bash", "scripts/release-gate.sh"]
    abba = ["python3", "-B", "benches/suite/abba.py"]
    env = {"GATE_NAME": f"abba-{family['name']}-{b[:8]}", "NRESE_FAMILY": family["path"],
           "GATE_CASES": ",".join(c["name"] for c in family["cases"]), "GATE_BASELINES": "a",
           "GATE_MAX_GB": str(max(c["cap_gb"] for c in family["cases"])), "NRESE_QUIET_SLOT": "0",
           "GATE_MIN_BLOCKS": str(family["blocks"]), "GATE_HOST": host, "GATE_ROUND": str(round_ - 1),
           "GATE_PREFIX": prefix, "ABBA_HARNESS": harness, "ABBA_ROUND": str(round_)}
    source = [{"name": "checkout", "source": {"git": checkout, "rev": rg.checkout_rev(checkout, b, harness)}}]
    revisions = list(dict.fromkeys([a, b, harness]))
    seconds = 30 * len(family["cases"])  # a case's run, roughly: the estimate only orders the queue
    return {
        f"{prefix}-build": {
            "id": f"{prefix}-build", "group": "abba", "kind": "owlrs.build", "env": env,
            "est_min": 25 * len(revisions) + 10,
            "steps": [[*abba, "check", family["path"]]] + [[*gate_, "build", r] for r in revisions]
            + [[*gate_, "prepare", harness]],
            "requires": {"hosts": [host], "cores": 16, "ram_gb": 12}, "inputs": source},
        prefix: {
            "id": prefix, "group": "abba", "kind": "owlrs.timed", "class": "exclusive", "after": [f"{prefix}-build"],
            "env": env, "est_min": round(family["blocks"] * (6 * seconds / 60 + 1) + 5), "stall_after_min": 60,
            "steps": [[*abba, "check", family["path"]]]
            + [[*gate_, "block", str(k), harness, "1", "-", f"a={a}", f"a2={a}", f"b={b}"]
               for k in range(1, family["blocks"] + 1)]
            + [["bash", "-c", 'python3 -B benches/suite/abba.py report "$1" "$FLEET_OUT" --json "$FLEET_OUT/abba-verdict.json" '
                '--out "$FLEET_OUT/abba-verdict.md"; code=$?; echo "$code" > "$FLEET_OUT/abba-exit"; [ "$code" != 2 ]',
                "abba", family["path"]], [*gate_, "clean"]],
            "requires": {"hosts": [host], "cores": 16, "ram_gb": max(12, max(c["cap_gb"] for c in family["cases"]))},
            "inputs": source}}


# --- the verdict ---------------------------------------------------------------------------------

def completeness(family: dict, manifest: dict | None, passes: list[dict]) -> list[str]:
    """Why a round's evidence can't be judged (empty: it can): its manifest, every block and slot,
    every process and content check on every side, every declared series' samples."""
    if manifest is None:
        return ["no manifest (abba-manifest.json): the round wasn't run from this family's check"]
    problems = [f"the manifest's {k} isn't the family's" for k in ("sha256", "sides", "blocks", "hypotheses")
                if manifest.get(k) != family[k]]
    problems += rg.schedule_errors(passes, list(SIDES), family["blocks"])
    blocks = {p["report"].get("block") for p in passes}
    missing = sorted(set(range(1, family["blocks"] + 1)) - blocks)
    if missing:
        problems.append(f"blocks {missing} have no reports")
    planned = [c["name"] for c in family["cases"]]
    revs = {"a": family["sides"]["a"], "a2": family["sides"]["a"], "b": family["sides"]["b"]}
    problems += rg.provenance_errors(passes, revs, manifest.get("harness", ""), planned)
    series = {}
    for h in family["hypotheses"]:
        _, case, dim = h.split("|", 2)
        if dim.startswith("time/"):
            series.setdefault(case, []).append(dim[len("time/"):])
    floors = {c["name"]: c["min_samples"] for c in family["cases"]}
    for side in SIDES:
        mine = [p for p in passes if p["side"] == side]
        if not mine:
            problems.append(f"no passes of {side}")
            continue
        for case in planned:
            e = rg.evidence(mine, case)
            if rg.state(e) != "ok" or e["unchecked"]:
                why = e["failed_checks"][:2] + e["failed_routes"][:1] + e["notes"][-1:]
                problems.append(f"{case} on {side}: {', '.join(map(str, sorted(set(e['status']))))}"
                                + (f" ({'; '.join(why)})" if why else "")
                                + (f", {e['unchecked']} checks unchecked" if e["unchecked"] else ""))
            for p in mine:
                for r in p["report"].get("records", []):
                    if r.get("case") != case:
                        continue
                    for s in series.get(case, []):
                        n = len((r.get("samples") or {}).get(s) or [])
                        if n < floors[case]:
                            problems.append(f"{case} on {side}, pass {p['n']}: {n} samples of {s}, {floors[case]} required")
    return problems


def judge(family: dict, results: Path) -> tuple[list[str], dict | None, dict | None, str]:
    """One round: its problems (no verdict), the A-A' arm's summary, the B vs A summary, its host."""
    path = results / "abba-manifest.json"
    manifest = json.loads(path.read_text(encoding="utf-8")) if path.is_file() else None
    passes = rg.load_passes(results / "passes") if (results / "passes").is_dir() else []
    problems = completeness(family, manifest, passes)
    host = (manifest or {}).get("host", "")
    if not problems and not host:
        problems = ["the manifest names no host"]
    if problems:
        return problems, None, None, host
    a = family["sides"]["a"]
    harness = manifest["harness"]
    context = {"host": host, "round": manifest.get("round", 1) - 1}
    of = lambda *names: [p for p in passes if p["side"] in names]  # noqa: E731
    catalog = family["catalog"]
    # The A-A' arm: A' against A and A against A', each over the family's dimensions; any
    # difference Holm finds there is the instrument's, and the round says nothing.
    aa = rg.gate(of("a", "a2"), {"a2": a, "a": a}, "a2", ["a"], harness, family["blocks"], catalog=catalog,
                 context=context, family=family["hypotheses"])
    back = rg.gate(of("a", "a2"), {"a": a, "a2": a}, "a", ["a2"], harness, family["blocks"], catalog=catalog,
                   context=context, family=[h.replace("a|", "a2|", 1) for h in family["hypotheses"]])
    problems += [f"A-A' arm: {e}" for e in aa["errors"] + back["errors"]]
    rejected = gatestats.holm({**{("aa", h): p for h, p in aa["pvalues"].items()},
                               **{("back", h): p for h, p in back["pvalues"].items()}})
    problems += [f"the A-A' arm itself differs: {h} ({'A slower' if d == 'aa' else 'A faster'} than its copy)"
                 for d, h in sorted(rejected)]
    if problems:
        return problems, aa, None, host
    # B against A: its spread at least the run's own A-A' floor, or its host's where larger.
    own = rg.noise_floor(aa)["cases"]
    hosts = (rg.read_floor(rg.FLOORS / f"{host}.toml") or {}).get("cases", {})
    floor = {"source": f"this round's A-A' arm ({family['blocks']} blocks)"
             + (f", or {host}'s where larger" if hosts else ""),
             "cases": {case: {d: max(sd, hosts.get(case, {}).get(d, 0.0)) for d, sd in dims.items()}
                       for case, dims in own.items()}}
    ab = rg.gate(of("a", "b"), {"b": family["sides"]["b"], "a": a}, "b", ["a"], harness, family["blocks"],
                 catalog=catalog, context=context, family=family["hypotheses"], floor=floor)
    problems += ab["errors"] + ab["blocking"]
    return problems, aa, ab, host


def report(family: dict, rounds: list[Path]) -> tuple[int, str, dict]:
    """The verdict over the rounds given (round 1 first) and its exit code."""
    judged = [judge(family, r) for r in rounds]
    lines = [f"# A-B-B-A {family['name']}: b {family['sides']['b']} against a {family['sides']['a']}", "",
             f"{len(family['hypotheses'])} hypotheses on {', '.join(dict.fromkeys(h for *_, h in judged if h)) or '-'}, "
             f"{family['blocks']} blocks a round, "
             f"{len(rounds)} rounds; family {family['path']} (sha256 {family['sha256'][:12]}).", ""]
    out = {"family": family["path"], "sha256": family["sha256"], "rounds": []}
    for k, (problems, aa, ab, host) in enumerate(judged, 1):
        out["rounds"].append({"problems": problems, "host": host, "pvalues": (ab or {}).get("pvalues"),
                              "aa_pvalues": (aa or {}).get("pvalues")})
        if problems:
            lines += [f"## Round {k}: no verdict", ""] + [f"- {p}" for p in problems] + [""]
    if any(problems for problems, *_ in judged):
        out["exit"] = NO_VERDICT
        lines.insert(4, "**No verdict** (exit 3): the evidence is missing, incomplete or failed, or the A-A' arm itself "
                        "differs. A failed experiment, not a pass.\n")
        return NO_VERDICT, "\n".join(lines), out
    decided = rg.decide([[ab for _, _, ab, _ in judged]])
    out["decided"] = {k: decided[k] for k in ("losses", "pending", "m")}
    if decided["losses"]:
        code, word = CONFIRMED, "**Confirmed loss** (exit 4): Holm over the family rejects, the rounds combined"
    elif decided["pending"]:
        code, word = LOSS_SHOWN, "**Loss shown** (exit 1): a confirmation round is due (abba.py jobs --round 2)"
    else:
        code, word = NO_LOSS, "**No loss shown** (exit 0)"
    out["exit"] = code
    lines.insert(4, word + ".\n")
    for k, (_, _, ab, _) in enumerate(judged, 1):
        lines += [f"## Round {k}", "", "| hypothesis | ratio | p | status |", "|---|---|---|---|"]
        for h in family["hypotheses"]:
            _, case, d = h.split("|", 2)
            r = ab["comparisons"]["a"][case]["dimensions"].get(d, {})
            lines.append(f"| {case} / {d} | {rg.fmt(r.get('ratio'))} | {r.get('p', 1.0):.3g} | {r.get('status', '-')} |")
        lines += ["", f"Floor: {ab['policy'].get('floor')}.", ""]
    return code, "\n".join(lines), out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="command", required=True)
    s = sub.add_parser("check")
    s.add_argument("family")
    s = sub.add_parser("jobs")
    s.add_argument("family")
    s.add_argument("--host", help="the timing host (overrides the family's)")
    s.add_argument("--round", type=int, default=1)
    s.add_argument("--harness", help="the revision whose harness runs it (default b)")
    s.add_argument("--checkout", default=ROOT.as_posix())
    s.add_argument("--out", type=Path, default=Path("."))
    s = sub.add_parser("report")
    s.add_argument("family")
    s.add_argument("results", type=Path, nargs="+", help="each round's results (FLEET_OUT), round 1 first")
    s.add_argument("--json", type=Path)
    s.add_argument("--out", type=Path)
    args = ap.parse_args()
    try:
        family = load_family(args.family)
    except (Invalid, OSError, tomllib.TOMLDecodeError) as error:
        problems = error.args[0] if isinstance(error, Invalid) else [str(error)]
        print("\n".join(f"invalid family {args.family}: {p}" for p in problems), file=sys.stderr)
        return INVALID
    if args.command == "check":
        print(f"{family['name']}: {len(family['hypotheses'])} hypotheses, {len(family['cases'])} cases, "
              f"{family['blocks']} blocks on {family['host']}")
        if os.environ.get("FLEET_OUT"):  # in a job: the manifest its report holds the results to
            harness = os.environ.get("ABBA_HARNESS") or git("rev-parse", "HEAD")
            manifest = manifest_of(family, harness, int(os.environ.get("ABBA_ROUND", "1")),
                                   os.environ.get("GATE_HOST") or family["host"])
            (Path(os.environ["FLEET_OUT"]) / "abba-manifest.json").write_text(json.dumps(manifest, indent=1),
                                                                              encoding="utf-8")
        return 0
    if args.command == "jobs":
        harness = git("rev-parse", args.harness or family["sides"]["b"], cwd=Path(args.checkout))
        found = jobs(family, harness, args.checkout, args.host or family["host"], args.round)
        args.out.mkdir(parents=True, exist_ok=True)
        for name, job in found.items():
            (args.out / f"{name}.toml").write_text(rg.toml_job(job), encoding="utf-8")
            print(args.out / f"{name}.toml")
        return 0
    code, text, out = report(family, args.results)
    print(text)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    if args.json:
        args.json.write_text(json.dumps(out, indent=1, default=str), encoding="utf-8")
    return code


if __name__ == "__main__":
    sys.exit(main())
