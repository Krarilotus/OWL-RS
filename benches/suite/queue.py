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
# On Windows, a job's processes and git open no console window (the runner is a scheduled
# task in the owner's session).
QUIET_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)


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
    # Saturday and Sunday may have windows of their own (weekend_windows).
    windows = queue.get("windows", [])
    if now.weekday() >= 5:
        windows = queue.get("weekend_windows", windows)
    if in_windows(windows, now):
        return None
    return f"outside the windows {', '.join(windows) or '(none)'}"


# --- the runner ------------------------------------------------------------------------------

class Queue:
    def __init__(self, directory: Path, repo: Path, results: Path, worktrees: Path, path: list[str] = (),
                 watch: bool = True):
        self.directory, self.repo, self.results, self.worktrees = directory, repo, results, worktrees
        self.path = list(path)
        self.watch = watch  # a timed job's host kept quiet (off in tests: it would stop the host's work)
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

    def expire(self, today: datetime.date, days: int = 0):
        """Results kept `days` days after their job ended (0: the same day only), then
        removed, fetched or not; their job says so."""
        limit = (today - datetime.timedelta(days=days)).isoformat()
        for path, job in self.jobs("done") + self.jobs("failed"):
            results = Path(job.get("results", ""))
            if job.get("finished", "")[:10] < limit and job.get("results") and results.is_dir():
                shutil.rmtree(results, ignore_errors=True)
                if days:
                    job["note"] = f"results removed after {days} days (hosts.toml queue.keep)"
                else:
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
        pidfile = self.directory / "running" / f"{job['id']}.pid"
        with open(log, "w", encoding="utf-8") as out:
            subprocess.run(["git", "-C", str(self.repo), "worktree", "remove", "--force", str(tree)],
                           capture_output=True, creationflags=QUIET_WINDOW)
            made = subprocess.run(["git", "-C", str(self.repo), "worktree", "add", "-q", "--detach", str(tree),
                                   f"queue/{job['id']}"], stdout=out, stderr=subprocess.STDOUT,
                                  creationflags=QUIET_WINDOW)
            env = {**os.environ, **{k: str(v) for k, v in job.get("env", {}).items()},
                   "QUEUE_JOB": job["id"], "QUEUE_RESULTS": str(results)}
            # The host's tools first (hosts.toml queue.path: its Python, cargo).
            env["PATH"] = os.pathsep.join([*(str(expand(p)) for p in self.path), env.get("PATH", "")])
            code = made.returncode
            timed = job.get("window", "quiet") == "quiet" and self.watch
            for command in job["commands"] if code == 0 else []:
                out.write(f"=== {datetime.datetime.now().isoformat(timespec='seconds')} {' '.join(command)}\n")
                out.flush()
                process = subprocess.Popen(command, cwd=tree, env=env, stdout=out, stderr=subprocess.STDOUT,
                                           creationflags=QUIET_WINDOW)
                # A host's guard stops the job by it (scripts/hosts/khaleesi-guard.py).
                pidfile.write_text(str(process.pid), encoding="utf-8")
                # A timed job keeps its host quiet while it runs, whatever its commands do
                # themselves (scripts/lib/quiet-watch.py): its verdict goes into the job.
                watcher = quiet_watch(results / "quiet", process.pid, job["id"]) if timed else None
                code = process.wait()
                if watcher is not None:
                    watcher.terminate()
                    watcher.wait(timeout=120)
                    log = results / "quiet" / "watch.log"
                    verdict = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
                    job["quiet"] = verdict[-1] if verdict else "no verdict"
                if code != 0:
                    break
            pidfile.unlink(missing_ok=True)
            if code == 0:
                head = subprocess.run(["git", "-C", str(tree), "rev-parse", "HEAD"], capture_output=True, text=True,
                                      creationflags=QUIET_WINDOW)
                job["commit"] = head.stdout.strip()
        subprocess.run(["git", "-C", str(self.repo), "worktree", "remove", "--force", str(tree)], capture_output=True,
                       creationflags=QUIET_WINDOW)
        job["finished"] = datetime.datetime.now().isoformat(timespec="seconds")
        job["exit"] = code
        job["results"] = str(results)
        if code != 0 and retreat is not None and retreat.exists():
            # Stopped because the host's owners came back: not the job's failure, so not an
            # attempt; its partial results stay, and it runs again.
            job["attempts"] -= 1
            job["stops"] = job.get("stops", 0) + 1
            job["note"] = "stopped when the host's owners came back; queued again, its partial results kept"
            state = "pending"
        else:
            state = "done" if code == 0 else "failed"
        if state != "pending":
            # The job's checkout ref goes with it: the host's repository keeps nothing stale.
            subprocess.run(["git", "-C", str(self.repo), "branch", "-D", f"queue/{job['id']}"], capture_output=True,
                           creationflags=QUIET_WINDOW)
        self.move(path, job, state)
        return state


def quiet_watch(state: Path, pid: int, token: str) -> subprocess.Popen | None:
    """The watcher over a timed command (Linux): this user's other busy processes stopped,
    other containers paused, until it ends."""
    watch = HERE.parents[1] / "scripts" / "lib" / "quiet-watch.py"
    if not Path("/proc").is_dir() or not watch.exists():
        return None
    state.mkdir(parents=True, exist_ok=True)
    return subprocess.Popen([sys.executable, str(watch), str(state), str(pid), token])


def here() -> tuple[str, dict]:
    found = hosts.current()
    if found is None or not found[1].get("queue"):
        sys.exit("this host has no queue in benches/hosts.toml")
    return found[0], found[1]["queue"]


def keep_days(queue: dict) -> int | None:
    """How many days a host keeps a job's results (hosts.toml queue.keep: same-day, N-days)."""
    keep = queue.get("keep")
    if keep == "same-day":
        return 0
    if keep and keep.endswith("-days") and keep[:-len("-days")].isdigit():
        return int(keep[:-len("-days")])
    return None


def work(args) -> int:
    if args.log:
        # A scheduled task without a console (pythonw): the runner's own lines go to a file.
        sys.stdout = sys.stderr = open(args.log, "a", encoding="utf-8", buffering=1)
    key, queue = here()
    q = Queue(expand(queue["dir"]), expand(queue.get("repo", "~/owlrs/repo.git")),
              expand(queue.get("results", "~/owlrs/results")), expand(queue["dir"]) / "wt", queue.get("path", []))
    retreat = expand(queue["retreat"]) if queue.get("retreat") else None
    print(f"{datetime.datetime.now():%F %T} queue of {key}: {q.directory}", flush=True)
    # A job a dead runner left running goes back to the queue, said so.
    for path, job in q.jobs("running"):
        job["note"] = "its runner stopped while it ran; queued again"
        q.move(path, job, "pending")
        print(f"{datetime.datetime.now():%F %T} {job['id']}: queued again (left running)", flush=True)
    said = ""
    reload = q.directory / "RELOAD"
    while True:
        # A new runner's code takes over between jobs: RELOAD in the queue directory.
        if reload.exists():
            reload.unlink()
            print(f"{datetime.datetime.now():%F %T} reloading", flush=True)
            os.execv(sys.executable, [sys.executable, *sys.argv])
        if keep_days(queue) is not None:
            q.expire(datetime.date.today(), keep_days(queue))
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
    day they end (`keep = "same-day"`). A running job's results so far: long jobs write
    theirs per case or chunk as they go, so they can be read before the job ends."""
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
    # A running job's results are fetched as far as they go (each case or chunk as it ends),
    # and stay there; a finished job's go on a same-day host.
    ended = remote(host, script("print(any((here(d) / s / (i + '.toml')).exists() for s in ('done', 'failed')))\n",
                                d=queue["dir"], i=args.id))
    if ended.stdout.strip() != "True":
        print("the job still runs: these are its results so far; fetch again later")
    elif queue.get("keep") == "same-day":
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
    s.add_argument("--log", help="the runner's lines to this file (a scheduled task without a console)")
    args = p.parse_args()
    return {"submit": submit, "status": status, "cancel": cancel, "fetch": fetch, "work": work}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
