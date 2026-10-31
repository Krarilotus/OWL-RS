#!/usr/bin/env python3
"""The support-stress suite's datasets (benches/suite/workloads.toml `support-stress`) into a
dataset directory: every `.nq` file benches/datasets.toml catalogues for that workload, made by
its `made_by` command (`perf_lab generate support-stress FILE key=value ...`) with the binary
PERF_LAB, and its sidecars beside it (FILE.reader, FILE.commits.ru, FILE.updates.ru). Each is
generated into a staging directory beside its target and published whole. A file already there
must come out
byte for byte the same (a regeneration that differs is a new release, frozen anew); it is left
as it is. The generator's answers, its oracle's for each file's reader, go to ANSWERS: the cases
pin them.

    python3 benches/fast/prepare-support-stress.py PERF_LAB DIR ANSWERS.json

Hashes and sizes for the catalogue: `python3 benches/suite/suite.py data freeze --data DIR FILE...`
in a scratch checkout, or sha256sum.
"""
from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SIDECARS = (".reader", ".commits.ru", ".updates.ru")


def digest(path: Path) -> str:
    with open(path, "rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def main(perf_lab: str, root: Path, answers_path: Path) -> int:
    catalog = tomllib.loads((ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["dataset"]
    answers = {}
    for entry in catalog:
        if entry.get("workload") != "support-stress" or not entry["file"].endswith(".nq"):
            continue
        command = entry["made_by"].split()
        if command[:4] != ["perf_lab", "generate", "support-stress", entry["file"]]:
            print(f"{entry['file']}: made_by isn't its generate command: {entry['made_by']}")
            return 2
        target = root / entry["file"]
        stage = target.parent / f".stage-{target.name}"
        shutil.rmtree(stage, ignore_errors=True)
        stage.mkdir(parents=True)
        made = stage / target.name
        done = subprocess.run([perf_lab, "generate", "support-stress", str(made), *command[4:]],
                              capture_output=True, text=True)
        if done.returncode:
            print(f"{entry['file']}: the generator failed: {done.stderr.strip()[-300:]}")
            return 1
        answers[entry["file"]] = json.loads(done.stdout)
        for suffix in ("", *SIDECARS):
            new, kept = Path(f"{made}{suffix}"), Path(f"{target}{suffix}")
            if kept.exists():
                if digest(new) != digest(kept):
                    print(f"{kept}: regenerated differently: a new release, not this one")
                    return 1
                state = "present, regenerated the same"
            else:
                new.replace(kept)
                state = "published"
            print(f"{entry['file']}{suffix}: {state}, {kept.stat().st_size} bytes, sha256 {digest(kept)}")
        shutil.rmtree(stage)
    answers_path.write_text(json.dumps(answers, indent=1, sort_keys=True), encoding="utf-8")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])))
