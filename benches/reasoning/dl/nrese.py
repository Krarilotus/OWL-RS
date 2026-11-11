#!/usr/bin/env python3
"""NRESE's classifiers in the reference comparison: results in the runner's format,
appended to CASES/results.tsv, so that `ore.py compare` sets them beside the references.

    python nrese.py el CASES [--binary target/release/examples/classify]
    python nrese.py dl CASES [--binary target/release/examples/dl_classify] [--only ID,...]
    python nrese.py consistency CASES [--binary …/tableau_consistency] [--beyond …/dl_beyond]

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
  candidates, branch points, clashes, the Horn bound's share). The status is the driver's
  own outcome: `inconsistent` (its verdict, as the references report it), `unsupported`
  (an input it doesn't decide, such as one outside OWL 2 DL), `timeout` (its `dl.timeout`
  reached or killed past `--timeout`), `undecided` (another budget), with the reasons it
  printed. Counts don't depend on the machine where the run is complete: set them beside a
  baseline's times.
- `consistency`: the hypertableau (`tableau_consistency`) on the DL tracks' consistency
  tasks; an input it refuses as outside OWL 2 DL goes to `nrese_dl::beyond`
  (`dl_beyond --decide`) in the whole seconds left, whose verdict is the row's, the detail
  column naming that path (`beyond: …; tableau: …`) and the time both runs'. One budget,
  `--timeout`, covers both and the reading: a decision past it is a `timeout`, as the
  references' runner counts it.
"""

import argparse
import hashlib
import pathlib
import subprocess
import sys
import time

import canonical
import reference

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
                        reference.IMAGE, "batch", "/work/manifest.nt.tsv",
                        "/work/results.nt.tsv", "/work/tax", "600"], check=True)
        manifest.unlink()
        (cases / "results.nt.tsv").unlink()
    binary = pathlib.Path(args.binary)
    with open(path, "a", encoding="utf-8", newline="\n") as out:
        for t in todo:
            taxonomy = next((cases / "tax" / f"{t}.{r}.tax" for r in ("hermit", "konclude", "elk", "openllet")
                             if (cases / "tax" / f"{t}.{r}.tax").exists()), None)
            if taxonomy is None:
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
            text = canonical.canonical(canonical.signature(taxonomy), pairs)
            (cases / "tax" / f"{t}.nrese-el.tax").write_text(text, encoding="utf-8", newline="\n")
            digest = hashlib.sha256(text.encode()).hexdigest()
            profile = next((l[len("profile "):] for l in reversed(run.stderr.splitlines())
                            if l.startswith("profile ")), "")
            out.write(f"{t}\tnrese-el\tclassify\tclassified\t{ms}\t{digest} {profile}".rstrip() + "\n")
            print(f"{t}: {ms} ms; {run.stderr.strip().splitlines()[-1] if run.stderr.strip() else ''}")


def outcome(code, stdout, stderr, wrote, ext):
    """A `dl_classify` run's status from its own outcome: the verdict ending its summary
    line (`… unsatisfiable, consistent|inconsistent|unknown`) and its `incomplete:` lines."""
    if code == -9:
        return "timeout"
    summary = next((l for l in reversed(stderr.splitlines()) if " unsatisfiable, " in l), "")
    verdict = summary.rsplit(", ", 1)[-1].strip() if summary else ""
    incomplete = [l for l in stdout.splitlines() if l.startswith("incomplete: ")]
    if code != 0 or not verdict:
        return "error"
    if verdict == "inconsistent":
        return "inconsistent"
    if any("(unsupported" in l for l in incomplete):
        return "unsupported"
    if any("dl.timeout reached" in l for l in incomplete):
        return "timeout"
    if incomplete or verdict != "consistent":
        return "undecided"
    return ("classified" if ext == "tax" else "realised") if wrote else "error"


def launch(command, timeout):
    """The exit code, standard output and error of a run killed 60 s past its own timeout."""
    try:
        run = subprocess.run(command, capture_output=True, text=True, timeout=timeout + 60)
        return run.returncode, run.stdout, run.stderr
    except subprocess.TimeoutExpired:
        return -9, "", "killed past the timeout"


def dl_tasks(rows, kinds, only):
    tasks = sorted({(r[0], r[2]) for r in rows if r[2] in kinds and ("-dl-" in r[0] or "-pure_dl-" in r[0])})
    if only:
        keep = {f"ore_ont_{i}" if i.isdigit() else i for i in only.split(",")}
        tasks = [(t, k) for t, k in tasks if t.split("-")[0] in keep or t in keep]
    return tasks


def dl(args):
    cases = pathlib.Path(args.cases).resolve()
    path, rows = results(cases)
    have = {(r[0], r[1]) for r in rows}
    tasks = dl_tasks(rows, ("classify", "realise"), args.only)
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
            code, stdout, stderr = launch(command, args.timeout)
            ms = int((time.monotonic() - started) * 1000)
            profile = next((l[len("profile "):] for l in reversed(stderr.splitlines())
                            if l.startswith("profile ")), "")
            status = outcome(code, stdout, stderr, target.exists(), ext)
            if status in ("classified", "realised", "inconsistent") and target.exists():
                detail = f"{hashlib.sha256(target.read_bytes()).hexdigest()} {profile}"
            elif status == "error":
                detail = stderr.strip()[-200:]
            else:
                incomplete = " | ".join(l[len("incomplete: "):] for l in stdout.splitlines()
                                        if l.startswith("incomplete: "))
                detail = f"{incomplete[:300]} {profile}"
            out.write(f"{t}\tnrese-dl\t{kind}\t{status}\t{ms}\t{detail}".rstrip() + "\n")
            out.flush()
            print(f"{t}: {status}, {ms} ms")


def decision(code, stdout, stderr):
    """A consistency run's status and detail from its line `file<TAB>answer<TAB>…<TAB>why`
    (`tableau_consistency`; `dl_beyond --decide`, whose undecided is the tableau's gave-up)."""
    line = next((l.split("\t") for l in stdout.splitlines() if l.count("\t") >= 2), None)
    if code == -9:
        return "timeout", "killed past the timeout"
    if code != 0 or line is None or line[1] in ("error", "parse-error"):
        return "error", (stderr.strip() or "\t".join(line or []))[-200:]
    answer, why = line[1], line[-1] or line[2]
    if answer in ("consistent", "inconsistent", "unsupported"):
        return answer, why[:300]
    out_of_time = "timeout" in why or "time budget ran out" in why  # the tableau's, beyond's
    return ("timeout" if out_of_time else "undecided"), why[:300]


def consistency(args):
    cases = pathlib.Path(args.cases).resolve()
    path, rows = results(cases)
    have = {(r[0], r[1]) for r in rows}
    with open(path, "a", encoding="utf-8", newline="\n") as out:
        for t, _ in dl_tasks(rows, ("consistency",), args.only):
            if (t, "nrese-dl") in have and not args.again:
                continue
            source = str(cases / "files" / f"{t.split('-')[0]}.owl")
            started = time.monotonic()
            status, detail = decision(*launch([args.binary, "--timeout", str(args.timeout), source], args.timeout))
            if status == "unsupported":
                # Outside OWL 2 DL: nrese_dl::beyond decides it, in the whole seconds left.
                rest = int(args.timeout - (time.monotonic() - started))
                if rest < 1:
                    status, detail = "timeout", f"beyond: no time left; tableau: {detail}"
                else:
                    verdict, why = decision(*launch([args.beyond, "--decide", "--timeout", str(rest), source], rest))
                    status, detail = verdict, f"beyond: {why}; tableau: {detail}"
            ms = int((time.monotonic() - started) * 1000)
            if status in ("consistent", "inconsistent") and ms > args.timeout * 1000:
                # One budget, reading included, as the references' runner counts it.
                status, detail = "timeout", f"decided past the {args.timeout} s budget: {detail}"
            out.write(f"{t}\tnrese-dl\tconsistency\t{status}\t{ms}\t{detail}".rstrip() + "\n")
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
    c = sub.add_parser("consistency")
    c.add_argument("cases")
    c.add_argument("--binary", default=str(ROOT / "target" / "release" / "examples" / "tableau_consistency"))
    c.add_argument("--beyond", default=str(ROOT / "target" / "release" / "examples" / "dl_beyond"))
    c.add_argument("--only", help="task ids or ORE numbers, comma-separated")
    c.add_argument("--timeout", type=int, default=300)
    c.add_argument("--again", action="store_true", help="run tasks that have a row already")
    args = p.parse_args()
    {"el": el, "dl": dl, "consistency": consistency}[args.command](args)


if __name__ == "__main__":
    main()
