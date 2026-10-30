"""The SNB mapping's invariants (snb_to_rdf.py): every edge and every value is kept, parallel edges
keep their own reifier, variant A reifies only edges with properties, variant B every edge.

    python3 -m pytest benches/property-graphs/test_snb_to_rdf.py
"""
from __future__ import annotations

import gzip
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent))
import snb_check  # noqa: E402
import snb_to_rdf  # noqa: E402

NS = snb_to_rdf.NS
V = snb_to_rdf.V
LONG_MAX = "9223372036854775807"


def write(root: Path, entity: str, header: str, *rows: str, kind: str = "dynamic") -> None:
    directory = root / kind / entity
    directory.mkdir(parents=True)
    with gzip.open(directory / "part-00000.csv.gz", "wt", encoding="utf-8", newline="") as f:
        f.write("\n".join([header, *rows]) + "\n")


@pytest.fixture
def fixture(tmp_path: Path) -> Path:
    root = tmp_path / "csv"
    (root / "static").mkdir(parents=True)
    write(root, "Place", "id|name|url|type|PartOfPlaceId", "0|Earth|u|continent|", "1|Here|u|city|0", kind="static")
    write(root, "Person", "creationDate|id|firstName|lastName|gender|birthday|locationIP|browserUsed|LocationCityId|language|email",
          f"2010-01-01T00:00:00.000+00:00|{LONG_MAX}|A|\"Quote\\|f|1980-02-29|ip|b|1|en;de|a@x;b@x",
          "2012-12-31T23:59:59.999+00:00|7|B|L|m|1999-12-31|ip|b|1|en|")
    write(root, "Person_knows_Person", "creationDate|Person1Id|Person2Id",
          f"2011-05-05T10:00:00.000+00:00|{LONG_MAX}|7",
          f"2011-06-06T10:00:00.000+00:00|{LONG_MAX}|7")  # a parallel edge
    return root


def run(fixture: Path, tmp_path: Path) -> tuple[list[str], list[str], dict]:
    out = tmp_path / "out"
    assert snb_to_rdf.main(["snb_to_rdf.py", str(fixture), str(out)]) == 0
    a = (out / "snb-a.nt").read_text(encoding="utf-8").splitlines()
    b = (out / "snb-b.nt").read_text(encoding="utf-8").splitlines()
    return a, b, json.loads((out / "counts.json").read_text(encoding="utf-8"))


def test_parallel_edges_keep_a_reifier_each_and_their_property(fixture, tmp_path):
    a, _, counts = run(fixture, tmp_path)
    s, o, p = f"<{NS}Person/{LONG_MAX}>", f"<{NS}Person/7>", f"<{V}knows>"
    assert a.count(f"{s} {p} {o} .") == 2  # one statement once loaded
    reifiers = [line.split()[0] for line in a if line.endswith(f"<<( {s} {p} {o} )>> .")]
    assert len(reifiers) == 2 and len(set(reifiers)) == 2
    dates = {line.split(" ", 2)[2] for line in a if line.split()[0] in reifiers and f"{V}creationDate" in line}
    assert dates == {f'"2011-05-05T10:00:00.000+00:00"^^<{snb_to_rdf.XSD}dateTime> .',
                     f'"2011-06-06T10:00:00.000+00:00"^^<{snb_to_rdf.XSD}dateTime> .'}
    knows = counts["edges"]["knows"]
    assert (knows["edges"], knows["parallel"], knows["reified_a"], knows["properties"]) == (2, 1, 2, {"creationDate": 2})


def test_an_edge_without_properties_is_reified_in_b_only(fixture, tmp_path):
    a, b, counts = run(fixture, tmp_path)
    term = f"<<( <{NS}Person/7> <{V}isLocatedIn> <{NS}Place/1> )>>"
    assert not [line for line in a if term in line]
    reified = [line for line in b if term in line]
    assert len(reified) == 1
    reifier = reified[0].split()[0]
    assert [line for line in b if line.startswith(reifier + " ")] == reified  # no property
    located = counts["edges"]["isLocatedIn"]
    assert (located["edges"], located["reified_a"], located["reified_b"]) == (2, 0, 2)
    # B is A plus one reifier statement per edge without properties.
    unreified = sum(c["edges"] - c["reified_a"] for c in counts["edges"].values())
    assert set(a) <= set(b) and len(b) - len(a) == unreified == 3  # 2 isLocatedIn, 1 isPartOf


def test_extreme_values_keep_their_lexical_form(fixture, tmp_path):
    a, _, counts = run(fixture, tmp_path)
    xsd = snb_to_rdf.XSD
    assert f'<{NS}Person/{LONG_MAX}> <{V}id> "{LONG_MAX}"^^<{xsd}long> .' in a
    assert f'<{NS}Person/{LONG_MAX}> <{V}birthday> "1980-02-29"^^<{xsd}date> .' in a
    assert f'<{NS}Person/7> <{V}creationDate> "2012-12-31T23:59:59.999+00:00"^^<{xsd}dateTime> .' in a
    assert f'<{NS}Person/{LONG_MAX}> <{V}lastName> "\\"Quote\\\\" .' in a
    person = counts["nodes"]["Person"]
    assert (person["min_id"], person["max_id"]) == (7, int(LONG_MAX))
    # The edges of each date range, with a subject holding them, for the load's check.
    assert counts["extremes"]["creationDate"] == {
        "min": ["2010-01-01T00:00:00.000+00:00", f"<{NS}Person/{LONG_MAX}>"],
        "max": ["2012-12-31T23:59:59.999+00:00", f"<{NS}Person/7>"]}
    assert counts["edges"]["knows"]["extremes"]["creationDate"]["max"] == [
        "2011-06-06T10:00:00.000+00:00", f"<{NS}e/knows/2>"]
    # Composite cells: one triple per value; an empty cell none.
    assert person["triples"]["email"] == 2 and person["cells"]["email"] == 1
    assert person["triples"]["language"] == 3


def test_the_checks_expectations_are_the_distinct_statements_written(fixture, tmp_path):
    a, b, counts = run(fixture, tmp_path)
    for variant, lines in (("A", a), ("B", b)):
        per = {}
        for line in set(lines):  # a store holds each statement once
            predicate = line.split(" ", 2)[1].strip("<>")
            per[predicate] = per.get(predicate, 0) + 1
        want = snb_check.expected(counts, variant)
        assert want["predicates"] == per and want["quads"] == len(set(lines))
        terms = {line.split(" ", 2)[2] for line in lines if snb_check.RDF + "reifies" in line}
        assert want["terms"] == len(terms)


def test_a_row_with_a_shifted_cell_is_refused(tmp_path):
    root = tmp_path / "csv"
    (root / "static").mkdir(parents=True)
    write(root, "Person_knows_Person", "creationDate|Person1Id|Person2Id", "2011-05-05T10:00:00.000+00:00|1|2|3")
    with pytest.raises(ValueError, match="4 cells, header has 3"):
        snb_to_rdf.main(["snb_to_rdf.py", str(root), str(tmp_path / "out")])
