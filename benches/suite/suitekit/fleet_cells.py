"""Selected registry cells to ordinary fleet sets; the suite remains the measurement driver."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import abba
from . import adapters, cases, driver, ledger, manifest, status, workloads as definitions
from .runtime import Mount


def names(text):
    return sorted(set(filter(None, (v.strip() for v in text.split(",")))))


def publication_problem(key, system):
    if key in {"rdfox", "allegrograph"} or system.get("refused"):
        return "refused: " + str(system.get("refused") or "never measure")
    if system.get("publish") != "free":
        return "restricted: no verified entitlement/export permission; private-only, not queued"


def eligible(root, key, system, workload, plan, settings, passed):
    """The admission boundary, also used for references; never create restricted outputs."""
    if why := publication_problem(key, system):
        return why
    if plan.unavailable:
        return plan.unavailable
    if missing := driver.verdict(system, workload):
        return "lacks " + missing
    if plan.kind != "cycle":
        return "no admitted fleet kit/driver evidence for this plan kind"
    instance = adapters.adapter(key)
    if instance is None or (plan.systems is not None and key not in plan.systems):
        return "no adapter for this plan"
    regime = next((r for r in plan.regimes if r in instance.regimes), None)
    if regime is None:
        return "unsupported regime: " + ",".join(plan.regimes)
    if key not in passed:
        return "no historical successful adapter evidence"
    ctx = adapters.Context(SimpleNamespace(kind="docker"), root, root, root, Mount("-", "/data"),
                           plan.mounts, 0, settings, "fleet-plan", tier_small=plan.small)
    if why := instance.supports(ctx, plan.inputs_of(key), regime):
        return why
    if plan.mounts:
        return "external assets have no fleet staging declaration"
    if plan.queries and not list(plan.queries.glob("*.rq")):
        return f"missing query assets: {plan.queries}"
    return None


def select(root, systems, workloads, args):
    catalog = cases.load()
    records = ledger.load_records(root / "benches/runs")
    passed = {p["system"] for r in records for p in r.get("pairs", []) if p.get("outcome") == "ok"}
    indexed = status.newest(records)
    settings = {"SPARQL_DATA": str(abba.rg.sparql_data())}
    selected, omitted, total = {}, [], 0.0
    for name in names(args.workloads or ",".join(workloads)):
        if name not in workloads:
            raise ValueError(f"unknown workload {name}")
        tiers = [t.split("=", 1)[1] for t in names(args.tiers) if t.split("=", 1)[0] == name]
        for tier in tiers or status.standard_tiers(name, workloads[name]):
            plans = definitions.plans(root, name, [tier], settings)
            plan = plans[0] if plans else None
            case = next((c for c in catalog if c.get("workload") == name and str(c.get("tier")) == tier), None)
            for key in names(args.systems or ",".join(k for k, s in systems.items() if not s.get("variant-of"))):
                label = f"{key} / {name} / {tier}"
                if key not in systems:
                    raise ValueError(f"unknown system {key}")
                why = eligible(root, key, systems[key], workloads[name], plan, settings, passed) if plan else "no suite plan"
                if not why and not case:
                    why = "no case resource/asset declaration"
                if not why:
                    frozen = abba.rg.frozen(set(abba.rg.volume_files(case)), root / "benches/datasets.toml")
                    if any(f not in frozen for f in abba.rg.volume_files(case)):
                        why = "input lacks frozen asset identity"
                # Slice 1 only trusts indexed successes at this exact harness; refresh is separate.
                entry = indexed.get((name, tier, key))
                if not why and args.gaps and entry and entry[1]["outcome"] == "ok":
                    if any(r["id"] == entry[2] and r.get("commit") == args.harness for r in records):
                        why = "already indexed at this harness (coverage freshness refresh deferred)"
                if not why:
                    regime = next(r for r in plan.regimes if r in adapters.adapter(key).regimes)
                    refs = names(args.references)
                    for ref in refs:
                        reason = (eligible(root, ref, systems[ref], workloads[name], plan, settings, passed)
                                  if ref in systems else "unknown reference")
                        if reason or next((r for r in plan.regimes if r in adapters.adapter(ref).regimes), None) != regime:
                            why = f"reference {ref}: {reason or 'different regime'}"
                            break
                if not why:
                    group = selected.get((name, tier), {"case": case, "systems": [], "regime": regime})
                    before = set(group["systems"]) | set(refs) if group["systems"] else set()
                    after = set(group["systems"]) | {key} | set(refs)
                    overhead = args.build_min * len({args.nrese, args.harness}) + args.prepare_min
                    added = (len(after) - len(before)) * case["est_min"] + (0 if before else overhead)
                    if total + added >= args.budget_hours * 60:
                        why = f"budget: {added / 60:.2f} additional host-hours exceeds remaining strict budget"
                    else:
                        group["systems"].append(key)
                        group["minutes"] = len(after) * case["est_min"] + overhead
                        group["references"] = refs
                        selected[name, tier] = group
                        total += added
                if why:
                    omitted.append(f"omit {label}: {why}")
                else:
                    print(f"{label}: {case['est_min'] / 60:.2f} host-hours, unmeasured catalogue estimate")
    return list(selected.values()), omitted, total


def jobs(groups, args):
    request = {k: v for k, v in vars(args).items() if k not in ("out", "submit", "plan", "systems", "workloads", "tiers", "gaps")}
    request["groups"] = groups
    token = hashlib.sha256(json.dumps(request, sort_keys=True, default=str).encode()).hexdigest()
    set_id, members = "suite-cells-" + token[:16], []
    for group in groups:
        case = group["case"]
        member = f"{set_id}-{case['name']}"
        cap = case["cap_gb"]
        needs = {"cores": args.cores, "ram_gb": cap + args.driver_gb,
                 "disk_gb": args.build_disk_gb + abba.rg.data_disk_gb([case])}
        payload = {"group": group, "nrese": args.nrese, "harness": args.harness, "member": member,
                   "result_root": args.result_root, "deadline_min": group["minutes"] * args.deadline_factor}
        members.append({"id": member, "group": "suite-cells", "kind": "owlrs.timed", "class": "exclusive",
                        "purpose": "timing", "priority": 600, "priority_reason": "catalogue cells below confirmation and merge gates",
                        "notify": False, "hold_between_steps": True, "est_min": group["minutes"],
                        "step_deadline_min": payload["deadline_min"], "results_at": f"{args.result_root}/{member}",
                        "steps": [["bash", "-c", 'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"; '
                                   'exec uv run --no-project python -B benches/suite/suite.py fleet --_member']],
                        "step_needs": [needs], "requires": {**needs, "os": "linux", "roles": abba.rg.TIMING_ROLES,
                        "has": sorted(set(case.get("needs", [])) | set(abba.rg.data_needs([case])) | {"docker"})},
                        "env": {"SUITE_CELL": json.dumps(payload), "DOCKER_CPUS": str(args.cores), "GATE_MAX_GB": str(cap),
                                "DOCKER_MEMORY": f"{cap}g",
                                "NRESE_GATE_DIR": f"~/owlrs/{member}", "GATE_NAME": member, "GATE_CASES": case["name"]},
                        "inputs": [{"name": "checkout", "source": {"git": str(args.checkout), "rev": args.harness}}]
                                  + abba.rg.side_inputs(str(args.checkout), args.harness, [args.nrese])})
    return set_id, token, members


def member(root, systems):
    """Fleet step glue only: existing build/data helpers, then the unchanged suite run."""
    payload = json.loads(os.environ["SUITE_CELL"])
    group = payload["group"]
    chosen = sorted(set(group["systems"]) | set(group["references"]))
    for key in chosen:
        if why := publication_problem(key, systems.get(key, {})):
            print(f"{key}: {why}", file=sys.stderr)
            return 2
    landing = Path(payload["result_root"]).expanduser() / payload["member"]
    if landing.resolve().is_relative_to(root.resolve()):
        raise ValueError("private result root must be outside the checkout")
    landing.mkdir(parents=True, exist_ok=True)
    (landing / "identity.json").write_text(json.dumps({"nrese": payload["nrese"], "harness": payload["harness"],
        "git": manifest.git(root), "machine": manifest.machine("process"), "classification": "private/free"}, indent=2), encoding="utf-8")
    out = Path(os.environ["FLEET_OUT"])
    env = {**os.environ, "PATH": str(Path.home() / ".cargo/bin") + os.pathsep + os.environ["PATH"]}
    gate = Path(env["NRESE_GATE_DIR"]).expanduser()
    env.update(NRESE_GATE_DIR=str(gate), SPARQL_DATA=str(abba.rg.sparql_data()))
    target = out.parent / "suite-client-target"
    env["CARGO_TARGET_DIR"] = str(target)
    code = 1
    with (landing / "stdout.log").open("a") as stdout, (landing / "stderr.log").open("a") as stderr:
        def run(command, capture=False):
            done = subprocess.run(command, cwd=root, env=env, stdout=subprocess.PIPE if capture else stdout,
                                  stderr=stderr, timeout=payload["deadline_min"] * 60, text=True)
            done.check_returncode()
            return done.stdout.strip() if capture else None
        try:
            for rev in sorted({payload["nrese"], payload["harness"]}):
                run(["bash", "scripts/release-gate.sh", "build", rev])
            run(["bash", "scripts/release-gate.sh", "prepare", payload["harness"]])
            env["GATE"] = str(gate)
            env["GATE_BIN"] = str(Path(env.get("GATE_BIN", "~/owlrs/gate-v2-bin")).expanduser())
            binary_dir = run(["bash", "-c", '. scripts/lib/gate-publish.sh; bin_of "$1"', "bin", payload["nrese"]], True)
            identity = manifest.binaries(Path(binary_dir), payload["nrese"])
            if not identity["verified"]:
                raise ValueError("source binaries failed existing build identity verification")
            env["NRESE_TARGET_VOLUME"] = binary_dir
            run(["bash", "scripts/cargo-guarded.sh", "build", "--release", "--locked",
                 "--manifest-path", "benches/nrese-bench-harness/Cargo.toml"])
            harness = target / "release/nrese-bench-harness"
            env["HARNESS"] = str(harness)
            identity.update(harness=payload["harness"], git=manifest.git(root),
                            source_tree=abba.git(root, "rev-parse", payload["nrese"] + "^{tree}"),
                            client_toolchain=run(["rustc", "-vV"], True),
                            client_flags=run(["bash", "scripts/cargo-guarded.sh", "--print-rustflags"], True),
                            client_sha256=hashlib.sha256(harness.read_bytes()).hexdigest(),
                            build_identity=(Path(binary_dir) / "IDENTITY").read_text(),
                            inputs=(gate / "inputs.txt").read_text(),
                            classification="private/free", machine=manifest.machine("process"))
            (landing / "identity.json").write_text(json.dumps(identity, indent=2), encoding="utf-8")
            command = ["uv", "run", "--no-project", "python", "-B", "benches/suite/suite.py", "run", "--skip-build",
                       "--systems", ",".join(chosen), "--workloads", group["case"]["workload"], "--tier",
                       f"{group['case']['workload']}={group['case']['tier']}", "--data", str(gate / "data"),
                       "--answers", "full", "--answers-reference", ",".join(group["references"]), "--results", str(landing)]
            run(command)
            if not all((landing / n).is_file() for n in ("results.csv", "manifest.json", "answers.json")):
                raise ValueError("suite result bundle incomplete")
            code = 0
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            print(str(error), file=stderr)
        finally:
            try:
                run(["bash", "scripts/release-gate.sh", "clean"])
            except (OSError, subprocess.SubprocessError) as error:
                print(f"cleanup: {error}", file=stderr)
                code = 1
    shutil.copytree(landing, out / "cell", dirs_exist_ok=True)
    return code


def main(argv, systems, workloads, root):
    p = argparse.ArgumentParser(prog="suite.py fleet")
    for option in ("systems", "workloads", "tiers"):
        p.add_argument("--" + option, default="")
    p.add_argument("--gaps", action="store_true")
    p.add_argument("--owner", default=os.environ.get("T3CODE_THREAD_ID") or os.environ.get("FLEET_OWNER"))
    p.add_argument("--nrese", default="refactor/engine-v2")
    p.add_argument("--references", default="jena,oxigraph")
    p.add_argument("--budget-hours", type=float, default=20)
    p.add_argument("--cores", type=int, default=16, help="existing timed-job CPU ask; unmeasured for cells")
    p.add_argument("--driver-gb", type=float, default=2, help="unmeasured driver overhead beyond the catalogue cap")
    p.add_argument("--build-disk-gb", type=float, default=20, help="unmeasured build disk beyond data_disk_gb")
    p.add_argument("--build-min", type=float, default=abba.rg.BUILD_MIN, help="existing build estimate per revision")
    p.add_argument("--prepare-min", type=float, default=10, help="existing plan_minutes data allowance, unmeasured")
    p.add_argument("--deadline-factor", type=float, default=2, help="unmeasured deadline/estimate ratio")
    p.add_argument("--result-root", default="~/owlrs/private/suite-fleet")
    p.add_argument("--out", type=Path, default=Path.home() / "owlrs/private/suite-fleet/plans")
    mode = p.add_mutually_exclusive_group()
    mode.add_argument("--plan", action="store_true")
    mode.add_argument("--submit", action="store_true")
    p.add_argument("--_member", action="store_true", help=argparse.SUPPRESS)
    args = p.parse_args(argv)
    if args._member:
        return member(root, systems)
    for key in ("budget_hours", "cores", "driver_gb", "build_disk_gb", "build_min", "prepare_min", "deadline_factor"):
        if not math.isfinite(getattr(args, key)) or getattr(args, key) < 0 or (key != "budget_hours" and getattr(args, key) == 0):
            p.error(f"{key} must be finite and {'nonnegative' if key == 'budget_hours' else 'positive'}")
    if not args.systems and not args.gaps:
        p.error("select --systems or --gaps")
    if not args.owner or len(names(args.references)) < 2:
        p.error("--owner (or FLEET_OWNER) and at least two --references are required")
    os.environ["FLEET_OWNER"] = args.owner
    args.checkout = root
    args.harness = abba.git(root, "rev-parse", "HEAD")
    args.nrese = abba.git(root, "rev-parse", "--verify", f"{args.nrese}^{{commit}}")
    try:
        if abba.git(root, "status", "--porcelain", "--untracked-files=normal"):
            raise ValueError("pinning requires a clean harness checkout; commit the suite changes first")
        groups, omitted, minutes = select(root, systems, workloads, args)
        print("\n".join(omitted))
        print(f"total {minutes / 60:.2f} host-hours, unmeasured including builds, preparation and references; budget <{args.budget_hours:g} h")
        if not groups:
            return 0
        set_id, token, members = jobs(groups, args)
        args.out.mkdir(parents=True, exist_ok=True)
        path = args.out / f"{set_id}.set.toml"
        path.write_text(abba.rg.toml_set({"id": set_id, "request": token, "fail": "stop-rest"}, members), encoding="utf-8")
        print(f"set:{set_id}: {path}; NRESE {args.nrese}, harness {args.harness}")
        for job in members:
            print(f"job:{job['id']}: {job['est_min'] / 60:.2f} host-hours, unmeasured; results {job['results_at']}")
        for verb in ("check", "estimate"):
            code, said = abba.rg.fleet_call(verb, str(path))
            print(f"fleet {verb} (exit {code}):\n{said}")
            if code:
                return 1
        if args.submit:
            code, said = abba.rg.fleet_call("submit", str(path))
            print(said)
            return int(code != 0)
        print("plan only: nothing submitted")
        return 0
    except (ValueError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        return 2
