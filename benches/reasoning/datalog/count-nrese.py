#!/usr/bin/env python3
"""The rule work of NRESE's batch executor on Datalog cases, in counts: bindings per round
and rule, probes, merged pairs, new facts (the reasoner's `rule_work` example). Counts
don't depend on the machine or the thread count, so they attribute a loss before any
timing does; the phase times it prints are untimed runs'.

    NRESE_RULE_WORK=.../rule_work python3 count-nrese.py [--out OUT] cspa-httpd=FACTS.nt ...
    python3 count-nrese.py --binary A=.../rule_work --binary B=.../rule_work --rounds 2 ...

OUT (by default the queue job's results, QUEUE_RESULTS) gets, per case, the program
(`CASE/program/program.n3`, as `datalog.py translate` writes it) and `CASE/rule_work.txt`;
with `--binary NAME=PATH` (several), each binary's runs in rounds, the binaries' order
rotated by one each round (A B, B A, ...), as `CASE/NAME-ROUND.txt`, and a summary of the
phase times per binary. `--threads N` sets the executor's threads; `--env NAME:VAR=VALUE`
sets a variable for one binary's runs (one binary under two settings: two names).
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
PHASES = re.compile(r"closure ([0-9.]+) s \| grounding ([0-9.]+) joins ([0-9.]+) modules ([0-9.]+) "
                    r"merge ([0-9.]+)")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=os.environ.get("QUEUE_RESULTS"))
    parser.add_argument("cases", nargs="+", help="CASE=FACTS.nt")
    parser.add_argument("--threads", type=int, default=0)
    parser.add_argument("--binary", action="append", default=[], help="NAME=PATH (several: rotated rounds)")
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--env", action="append", default=[],
                        help="NAME:VAR=VALUE, set for binary NAME's runs (several)")
    args = parser.parse_args(argv)
    binaries = [tuple(b.split("=", 1)) for b in args.binary]
    envs: dict[str, dict[str, str]] = {}
    for item in args.env:
        name, _, assignment = item.partition(":")
        var, _, value = assignment.partition("=")
        envs.setdefault(name, {})[var] = value
    if not binaries:
        binary = os.environ.get("NRESE_RULE_WORK")
        if not binary:
            print("set NRESE_RULE_WORK to the rule_work example's binary, or pass --binary", file=sys.stderr)
            return 2
        binaries = [("rule_work", binary)]
    for _, binary in binaries:
        if not Path(binary).exists():
            print(f"no binary at {binary}", file=sys.stderr)
            return 2
    if args.out is None:
        print("--out OUT (or QUEUE_RESULTS)", file=sys.stderr)
        return 2
    single = len(binaries) == 1 and args.rounds == 1
    failed = 0
    for pair in args.cases:
        case, _, facts = pair.partition("=")
        work = args.out / case
        program = work / "program"
        work.mkdir(parents=True, exist_ok=True)
        subprocess.run([sys.executable, str(HERE / "datalog.py"), "translate", case, str(program)],
                       check=True, stdout=subprocess.DEVNULL)
        times: dict[str, list[tuple[float, ...]]] = {}
        for round_ in range(args.rounds):
            shift = round_ % len(binaries)
            for name, binary in binaries[shift:] + binaries[:shift]:
                command = [str(Path(binary).resolve()), "--rules", str(program / "program.n3"), "--by-rule", facts]
                if args.threads:
                    command[1:1] = ["--threads", str(args.threads)]
                log_path = work / ("rule_work.txt" if single else f"{name}-{round_ + 1}.txt")
                with open(log_path, "w", encoding="utf-8") as log:
                    code = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                                          env={**os.environ, **envs.get(name, {})}).returncode
                text = log_path.read_text(encoding="utf-8")
                print(f"{case} {name} round {round_ + 1}: exit {code}", flush=True)
                if single:
                    print(text, flush=True)
                match = PHASES.search(text)
                if match:
                    times.setdefault(name, []).append(tuple(float(x) for x in match.groups()))
                failed += code != 0
        for name, runs in times.items():
            columns = list(zip(*runs))
            line = " ".join(f"{label} {min(values):.3f}-{max(values):.3f}" for label, values in
                            zip(("closure", "grounding", "joins", "modules", "merge"), columns))
            print(f"{case} {name}: {line} ({len(runs)} runs)", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
