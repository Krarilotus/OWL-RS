"""LDBC SNB (composite-merged-fk CSV, the Interactive v2 initial data set) to RDF 1.2 N-Triples.

    python3 -I snb_to_rdf.py CSV_ROOT OUT_DIR

CSV_ROOT is the directory holding `static/` and `dynamic/` (the datagen's `initial_snapshot`).
Writes OUT_DIR/snb-a.nt, OUT_DIR/snb-b.nt and OUT_DIR/counts.json in one pass over the CSV.

The mapping (one place; README.md beside this file explains it):
- a node is `<{NS}{Type}/{id}>`, typed `rdf:type snb:{Type}`; a `type` column (Place: city, country,
  continent; Organisation: university, company) adds a second label, `snb:City` and so on;
- a node column is a property `snb:{column}` with the datatype of `DATATYPES` (else a plain
  string); the composite columns (`COMPOSITE`) hold `;`-separated values, one triple each; an
  empty cell is no triple;
- an edge is the triple `s snb:{label} o`: the foreign-key columns of a node file (`FK`) and the
  rows of an edge file (`Src_label_Dst`, its two id columns the ends, every other column a
  property of the edge);
- an edge with properties gets a reifier `<{NS}e/{label}/{n}>` (n: the edge's ordinal among its
  label's edges, node files first, then edge files, each in part order; parallel edges keep their own) with `r rdf:reifies <<( s p o )>>` and `r snb:{column}`
  per property. Variant A reifies those edges only; variant B every edge, the foreign-key edges
  with a reifier that has no property.

Every value is kept: counts.json has, per node type and edge label, the CSV rows and cells read
and the triples written, so a load's counts can be checked against it (M0).
"""
from __future__ import annotations

import csv
import gzip
import json
import sys
from array import array
from pathlib import Path

NS = "http://ldbcouncil.org/snb/"
V = NS + "vocab#"
RDF_TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
REIFIES = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies>"
XSD = "http://www.w3.org/2001/XMLSchema#"

DATATYPES = {
    "creationDate": "dateTime",
    "birthday": "date",
    "id": "long",
    "length": "int",
    "classYear": "int",
    "workFrom": "int",
}
# Node columns holding several values (SNB's composite attributes).
COMPOSITE = {("Person", "language"), ("Person", "email")}
# Foreign-key columns of node files: (label, target type, reversed). Reversed: the edge points
# from the target to the row's node (a forum contains its posts).
FK = {
    "CreatorPersonId": ("hasCreator", "Person", False),
    "LocationCountryId": ("isLocatedIn", "Place", False),
    "LocationCityId": ("isLocatedIn", "Place", False),
    "LocationPlaceId": ("isLocatedIn", "Place", False),
    "ParentPostId": ("replyOf", "Post", False),
    "ParentCommentId": ("replyOf", "Comment", False),
    "ContainerForumId": ("containerOf", "Forum", True),
    "ModeratorPersonId": ("hasModerator", "Person", False),
    "PartOfPlaceId": ("isPartOf", "Place", False),
    "TypeTagClassId": ("hasType", "TagClass", False),
    "SubclassOfTagClassId": ("isSubclassOf", "TagClass", False),
}
# The node type an edge file's end names.
END_TYPE = {"University": "Organisation", "Company": "Organisation"}
NODE_TYPES = ("Person", "Forum", "Post", "Comment", "Place", "Organisation", "Tag", "TagClass")


def iri(local: str) -> str:
    return f"<{NS}{local}>"


def literal(value: str, datatype: str | None) -> str:
    text = (value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("\r", "\\r"))
    return f'"{text}"^^<{XSD}{datatype}>' if datatype else f'"{text}"'


def parts(directory: Path) -> list[Path]:
    """An entity's CSV parts in name order (the order fixes the reifiers' ordinals)."""
    return sorted(p for p in directory.iterdir() if ".csv" in p.name)


def rows(directory: Path):
    """The header, then each row of an entity's parts; a row whose cell count differs from the
    header's is an error (a value holding the separator would shift every cell after it)."""
    header = None
    for path in parts(directory):
        opener = gzip.open if path.name.endswith(".gz") else open
        with opener(path, "rt", encoding="utf-8", newline="") as f:
            reader = csv.reader(f, delimiter="|", quoting=csv.QUOTE_NONE)
            this = next(reader)
            if header is None:
                header = this
                yield header
            elif this != header:
                raise ValueError(f"{path}: header {this} differs from {header}")
            for row in reader:
                if len(row) != len(header):
                    raise ValueError(f"{path}:{reader.line_num}: {len(row)} cells, header has {len(header)}")
                yield row


class Out:
    """The two variants' files: `both` writes a line to each, `b` to variant B only."""

    def __init__(self, directory: Path):
        self.a = open(directory / "snb-a.nt", "w", encoding="utf-8", newline="\n", buffering=1 << 22)
        self.bf = open(directory / "snb-b.nt", "w", encoding="utf-8", newline="\n", buffering=1 << 22)
        self.lines = {"a": 0, "b": 0}

    def both(self, line: str) -> None:
        self.a.write(line)
        self.bf.write(line)
        self.lines["a"] += 1
        self.lines["b"] += 1

    def b(self, line: str) -> None:
        self.bf.write(line)
        self.lines["b"] += 1

    def close(self) -> None:
        self.a.close()
        self.bf.close()


def edge(out: Out, s: str, label: str, o: str, ordinal: int, props: list[tuple[str, str]], counts: dict) -> None:
    """The edge `s label o` with its properties (column, value; empty values dropped)."""
    p = f"<{V}{label}>"
    out.both(f"{s} {p} {o} .\n")
    c = counts.setdefault(label, {"edges": 0, "reified_a": 0, "reified_b": 0, "properties": {}})
    c["edges"] += 1
    kept = [(k, v) for k, v in props if v != ""]
    r = iri(f"e/{label}/{ordinal}")
    line = f"{r} {REIFIES} <<( {s} {p} {o} )>> .\n"
    if kept:
        out.both(line)
        c["reified_a"] += 1
        for k, v in kept:
            out.both(f"{r} <{V}{k}> {literal(v, DATATYPES.get(k))} .\n")
            c["properties"][k] = c["properties"].get(k, 0) + 1
            if k in DATES:
                extreme(c.setdefault("extremes", {}), k, v, r)
    else:
        out.b(line)
    c["reified_b"] += 1


DATES = {k for k, t in DATATYPES.items() if t in ("date", "dateTime")}


def extreme(extremes: dict, column: str, value: str, subject: str) -> None:
    """The smallest and largest value of a date column and a subject holding it (the
    datagen writes one lexical form, `…T…+00:00` or a plain date, so text order is time
    order): the edges of the range, which a load is checked to keep."""
    e = extremes.setdefault(column, {"min": [value, subject], "max": [value, subject]})
    if value < e["min"][0]:
        e["min"] = [value, subject]
    if value > e["max"][0]:
        e["max"] = [value, subject]


def nodes(root: Path, out: Out, counts: dict) -> None:
    for directory in sorted(d for kind in ("static", "dynamic") for d in (root / kind).iterdir()):
        name = directory.name
        if "_" in name:
            continue
        if name not in NODE_TYPES:
            raise ValueError(f"unknown entity {name}")
        it = rows(directory)
        header = next(it)
        at = header.index("id")
        node = counts["nodes"].setdefault(name, {"rows": 0, "cells": {}, "triples": {}})
        ids = array("q")
        ordinal = counts["ordinals"]
        label_of = f"<{V}{name}>"
        for row in it:
            node["rows"] += 1
            ids.append(int(row[at]))
            s = iri(f"{name}/{row[at]}")
            out.both(f"{s} {RDF_TYPE} {label_of} .\n")
            for column, value in zip(header, row):
                if value == "":
                    continue
                node["cells"][column] = node["cells"].get(column, 0) + 1
                if column in FK:
                    label, target, reverse = FK[column]
                    o = iri(f"{target}/{value}")
                    ordinal[label] = ordinal.get(label, 0) + 1
                    if reverse:
                        edge(out, o, label, s, ordinal[label], [], counts["edges"])
                    else:
                        edge(out, s, label, o, ordinal[label], [], counts["edges"])
                elif column == "type":
                    out.both(f"{s} {RDF_TYPE} <{V}{value[:1].upper()}{value[1:]}> .\n")
                    node["triples"]["type"] = node["triples"].get("type", 0) + 1
                else:
                    values = value.split(";") if (name, column) in COMPOSITE else [value]
                    if len(set(values)) != len(values):
                        # A value twice in one cell is one statement in RDF.
                        repeated = node.setdefault("repeated_values", {})
                        repeated[column] = repeated.get(column, 0) + len(values) - len(set(values))
                    if column in DATES:
                        extreme(counts["extremes"], column, value, s)
                    for v in values:
                        out.both(f"{s} <{V}{column}> {literal(v, DATATYPES.get(column))} .\n")
                    node["triples"][column] = node["triples"].get(column, 0) + len(values)
        # Ids are unique per type: so are the foreign-key edges, one per row and column.
        ids = array("q", sorted(ids))
        duplicate = sum(1 for i in range(1, len(ids)) if ids[i] == ids[i - 1])
        if duplicate:
            raise ValueError(f"{name}: {duplicate} duplicate ids")
        if ids:
            node["min_id"], node["max_id"] = ids[0], ids[-1]
            node["max_id_iri"] = iri(f"{name}/{ids[-1]}")


def edges(root: Path, out: Out, counts: dict) -> None:
    for directory in sorted(d for kind in ("static", "dynamic") for d in (root / kind).iterdir()):
        name = directory.name
        if "_" not in name:
            continue
        src_type, label, dst_type = name.split("_")
        src_type, dst_type = END_TYPE.get(src_type, src_type), END_TYPE.get(dst_type, dst_type)
        it = rows(directory)
        header = next(it)
        ends = [i for i, column in enumerate(header) if column.lower().endswith("id")]
        if len(ends) != 2:
            raise ValueError(f"{name}: id columns {[header[i] for i in ends]}, expected two")
        props = [i for i in range(len(header)) if i not in ends]
        pairs = []
        ordinal = counts["ordinals"]
        for row in it:
            s, o = row[ends[0]], row[ends[1]]
            pairs.append((int(s) << 64) | int(o))
            ordinal[label] = ordinal.get(label, 0) + 1
            edge(out, iri(f"{src_type}/{s}"), label, iri(f"{dst_type}/{o}"), ordinal[label],
                 [(header[i], row[i]) for i in props], counts["edges"])
        # Parallel edges (the same ends twice under one label) keep a reifier each; the base
        # triple is one statement, so the store holds `edges - parallel` of them.
        pairs.sort()
        parallel = sum(1 for i in range(1, len(pairs)) if pairs[i] == pairs[i - 1])
        c = counts["edges"].setdefault(label, {"edges": 0, "reified_a": 0, "reified_b": 0, "properties": {}})
        c["parallel"] = c.get("parallel", 0) + parallel
        c.setdefault("files", {})[name] = len(pairs)


def main(argv: list[str]) -> int:
    root, out_dir = Path(argv[1]), Path(argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)
    counts = {"nodes": {}, "edges": {}, "ordinals": {}, "extremes": {}}
    out = Out(out_dir)
    try:
        nodes(root, out, counts)
        edges(root, out, counts)
    finally:
        out.close()
    del counts["ordinals"]
    counts["lines"] = out.lines
    (out_dir / "counts.json").write_text(json.dumps(counts, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
