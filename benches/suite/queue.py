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
import shutil
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
    if not entry.get("queue"):
        out.append(f"{job['host']} has no queue (hosts.toml [host.{job['host']}.queue])")
    timed = job.get("window", "quiet") == "quiet"
    if timed and ("measure" not in entry.get("roles", []) or entry.get("queue", {}).get("timing") is False):
        out.append(f"{job['host']} takes no timed job (hosts.toml roles, queue.timing): window = \"any\" for counts, "
                   "downloads and preparation")
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
    """Whether a compiler runs on this host (anyone's: a timing waits for all of them)."""
    if os.name == "nt":
        tasks = subprocess.run(["tasklist"], capture_output=True, text=True).stdout.lower()
        return any(line.startswith(("rustc.exe", "cargo.exe")) for line in tasks.splitlines())
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

    def expire(self, today: datetime.date):
        """On a host that keeps results only the day they end: those of earlier days, which
        nobody fetched, are removed; their job says so."""
        for path, job in self.jobs("done") + self.jobs("failed"):
            results = Path(job.get("results", ""))
            if job.get("finished", "")[:10] < today.isoformat() and job.get("results") and results.is_dir():
                shutil.rmtree(results, ignore_errors=True)
                job["note"] = "results removed unfetched at the end of their day (keep = same-day)"
                path.write_text(toml(job), encoding="utf-8")

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
        if queue.get("keep") == "same-day":
            q.expire(datetime.date.today())
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


# --- from anywhere: the host's side through its Python --------------------------------------

MARK = b"#--queue-data--#"

def remote(host: dict, script: str, data: bytes | None = None, binary: bool = False, timeout: float = 120):
    """Runs a Python script on the host through ssh (its queue's `python`: python3, python on
    Windows, `wsl -e python3` in a WSL distribution), the same on every kind of host."""
    python = host.get("queue", {}).get("python", "python3")
    # The script, then DATA after a marker; the bootstrap has neither a backslash nor a
    # double quote, so cmd, a POSIX shell and wsl pass it alike.
    payload = script.encode() + MARK + (data or b"")
    bootstrap = "import sys;s,_,DATA=sys.stdin.buffer.read().partition(b'" + MARK.decode() + "');exec(s)"
    done = subprocess.run(["ssh", "-o", "BatchMode=yes", host["ssh"], f'{python} -c "{bootstrap}"'],
                          input=payload, capture_output=True, timeout=timeout)
    if not binary:
        done.stdout = done.stdout.decode("utf-8", "replace")
    done.stderr = done.stderr.decode("utf-8", "replace")
    return done


def script(body: str, **values) -> str:
    """A remote script with `values` bound as Python literals (paths expanded on the host)."""
    head = "import os, pathlib, shutil, sys, tarfile\n"
    head += "def here(p): return pathlib.Path(os.path.expanduser(p))\n"
    head += "".join(f"{k} = {v!r}\n" for k, v in values.items())
    return head + body


def submit(args) -> int:
    job = read_job(Path(args.job))
    registry = hosts.load()
    found = job_problems(job, registry)
    if found:
        print("\n".join(f"refused: {p}" for p in found), file=sys.stderr)
        return 2
    host = registry["host"][job["host"]]
    queue = host["queue"]
    repo = queue.get("repo", "~/owlrs/repo.git")
    made = remote(host, script("r = here(repo)\nif not r.exists():\n    import subprocess\n"
                               "    subprocess.run(['git', 'init', '-q', '--bare', str(r)], check=True)\n", repo=repo))
    if made.returncode != 0:
        print(f"no repository on {job['host']}: {made.stderr.strip()[-300:]}", file=sys.stderr)
        return 2
    target = repo.replace("~/", "")
    push = ["git", "-c", "core.hooksPath=/dev/null", "push", "-q", "-f"]
    if queue.get("receive_pack"):
        push.append(f"--receive-pack={queue['receive_pack']}")
    pushed = subprocess.run([*push, f"{host['ssh']}:{target}", f"{job['checkout']}:refs/heads/queue/{job['id']}"],
                            capture_output=True, text=True)
    if pushed.returncode != 0:
        print(f"push failed: {pushed.stderr.strip()}", file=sys.stderr)
        return 2
    job["submitted"] = datetime.datetime.now().isoformat(timespec="seconds")
    job.setdefault("workload", workload_of(job) or "")
    job["source"] = subprocess.run(["git", "rev-parse", job["checkout"]], capture_output=True, text=True).stdout.strip()
    done = remote(host, script("p = here(d) / 'pending' / (i + '.toml')\np.parent.mkdir(parents=True, exist_ok=True)\n"
                               "if p.exists(): sys.exit('already pending')\n"
                               "p.write_bytes(DATA)\n",
                               d=queue["dir"], i=job["id"]), data=toml(job).encode())
    if done.returncode != 0:
        print(f"not queued: {done.stderr.strip()[-300:]}", file=sys.stderr)
        return 2
    print(f"queued {job['id']} on {job['host']} ({job['checkout']} at {job['source'][:10]})")
    return 0


LISTING = """
for s in states:
    for f in sorted((here(d) / s).glob('*.toml')):
        print('== ' + s + ' ' + f.name)
        print(f.read_text(encoding='utf-8'))
"""


def status(args) -> int:
    registry = hosts.load()
    for key, host in registry.get("host", {}).items():
        if not host.get("queue") or not host.get("ssh") or (args.host and key != args.host):
            continue
        listing = remote(host, script(LISTING, d=host["queue"]["dir"], states=list(STATES)), timeout=60)
        print(f"# {key}")
        if listing.returncode != 0:
            print(f"  unreachable: {listing.stderr.strip()[-200:]}")
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
    done = remote(host, script("p = here(d) / 'pending' / (i + '.toml')\nif not p.exists(): sys.exit('not pending')\n"
                               "p.unlink()\n", d=host["queue"]["dir"], i=args.id))
    print("cancelled" if done.returncode == 0 else done.stderr.strip()[-200:])
    return done.returncode


FETCH = """
r = here(results) / i
if not r.is_dir(): sys.exit('no results for ' + i)
with tarfile.open(fileobj=sys.stdout.buffer, mode='w|gz') as t:
    t.add(str(r), arcname=i)
"""


def fetch(args) -> int:
    """A job's results here (`--to`), then removed there on a host that keeps them only the
    day they end (`keep = "same-day"`)."""
    import io
    host = hosts.load()["host"][args.host]
    queue = host["queue"]
    results = queue.get("results", "~/owlrs/results")
    done = remote(host, script(FETCH, results=results, i=args.id), binary=True, timeout=3600)
    if done.returncode != 0:
        print(done.stderr.strip()[-300:], file=sys.stderr)
        return 2
    import tarfile
    target = Path(args.to)
    target.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(done.stdout), mode="r:gz") as t:
        t.extractall(target, filter="data")
    print(f"fetched {args.id} into {target / args.id}")
    if queue.get("keep") == "same-day":
        removed = remote(host, script("shutil.rmtree(here(results) / i)\n", results=results, i=args.id))
        print("removed there (same-day host)" if removed.returncode == 0 else removed.stderr.strip()[-200:])
    return 0


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
    s = sub.add_parser("fetch")
    s.add_argument("id")
    s.add_argument("--host", required=True)
    s.add_argument("--to", required=True, help="the directory the job's results go to")
    s = sub.add_parser("work")
    s.add_argument("--once", action="store_true", help="run at most one job, then stop")
    args = p.parse_args()
    return {"submit": submit, "status": status, "cancel": cancel, "fetch": fetch, "work": work}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
