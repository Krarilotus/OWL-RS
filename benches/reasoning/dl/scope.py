#!/usr/bin/env python3
"""The small-scope campaign (docs/design/owl2-dl.md §11): every OWL 2 DL ontology up to a
bound (nrese_owl::scope), answered by NRESE (examples/scope_check) and by the reference
reasoners (the runner's nrese.dl.Scope), compared case by case.

    python scope.py run SCOPE DIR [--slice N] [--jobs J] [--timeout S] [--heap 1536m] [--reasoners hermit]
    python scope.py second SCOPE DIR [--jobs J] [--timeout S] [--reasoners openllet]
    python scope.py compare DIR [--cases FILE] [--show N]
    python scope.py record SCOPE DIR OUT [--adjudicated FILE]

SCOPE is an exhaustive scope (`4,4,4`, nrese_owl::scope::Scope) or a covering array
(`cover:3`: every triple of features, nrese_owl::scope::Covering::campaign).

`run` works through the scope in slices of N cases (default 200000): writes a slice's
cases (target/release/examples/scope), runs NRESE on it and the references in J JVMs,
keeps the answers (DIR/nrese.tsv, DIR/reference.tsv) and deletes the slice's cases;
resumable. Needs the release examples built (`cargo build --release -p nrese-owl
--example scope -p nrese-dl --example scope_check`) and the runner's jar
(DL_RUNNER_JAR, with nrese.dl.Scope on the class path: DL_RUNNER_CLASSES).

`second` asks the further reasoners about the cases the first left open: where it gave
no decision, or one other than NRESE's (Openllet takes about three times HermiT's time,
so it runs only there).

`record` writes the decided answer of every case for the gate's test
(crates/nrese-dl/tests/scope): the references' where those that answered agree, the
adjudicated one (`--adjudicated`, lines `scope:index<TAB>code<TAB>why`, those of
SCOPE) where they don't or none answered; it refuses a case with neither. Two base-64 digits a case
(`ALPHABET[code // 64]`, `ALPHABET[code % 64]`, the code `1024` for `i`, else the ten
bits as a number, the first question highest), 64 cases a line.

`compare` decides each case by the references that answered (`i`, `c` + bits), never by
a vote: a case where NRESE and every answering reference agree is `agree`; one where NRESE
differs from references that agree is `differs` (a model certificate of NRESE's answer
settles it against the reference: `differs-certified`); references that differ are
`disputed`; no reference answering leaves NRESE's certificates (`certified-only`, where
every claim of the answer that a model can show is shown, else `open`). NRESE's `u`, `t`,
`x`, `p` are `undecided`.
"""

import argparse
import collections
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
EXE = ".exe" if os.name == "nt" else ""
QUESTIONS = ["consistent", "A sat", "B sat", "A<=B", "B<=A", "T<=A", "T<=B", "a:A", "a:B", "b:A", "b:B"]


def lines(path):
    if not path.exists():
        return []
    return [l for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def decided(code):
    return code == "i" or (code.startswith("c") and len(code) == 11)


def source(scope):
    """The examples' arguments for SCOPE: `cover:T`, the covering array of strength T, or
    an exhaustive scope such as `4,4,4`."""
    if scope.startswith("cover:"):
        return ["--covering", scope.removeprefix("cover:")]
    return ["--scope", scope]


def run(scope, out, slice_size, jobs, timeout, reasoners, heap):
    out.mkdir(parents=True, exist_ok=True)
    examples = ROOT / "target" / "release" / "examples"
    cp = classpath()
    nrese, reference = out / "nrese.tsv", out / "reference.tsv"
    start = 0
    while True:
        end = start + slice_size
        cases = out / f"cases-{start}.ofn"
        subprocess.run([str(examples / f"scope{EXE}"), *source(scope), "--out", str(cases),
                        "--from", str(start), "--to", str(end)], check=True)
        count = len(lines(cases))
        if count == 0:
            cases.unlink()
            break
        done = {l.split("\t")[0] for l in lines(nrese)}
        if not all(l.split("\t")[0] in done for l in lines(cases)):
            part = out / f"nrese-{start}.tsv"
            subprocess.run([str(examples / f"scope_check{EXE}"), *source(scope), "--out", str(part),
                            "--from", str(start), "--to", str(end)], check=True)
            with open(nrese, "a", encoding="utf-8", newline="\n") as f:
                f.write("\n".join(l for l in lines(part) if l.split("\t")[0] not in done) + "\n")
            part.unlink()
        parts = [out / f"reference-{start}-{j}.tsv" for j in range(jobs)]
        references(cp, cases, reference, parts, jobs, reasoners, timeout, heap)
        cases.unlink()
        print(f"cases {start}..{start + count} done", file=sys.stderr)
        if count < slice_size:
            break
        start = end


def references(cp, cases, out, parts, jobs, reasoners, timeout, heap):
    """Runs the references on `cases` in `jobs` JVMs, each over the cases of its residue;
    a JVM that timed out (exit 3) is started again and resumes. Appends to `out`."""
    procs = {}
    while True:
        for j in range(jobs):
            if j in procs and procs[j].poll() is None:
                continue
            if j in procs and procs[j].returncode != 3:
                continue
            procs[j] = subprocess.Popen(
                ["java", f"-Xmx{heap}", "-cp", cp, "nrese.dl.Scope", str(cases), str(parts[j]),
                 reasoners, str(timeout), str(j), str(jobs)],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if all(p.poll() is not None and p.returncode != 3 for p in procs.values()):
            break
        for p in procs.values():
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
    with open(out, "a", encoding="utf-8", newline="\n") as f:
        for p in parts:
            body = lines(p)
            if body:
                f.write("\n".join(body) + "\n")
            p.unlink(missing_ok=True)


def classpath():
    jar = os.environ.get("DL_RUNNER_JAR", str(ROOT / "tmp" / "scope-runner" / "dl-runner.jar"))
    classes = os.environ.get("DL_RUNNER_CLASSES", str(ROOT / "tmp" / "scope-runner" / "classes"))
    return os.pathsep.join([jar, classes])


def second(scope, out, jobs, timeout, reasoners, heap):
    nrese = {l.split("\t")[0]: l.split("\t")[1] for l in lines(out / "nrese.tsv")}
    first = collections.defaultdict(dict)
    for l in lines(out / "reference.tsv"):
        f = l.split("\t")
        first[f[0]][f[1]] = f[2]
    asked = set(reasoners.split(","))
    wanted = []
    for index, code in nrese.items():
        answers = first.get(index, {})
        if asked <= set(answers):
            continue
        decided_codes = {c for name, c in answers.items() if decided(c)}
        if not decided_codes or decided_codes != {code}:
            wanted.append(index)
    print(f"{len(wanted)} cases for {reasoners}", file=sys.stderr)
    if not wanted:
        return
    listing = out / "second.txt"
    listing.write_text("\n".join(wanted) + "\n", encoding="utf-8")
    cases = out / "second.ofn"
    examples = ROOT / "target" / "release" / "examples"
    subprocess.run([str(examples / f"scope{EXE}"), *source(scope), "--out", str(cases),
                    "--only", str(listing)], check=True)
    parts = [out / f"second-{j}.tsv" for j in range(jobs)]
    references(classpath(), cases, out / "reference.tsv", parts, jobs, reasoners, timeout, heap)
    cases.unlink()
    listing.unlink()


ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"


def encode(code):
    n = 1024 if code == "i" else int(code[1:], 2)
    return ALPHABET[n // 64] + ALPHABET[n % 64]


def record(scope, out, target, adjudicated):
    nrese = {l.split("\t")[0]: l.split("\t")[1] for l in lines(out / "nrese.tsv")}
    refs = collections.defaultdict(dict)
    for l in lines(out / "reference.tsv"):
        f = l.split("\t")
        refs[f[0]][f[1]] = f[2]
    settled = {}
    if adjudicated:
        for l in lines(pathlib.Path(adjudicated)):
            if not l.startswith("#"):
                f = l.split("\t")
                at, _, index = f[0].partition(":")
                if at == scope:
                    settled[index] = f[1]
    codes, missing = [], []
    for index in sorted(nrese, key=int):
        answers = {c for c in refs.get(index, {}).values() if decided(c)}
        if index in settled:
            codes.append(settled[index])
        elif len(answers) == 1:
            codes.append(answers.pop())
        else:
            missing.append(index)
    if missing:
        sys.exit(f"{len(missing)} cases neither decided by the references nor adjudicated: "
                 + " ".join(missing[:20]))
    text = "".join(encode(c) for c in codes)
    body = "\n".join(text[i:i + 128] for i in range(0, len(text), 128))
    pathlib.Path(target).write_text(body + "\n", encoding="utf-8", newline="\n")
    print(f"{len(codes)} cases recorded in {target}")


def certifiable(code, k):
    """Whether claim k of a consistent answer is one a model shows (consistency, a
    satisfiable class, a non-entailment)."""
    if k == 0:
        return True
    bit = code[k]
    return bit == "1" if k in (1, 2) else bit == "0"


def compare(out, cases_file, show):
    nrese = {}
    for l in lines(out / "nrese.tsv"):
        f = l.split("\t")
        nrese[f[0]] = (f[1], f[2])
    refs = collections.defaultdict(dict)
    for l in lines(out / "reference.tsv"):
        f = l.split("\t")
        refs[f[0]][f[1]] = f[2]
    status = collections.Counter()
    certified = collections.Counter()
    listed = collections.defaultdict(list)
    failures = collections.Counter()
    for index, (code, certs) in nrese.items():
        answers = refs.get(index, {})
        for name, c in answers.items():
            if not decided(c):
                failures[(name, c[:1])] += 1
        if not decided(code):
            s = "undecided"
        else:
            if code.startswith("c"):
                for k in range(11):
                    if certifiable(code, k):
                        certified["claims a model can show"] += 1
                        certified["shown by a checked model"] += certs[k] == "m"
            ref_codes = {c for c in answers.values() if decided(c)}
            if not ref_codes:
                shown = code.startswith("c") and all(
                    certs[k] == "m" for k in range(11) if certifiable(code, k))
                needs = code == "i" or any(not certifiable(code, k) for k in range(11))
                s = "certified-only" if shown and not needs else "open"
            elif len(ref_codes) > 1:
                s = "disputed"
            elif code in ref_codes:
                s = "agree"
            else:
                (ref,) = ref_codes
                diff = [k for k in range(11) if code.startswith("c") and ref.startswith("c")
                        and code[k] != ref[k]]
                proven = code.startswith("c") and (ref == "i" and certs[0] == "m" or diff and all(
                    certs[k] == "m" and certifiable(code, k) for k in diff))
                s = "differs-certified" if proven else "differs"
        status[s] += 1
        if s not in ("agree", "certified-only"):
            listed[s].append(index)
    total = sum(status.values())
    print(f"{total} cases")
    for s, n in status.most_common():
        print(f"  {s:<20} {n}")
    for k, n in certified.items():
        print(f"  {k}: {n}")
    for (name, c), n in sorted(failures.items()):
        print(f"  {name} answered {c!r}: {n}")
    texts = {}
    if cases_file and listed:
        wanted = {i for v in listed.values() for i in v}
        for l in lines(pathlib.Path(cases_file)):
            i, _, doc = l.partition("\t")
            if i in wanted:
                texts[i] = doc[doc.find("NamedIndividual(:b))") + len("NamedIndividual(:b))"):].strip()
    for s, ids in listed.items():
        print(f"\n{s}:")
        for i in ids[:show]:
            n_code, certs = nrese[i]
            r = refs.get(i, {})
            print(f"  {i}\tnrese {n_code} {certs}\t" + " ".join(f"{k} {v}" for k, v in sorted(r.items())))
            if i in texts:
                print(f"    {texts[i]}")
    return status


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("scope")
    r.add_argument("dir")
    r.add_argument("--slice", type=int, default=200_000)
    r.add_argument("--jobs", type=int, default=4)
    r.add_argument("--timeout", type=int, default=20)
    r.add_argument("--reasoners", default="hermit")
    r.add_argument("--heap", default="1536m")
    s2 = sub.add_parser("second")
    s2.add_argument("scope")
    s2.add_argument("dir")
    s2.add_argument("--jobs", type=int, default=4)
    s2.add_argument("--timeout", type=int, default=20)
    s2.add_argument("--reasoners", default="openllet")
    s2.add_argument("--heap", default="1536m")
    rec = sub.add_parser("record")
    rec.add_argument("scope")
    rec.add_argument("dir")
    rec.add_argument("out")
    rec.add_argument("--adjudicated")
    c = sub.add_parser("compare")
    c.add_argument("dir")
    c.add_argument("--cases")
    c.add_argument("--show", type=int, default=20)
    a = p.parse_args()
    if a.cmd == "run":
        run(a.scope, pathlib.Path(a.dir), a.slice, a.jobs, a.timeout, a.reasoners, a.heap)
    elif a.cmd == "second":
        second(a.scope, pathlib.Path(a.dir), a.jobs, a.timeout, a.reasoners, a.heap)
    elif a.cmd == "record":
        record(a.scope, pathlib.Path(a.dir), a.out, a.adjudicated)
    else:
        compare(pathlib.Path(a.dir), a.cases, a.show)


if __name__ == "__main__":
    main()
