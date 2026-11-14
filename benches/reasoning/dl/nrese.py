#!/usr/bin/env python3
"""NRESE's classifiers in the reference comparison: results in the runner's format,
appended to CASES/results.tsv, so that `ore.py compare` sets them beside the references.

    python nrese.py el CASES [--binary target/release/examples/classify]
    python nrese.py dl CASES [--binary target/release/examples/dl_classify] [--only ID,...]
    python nrese.py consistency CASES [--binary …/tableau_consistency] [--beyond …/dl_beyond]

- `el`: the OWL 2 EL classifier (crates/nrese-reasoner/src/classify.rs), on the EL
  track's tasks. It reads N-Triples: the runner's `ntriples` task converts each ontology
  first (`CASES/tax/ID.nt`, made here through the image unless `CASES/tax/ntriples.tsv`
  records that file's completed conversion: its hash). The closure it
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

A task whose step did not run (its input file missing, its program not started, its N-Triples
not made) gets a `not-run` row: time 0, the reason in the detail column. It is no answer:
`ore.py` counts it apart from every outcome, the driver exits 2, and a later run takes the
task again.
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
sys.path.insert(0, str(HERE.parents[1] / "suite"))
from suitekit import ns  # noqa: E402  (the suite's one container launcher: ns.run_args)

# A task whose step did not run: its input missing, its program not started, not found or not
# executable (exit 126, 127), its N-Triples never made. No answer of the reasoner's: `ore.py`
# counts it apart from every outcome, and the driver that wrote it exits 2 (D16-582: a step that
# did not run read as an undecided task, and the job ended as succeeded).
NOT_RUN = "not-run"


class NotRun(Exception):
    """A task's step that did not run, with the reason its row carries."""


def results(cases, name="results.tsv"):
    path = pathlib.Path(cases) / name
    rows = [l.rstrip("\n").split("\t") for l in open(path, encoding="utf-8")] if path.exists() else []
    return path, rows


def answered(rows):
    """(task, system) whose latest row is not `not-run`: a later run takes the others again."""
    latest = {(r[0], r[1]): r[3:4] for r in rows}
    return {key for key, status in latest.items() if status != [NOT_RUN]}


def not_run(out, task, system, kind, why):
    out.write(f"{task}\t{system}\t{kind}\t{NOT_RUN}\t0\t{' '.join(str(why).split())[:300]}\n")
    out.flush()
    print(f"{task}: {NOT_RUN}: {why}")


def finished(missed):
    """The driver's exit status: 2 when any task's step did not run, else 0."""
    if missed:
        print(f"{missed} tasks did not run (rows `{NOT_RUN}`)", file=sys.stderr)
    return 2 if missed else 0


RECORD = "ntriples.tsv"  # CASES/tax: each completed conversion, task and its file's sha256


def converted(cases):
    """The tasks whose N-Triples file is the one a completed conversion recorded: a file the runner
    left after a timeout, a kill or an older run is not one."""
    tax = cases / "tax"
    _, rows = results(tax, RECORD)
    return {r[0] for r in rows if len(r) > 1 and (tax / f"{r[0]}.nt").is_file()
            and hashlib.sha256((tax / f"{r[0]}.nt").read_bytes()).hexdigest() == r[1]}


def ntriples(cases, tasks, timeout=600):
    """Each task's N-Triples (`CASES/tax/ID.nt`), converted by the runner in its image: a container
    started as every suite container (ns.run_args: in a fleet job its `fleet.job` label, the declared
    memory and CPUs; the image's JVM caps its own heap elsewhere), run by the image's ID pinned at
    the start. A file counts only with the runner's `converted` row, and is then recorded (RECORD);
    the reason of every task left without one."""
    manifest, report = cases / "manifest.nt.tsv", cases / "results.nt.tsv"
    manifest.write_text("".join(f"{t}\tconvert\tntriples\t/work/files/{t.split('-el-')[0]}.owl\n" for t in tasks),
                        encoding="utf-8")
    report.unlink(missing_ok=True)
    try:
        ns.pin_images([reference.IMAGE], ROOT)
        run = subprocess.run([ns.executable(), *ns.run_args(ROOT, "reasoning"), "--rm",
                              "-v", f"{cases.as_posix()}:/work", ns.image_id(reference.IMAGE, ROOT), "batch",
                              "/work/manifest.nt.tsv", "/work/results.nt.tsv", "/work/tax", str(timeout)],
                             capture_output=True, text=True)
        failed = f"the conversion exited {run.returncode}: {run.stderr.strip()[-200:]}" if run.returncode else ""
    except (ns.ImageError, ValueError, OSError) as error:
        failed = f"the conversion did not start: {error}"
    _, said = results(cases, report.name)
    said = {f[0]: f for f in said}
    manifest.unlink()
    report.unlink(missing_ok=True)
    unmade = {}
    with open(cases / "tax" / RECORD, "a", encoding="utf-8", newline="\n") as record:
        for t in tasks:
            row, nt = said.get(t), cases / "tax" / f"{t}.nt"
            if row and row[3:4] == ["converted"] and nt.is_file():
                record.write(f"{t}\t{hashlib.sha256(nt.read_bytes()).hexdigest()}\n")
                continue
            unmade[t] = "no N-Triples: " + (f"the conversion said {row[3]}: {' '.join(row[5:])}" if row else
                                            failed or "the conversion wrote no row for it")
    return unmade


def el(args):
    cases = pathlib.Path(args.cases).resolve()
    path, rows = results(cases)
    have = answered(rows)
    tasks = sorted({r[0] for r in rows if "-el-classification" in r[0]})
    todo = [t for t in tasks if (t, "nrese-el") not in have]
    made = converted(cases)
    missing = [t for t in todo if t not in made]
    unmade = ntriples(cases, missing) if missing else {}
    binary = pathlib.Path(args.binary)
    missed = 0
    with open(path, "a", encoding="utf-8", newline="\n") as out:
        for t in todo:
            taxonomy = next((cases / "tax" / f"{t}.{r}.tax" for r in ("hermit", "konclude", "elk", "openllet")
                             if (cases / "tax" / f"{t}.{r}.tax").exists()), None)
            nt = cases / "tax" / f"{t}.nt"
            closure = cases / "tax" / f"{t}.nrese-el.tsv"
            started = time.monotonic()
            try:
                if taxonomy is None:
                    raise NotRun("no reference taxonomy to take its signature from")
                if t in unmade:
                    raise NotRun(unmade[t])
                # The classifier has no budget of its own: killed at the timeout, a `timeout` row.
                code, _, stderr = launch([str(binary), "--out", str(closure), "--threads", str(args.threads), str(nt)],
                                         args.timeout, inputs=[nt], grace=0)
            except NotRun as why:
                not_run(out, t, "nrese-el", "classify", why)
                missed += 1
                continue
            ms = int((time.monotonic() - started) * 1000)
            if code == -9:
                out.write(f"{t}\tnrese-el\tclassify\ttimeout\t{ms}\tkilled at the {args.timeout} s timeout\n")
                out.flush()
                print(f"{t}: timeout, {ms} ms")
                continue
            if code != 0:
                out.write(f"{t}\tnrese-el\tclassify\terror\t{ms}\t{' '.join(stderr.strip()[-200:].split())}\n")
                out.flush()
                continue
            pairs = [tuple(l.rstrip("\n").split("\t")[:2]) for l in open(closure, encoding="utf-8") if "\t" in l]
            text = canonical.canonical(canonical.signature(taxonomy), pairs)
            (cases / "tax" / f"{t}.nrese-el.tax").write_text(text, encoding="utf-8", newline="\n")
            digest = hashlib.sha256(text.encode()).hexdigest()
            profile = next((l[len("profile "):] for l in reversed(stderr.splitlines())
                            if l.startswith("profile ")), "")
            out.write(f"{t}\tnrese-el\tclassify\tclassified\t{ms}\t{digest} {profile}".rstrip() + "\n")
            out.flush()
            print(f"{t}: {ms} ms; {stderr.strip().splitlines()[-1] if stderr.strip() else ''}")
    return finished(missed)


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


def launch(command, timeout, inputs=(), grace=60):
    """The one launch path of the three drivers: the exit code, standard output and error of a run
    killed `grace` s past its timeout (-9). NotRun when an input file is missing or the program
    does not start (not found, not executable, or exit 126/127: a library the loader can't find)."""
    missing = [str(p) for p in inputs if not pathlib.Path(p).is_file()]
    if missing:
        raise NotRun(f"input missing: {', '.join(missing)}")
    try:
        run = subprocess.run(command, capture_output=True, text=True, timeout=timeout + grace)
    except subprocess.TimeoutExpired:
        return -9, "", "killed past the timeout"
    except OSError as error:
        raise NotRun(f"{command[0]} did not start: {error}")
    if run.returncode in (126, 127):  # a launcher's or the loader's: the program never ran
        raise NotRun(f"{command[0]} did not start (exit {run.returncode}): {run.stderr.strip()[-200:]}")
    return run.returncode, run.stdout, run.stderr


def dl_tasks(rows, kinds, only):
    tasks = sorted({(r[0], r[2]) for r in rows if r[2] in kinds and ("-dl-" in r[0] or "-pure_dl-" in r[0])})
    if only:
        keep = {f"ore_ont_{i}" if i.isdigit() else i for i in only.split(",")}
        tasks = [(t, k) for t, k in tasks if t.split("-")[0] in keep or t in keep]
    return tasks


def dl(args):
    cases = pathlib.Path(args.cases).resolve()
    path, rows = results(cases)
    have = answered(rows)
    tasks = dl_tasks(rows, ("classify", "realise"), args.only)
    binary = pathlib.Path(args.binary)
    missed = 0
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
                code, stdout, stderr = launch(command, args.timeout, inputs=[source])
            except NotRun as why:
                not_run(out, t, "nrese-dl", kind, why)
                missed += 1
                continue
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
    return finished(missed)


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
    have = answered(rows)
    missed = 0
    with open(path, "a", encoding="utf-8", newline="\n") as out:
        for t, _ in dl_tasks(rows, ("consistency",), args.only):
            if (t, "nrese-dl") in have and not args.again:
                continue
            source = str(cases / "files" / f"{t.split('-')[0]}.owl")
            started = time.monotonic()
            try:
                status, detail = decision(*launch([args.binary, "--timeout", str(args.timeout), source], args.timeout,
                                                  inputs=[source]))
                if status == "unsupported":
                    # Outside OWL 2 DL: nrese_dl::beyond decides it, in the whole seconds left.
                    rest = int(args.timeout - (time.monotonic() - started))
                    if rest < 1:
                        status, detail = "timeout", f"beyond: no time left; tableau: {detail}"
                    else:
                        try:
                            verdict, why = decision(*launch([args.beyond, "--decide", "--timeout", str(rest), source],
                                                            rest, inputs=[source]))
                        except NotRun as error:
                            raise NotRun(f"beyond: {error}; tableau: {detail}")
                        status, detail = verdict, f"beyond: {why}; tableau: {detail}"
            except NotRun as why:
                not_run(out, t, "nrese-dl", "consistency", why)
                missed += 1
                continue
            ms = int((time.monotonic() - started) * 1000)
            if status in ("consistent", "inconsistent") and ms > args.timeout * 1000:
                # One budget, reading included, as the references' runner counts it.
                status, detail = "timeout", f"decided past the {args.timeout} s budget: {detail}"
            out.write(f"{t}\tnrese-dl\tconsistency\t{status}\t{ms}\t{detail}".rstrip() + "\n")
            out.flush()
            print(f"{t}: {status}, {ms} ms")
    return finished(missed)


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
    sys.exit({"el": el, "dl": dl, "consistency": consistency}[args.command](args))


if __name__ == "__main__":
    main()
