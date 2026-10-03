#!/usr/bin/env python3
"""The W3C OWL 2 test cases under the direct semantics, on the reference reasoners
(docs/design/owl2-dl.md §11, work package 2.3).

    python w3c.py prepare ALL_RDF CASES          # every DL test with the direct semantics
    python w3c.py manifest CASES REASONER...     # CASES/manifest.tsv (container paths)
    docker run --rm -v "$PWD/CASES:/work" nrese-bench/dl-reference \
        batch /work/manifest.tsv /work/results.tsv /work/tax 60
    python w3c.py compare CASES [--disputed disputed.tsv] [--out results/w3c-DATE.tsv]

A test's premise (and conclusion or non-conclusion) is taken in RDF/XML where the suite
has it, else in functional syntax, else in OWL/XML; the OWL API reads all three. ELK
runs on the tests of the EL profile only. Each result is checked against the test's
type; a wrong result or an error not recorded in the DISPUTED file fails the run (exit 1);
timeouts and unsupported constructs are counted. DISPUTED records are settled by hand
with a witness, never by a vote.
"""

import argparse
import collections
import csv
import pathlib
import sys

from rdflib import Graph, Literal, Namespace, RDF, URIRef

TEST = Namespace("http://www.w3.org/2007/OWL/testOntology#")
TYPES = {
    TEST.ConsistencyTest: ("consistency", "consistent"),
    TEST.InconsistencyTest: ("consistency", "inconsistent"),
    TEST.PositiveEntailmentTest: ("entailment", "entailed"),
    TEST.NegativeEntailmentTest: ("entailment", "not-entailed"),
}
# (property suffix, extension), by preference.
FORMATS = [("rdfXml", "rdf"), ("fs", "ofn"), ("owlXml", "owx")]
PROFILE_OF = {"elk": TEST.EL}


def document(g, test, role):
    for prefix, ext in FORMATS:
        for o in g.objects(test, TEST[f"{prefix}{role}Ontology"]):
            if isinstance(o, Literal):
                return str(o), ext
    return None


def page_of(test):
    """The test's wiki page id (its IRI's last segment, `-2D` style escapes decoded): the
    name the working group's results use."""
    import re

    local = str(test).rsplit("/", 1)[-1]
    return re.sub(r"-([0-9A-F]{2})", lambda m: chr(int(m.group(1), 16)), local).replace("_", " ")


def prepare(args):
    g = Graph()
    g.parse(args.all_rdf, format="xml")
    cases = pathlib.Path(args.cases)
    cases.mkdir(parents=True, exist_ok=True)
    index = []
    for test in sorted(set(g.subjects(TEST.species, TEST.DL))):
        if (test, TEST.semantics, TEST.DIRECT) not in g:
            continue
        name = str(g.value(test, TEST.identifier) or test)
        status = str(g.value(test, TEST.status) or "").rsplit("#", 1)[-1]
        profiles = sorted(str(p).rsplit("#", 1)[-1] for p in g.objects(test, TEST.profile))
        premise = document(g, test, "Premise") or document(g, test, "Input")
        if premise is None:
            continue
        slug = f"t{len(index):04d}"
        folder = cases / slug
        folder.mkdir(exist_ok=True)
        text, ext = premise
        (folder / f"premise.{ext}").write_text(text, encoding="utf-8")
        premise_file = f"{slug}/premise.{ext}"
        for kind in g.objects(test, RDF.type):
            if kind not in TYPES:
                continue
            task, expected = TYPES[kind]
            conclusion_file = ""
            if task == "entailment":
                role = "Conclusion" if expected == "entailed" else "NonConclusion"
                found = document(g, test, role)
                if found is None:
                    continue
                ctext, cext = found
                (folder / f"{role.lower()}.{cext}").write_text(ctext, encoding="utf-8")
                conclusion_file = f"{slug}/{role.lower()}.{cext}"
            index.append((f"{slug}-{task}-{expected}", name, status, ",".join(profiles),
                          task, expected, premise_file, conclusion_file, page_of(test)))
    with open(cases / "index.tsv", "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["id", "name", "status", "profiles", "task", "expected", "premise",
                    "conclusion", "page"])
        w.writerows(index)
    print(f"{len(index)} tasks from {len(set(r[1] for r in index))} tests in {cases}")


def read_index(cases):
    with open(pathlib.Path(cases) / "index.tsv", encoding="utf-8") as f:
        return list(csv.DictReader(f, delimiter="\t"))


def manifest(args):
    rows = read_index(args.cases)
    lines = []
    for reasoner in args.reasoners:
        profile = PROFILE_OF.get(reasoner)
        for r in rows:
            if profile is not None and str(profile).rsplit("#", 1)[-1] not in r["profiles"].split(","):
                continue
            fields = [r["id"], reasoner, r["task"], f"/work/{r['premise']}"]
            if r["conclusion"]:
                fields.append(f"/work/{r['conclusion']}")
            lines.append("\t".join(fields))
    path = pathlib.Path(args.cases) / "manifest.tsv"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(lines)} tasks in {path}")


def read_disputed(path):
    """`id<TAB>reasoner<TAB>status<TAB>why`: known differences, each with its reason."""
    known = {}
    if path and pathlib.Path(path).exists():
        for line in pathlib.Path(path).read_text(encoding="utf-8").splitlines():
            if line.strip() and not line.startswith("#"):
                parts = line.split("\t")
                known[(parts[0], parts[1])] = (parts[2], parts[3] if len(parts) > 3 else "")
    return known


def compare(args):
    rows = {r["id"]: r for r in read_index(args.cases)}
    known = read_disputed(args.disputed)
    tally = collections.defaultdict(collections.Counter)
    unexpected, settled, out = [], [], []
    with open(pathlib.Path(args.cases) / "results.tsv", encoding="utf-8") as f:
        for line in f:
            tid, reasoner, task, status, ms, detail = (line.rstrip("\n").split("\t") + [""])[:6]
            r = rows[tid]
            expected = r["expected"]
            if status == expected:
                verdict = "pass"
            elif status in ("unsupported", "timeout", "parse-error", "error"):
                verdict = status
            else:
                verdict = "wrong-result"
            tally[reasoner][verdict] += 1
            out.append([r["name"], r["status"], r["task"], expected, reasoner, status, ms,
                        verdict, detail])
            # Timeouts and unsupported constructs are counted, not failures; a wrong result
            # or an error fails the run unless it is recorded.
            if verdict in ("wrong-result", "error", "parse-error"):
                record = known.get((r["name"] + "/" + r["task"], reasoner))
                if record is not None and record[0] == status:
                    settled.append((r["name"], reasoner, status, record[1]))
                elif r["status"] != "Rejected":
                    unexpected.append((r["name"], r["task"], reasoner, expected, status, detail))
    for reasoner, counts in sorted(tally.items()):
        total = sum(counts.values())
        parts = ", ".join(f"{k} {v}" for k, v in sorted(counts.items()))
        print(f"{reasoner}: {total} tasks: {parts}")
    for item in settled:
        print("DISPUTED (recorded):", *item, sep="\t")
    for item in unexpected:
        print("UNEXPECTED:", *item, sep="\t")
    if args.out:
        with open(args.out, "w", newline="", encoding="utf-8") as f:
            w = csv.writer(f, delimiter="\t", lineterminator="\n")
            w.writerow(["test", "status", "task", "expected", "reasoner", "result", "ms",
                        "verdict", "detail"])
            w.writerows(sorted(out))
    return 1 if unexpected else 0


TYPE_NAMES = {
    "Consistency": ("consistency", "consistent"),
    "Inconsistency": ("consistency", "inconsistent"),
    "Positive Entailment": ("entailment", "entailed"),
    "Negative Entailment": ("entailment", "not-entailed"),
}


def published(args):
    """The W3C working group's results of 14 December 2009 (Test_results.html, the CR exit
    criteria), DL tables: `identifier<TAB>task<TAB>expected<TAB>reasoner<TAB>result`."""
    import html as htmllib
    import re

    text = pathlib.Path(args.html).read_text(encoding="utf-8")
    out = []
    for section in ("OWL_2_DL_Approved_Test_Cases", "OWL_2_DL_Extra_Credit_Test_Cases"):
        start = text.find(f'id="{section}"')
        table = text[start:text.find("</table>", start)]
        heads = [re.sub(r"<[^>]+>", "", h).strip() for h in re.findall(r"<th>(.*?)</th>", table, re.S)]
        reasoners = [h.lower().replace("++", "pp") for h in heads[2:]]
        test = None
        for row in re.findall(r"<tr>(.*?)</tr>", table, re.S)[1:]:
            cells = re.findall(r"<td[^>]*>(.*?)</td>", row, re.S)
            link = re.search(r'href="http://owl\.semanticweb\.org/id/([^"]+)"', row)
            if link:
                ident = re.sub(r"-([0-9A-F]{2})", lambda m: chr(int(m.group(1), 16)), link.group(1))
                test = htmllib.unescape(ident).replace("_", " ")
                cells = cells[1:]
            kind = re.sub(r"<[^>]+>", "", cells[0]).strip()
            task, expected = TYPE_NAMES[kind]
            for reasoner, cell in zip(reasoners, cells[1:]):
                out.append((test, task, expected, reasoner, re.sub(r"<[^>]+>", "", cell).strip()))
    with open(args.out, "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n")
        w.writerow(["test", "task", "expected", "reasoner", "result"])
        w.writerows(out)
    print(f"{len(out)} published results in {args.out}")


def against(args):
    """Our run of a reasoner against its published results: a published `Pass` must pass."""
    ours = {}
    rows = {r["id"]: r for r in read_index(args.cases)}
    for line in open(pathlib.Path(args.cases) / "results.tsv", encoding="utf-8"):
        tid, reasoner, task, status = line.rstrip("\n").split("\t")[:4]
        if reasoner == args.reasoner:
            r = rows[tid]
            ours[(r["page"], task, r["expected"])] = status
    counts = collections.Counter()
    lost = []
    with open(args.published, encoding="utf-8") as f:
        for p in csv.DictReader(f, delimiter="\t"):
            if p["reasoner"] != (args.published_as or args.reasoner):
                continue
            k = (p["test"], p["task"], p["expected"])
            if k not in ours:
                counts["not run here"] += 1
                continue
            now = "Pass" if ours[k] == p["expected"] else ours[k]
            counts[f"published {p['result']}, here {'Pass' if now == 'Pass' else 'not'}"] += 1
            if p["result"] == "Pass" and now != "Pass":
                lost.append((p["test"], p["task"], p["expected"], ours[k]))
    for k, v in sorted(counts.items()):
        print(f"{args.reasoner}: {k}: {v}")
    for item in lost:
        print("PUBLISHED PASS NOT REPRODUCED:", *item, sep="\t")
    return 1 if lost else 0


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    a = sub.add_parser("prepare")
    a.add_argument("all_rdf")
    a.add_argument("cases")
    m = sub.add_parser("manifest")
    m.add_argument("cases")
    m.add_argument("reasoners", nargs="+")
    c = sub.add_parser("compare")
    c.add_argument("cases")
    c.add_argument("--disputed", default=str(pathlib.Path(__file__).with_name("disputed.tsv")))
    c.add_argument("--out")
    pub = sub.add_parser("published")
    pub.add_argument("html")
    pub.add_argument("out")
    ag = sub.add_parser("against")
    ag.add_argument("cases")
    ag.add_argument("published")
    ag.add_argument("reasoner")
    ag.add_argument("--published-as", help="the reasoner's name in the published results (openllet: pellet)")
    args = p.parse_args()
    if args.command == "prepare":
        prepare(args)
    elif args.command == "manifest":
        manifest(args)
    elif args.command == "published":
        published(args)
    elif args.command == "against":
        sys.exit(against(args))
    else:
        sys.exit(compare(args))


if __name__ == "__main__":
    main()
