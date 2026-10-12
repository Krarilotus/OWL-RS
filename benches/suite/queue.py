#!/usr/bin/env python3
"""The measurement queue: timed jobs batched per measure host, one at a time, in the
windows the host allows (benches/README.md#the-measurement-queue).

    python benches/suite/queue.py submit JOB.toml          push the job's checkout to its host, queue it
    python benches/suite/queue.py status [--host HOST]     what is pending, running, done and failed
    python benches/suite/queue.py cancel ID --host HOST    a pending job back out
    python benches/suite/queue.py work [--once]            on a host: run its queue (one job at a time)

A job (TOML):

    id = "sparql-bsbm-100k-1"           # unique on its host
    group = "sparql"                    # who asks (benches/cases/ group)
    host = "office-pc"                  # hosts.toml key: every system of the job runs there
    checkout = "suite/sparql"           # a local ref; submit pushes it to the host as queue/<id>
    commands = [["python3", "benches/fast/fast.py", "build"],
                ["python3", "benches/fast/fast.py", "compete", "--cases", "bsbm-explore-100k"]]
    env = { DOCKER_CPUS = "8", FAST_MAX_GB = "24" }   # a matched scenario's limits, the binaries
    needs = ["docker"]                  # what the host must have
    window = "quiet"                    # quiet: only when the host is free for timings; any: also else
    workload = "bsbm"                   # its family: its place in suite/schedule.toml orders the queue
    priority = 5                        # within a family, lower first; then the order of submission
    est_min = 90

`work` runs on the host (not as suite.py: the office PC's guard stops suite.py and fast.py
processes when its owners come back, the jobs but not their runner). Per job: a fresh
worktree of its checkout, the commands in order with its environment (QUEUE_JOB and
QUEUE_RESULTS name the job and its results directory, ~/owlrs/results/<id>), a log, then the
job file moves to done/ or failed/ with its exit status, times and log. A job stopped
while the host's retreat file appeared goes back to pending to run again (at most three
times). Timings come only from a quiet window: the host's `queue` in hosts.toml says when
(`windows` of local time, or while its `free` file exists, never while `retreat` exists,
never while a compiler runs); its `path` comes first on a job's PATH (the host's Python 3.11
or later, cargo). Answers before time: a timed suite or compete command
captures every answer (compete always does; `suite.py run` needs `--answers full`).
"""
from __future__ import annotations

import argparse
import datetime
import os
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from suitekit import hosts, schedule  # noqa: E402

STATES = ("pending", "running", "done", "failed")
MAX_ATTEMPTS = 3
POLL_S = 60


# --- jobs ------------------------------------------------------------------------------------

def read_job(path: Path) -> dict:
    with open(path, "rb") as f:
        return tomllib.load(f)


def toml(job: dict) -> str:
    from suitekit.ledger import toml_value
    return "".join(f"{k} = {toml_value(v)}\n" for k, v in job.items())


def workload_of(job: dict) -> str | None:
    """The benchmark family a job measures: its `workload`, else its cases' (benches/cases/),
    else `fast` for a fast-suite command."""
    if job.get("workload"):
        return job["workload"]
    for command in job.get("commands", []):
        if "--cases" in command and command.index("--cases") + 1 < len(command):
            from suitekit import cases
            names = set(command[command.index("--cases") + 1].split(","))
            found = {c.get("workload") for c in cases.load() if c["name"] in names} - {None}
            if found:
                return min(found, key=schedule.rank)
        if any(part.endswith("fast/fast.py") for part in command):
            return "fast"
    return None


def job_problems(job: dict, registry: dict | None = None) -> list[str]:
    """What keeps a job out of the queue."""
    registry = hosts.load() if registry is None else registry
    out = [f"missing {k}" for k in ("id", "group", "host", "checkout", "commands") if k not in job]
    if out:
        return out
    entry = registry.get("host", {}).get(job["host"])
    if entry is None:
        return [f"host {job['host']!r} is not in hosts.toml"]
    if "measure" not in entry.get("roles", []):
        out.append(f"{job['host']} doesn't measure (hosts.toml roles)")
    if not entry.get("queue"):
        out.append(f"{job['host']} has no queue (hosts.toml [host.{job['host']}.queue])")
    lacking = sorted(set(job.get("needs", [])) - set(entry.get("has", [])))
    if lacking:
        out.append(f"{job['host']} lacks {', '.join(lacking)}")
    if job.get("window", "quiet") not in ("quiet", "any"):
        out.append("window is quiet or any")
    if not all(isinstance(c, list) and c and all(isinstance(a, str) for a in c) for c in job["commands"]):
        out.append("commands is a list of argument lists")
    for command in job["commands"]:
        text = " ".join(command)
        if "suite.py run" in text and "--dry-run" not in text and "--answers" not in command:
            out.append("a timed suite.py run captures its answers first: --answers full --answers-reference ...")
    return out


# --- the host's windows ----------------------------------------------------------------------

def expand(path: str) -> Path:
    return Path(os.path.expanduser(path))


def in_windows(windows: list[str], now: datetime.datetime) -> bool:
    """Whether `now` (local time) falls in one of `windows`, each "HH:MM-HH:MM" (across
    midnight when the end is earlier)."""
    minute = now.hour * 60 + now.minute
    for window in windows:
        start, end = ([int(x) for x in part.split(":")] for part in window.split("-"))
        a, b = start[0] * 60 + start[1], end[0] * 60 + end[1]
        if (a <= minute < b) if a < b else (minute >= a or minute < b):
            return True
    return False


def compilers() -> bool:
    return subprocess.run(["pgrep", "-x", "rustc|cargo"], capture_output=True).returncode == 0


def open_for(job: dict, queue: dict, now: datetime.datetime, busy=compilers) -> str | None:
    """Why `job` may not start now on this host, or None."""
    if queue.get("retreat") and expand(queue["retreat"]).exists():
        return "the host's owners are present (retreat)"
    if job.get("window", "quiet") == "any":
        return None
    if busy():
        return "a compiler is running"
    if queue.get("free") and expand(queue["free"]).exists():
        return None
    if in_windows(queue.get("windows", []), now):
        return None
    return f"outside the windows {', '.join(queue.get('windows', [])) or '(none)'}"


# --- the runner ------------------------------------------------------------------------------

class Queue:
    def __init__(self, directory: Path, repo: Path, results: Path, worktrees: Path, path: list[str] = ()):
        self.directory, self.repo, self.results, self.worktrees = directory, repo, results, worktrees
        self.path = list(path)
        for state in STATES:
            (directory / state).mkdir(parents=True, exist_ok=True)

    def jobs(self, state: str) -> list[tuple[Path, dict]]:
        """A state's jobs in the order they run: by their family's place in the schedule
        (schedule.toml), then priority, then submission."""
        found = [(p, read_job(p)) for p in (self.directory / state).glob("*.toml")]
        return sorted(found, key=lambda pj: (schedule.rank(workload_of(pj[1])), pj[1].get("priority", 5),
                                             pj[1].get("submitted", ""), pj[0].name))

    def next(self, queue: dict, now: datetime.datetime, busy=compilers) -> tuple[Path, dict] | tuple[None, str]:
        why = []
        for path, job in self.jobs("pending"):
            reason = open_for(job, queue, now, busy)
            if reason is None:
                return path, job
            why.append(f"{job['id']}: {reason}")
        return None, "; ".join(why)

    def move(self, path: Path, job: dict, state: str) -> Path:
        target = self.directory / state / path.name
        target.write_text(toml(job), encoding="utf-8")
        if path != target:
            path.unlink()
        return target

    def run(self, path: Path, job: dict, retreat: Path | None) -> str:
        """Runs one job; the state it ends in."""
        job["attempts"] = job.get("attempts", 0) + 1
        job["started"] = datetime.datetime.now().isoformat(timespec="seconds")
        path = self.move(path, job, "running")
        results = self.results / job["id"]
        results.mkdir(parents=True, exist_ok=True)
        tree = self.worktrees / job["id"]
        log = results / f"queue-{job['attempts']}.log"
        code = 0
        with open(log, "w", encoding="utf-8") as out:
            subprocess.run(["git", "-C", str(self.repo), "worktree", "remove", "--force", str(tree)],
                           capture_output=True)
            made = subprocess.run(["git", "-C", str(self.repo), "worktree", "add", "-q", "--detach", str(tree),
                                   f"queue/{job['id']}"], stdout=out, stderr=subprocess.STDOUT)
            env = {**os.environ, **{k: str(v) for k, v in job.get("env", {}).items()},
                   "QUEUE_JOB": job["id"], "QUEUE_RESULTS": str(results)}
            # The host's tools first (hosts.toml queue.path: its Python, cargo).
            env["PATH"] = os.pathsep.join([*(str(expand(p)) for p in self.path), env.get("PATH", "")])
            code = made.returncode
            for command in job["commands"] if code == 0 else []:
                out.write(f"=== {datetime.datetime.now().isoformat(timespec='seconds')} {' '.join(command)}\n")
                out.flush()
                code = subprocess.run(command, cwd=tree, env=env, stdout=out, stderr=subprocess.STDOUT).returncode
                if code != 0:
                    break
            if code == 0:
                head = subprocess.run(["git", "-C", str(tree), "rev-parse", "HEAD"], capture_output=True, text=True)
                job["commit"] = head.stdout.strip()
        subprocess.run(["git", "-C", str(self.repo), "worktree", "remove", "--force", str(tree)], capture_output=True)
        job["finished"] = datetime.datetime.now().isoformat(timespec="seconds")
        job["exit"] = code
        job["results"] = str(results)
        if code != 0 and retreat is not None and retreat.exists() and job["attempts"] < MAX_ATTEMPTS:
            job["note"] = "stopped when the host's owners came back; queued again"
            state = "pending"
        else:
            state = "done" if code == 0 else "failed"
        self.move(path, job, state)
        return state


def here() -> tuple[str, dict]:
    found = hosts.current()
    if found is None or not found[1].get("queue"):
        sys.exit("this host has no queue in benches/hosts.toml")
    return found[0], found[1]["queue"]


def work(args) -> int:
    key, queue = here()
    q = Queue(expand(queue["dir"]), expand(queue.get("repo", "~/owlrs/repo.git")),
              expand(queue.get("results", "~/owlrs/results")), expand(queue["dir"]) / "wt", queue.get("path", []))
    retreat = expand(queue["retreat"]) if queue.get("retreat") else None
    print(f"{datetime.datetime.now():%F %T} queue of {key}: {q.directory}", flush=True)
    said = ""
    while True:
        path, job = q.next(queue, datetime.datetime.now())
        if path is None:
            if job and job != said:
                print(f"{datetime.datetime.now():%F %T} waiting: {job}", flush=True)
                said = job
            if args.once:
                return 0
            time.sleep(POLL_S)
            continue
        said = ""
        print(f"{datetime.datetime.now():%F %T} start {job['id']} (attempt {job.get('attempts', 0) + 1})", flush=True)
        state = q.run(path, job, retreat)
        print(f"{datetime.datetime.now():%F %T} {job['id']}: {state}", flush=True)
        if args.once:
            return 0


# --- submitting, from anywhere ---------------------------------------------------------------

def ssh(host: dict, command: str, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(["ssh", "-o", "BatchMode=yes", host["ssh"], command], capture_output=True, text=True,
                          **kwargs)


def submit(args) -> int:
    job = read_job(Path(args.job))
    registry = hosts.load()
    found = job_problems(job, registry)
    if found:
        print("\n".join(f"refused: {p}" for p in found), file=sys.stderr)
        return 2
    host = registry["host"][job["host"]]
    queue = host["queue"]
    repo = queue.get("repo", "~/owlrs/repo.git").replace("~/", "")
    pushed = subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "push", "-q", "-f", f"{host['ssh']}:{repo}",
                             f"{job['checkout']}:refs/heads/queue/{job['id']}"], capture_output=True, text=True)
    if pushed.returncode != 0:
        print(f"push failed: {pushed.stderr.strip()}", file=sys.stderr)
        return 2
    job["submitted"] = datetime.datetime.now().isoformat(timespec="seconds")
    job.setdefault("workload", workload_of(job) or "")
    job["source"] = subprocess.run(["git", "rev-parse", job["checkout"]], capture_output=True, text=True).stdout.strip()
    pending = f"{queue['dir']}/pending/{job['id']}.toml"
    done = ssh(host, f"mkdir -p {queue['dir']}/pending && test ! -e {pending} && cat > {pending}", input=toml(job))
    if done.returncode != 0:
        print(f"not queued (a job {job['id']} is already pending there?): {done.stderr.strip()}", file=sys.stderr)
        return 2
    print(f"queued {job['id']} on {job['host']} ({job['checkout']} at {job['source'][:10]})")
    return 0


def status(args) -> int:
    registry = hosts.load()
    for key, host in registry.get("host", {}).items():
        if not host.get("queue") or (args.host and key != args.host):
            continue
        listing = ssh(host, "for s in " + " ".join(STATES) + f"; do for f in {host['queue']['dir']}/$s/*.toml; do "
                      "[ -e \"$f\" ] && echo \"== $s $f\" && cat \"$f\"; done; done; true", timeout=60)
        print(f"# {key}")
        if listing.returncode != 0:
            print(f"  unreachable: {listing.stderr.strip()[:200]}")
            continue
        for block in listing.stdout.split("== ")[1:]:
            head, _, body = block.partition("\n")
            state = head.split()[0]
            job = tomllib.loads(body)
            when = job.get("finished") or job.get("started") or job.get("submitted", "")
            print(f"  {state:<8} {job['id']:<32} {job.get('group', '?'):<10} exit {job.get('exit', '-'):<4} {when}"
                  + (f"  {job['note']}" if job.get("note") else ""))
    return 0


def cancel(args) -> int:
    host = hosts.load()["host"][args.host]
    done = ssh(host, f"rm {host['queue']['dir']}/pending/{args.id}.toml")
    print("cancelled" if done.returncode == 0 else f"not pending: {done.stderr.strip()}")
    return done.returncode


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    s = sub.add_parser("submit")
    s.add_argument("job")
    s = sub.add_parser("status")
    s.add_argument("--host")
    s = sub.add_parser("cancel")
    s.add_argument("id")
    s.add_argument("--host", required=True)
    s = sub.add_parser("work")
    s.add_argument("--once", action="store_true", help="run at most one job, then stop")
    args = p.parse_args()
    return {"submit": submit, "status": status, "cancel": cancel, "work": work}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
