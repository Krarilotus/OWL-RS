#!/usr/bin/env python3
"""Runs a manifest on the reference reasoners (nrese-bench/dl-reference), robustly.

    python reference.py run CASES [--timeout 60] [--cpus 8] [--memory 12g] [--name NAME]

CASES/manifest.tsv is run in one JVM per batch (start-up paid once), results appended to
CASES/results.tsv, taxonomies under CASES/tax. Resumable: tasks with a result are
skipped. A reasoner that stops answering (HermiT ignores interrupts in some loops) is
caught by a watchdog: no new result for the timeout plus a grace period stops the
container, the stalled task is recorded as `timeout`, and the batch resumes after it.
The container is always removed.
"""

import argparse
import pathlib
import subprocess
import sys
import time

IMAGE = "nrese-bench/dl-reference"
GRACE = 30


def lines(path):
    if not path.exists():
        return []
    return [l for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def key(line):
    f = line.split("\t")
    return (f[0], f[1])


def run(cases, timeout, cpus, memory, name="nrese-dl-reference"):
    cases = pathlib.Path(cases).resolve()
    manifest = [l for l in lines(cases / "manifest.tsv") if not l.startswith("#")]
    results = cases / "results.tsv"
    part = cases / "results.part.tsv"
    while True:
        done = {key(l) for l in lines(results)}
        rest = [l for l in manifest if key(l) not in done]
        if not rest:
            break
        (cases / "manifest.rest.tsv").write_text("\n".join(rest) + "\n", encoding="utf-8")
        part.unlink(missing_ok=True)
        subprocess.run(["docker", "rm", "-f", name], capture_output=True)
        subprocess.run(
            ["docker", "run", "-d", "--name", name, f"--cpus={cpus}", "-m", memory,
             "-v", f"{cases.as_posix()}:/work", IMAGE, "batch", "/work/manifest.rest.tsv",
             "/work/results.part.tsv", "/work/tax", str(timeout)],
            check=True, capture_output=True)
        seen, last = 0, time.monotonic()
        stalled = False
        while True:
            time.sleep(2)
            now = len(lines(part))
            if now != seen:
                seen, last = now, time.monotonic()
            state = subprocess.run(["docker", "inspect", "-f", "{{.State.Running}}", name],
                                   capture_output=True, text=True).stdout.strip()
            if state != "true":
                break
            if time.monotonic() - last > timeout + GRACE:
                stalled = True
                subprocess.run(["docker", "stop", "-t", "2", name], capture_output=True)
                break
        finished = lines(part)
        code = subprocess.run(["docker", "inspect", "-f", "{{.State.ExitCode}}", name],
                              capture_output=True, text=True).stdout.strip()
        with open(results, "a", encoding="utf-8", newline="\n") as f:
            for l in finished:
                f.write(l + "\n")
            # Exit 3: the runner asks for a fresh JVM after a timeout; nothing is lost.
            if len(finished) < len(rest) and (stalled or code != "3"):
                f_ = rest[len(finished)].split("\t")
                status = "timeout" if stalled else "error"
                detail = "stalled past the timeout" if stalled else "the JVM exited"
                f.write("\t".join([f_[0], f_[1], f_[2], status, str(timeout * 1000), detail]) + "\n")
        print(f"{len(done) + len(finished) + (len(finished) < len(rest))} of {len(manifest)}",
              file=sys.stderr)
    subprocess.run(["docker", "rm", "-f", name], capture_output=True)
    part.unlink(missing_ok=True)
    (cases / "manifest.rest.tsv").unlink(missing_ok=True)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    r = sub.add_parser("run")
    r.add_argument("cases")
    r.add_argument("--timeout", type=int, default=60)
    r.add_argument("--cpus", default="8")
    r.add_argument("--memory", default="12g")
    r.add_argument("--name", default="nrese-dl-reference", help="the container's name (one per concurrent run)")
    args = p.parse_args()
    run(args.cases, args.timeout, args.cpus, args.memory, args.name)


if __name__ == "__main__":
    main()
