#!/usr/bin/env python3
"""A targeted A-B-B-A: one change (B) against its base (A), on the release gate's machinery, with
an interleaved A-A' arm. A family is built from the case registry by `run`, or declared in a file;
this checks it, writes its fleet job and gives the verdict. Nothing here re-implements timing or
statistics: the blocks are release-gate.sh's (a palindrome A A' B B A' A per case, every side a
fresh copy of its build, seeded orientation, the quiet watcher, at most one repeat of a disturbed
block), the tests release_gate.py's (gate(), Holm, rounds by decide(), the floor).

    abba.py run --a REV --b REV (--cases A,B | --areas X,Y) [--blocks N] [--round K] [--plan]
                                             one command: the family from benches/cases, its plan
                                             (estimate, the hosts that can take it, what waits there),
                                             then one fleet set (fleet wait set:ID); nothing to commit
    abba.py check FAMILY                     the family, valid or not (exit 2); in a job, its manifest
    abba.py jobs FAMILY [--host H] [--round K] [--harness REV] [--out DIR]
                                             the fleet set of a family file: one timed job that
                                             builds on the host it lands on, then times there
    abba.py report FAMILY RESULTS...          the verdict from each round's results (in round order)

FAMILY is a file, or env:ABBA_FAMILY (the family's text in that variable: how a job carries a
family of registry cases). A family of registry cases runs from any file, committed or not; one
with cases of its own lives in the repository (benches/abba/), since the harness's fast suite
reads them there (NRESE_FAMILY).

A family file (benches/abba/NAME.toml, or what `run` writes):

    name = "lcp"                  # the jobs' names: abba-lcp-...
    host = "phuoc-yu"             # optional: pin every block to this host (--host overrides); without
                                  # it the fleet picks a timing host by capability
    blocks = 3                    # a round's blocks: at least 3 (one whole square), in whole squares
    [sides]
    a = "REV"                     # the base: a commit id, resolved to its full id (a short one too)
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
import uuid
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
MIN_BLOCKS = 3  # one whole Latin square of A, A' and B; a family may ask for more (owner, 20 Oct 2026)
NO_LOSS, LOSS_SHOWN, INVALID, NO_VERDICT, CONFIRMED = 0, 1, 2, 3, 4
NAME = re.compile(r"^[a-z0-9][a-z0-9-]*$")
REV = re.compile(r"^[0-9a-f]{7,40}$")
# A family carried in a job's environment (`run`, and `jobs` of a family of registry cases): its
# text in ABBA_FAMILY, read by the job's check and report as this name.
ENV_FAMILY = "env:ABBA_FAMILY"


class Invalid(Exception):
    """A family that can't be run: its problems, one line each."""


def family_path(path: str | Path, repo: Path) -> str:
    """The family's path as the jobs and NRESE_FAMILY name it: relative to the checkout `repo`."""
    resolved = Path(path).resolve()
    try:
        return resolved.relative_to(Path(repo).resolve()).as_posix()
    except ValueError:
        raise Invalid([f"{path}: a family with cases of its own lives in the repository (benches/abba/), "
                       "where the harness's fast suite reads them; a family of registry cases runs from anywhere"])


def read_family(path: str | Path, repo: Path) -> tuple[str, bytes, bool]:
    """The family's name as the jobs give it, its bytes, and whether it lies in the checkout `repo`:
    env:ABBA_FAMILY (its text in that variable), a file of the repository (its relative path), or
    a file elsewhere (its own path)."""
    if str(path) == ENV_FAMILY:
        text = os.environ.get("ABBA_FAMILY")
        if not text:
            raise Invalid([f"{ENV_FAMILY}: ABBA_FAMILY is empty or not set"])
        return ENV_FAMILY, text.encode("utf-8"), False
    resolved = Path(path).resolve()
    try:
        return resolved.relative_to(Path(repo).resolve()).as_posix(), resolved.read_bytes(), True
    except ValueError:
        return resolved.as_posix(), resolved.read_bytes(), False


def full_ids(sides: dict[str, str], repo: Path) -> tuple[dict[str, str], list[str]]:
    """The sides as full commit ids, resolved against the checkout `repo` (the harness's). A pass
    records its binary's full revision and the gate compares it exactly, so a short id gave every
    pass of a complete run "no verdict" (lcp-passes, 21 Oct 2026). A short id that names no commit
    there, or more than one, is refused; a full one is taken as it is (its build checks it)."""
    out, problems = {}, []
    for side, rev in sides.items():
        if len(rev) == 40:
            out[side] = rev
            continue
        found = subprocess.run(["git", "rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}"], cwd=repo,
                               capture_output=True, text=True)
        if found.returncode == 0 and REV.match(found.stdout.strip()):
            out[side] = found.stdout.strip()
        else:
            problems.append(f"side {side}: {rev} names no single commit of this checkout: give its full id")
    return out, problems


def load_family(path: str | Path, repo: Path, catalog: list[dict] | None = None) -> dict:
    """The family, resolved and validated: a file (read_family(); one with cases of its own must
    be a file of the checkout `repo`, main's is ROOT), its sides as full ids of that checkout
    (full_ids()), its cases (its own, or the catalogue's: the one beside this module, benches/cases
    of the tree under test, whatever `repo` is; each with a content check), its hypotheses (b vs a,
    per case and declared dimension; `catalog`: the registry to take them from, by default this
    tree's). Raises Invalid."""
    rel, raw, inside = read_family(path, repo)
    data = tomllib.loads(raw.decode("utf-8"))
    own = [e for e in data.get("case", []) if "catalog" not in e]
    if own and not inside:
        raise Invalid([f"{path}: a family with cases of its own lives in the repository (benches/abba/), where the "
                       "harness's fast suite reads them; a family of registry cases runs from anywhere"])
    problems = []
    name, host, blocks = data.get("name", ""), data.get("host", ""), data.get("blocks", MIN_BLOCKS)
    sides = data.get("sides", {})
    if not NAME.match(str(name)):
        problems.append(f"name {name!r}: lower-case letters, digits and -")
    if not isinstance(blocks, int) or blocks < MIN_BLOCKS:
        problems.append(f"blocks {blocks!r}: at least {MIN_BLOCKS}")
    else:
        # Whole Latin squares of the three sides (release_gate.whole_squares(); Sol r1: 8 blocks
        # left A, A' or B opening one block fewer).
        blocks = rg.whole_squares(blocks, len(SIDES))
    if set(sides) != {"a", "b"} or not all(isinstance(v, str) and REV.match(v) for v in sides.values()):
        problems.append("sides: exactly a and b, each a commit id (7 to 40 hex digits)")
    else:
        sides, unresolved = full_ids(sides, repo)
        problems += unresolved
    entries = data.get("case", [])
    if not entries:
        problems.append("no [[case]]")
    if own:
        catalog = cases_registry.load(family=Path(repo).resolve() / rel)
    elif catalog is None:
        catalog = cases_registry.load()
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
        cases.append({"name": case["name"], "cap_gb": case.get("cap_gb", 4), "min_samples": int(entry.get("min_samples", 1)),
                      "checks": sorted(case.get("check") or {}), "tool": case.get("tool", "perf_lab")})
        hypotheses += [rg.hypothesis("a", case["name"], d) for d in wanted if d in offered]
    if len({c["name"] for c in cases}) != len(cases):
        problems.append("a case is declared twice")
    if problems:
        raise Invalid(problems)
    return {"path": rel, "sha256": hashlib.sha256(raw).hexdigest(), "name": name, "host": host, "blocks": blocks,
            "sides": dict(sides), "cases": cases, "hypotheses": hypotheses, "catalog": catalog,
            "text": raw.decode("utf-8"), "own": bool(own)}


# --- a family from the registry (`run`) ---------------------------------------------------------

def select(catalog: list[dict], cases: list[str], areas: list[str]) -> list[str]:
    """The registry's cases a run names: `cases` by name, then every case of `areas` that can be
    timed (a content check, no declared outcome), in registry order, each once. Raises Invalid on
    an unknown name or area, a named case that can't be timed, or an empty selection."""
    by_name = {c["name"]: c for c in catalog}
    problems = [f"no case {n!r} in benches/cases/" for n in cases if n not in by_name]
    known = {c["area"] for c in catalog}
    problems += [f"no area {a!r} (areas: {', '.join(sorted(known))})" for a in areas if a not in known]
    timeable = lambda c: bool(c.get("check")) and c.get("outcome", "ok") == "ok"  # noqa: E731
    for n in cases:
        if n in by_name and not timeable(by_name[n]):
            why = "no content check" if not by_name[n].get("check") else f"a declared outcome ({by_name[n]['outcome']})"
            problems.append(f"{n}: {why}: not timed")
    for a in areas:
        if a in known and not any(timeable(c) for c in catalog if c["area"] == a):
            problems.append(f"area {a}: no case with a content check and no declared outcome")
    chosen = list(dict.fromkeys([n for n in cases if n in by_name]
                                + [c["name"] for c in catalog if c["area"] in areas and timeable(c)]))
    if not chosen and not problems:
        problems.append("no case: name --cases or --areas")
    if problems:
        raise Invalid(problems)
    return chosen


def family_text(name: str, sides: dict[str, str], cases: list[str], blocks: int, min_samples: int = 1) -> str:
    """A family of registry cases as a family file's text (LF only: its SHA-256 is the family's
    identity, on every host)."""
    lines = [f'# A-B-B-A of b against a over registry cases (benches/suite/abba.py run)',
             f'name = "{name}"', f"blocks = {blocks}", "[sides]", f'a = "{sides["a"]}"', f'b = "{sides["b"]}"']
    for case in cases:
        lines += ["[[case]]", f'catalog = "{case}"'] + ([f"min_samples = {min_samples}"] if min_samples > 1 else [])
    return "\n".join(lines) + "\n"


def run_name(name: str | None, sides: dict[str, str], harness: str, cases: list[str], blocks: int,
             min_samples: int) -> str:
    """A run's family name, and so its set's id: the request's own (Sol r1 S2: another A, case list,
    block count or sample floor gave the same id, and the fleet refused the new set as the old one)."""
    request = f"{sides['a']} {sides['b']} {harness} {','.join(cases)} {blocks} {min_samples}"
    return f"{name or 'run'}-{hashlib.sha256(request.encode()).hexdigest()[:8]}"


def suggested_blocks(cases: list[str], measure: dict) -> int:
    """A first round's blocks from the cases' calibration (measure.toml: a round's `blocks`, or in
    the older form a side's `passes`, two to a block), at least one square: effort follows spread
    (benches/abba/README.md)."""
    def need(entry: dict) -> int:
        return int(entry["blocks"]) if "blocks" in entry else (int(entry.get("passes", 0)) + 1) // 2
    return rg.whole_squares(max([need(measure.get(c, {})) for c in cases] + [MIN_BLOCKS]), len(SIDES))


def estimate(family: dict, measure: dict, builds: int) -> dict:
    """The job's minutes (release_gate.plan_minutes(): the gate's model, the same for both tools):
    its builds and data, shareable, then its blocks alone, every block all its cases on A, A' and B."""
    est = rg.plan_minutes(builds, len(SIDES), [[c["name"] for c in family["cases"]]] * family["blocks"], measure)
    return est | {"block_min": round(est["timed_min"] / family["blocks"], 1), "total_min": round(est["total_min"])}


def manifest_of(family: dict, harness: str, round_: int, host: str, job: str = "") -> dict:
    """What a round is held to; `host` is the one its blocks ran on (jobs --host may override the
    family's), whose floor the report takes. `session`: drawn anew by every check, and `job` the
    fleet job that ran it, so that a round is told from any other (rounds_errors())."""
    return {k: family[k] for k in ("path", "sha256", "name", "blocks", "sides", "cases", "hypotheses")} | {
        "harness": harness, "round": round_, "host": host, "session": uuid.uuid4().hex, "job": job}


def git(repo: Path, *args: str) -> str:
    """git in the checkout `repo`, named by every caller (a test's own repository, never this one's
    history by default)."""
    return subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True, check=True).stdout.strip()


# --- jobs --------------------------------------------------------------------------------------

def jobs(family: dict, harness: str, checkout: str, host: str = "", round_: int = 1,
         priority: int = 790, reason: str = "") -> dict[str, dict]:
    """The family's fleet job, one: on the timing host the fleet picks by capability (TIMING_ROLES,
    Linux, the cases' needs and caps; `host` pins it, with its reason), its check (the manifest the
    report holds the results to, naming that host), the builds of A, B and the harness and the
    data's preparation, all in its first step, the one untimed (shareable) step; then alone its
    blocks, its report (the verdict in abba-verdict.json; the step fails only for an invalid
    family) and clean-up. Builds and blocks share the host, so nothing is staged between jobs: the
    fleet keeps an exclusive job's host only before an exclusive step (planner held_between_steps;
    Sol r1 S1, 2 Nov 2026), so every step after the first is one."""
    a, b = family["sides"]["a"], family["sides"]["b"]
    prefix = f"abba-{family['name']}-{b[:8]}" + (f"-r{round_}" if round_ > 1 else "")
    gate_ = ["bash", "scripts/release-gate.sh"]
    abba = ["python3", "-B", "benches/suite/abba.py"]
    cap = max(c["cap_gb"] for c in family["cases"])
    env = {"GATE_NAME": f"abba-{family['name']}-{b[:8]}",
           "GATE_CASES": ",".join(c["name"] for c in family["cases"]), "GATE_BASELINES": "a",
           "GATE_MAX_GB": str(cap), "NRESE_QUIET_SLOT": "0",
           "GATE_MIN_BLOCKS": str(family["blocks"]), "GATE_ROUND": str(round_ - 1),
           "GATE_PREFIX": prefix, "ABBA_HARNESS": harness, "ABBA_ROUND": str(round_)}
    if family["own"]:
        env["NRESE_FAMILY"] = family["path"]  # the harness's fast suite reads its own cases there
        named = family["path"]
    else:
        env["ABBA_FAMILY"] = family["text"]  # the family itself: nothing to commit
        named = ENV_FAMILY
    if host:
        env["GATE_HOST"] = host
    source = [{"name": "checkout", "source": {"git": checkout, "rev": rg.checkout_rev(checkout, b, harness)}}]
    revisions = list(dict.fromkeys([a, b, harness]))
    prepare = [["bash", "-c", 'set -e; family=$1; shift; python3 -B benches/suite/abba.py check "$family"; '
                'for rev in "$@"; do bash scripts/release-gate.sh build "$rev"; done; '
                'bash scripts/release-gate.sh prepare "$ABBA_HARNESS"', "prepare", named, *revisions]]
    blocks = [[*gate_, "block", str(k), harness, "1", "-", f"a={a}", f"a2={a}", f"b={b}"]
              for k in range(1, family["blocks"] + 1)]
    after = [["bash", "-c", 'python3 -B benches/suite/abba.py report "$1" "$FLEET_OUT" --json "$FLEET_OUT/abba-verdict.json" '
              '--out "$FLEET_OUT/abba-verdict.md"; code=$?; echo "$code" > "$FLEET_OUT/abba-exit"; [ "$code" != 2 ]',
              "abba", named], [*gate_, "clean"]]
    steps = prepare + blocks + after
    untimed = [0]
    by_name = {c["name"]: c for c in family["catalog"]}
    has = rg.data_needs([by_name[c["name"]] for c in family["cases"] if c["name"] in by_name])
    requires = {"roles": rg.TIMING_ROLES, "os": "linux", "cores": 16, "ram_gb": max(12, cap)} | ({"has": has} if has else {})
    job = {"id": prefix, "group": "abba", "kind": "owlrs.timed", "class": "exclusive",
           "priority": priority, "priority_reason": reason or f"A-B-B-A: {b[:8]} against {a[:8]}, "
           f"{len(family['cases'])} cases", "notify": False,
           "env": env, "est_min": 0, "stall_after_min": 60, "steps": steps, "untimed_steps": untimed,
           "step_needs": [{"cores": 16, "ram_gb": 12}], "step_deadline_min": rg.BUILD_MIN * len(revisions) + 120,
           "requires": requires, "inputs": source + rg.side_inputs(checkout, source[0]["source"]["rev"], revisions)}
    if host:
        requires["hosts"] = [host]
        job["pin_reason"] = "the family names its timing host (its floor, or a run to reproduce there)"
    measure = measure_at(checkout, harness)
    job["est_min"] = estimate(family, measure, len(revisions))["total_min"]
    return {prefix: job}


def measure_at(checkout: str, rev: str) -> dict:
    """The harness's calibration (benches/fast/measure.toml at `rev`), {} where it has none."""
    shown = subprocess.run(["git", "-C", checkout, "show", f"{rev}:benches/fast/measure.toml"],
                           capture_output=True, text=True, encoding="utf-8")
    return tomllib.loads(shown.stdout).get("case", {}) if shown.returncode == 0 else {}


def set_file(set_id: str, members: list[dict]) -> str:
    """One fleet set of `members`: one wait, one cancel, one result; a member that fails stops
    the rest."""
    return rg.toml_set({"id": set_id, "fail": "stop-rest"}, members)


# --- the verdict ---------------------------------------------------------------------------------

def missing_checks(record: dict, paths: list[str], per_invocation: bool = True) -> list[str]:
    """The declared content checks (`paths`) a run has no passing result for. Sol, 20 Oct 2026:
    a run with no check results counted as checked (r1); then an invocation whose check list was
    empty or absent borrowed a sibling's results (r2). With `per_invocation` (perf_lab: fast.py
    checks every invocation's answer, its observation's `checks`, and the record's own are only
    the last invocation's), every observation must prove every path itself, and only a record
    without observations is proved by its own results. Otherwise (the other tools: one
    observation, the record's checks) the record's results, and each observation that reports
    checks must cover them too."""
    observations = record.get("observations") or []
    if per_invocation and observations:
        units = [o.get("checks") or [] for o in observations]
    else:
        units = [list(record.get("checks") or []), *[o["checks"] for o in observations if o.get("checks")]]
    return sorted({p for unit in units for p in paths
                   if not any(c.get("path") == p and c.get("ok") is True for c in unit)})


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
    declared = {c["name"]: c["checks"] for c in family["cases"]}
    per_invocation = {c["name"]: c.get("tool", "perf_lab") == "perf_lab" for c in family["cases"]}
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
            unproven = {}
            for p in mine:
                for r in p["report"].get("records", []):
                    if r.get("case") != case:
                        continue
                    for path in missing_checks(r, declared[case], per_invocation[case]):
                        unproven.setdefault(path, []).append(p["n"])
                    for s in series.get(case, []):
                        n = len((r.get("samples") or {}).get(s) or [])
                        if n < floors[case]:
                            problems.append(f"{case} on {side}, pass {p['n']}: {n} samples of {s}, {floors[case]} required")
            if unproven:
                problems.append(f"{case} on {side}: no passing result for its content checks "
                                + "; ".join(f"{path} (passes {', '.join(map(str, ns))})"
                                            for path, ns in sorted(unproven.items())[:3])
                                + (f", and {len(unproven) - 3} more" if len(unproven) > 3 else ""))
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


def rounds_errors(rounds: list[Path]) -> list[str]:
    """Why the rounds given aren't rounds 1, 2, ... of their own (empty: they are). Sol, 20 Oct 2026
    (r1): the same round given twice confirmed its own loss. Each must be a distinct run with
    fresh copies: its own directory, manifest round k at place k, its own check's session and
    fleet job, no pass file that another round has too, its blocks after the round before."""
    errors, seen, sessions, jobs_, spans = [], {}, {}, {}, []
    for k, results in enumerate(rounds, 1):
        where = Path(results).resolve()
        if where in seen:
            errors.append(f"round {k} is round {seen[where]}'s directory again ({results})")
            continue
        seen[where] = k
        path = where / "abba-manifest.json"
        manifest = json.loads(path.read_text(encoding="utf-8")) if path.is_file() else {}
        if manifest and manifest.get("round") != k:
            errors.append(f"round {k} ({results}): its manifest says round {manifest.get('round')}")
        for key, owners, what in (("session", sessions, "check's session"), ("job", jobs_, "fleet job")):
            value = manifest.get(key)
            if value and value in owners:
                errors.append(f"round {k} has round {owners[value]}'s {what} ({value})")
            elif value:
                owners[value] = k
        starts, ends = [], []
        for p in sorted((where / "passes").glob("pass-*.json")) if (where / "passes").is_dir() else []:
            digest = hashlib.sha256(p.read_bytes()).hexdigest()
            if digest in seen:
                errors.append(f"round {k}'s {p.name} is round {seen[digest]}'s evidence again")
            seen.setdefault(digest, k)
            report_ = json.loads(p.read_text(encoding="utf-8"))
            starts.append(report_.get("started_ns") or 0)
            ends.append(report_.get("finished_ns") or 0)
        if starts:
            spans.append((k, min(starts), max(ends)))
    for (j, _, end), (k, start, _) in zip(spans, spans[1:]):
        if start <= end:
            errors.append(f"round {k} didn't start after round {j} ended: not a confirmation of it")
    return errors


def report(family: dict, rounds: list[Path]) -> tuple[int, str, dict]:
    """The verdict over the rounds given (round 1 first) and its exit code."""
    if errors := rounds_errors(rounds):
        text = "\n".join([f"# A-B-B-A {family['name']}: no verdict", "",
                          "**No verdict** (exit 3): the results given aren't distinct rounds in order.", ""]
                         + [f"- {e}" for e in errors])
        return NO_VERDICT, text, {"family": family["path"], "exit": NO_VERDICT, "rounds_errors": errors}
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


def plan_text(family: dict, job: dict, harness: str, measure: dict, set_id: str) -> str:
    """What a run will do and cost, before anything is queued (intent.bench-by-purpose: "I could
    see estimated time value for those to run to see how much it would block")."""
    est = estimate(family, measure, len(dict.fromkeys([*family["sides"].values(), harness])))
    by_name = {c["name"]: c for c in family["catalog"]}
    lines = [f"{set_id}: b {family['sides']['b'][:12]} against a {family['sides']['a'][:12]}, "
             f"harness {harness[:12]}, round {job['env']['ABBA_ROUND']}",
             f"  cases ({len(family['cases'])}): " + ", ".join(
                 f"{c['name']} ({by_name.get(c['name'], {}).get('area', '?')}, cap {c['cap_gb']} GB, "
                 + (f"{measure[c['name']]['seconds']} s a pass" if measure.get(c["name"], {}).get("seconds")
                    else "uncalibrated") + ")" for c in family["cases"]),
             f"  hypotheses: {len(family['hypotheses'])} (Holm over the family); blocks: {family['blocks']} "
             f"(whole squares of A, A' and B), {est['block_min']} min each",
             f"  estimate: builds and data up to {est['build_min']} min (shareable; a build of the same identity "
             f"is reused), then {est['timed_min']} min alone on one timing host; {est['total_min']} min, "
             f"{est['total_min'] / 60:.2f} host-hours, {est['timed_min'] / 60:.2f} of them exclusive"]
    if est["guessed"]:
        lines.append(f"  not calibrated (benches/fast/measure.toml): {', '.join(est['guessed'])}: a minute a pass assumed")
    asks = job["requires"]
    lines.append(f"  asks: {job['kind']}, roles {'+'.join(asks['roles'])}, {asks['os']}, {asks['cores']} cores, "
                 f"{asks['ram_gb']} GB" + (f", has {'+'.join(asks['has'])}" if asks.get("has") else "")
                 + (f", pinned to {asks['hosts'][0]} ({job['pin_reason']})" if asks.get("hosts") else "")
                 + f"; priority {job['priority']}")
    return "\n".join(lines)


def run(args) -> int:
    """`run`: the family from the registry, its plan, its set; submitted unless --plan."""
    checkout = Path(args.checkout)
    sides, problems = full_ids({"a": args.a, "b": args.b}, checkout)
    harness = git(checkout, "rev-parse", args.harness or sides.get("b", args.b)) if not problems else ""
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return INVALID
    catalog = cases_registry.load_at(checkout, harness)  # the registry the job will run
    try:
        chosen = select(catalog, [c for c in (args.cases or "").split(",") if c],
                        [a for a in (args.areas or "").split(",") if a])
    except Invalid as error:
        print("\n".join(error.args[0]), file=sys.stderr)
        return INVALID
    measure = measure_at(checkout.as_posix(), harness)
    blocks = args.blocks or suggested_blocks(chosen, measure)
    name = run_name(args.name, sides, harness, chosen, blocks, args.min_samples)
    text = family_text(name, sides, chosen, blocks, args.min_samples)
    out = args.out or ROOT / "benches" / "suite" / "results" / "abba" / f"{name}-{sides['b'][:8]}"
    out.mkdir(parents=True, exist_ok=True)
    path = out / "family.toml"
    path.write_bytes(text.encode("utf-8"))
    try:
        family = load_family(path, checkout, catalog)
    except Invalid as error:
        print("\n".join(f"invalid family: {p}" for p in error.args[0]), file=sys.stderr)
        return INVALID
    found = jobs(family, harness, checkout.as_posix(), args.host or "", args.round, args.priority, args.reason or "")
    set_id = next(iter(found))
    set_path = out / f"{set_id}.set.toml"
    set_path.write_bytes(set_file(set_id, list(found.values())).encode("utf-8"))
    print(plan_text(family, found[set_id], harness, measure, f"set:{set_id}"))
    print(f"  family: {path} (sha256 {family['sha256'][:12]}); set file: {set_path}")
    code, said = rg.fleet_call("check", str(set_path))
    print("  fleet check" + (f" (exit {code})" if code else "") + ":\n    " + "\n    ".join(said.splitlines() or ["-"]))
    if args.plan:
        print("plan only: nothing queued")
        return 0
    if code != 0:
        print("not submitted: fleet check didn't take it (above)", file=sys.stderr)
        return 1
    code, said = rg.fleet_call("submit", str(set_path))
    print(said)
    if code != 0:
        return 1
    print(f"wait: fleet wait set:{set_id}   verdict: fleet results set:{set_id}, then its member's "
          f"abba-verdict.md; a combined report: abba.py report {path} R1 R2")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="command", required=True)
    s = sub.add_parser("run", help="time b against a over registry cases: plan, then one fleet set")
    s.add_argument("--a", required=True, help="the base, a commit")
    s.add_argument("--b", required=True, help="the change, a commit (A = B: an A/A run)")
    s.add_argument("--cases", help="registry cases, comma-separated")
    s.add_argument("--areas", help="registry areas: every case of each that can be timed")
    s.add_argument("--blocks", type=int, help="a round's blocks (default: from the cases' calibrated spread, at least 3)")
    s.add_argument("--min-samples", type=int, default=1)
    s.add_argument("--round", type=int, default=1)
    s.add_argument("--name", help="the family's name (default: from its cases); a new name is a new set")
    s.add_argument("--host", help="pin to this timing host (default: the fleet picks one by capability)")
    s.add_argument("--harness", help="the revision whose harness runs it (default b)")
    s.add_argument("--priority", type=int, default=790)
    s.add_argument("--reason", help="the priority's reason (default: what is timed)")
    s.add_argument("--checkout", default=ROOT.as_posix())
    s.add_argument("--out", type=Path, help="where the family and set files go (default suite/results/abba/...)")
    s.add_argument("--plan", action="store_true", help="the plan and fleet's check only: nothing queued")
    s = sub.add_parser("check")
    s.add_argument("family")
    s = sub.add_parser("jobs")
    s.add_argument("family")
    s.add_argument("--host", help="pin to this timing host (overrides the family's)")
    s.add_argument("--round", type=int, default=1)
    s.add_argument("--harness", help="the revision whose harness runs it (default b)")
    s.add_argument("--priority", type=int, default=790)
    s.add_argument("--reason")
    s.add_argument("--checkout", default=ROOT.as_posix())
    s.add_argument("--out", type=Path, default=Path("."))
    s = sub.add_parser("report")
    s.add_argument("family")
    s.add_argument("results", type=Path, nargs="+", help="each round's results (FLEET_OUT), round 1 first")
    s.add_argument("--json", type=Path)
    s.add_argument("--out", type=Path)
    args = ap.parse_args()
    if args.command == "run":
        return run(args)
    try:
        family = load_family(args.family, ROOT)
    except (Invalid, OSError, tomllib.TOMLDecodeError) as error:
        problems = error.args[0] if isinstance(error, Invalid) else [str(error)]
        print("\n".join(f"invalid family {args.family}: {p}" for p in problems), file=sys.stderr)
        return INVALID
    if args.command == "check":
        # In a job, the host it runs on: a pinned family's (GATE_HOST), else the one the fleet chose.
        host = os.environ.get("GATE_HOST") or os.environ.get("FLEET_HOST") or family["host"]
        print(f"{family['name']}: {len(family['hypotheses'])} hypotheses, {len(family['cases'])} cases, "
              f"{family['blocks']} blocks (whole squares of A, A' and B) on {host or 'a host the fleet picks'}")
        if os.environ.get("FLEET_OUT"):  # in a job: the manifest its report holds the results to
            harness = os.environ.get("ABBA_HARNESS") or git(ROOT, "rev-parse", "HEAD")
            manifest = manifest_of(family, harness, int(os.environ.get("ABBA_ROUND", "1")), host,
                                   os.environ.get("FLEET_JOB", ""))
            (Path(os.environ["FLEET_OUT"]) / "abba-manifest.json").write_text(json.dumps(manifest, indent=1),
                                                                              encoding="utf-8")
        return 0
    if args.command == "jobs":
        harness = git(Path(args.checkout), "rev-parse", args.harness or family["sides"]["b"])
        found = jobs(family, harness, args.checkout, args.host or family["host"], args.round, args.priority,
                     args.reason or "")
        args.out.mkdir(parents=True, exist_ok=True)
        set_id = next(iter(found))
        path = args.out / f"{set_id}.set.toml"
        path.write_bytes(set_file(set_id, list(found.values())).encode("utf-8"))
        print(plan_text(family, found[set_id], harness, measure_at(args.checkout, harness), f"set:{set_id}"))
        print(f"  submit: fleet submit {path}   wait: fleet wait set:{set_id}")
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
