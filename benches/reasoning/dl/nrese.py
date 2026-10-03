#!/usr/bin/env python3
"""NRESE's classifiers in the reference comparison: results in the runner's format,
appended to CASES/results.tsv, so that `ore.py compare` sets them beside the references.

    python nrese.py el CASES [--binary target/release/examples/classify]

- `el`: the OWL 2 EL classifier (crates/nrese-reasoner/src/classify.rs), on the EL
  track's tasks. It reads N-Triples: the runner's `ntriples` task converts each ontology
  first (`CASES/tax/ID.nt`, made here through the image when missing). The closure it
  writes becomes the canonical taxonomy with the signature of a reference run
  (`canonical.py`), so equal hashes mean equal taxonomies. The detail column holds the
  hash, then the classifier's profile: its phases in ms and its counters (`profile` line
  of the `classify` example).
"""

import argparse
import hashlib
import pathlib
import subprocess
import sys
import time

import canonical

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def results(cases):
    path = pathlib.Path(cases) / "results.tsv"
    rows = [l.rstrip("\n").split("\t") for l in open(path, encoding="utf-8")] if path.exists() else []
    return path, rows


def el(args):
    cases = pathlib.Path(args.cases).resolve()
    path, rows = results(cases)
    have = {(r[0], r[1]) for r in rows}
    tasks = sorted({r[0] for r in rows if "-el-classification" in r[0]})
    todo = [t for t in tasks if (t, "nrese-el") not in have]
    # The N-Triples of each, through the runner.
    missing = [t for t in todo if not (cases / "tax" / f"{t}.nt").exists()]
    if missing:
        manifest = cases / "manifest.nt.tsv"
        manifest.write_text("".join(
            f"{t}\tconvert\tntriples\t/work/files/{t.split('-el-')[0]}.owl\n" for t in missing),
            encoding="utf-8")
        subprocess.run(["docker", "run", "--rm", "-v", f"{cases.as_posix()}:/work",
                        "nrese-bench/dl-reference", "batch", "/work/manifest.nt.tsv",
                        "/work/results.nt.tsv", "/work/tax", "600"], check=True)
        manifest.unlink()
        (cases / "results.nt.tsv").unlink()
    binary = pathlib.Path(args.binary)
    with open(path, "a", encoding="utf-8", newline="\n") as out:
        for t in todo:
            reference = next((cases / "tax" / f"{t}.{r}.tax" for r in ("hermit", "konclude", "elk", "openllet")
                              if (cases / "tax" / f"{t}.{r}.tax").exists()), None)
            if reference is None:
                print(f"{t}: no reference taxonomy, skipped", file=sys.stderr)
                continue
            closure = cases / "tax" / f"{t}.nrese-el.tsv"
            started = time.monotonic()
            run = subprocess.run([str(binary), "--out", str(closure), "--threads", str(args.threads),
                                  str(cases / "tax" / f"{t}.nt")],
                                 capture_output=True, text=True, timeout=args.timeout)
            ms = int((time.monotonic() - started) * 1000)
            if run.returncode != 0:
                out.write(f"{t}\tnrese-el\tclassify\terror\t{ms}\t{run.stderr.strip()[-200:]}\n")
                continue
            pairs = [tuple(l.rstrip("\n").split("\t")[:2]) for l in open(closure, encoding="utf-8") if "\t" in l]
            text = canonical.canonical(canonical.signature(reference), pairs)
            (cases / "tax" / f"{t}.nrese-el.tax").write_text(text, encoding="utf-8", newline="\n")
            digest = hashlib.sha256(text.encode()).hexdigest()
            profile = next((l[len("profile "):] for l in reversed(run.stderr.splitlines())
                            if l.startswith("profile ")), "")
            out.write(f"{t}\tnrese-el\tclassify\tclassified\t{ms}\t{digest} {profile}".rstrip() + "\n")
            print(f"{t}: {ms} ms; {run.stderr.strip().splitlines()[-1] if run.stderr.strip() else ''}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    e = sub.add_parser("el")
    e.add_argument("cases")
    e.add_argument("--binary", default=str(ROOT / "target" / "release" / "examples" / "classify"))
    e.add_argument("--threads", type=int, default=1, help="workers of the saturation (default 1)")
    e.add_argument("--timeout", type=int, default=600)
    args = p.parse_args()
    el(args)


if __name__ == "__main__":
    main()
