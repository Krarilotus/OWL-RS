#!/usr/bin/env python3
"""/verify: proves HTTP-level behaviour of nrese-server by driving a real one, and keeps the
evidence (the skill: .claude/skills/verify/SKILL.md; scenario files: README.md).

    python scripts/verify/verify.py run --server PATH [--set KEY=VALUE]... SCENARIO...
    python scripts/verify/verify.py run --server-rev SHA SCENARIO...     # a fleet host's build (jobs)
    python scripts/verify/verify.py run --url http://127.0.0.1:8080 SCENARIO...   # a server already running
    python scripts/verify/verify.py selftest --server PATH | --server-rev SHA | --url URL
    python scripts/verify/verify.py doctor --url URL | --server PATH
    python scripts/verify/verify.py check SCENARIO...                  # read and check the files only
    python scripts/verify/verify.py jobs --rev REV --reason TEXT [--harness REV] SCENARIO...

Each scenario, in five phases:
1. **Launch:** a fresh temporary data directory, the server from the given build on a free port
   of 127.0.0.1, the scenario's settings (`[server] set`), then this command's `--set`, over
   `store.mode = "on-disk"`; ready when /readyz answers 200.
2. **Doctor:** read-only: /healthz, /readyz, /version, /api/v1/repositories.
3. **Drive:** the scenario's steps in order, each request checked against its expectations;
   captured values reach later steps. A failed step doesn't stop the others; once the server has
   exited, the remaining steps are recorded as not run.
4. **Evidence** (EVIDENCE/<scenario>/): every request and response (headers, the body's first
   64 KiB and the SHA-256 and length of all of it, timings), the server's log, the doctor's
   readings, and summary.md and summary.json beside them, each step passed or failed with why.
5. **Cleanup:** the server stopped and its directory removed, always (`--keep` keeps the directory).

EVIDENCE is $FLEET_OUT/evidence (selftest: $FLEET_OUT/selftest) in a fleet job, else
tmp/verify/<time> in the repository. The summary's path is printed last. Exit: 0 every step
passed; 1 a step failed (selftest: a check misjudged); 2 a scenario or the command is wrong;
3 the server didn't come up, the doctor found it unwell, or the cleanup failed.
"""
from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import signal
import socket
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

if sys.version_info < (3, 11):
    sys.exit("verify.py needs Python 3.11 or later (tomllib)")

from verifykit import checks, client, jobs, scenario as scenarios, selftest  # noqa: E402
from verifykit.server import LaunchError, Server, sha256_of  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
DOCTOR = ("/healthz", "/readyz", "/version", "/api/v1/repositories")
CACHE = Path(os.environ.get("NRESE_VERIFY_CACHE", "~/fleet/cache/owlrs-verify")).expanduser()


class Interrupted(Exception):
    """SIGTERM (fleet's cancel, a host's guard): unwinds through the cleanup like Ctrl-C."""


def _terminated(*_):
    raise Interrupted("terminated")


# --- phase 2: doctor ---------------------------------------------------------------------------

def doctor(base: str) -> dict:
    """Read-only readings of a running server; `ok` when /healthz and /readyz answer 200."""
    readings = {}
    for path in DOCTOR:
        reading = {"status": None}
        try:
            with urllib.request.urlopen(base + path, timeout=10) as response:
                reading["status"], text = response.status, response.read(1 << 20).decode("utf-8", "replace")
        except urllib.error.HTTPError as e:
            reading["status"], text = e.code, e.read(1 << 20).decode("utf-8", "replace")
        except OSError as e:
            reading["error"], text = str(e), ""
        try:
            reading["body"] = json.loads(text)
        except ValueError:
            reading["body"] = text[:2000]
        readings[path] = reading
    version, ready = readings["/version"].get("body"), readings["/readyz"].get("body")
    version = version if isinstance(version, dict) else {}
    ready = ready if isinstance(ready, dict) else {}
    repos = readings["/api/v1/repositories"].get("body")
    ids = [r.get("id") for r in repos] if isinstance(repos, list) else f"status {readings['/api/v1/repositories']['status']}"
    line = (f"{version.get('service', '?')} {version.get('version', '?')}, store {version.get('store_mode', '?')}, "
            f"reasoning {version.get('reasoning_mode', '?')}; readyz {ready.get('status', '?')}/"
            f"{ready.get('consistency', '?')}, revision {ready.get('revision', '?')}, "
            f"{ready.get('quad_count', '?')} quads; repositories: {ids}")
    ok = readings["/healthz"]["status"] == 200 and readings["/readyz"]["status"] == 200
    return {"ok": ok, "line": line, "readings": readings}


# --- phase 3: drive ----------------------------------------------------------------------------

def evaluate(step: dict, record: dict) -> list[str]:
    """The step's failed expectations (empty: it passed)."""
    if record["error"]:
        return [record["error"]]
    expect, response, body = step["expect"], record["response"], record["body"]
    failures = []
    if response["status"] not in expect["status"]:
        failures.append(f"status: expected {' or '.join(map(str, expect['status']))}, got {response['status']}"
                        + _problem(record))
    for name, predicate in expect["headers"].items():
        value = client.header(record, name)
        failures += checks.check(f"header {name}", value, predicate, present=value is not None)
    needs_body = expect["body"] or expect["json"] or expect["rows"] is not None
    if needs_body and response["body"]["beyond_read_limit"]:
        return failures + [f"body: {response['body']['bytes']} bytes, past the step's read limit "
                           f"({step['read_limit']} bytes): raise read_limit_mib to check it"]
    text = body.decode("utf-8", errors="replace")
    for predicate in expect["body"]:
        failures += checks.check("body", text, predicate)
    if expect["json"]:
        try:
            document = json.loads(text)
        except ValueError as e:
            return failures + [f"json: the body isn't JSON ({e}); it begins {checks.show(text[:80])}"]
        for path, predicate in expect["json"]:
            try:
                value, present = checks.json_path(document, path), True
            except checks.PathError as e:
                if "present" not in predicate:
                    failures.append(f"json {e}")
                    continue
                value, present = None, False
            failures += checks.check(f"json {path}", value, predicate, present=present)
    if expect["rows"] is not None:
        try:
            rows = checks.count_rows(client.header(record, "content-type"), body)
            if rows != expect["rows"]:
                failures.append(f"rows: expected {expect['rows']}, got {rows}")
        except ValueError as e:
            failures.append(f"rows: {e}")
    return failures


def _problem(record: dict) -> str:
    """A problem document's title and detail (RFC 9457), to say why a status came."""
    if checks.media_type(client.header(record, "content-type")) != "application/problem+json":
        return ""
    try:
        problem = json.loads(record["body"])
    except ValueError:
        return ""
    said = ": ".join(str(problem[k]) for k in ("title", "detail") if problem.get(k))
    return f" ({said})" if said else ""


def capture(step: dict, record: dict) -> tuple[dict[str, str], list[str]]:
    captured, failures = {}, []
    for name, spec in step["capture"].items():
        try:
            if record["error"]:
                raise ValueError("no response")
            if "header" in spec:
                value = client.header(record, spec["header"])
                if value is None:
                    raise ValueError(f"no header {spec['header']}")
            elif "json" in spec:
                value = checks.json_path(json.loads(record["body"]), spec["json"])
            else:
                value = record["body"].decode("utf-8", errors="replace")
            value = value if isinstance(value, str) else json.dumps(value)
            if "regex" in spec:
                m = re.search(spec["regex"], value)
                if not m:
                    raise ValueError(f"/{spec['regex']}/ doesn't match {checks.show(value)}")
                value = m.group(1) if m.groups() else m.group(0)
            captured[name] = value
        except (ValueError, checks.PathError) as e:
            failures.append(f"capture {name}: {e}")
    return captured, failures


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")[:48] or "step"


def drive(sc: scenarios.Scenario, base: str, server: Server | None, out: Path, evidence_bytes: int) -> list[dict]:
    variables = {**sc.vars, "base": base, "port": base.rsplit(":", 1)[-1]}
    results = []
    gone = None
    for step in sc.steps:
        name, files = step["name"], out / "steps" / f"{step['index']:02d}-{slug(step['name'])}"
        result = {"index": step["index"], "name": name, "ok": False, "failures": [], "status": None,
                  "ms": None, "record": f"steps/{files.name}.json"}
        results.append(result)
        if gone:
            result["failures"] = [f"not run: {gone}"]
            result["record"] = None
            continue
        if step.get("restart"):
            try:
                start = server.restart()
                variables.update(base=server.base, port=str(server.port))
                result.update(ok=True, ms=round(start["ready_s"] * 1000, 1), note=f"restarted: pid {start['pid']}, "
                              f"port {start['port']}, ready after {start['ready_s']} s")
            except LaunchError as e:
                result["failures"] = [f"restart: {e}"]
                gone = "the server didn't come back after a restart"
            _write(files.with_suffix(".json"), result)
            continue
        missing = sorted({m for m in scenarios.VAR.findall(repr(step)) if m not in variables})
        if missing:
            result["failures"] = [f"not run: {', '.join('${' + m + '}' for m in missing)} not captured (an earlier step failed)"]
            result["record"] = None
            continue
        live = scenarios.substitute(step, variables)
        record = client.exchange(live, variables["base"], files, evidence_bytes)
        failures = evaluate(live, record)
        captured, capture_failures = capture(live, record)
        variables.update(captured)
        result.update(ok=not failures and not capture_failures, failures=failures + capture_failures,
                      status=record["response"]["status"] if record["response"] else None,
                      ms=record["timings_ms"].get("total"))
        _write(files.with_suffix(".json"), {k: v for k, v in record.items() if k != "body"}
               | {"step": step["name"], "ok": result["ok"], "failures": result["failures"], "captured": captured})
        if record["error"] and server and server.exited() is not None:
            gone = f"the server exited with code {server.exited()} (its log: server.log)"
    return results


# --- the phases, per scenario ------------------------------------------------------------------

def run_one(sc: scenarios.Scenario, out: Path, target: dict, settings: dict[str, str], args) -> dict:
    out.mkdir(parents=True, exist_ok=True)
    (out / ("scenario.toml" if sc.path else "scenario.json")).write_text(sc.text, encoding="utf-8")
    result = {"scenario": sc.name, "dir": out.name, "file": str(sc.path) if sc.path else None,
              "description": sc.description, "ok": False, "error": None, "doctor": None, "launch": None,
              "cleanup": None, "steps": []}
    server = None
    try:
        if target.get("url"):
            base = target["url"].rstrip("/")
        else:
            server = Server(target["command"], {**sc.settings, **settings}, sc.env, out / "server.log",
                            start_timeout=args.start_timeout, scratch=args.scratch)
            result["launch"] = {**server.start(), "binary": target["binary"]}
            _write(out / "launch.json", result["launch"])
            base = server.base
        print(f"[{sc.name}] server at {base}", flush=True)
        report = doctor(base)
        _write(out / "doctor.json", report)
        result["doctor"] = report["line"]
        print(f"[{sc.name}] doctor: {report['line']}", flush=True)
        if not report["ok"]:
            result["error"] = f"the doctor found it unwell: /healthz {report['readings']['/healthz']['status']}, " \
                              f"/readyz {report['readings']['/readyz']['status']}"
        else:
            result["steps"] = drive(sc, base, server, out, args.evidence_kib * 1024)
            for step in result["steps"]:
                mark = "pass" if step["ok"] else "FAIL"
                print(f"[{sc.name}] {mark} {step['index']:2d} {step['name']}"
                      + "".join(f"\n        {f}" for f in step["failures"]), flush=True)
    except LaunchError as e:
        result["error"] = f"launch: {e}"
    finally:
        if server:
            result["cleanup"] = server.cleanup(keep=args.keep)
    result["ok"] = not result["error"] and all(s["ok"] for s in result["steps"])
    return result


def _write(path: Path, data) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=1, ensure_ascii=False, default=str) + "\n", encoding="utf-8")


def cgroup_peak_mib() -> float | None:
    """memory.peak of this process's cgroup (Linux, cgroup v2): under fleet, the step's scope,
    the runner and its servers together. What a run job's ram_gb should cover."""
    try:
        line = Path("/proc/self/cgroup").read_text().strip().splitlines()[-1]
        peak = Path("/sys/fs/cgroup" + line.split("::", 1)[1], "memory.peak").read_text().strip()
        return round(int(peak) / 2**20, 1)
    except (OSError, IndexError, ValueError):
        return None


def summarise(evidence: Path, results: list[dict], target: dict, title: str, extra: list[str] | None = None) -> Path:
    """summary.json and summary.md in `evidence`; the path of summary.md."""
    context = {"host": socket.gethostname(), "platform": platform.platform(), "python": platform.python_version(),
               "fleet_job": os.environ.get("FLEET_JOB"), "time": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
               "server": target.get("binary") or {"url": target.get("url")}, "cgroup_peak_mib": cgroup_peak_mib()}
    _write(evidence / "summary.json", {**context, "title": title, "notes": extra or [], "scenarios": results})
    server = target.get("binary")
    lines = [f"# {title}", ""]
    if server:
        lines.append(f"- server: `{server['path']}` (sha256 {server['sha256'][:16]}…"
                     + (f", revision {server['revision']}" if server.get("revision") else "") + ")")
    else:
        lines.append(f"- server: {target.get('url', 'none')}")
    lines.append(f"- host: {context['host']} ({context['platform']}), {context['time']}"
                 + (f", fleet job {context['fleet_job']}" if context["fleet_job"] else ""))
    if context["cgroup_peak_mib"]:
        lines.append(f"- memory peak of this cgroup (the runner and its servers): {context['cgroup_peak_mib']} MiB")
    lines += [f"- {n}" for n in extra or []]
    for r in results:
        steps = r["steps"]
        lines += ["", f"## {r['scenario']}: {'passed' if r['ok'] else 'FAILED'} "
                      f"({sum(s['ok'] for s in steps)} of {len(steps)} steps)", ""]
        if r["description"]:
            lines += [r["description"], ""]
        if r["doctor"]:
            lines += [f"Doctor: {r['doctor']}", ""]
        if r["error"]:
            lines += [f"**Error:** {r['error']}", ""]
        if steps:
            lines += ["| # | step | result | status | ms | evidence |", "|---|---|---|---|---|---|"]
            for s in steps:
                record = f"[{s['record']}]({r['dir']}/{s['record']})" if s["record"] else ""
                lines.append(f"| {s['index']} | {s['name']} | {'pass' if s['ok'] else '**FAIL**'} | "
                             f"{s['status'] or ''} | {s['ms'] or ''} | {record} |")
            failed = [s for s in steps if not s["ok"]]
            if failed:
                lines += ["", "Failures:", ""] + [f"- step {s['index']} ({s['name']}): {f}"
                                                   for s in failed for f in s["failures"]]
        cleanup = r["cleanup"]
        if cleanup:
            lines += ["", f"Cleanup: server exit code {cleanup['exit_code']}, directory "
                          f"{'removed' if cleanup['removed'] else 'kept' if cleanup['kept'] else 'NOT removed'}"
                      + (f" ({cleanup['directory']})" if not cleanup["removed"] else "")
                      + (f"; **{cleanup['error']}**" if cleanup["error"] else "")]
    path = evidence / "summary.md"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


# --- the commands ------------------------------------------------------------------------------

def target_of(args) -> dict:
    """Where the server comes from: --url (running), --server (a binary), --server-rev (a
    fleet host's build, its checksum checked)."""
    if args.url:
        return {"url": args.url}
    if args.server_rev:
        binary = CACHE / f"bin-{args.server_rev[:12]}" / "nrese-server"
        sums = binary.parent / "SHA256SUMS"
        if not binary.is_file() or not sums.is_file():
            raise LaunchError(f"no build of {args.server_rev[:12]} at {binary.parent} (submit its build job: verify.py jobs)")
        if sums.read_text().split()[0] != sha256_of(binary):
            raise LaunchError(f"{binary}: its checksum doesn't hold (SHA256SUMS): build it again")
    else:
        binary = Path(args.server).expanduser().resolve()
        if not binary.is_file():
            raise LaunchError(f"no server binary at {binary}")
    revision = binary.parent / "REVISION"
    return {"command": [str(binary)],
            "binary": {"path": str(binary), "sha256": sha256_of(binary),
                       "revision": revision.read_text().strip() if revision.is_file() else None}}


def evidence_dir(args, kind: str) -> Path:
    if args.evidence:
        path = Path(args.evidence)
        if path.exists() and any(path.iterdir()):
            raise SystemExit(f"{path}: exists and isn't empty: name another evidence directory")
        return path
    if os.environ.get("FLEET_OUT"):
        path = Path(os.environ["FLEET_OUT"]) / kind
        if path.exists():
            shutil.rmtree(path)  # this job's own, from a step run before (requeued)
        return path
    return ROOT / "tmp" / "verify" / f"{time.strftime('%Y%m%d-%H%M%S')}-{kind}"


def overrides(pairs: list[str]) -> dict[str, str]:
    out = {}
    for pair in pairs:
        key, sep, value = pair.partition("=")
        if not sep or not key:
            raise SystemExit(f"--set takes KEY=VALUE, not {pair!r}")
        if key in scenarios.OWNED_KEYS:
            raise SystemExit(f"--set {key}: the launcher sets it (a fresh one per run)")
        out[key] = value
    return out


def cmd_run(args, loaded: list[scenarios.Scenario], kind: str) -> tuple[int, Path, dict, list[dict]]:
    """Each scenario through the five phases: the exit code, the evidence directory, where the
    server came from, and each scenario's result."""
    if args.url and any(s.get("restart") for sc in loaded for s in sc.steps):
        raise SystemExit("restart steps need a server of the runner's (--server or --server-rev), not --url")
    settings = overrides(args.set)
    evidence = evidence_dir(args, kind)
    evidence.mkdir(parents=True, exist_ok=True)
    try:
        target = target_of(args)
    except LaunchError as e:
        print(f"verify: {e}", file=sys.stderr)
        return 3, evidence, {}, [{"scenario": sc.name, "dir": sc.name, "file": str(sc.path),
                                  "description": sc.description, "ok": False, "error": f"launch: {e}",
                                  "doctor": None, "launch": None, "cleanup": None, "steps": []} for sc in loaded]
    results, names = [], set()
    for sc in loaded:
        name = sc.name if sc.name not in names else f"{sc.name}-{len(names) + 1}"
        names.add(name)
        results.append(run_one(sc, evidence / name, target, settings, args))
    infra = any(r["error"] or (r["cleanup"] and r["cleanup"]["error"]) for r in results)
    failed = any(not s["ok"] for r in results for s in r["steps"])
    return 1 if failed else 3 if infra else 0, evidence, target, results


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    def server_options(p, scenario_files: bool = True):
        where = p.add_mutually_exclusive_group(required=True)
        where.add_argument("--server", help="the nrese-server binary to launch")
        where.add_argument("--server-rev", help=f"the build of this commit on this host ({CACHE}/bin-<sha12>)")
        where.add_argument("--url", help="a server already running (no launch, no cleanup of it)")
        p.add_argument("--set", action="append", default=[], metavar="KEY=VALUE",
                       help="a server setting (a config file key), over the scenario's")
        p.add_argument("--evidence", help="the evidence directory (new or empty)")
        p.add_argument("--scratch", type=Path, help="where the temporary data directories go (the system's temp)")
        p.add_argument("--keep", action="store_true", help="keep each server's data directory")
        p.add_argument("--start-timeout", type=float, default=120.0, help="seconds until /readyz must answer 200")
        p.add_argument("--evidence-kib", type=int, default=64, help="each body's first KiB kept in the evidence")
        if scenario_files:
            p.add_argument("scenarios", nargs="+", type=Path)

    server_options(sub.add_parser("run", help="launch, doctor, drive, evidence, cleanup: each scenario"))
    server_options(sub.add_parser("selftest", help="wrong expectations must fail, each with its message"),
                   scenario_files=False)
    p = sub.add_parser("doctor", help="read-only health readings of a server")
    where = p.add_mutually_exclusive_group(required=True)
    where.add_argument("--url")
    where.add_argument("--server")
    p.add_argument("--start-timeout", type=float, default=120.0)
    p = sub.add_parser("check", help="read and check scenario files, nothing started")
    p.add_argument("scenarios", nargs="+", type=Path)
    p = sub.add_parser("jobs", help="the fleet jobs (build, then run) as TOML files")
    p.add_argument("--rev", required=True, help="the server's commit")
    p.add_argument("--harness", help="the runner's and scenarios' commit (default: --rev)")
    p.add_argument("--repo", type=Path, default=ROOT, help="the repository fleet takes them from")
    p.add_argument("--out", type=Path, default=ROOT / "tmp" / "verify" / "jobs")
    p.add_argument("--priority", type=int, default=600, help="600-700 exploratory; 790 a v1.5 fix verification")
    p.add_argument("--reason", required=True, help="why the run is worth its place in the queue")
    p.add_argument("--verifies", help="the commit or issue a fix verification verifies")
    p.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    p.add_argument("--no-selftest", action="store_true", help="leave out the self-test step")
    p.add_argument("scenarios", nargs="+", help="paths in the harness commit")
    args = parser.parse_args(argv)
    signal.signal(signal.SIGTERM, _terminated)

    if args.command == "jobs":
        out = jobs.jobs(args.repo, args.rev, args.harness or args.rev, [Path(s).as_posix() for s in args.scenarios],
                        args.set, args.priority, args.reason, args.verifies, not args.no_selftest)
        args.out.mkdir(parents=True, exist_ok=True)
        for job_id, job in out.items():
            path = args.out / f"{job_id}.toml"
            path.write_text(jobs.toml_job(job), encoding="utf-8")
            print(path)
        return 0
    if args.command == "doctor":
        if args.url:
            report = doctor(args.url.rstrip("/"))
        else:
            target = target_of(argparse.Namespace(url=None, server_rev=None, server=args.server))
            log = Path(os.environ.get("TEMP", "/tmp")) / f"verify-doctor-{os.getpid()}.log"
            server = Server(target["command"], {}, {}, log, start_timeout=args.start_timeout)
            try:
                server.start()
                report = doctor(server.base)
            finally:
                server.cleanup()
                log.unlink(missing_ok=True)
        print(json.dumps(report["readings"], indent=1))
        print(f"doctor: {'ok' if report['ok'] else 'UNWELL'}: {report['line']}")
        return 0 if report["ok"] else 3
    if args.command == "selftest":
        loaded = [scenarios.from_dict(selftest.scenario(), "selftest")]
        loaded[0].text = json.dumps(selftest.scenario(), indent=1)
    else:
        try:
            loaded = [scenarios.load(p) for p in args.scenarios]
        except (scenarios.ScenarioError, OSError) as e:
            print(f"verify: {e}", file=sys.stderr)
            return 2
    if args.command == "check":
        for sc in loaded:
            print(f"{sc.path}: {sc.name}, {len(sc.steps)} steps")
        return 0
    try:
        if args.command == "run":
            code, evidence, target, results = cmd_run(args, loaded, "evidence")
            passed = sum(r["ok"] for r in results)
            summary = summarise(evidence, results, target, f"/verify: {passed} of {len(results)} scenarios passed")
        else:
            code, evidence, target, results = cmd_run(args, loaded, "selftest")
            problems = selftest.judge(results[0])
            verdict = ("passed: every wrong expectation failed with its message, and the controls passed"
                       if not problems else "FAILED: " + "; ".join(problems))
            summary = summarise(evidence, results, target, f"/verify self-test {verdict.split(':')[0]}",
                                [f"self-test {verdict}", "the steps named wrong fail on purpose"])
            print(f"self-test {verdict}")
            # A server that didn't come up (or go) judges nothing: that's the infrastructure's.
            code = 3 if results[0]["error"] or (results[0]["cleanup"] or {}).get("error") else 1 if problems else 0
    except (Interrupted, KeyboardInterrupt):
        print("verify: interrupted; the servers were stopped and their directories removed", file=sys.stderr)
        return 3
    print(f"summary: {summary}")
    return code


if __name__ == "__main__":
    sys.exit(main())
