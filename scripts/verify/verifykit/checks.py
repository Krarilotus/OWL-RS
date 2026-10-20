"""What a step expects of its response, checked: predicates on a value, JSON paths, and row counts.
Every failure is one line that says what was expected and what came instead, so a summary can be
read without opening the response."""
from __future__ import annotations

import csv
import io
import json
import re
import xml.etree.ElementTree as ElementTree

PREDICATES = ("equals", "contains", "not_contains", "matches", "length", "present")


class PathError(ValueError):
    """A JSON path that doesn't parse, or names nothing in the document."""


_TOKEN = re.compile(r"\.([A-Za-z_][A-Za-z0-9_-]*)|\['((?:[^'\\]|\\.)*)'\]|\[(\d+)\]|\[\*\]")


def parse_path(path: str) -> list[str | int | None]:
    """`$.a.b[0]['c d'][*]` as its steps: a key, an index, or None for every element."""
    if not path.startswith("$"):
        raise PathError(f"{path!r}: a JSON path starts with $")
    steps: list[str | int | None] = []
    at = 1
    while at < len(path):
        m = _TOKEN.match(path, at)
        if not m:
            raise PathError(f"{path!r}: can't read it from {path[at:]!r} on "
                            "(keys .name or ['name'], indexes [0], every element [*])")
        name, quoted, index = m.groups()
        steps.append(name if name is not None else
                     re.sub(r"\\(.)", r"\1", quoted) if quoted is not None else
                     int(index) if index is not None else None)
        at = m.end()
    return steps


def json_path(document, path: str):
    """The value at `path`. With [*] it is the list of the values found under each element
    (elements without the rest of the path left out); without, a missing key is a PathError."""
    values, every = [document], False
    for step in parse_path(path):
        found = []
        for value in values:
            if step is None:
                if not isinstance(value, (list, dict)):
                    raise PathError(f"{path}: [*] of a {type(value).__name__}")
                found.extend(value if isinstance(value, list) else value.values())
            elif isinstance(step, int):
                if isinstance(value, list) and step < len(value):
                    found.append(value[step])
                elif not every:
                    raise PathError(f"{path}: no element [{step}]"
                                    + (f" (the list has {len(value)})" if isinstance(value, list) else ""))
            elif isinstance(value, dict) and step in value:
                found.append(value[step])
            elif not every:
                raise PathError(f"{path}: no key {step!r}"
                                + (f" (keys: {', '.join(list(value)[:12])})" if isinstance(value, dict) else
                                   f" in a {type(value).__name__}"))
        every = every or step is None
        values = found
    return values if every else values[0]


def show(value, limit: int = 160) -> str:
    """A value as JSON (text in quotes, its line breaks escaped), cut to `limit` characters with
    its length. The evidence files hold the exact bytes."""
    if isinstance(value, str):
        cut = value[:limit].replace("\r", "\\r").replace("\n", "\\n").replace("\t", "\\t")
        return f'"{cut}"' if len(value) <= limit else f'"{cut}…" ({len(value)} characters)'
    text = json.dumps(value, ensure_ascii=False)
    return text if len(text) <= limit else f"{text[:limit]}… ({len(text)} characters)"


def check(what: str, value, predicate: dict, *, present: bool = True) -> list[str]:
    """The failures of `predicate` on `value` (none: it holds), each starting with `what`.
    `present` False means there is no value (an absent header, a JSON path naming nothing)."""
    failures = []
    if "present" in predicate:
        if predicate["present"] and not present:
            return [f"{what}: expected present, absent"]
        if not predicate["present"] and present:
            return [f"{what}: expected absent, got {show(value)}"]
    if not present:
        rest = {k: v for k, v in predicate.items() if k != "present"}
        return [f"{what}: absent (expected {describe(rest)})"] if rest else []
    if "equals" in predicate and value != predicate["equals"]:
        failures.append(f"{what}: expected {show(predicate['equals'])}, got {show(value)}")
    for key, negated in (("contains", False), ("not_contains", True)):
        if key not in predicate:
            continue
        wanted = predicate[key]
        held = (wanted in value) if isinstance(value, (list, dict)) else (str(wanted) in _text(value))
        if held == negated:
            failures.append(f"{what}: expected {'not ' if negated else ''}to contain {show(wanted)}, "
                            + (f"found it in {show(value)}" if negated else f"got {show(value)}"))
    if "matches" in predicate and not re.search(predicate["matches"], _text(value)):
        failures.append(f"{what}: expected to match /{predicate['matches']}/, got {show(value)}")
    if "length" in predicate:
        if not isinstance(value, (list, dict, str)):
            failures.append(f"{what}: expected length {predicate['length']}, got a {type(value).__name__} {show(value)}")
        elif len(value) != predicate["length"]:
            failures.append(f"{what}: expected length {predicate['length']}, got {len(value)}")
    return failures


def describe(predicate: dict) -> str:
    return ", ".join(f"{k} {show(v)}" for k, v in predicate.items())


def _text(value) -> str:
    return value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)


def media_type(content_type: str | None) -> str:
    return (content_type or "").split(";")[0].strip().lower()


def count_rows(content_type: str | None, body: bytes) -> int:
    """The rows of a SPARQL result (JSON, XML, CSV, TSV) or the statements of N-Triples or
    N-Quads; a ValueError naming the media type for anything else."""
    kind = media_type(content_type)
    text = body.decode("utf-8", errors="replace")
    if kind in ("application/sparql-results+json", "application/json"):
        document = json.loads(text)
        if not isinstance(document, dict) or "results" not in document:
            raise ValueError("a JSON answer without results.bindings (an ASK answer has no rows)")
        return len(document["results"]["bindings"])
    if kind in ("application/sparql-results+xml", "application/xml"):
        root = ElementTree.fromstring(text)
        return sum(1 for e in root.iter() if e.tag.rsplit("}", 1)[-1] == "result")
    if kind == "text/csv":
        return max(0, sum(1 for row in csv.reader(io.StringIO(text)) if row) - 1)
    if kind == "text/tab-separated-values":
        return max(0, sum(1 for line in text.splitlines() if line) - 1)
    if kind in ("application/n-triples", "application/n-quads", "text/plain"):
        return sum(1 for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#"))
    raise ValueError(f"can't count the rows of {kind or 'a response without a Content-Type'}")
