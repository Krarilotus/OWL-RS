#!/usr/bin/env python3
"""NRESE's classifiers in the reference comparison: results in the runner's format,
appended to CASES/results.tsv, so that `ore.py compare` sets them beside the references.

    python nrese.py el CASES [--binary target/release/examples/classify]
    python nrese.py dl CASES [--binary target/release/examples/dl_classify] [--only ID,...]

- `el`: the OWL 2 EL classifier (crates/nrese-reasoner/src/classify.rs), on the EL
  track's tasks. It reads N-Triples: the runner's `ntriples` task converts each ontology
  first (`CASES/tax/ID.nt`, made here through the image when missing). The closure it
  writes becomes the canonical taxonomy with the signature of a reference run
  (`canonical.py`), so equal hashes mean equal taxonomies. The detail column holds the
  hash, then the classifier's profile: its phases in ms and its counters (`profile` line
  of the `classify` example).
- `dl`: the DL driver (`nrese_dl::classify`, the `dl_classify` example) on the DL tracks'
  classification and realisation tasks, reading each task's functional-syntax file
  (`CASES/files/ID.owl`) and writing the canonical taxonomy or realisation itself. The
  detail column holds the hash, then the driver's profile (phases and counters: tests,
  candidates, branch points, clashes, the Horn bound's share); a task that ends
  incomplete or past `--timeout` is `timeout`, with the reasons it printed. Counts don't
  depend on the machine where the run is complete: set them beside a baseline's times.
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


def dl(args):
    cases = pathlib.Path(args.cases).resolve()
    path, rows = results(cases)
    have = {(r[0], r[1]) for r in rows}
    tasks = sorted({(r[0], r[2]) for r in rows if r[2] in ("classify", "realise")
                    and ("-dl-" in r[0] or "-pure_dl-" in r[0])})
    if args.only:
        keep = {f"ore_ont_{i}" if i.isdigit() else i for i in args.only.split(",")}
        tasks = [(t, k) for t, k in tasks if t.split("-")[0] in keep or t in keep]
    binary = pathlib.Path(args.binary)
    with open(path, "a", encoding="utf-8", newline="\n") as out:
        for t, kind in tasks:
            if (t, "nrese-dl") in have and not args.again:
                continue
            ext = "tax" if kind == "classify" else "real"
            target = cases / "tax" / f"{t}.nrese-dl.{ext}"
            target.unlink(missing_ok=True)
            source = cases / "files" / f"{t.split('-')[0]}.owl"
            command = [str(binary), "--tax" if ext == "tax" else "--real", str(target),
                       "--threads", str(args.threads), "--timeout", str(args.timeout),
                       *args.extra, str(source)]
            started = time.monotonic()
            try:
                run = subprocess.run(command, capture_output=True, text=True, timeout=args.timeout + 60)
                code, stdout, stderr = run.returncode, run.stdout, run.stderr
            except subprocess.TimeoutExpired:
                code, stdout, stderr = -9, "", "killed past the timeout"
            ms = int((time.monotonic() - started) * 1000)
            profile = next((l[len("profile "):] for l in reversed(stderr.splitlines())
                            if l.startswith("profile ")), "")
            incomplete = " | ".join(l[len("incomplete: "):] for l in stdout.splitlines()
                                    if l.startswith("incomplete: "))
            if code == 0 and target.exists() and not incomplete:
                digest = hashlib.sha256(target.read_bytes()).hexdigest()
                status, detail = ("classified" if ext == "tax" else "realised"), f"{digest} {profile}"
            elif code in (0, -9):
                status, detail = "timeout", f"{incomplete[:300]} {profile}"
            else:
                status, detail = "error", stderr.strip()[-200:]
            out.write(f"{t}\tnrese-dl\t{kind}\t{status}\t{ms}\t{detail}".rstrip() + "\n")
            out.flush()
            print(f"{t}: {status}, {ms} ms")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    e = sub.add_parser("el")
    e.add_argument("cases")
    e.add_argument("--binary", default=str(ROOT / "target" / "release" / "examples" / "classify"))
    e.add_argument("--threads", type=int, default=1, help="workers of the saturation (default 1)")
    e.add_argument("--timeout", type=int, default=600)
    d = sub.add_parser("dl")
    d.add_argument("cases")
    d.add_argument("--binary", default=str(ROOT / "target" / "release" / "examples" / "dl_classify"))
    d.add_argument("--only", help="task ids or ORE numbers, comma-separated")
    d.add_argument("--threads", type=int, default=1)
    d.add_argument("--timeout", type=int, default=300)
    d.add_argument("--again", action="store_true", help="run tasks that have a row already")
    d.add_argument("extra", nargs="*", help="further dl_classify options, after --")
    args = p.parse_args()
    {"el": el, "dl": dl}[args.command](args)


if __name__ == "__main__":
    main()
