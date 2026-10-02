"""Drops N-Triples lines that strict parsers reject.

Two kinds of invalid data occur in real dumps (DBpedia, Wikidata):
- IRIs with code points RFC 3987 forbids
- language tags that aren't well-formed BCP 47, e.g. a subtag longer than 8 characters

Strict parsers (Oxigraph, NRESE, Jena) reject the whole file and lenient ones keep the
lines, so the comparison would feed systems different data. The scorecard therefore drops
these lines once, before any system sees the file, and reports how many it dropped.

    python nt-filter.py < in.nt > out.nt
"""

import re
import sys

# IRI characters allowed by RFC 3987 (iunreserved/ucschar/iprivate, reserved, '%'), minus
# the characters N-Triples forbids inside <...>.
_UCSCHAR = (
    " -퟿豈-﷏ﷰ-￯"
    "\U00010000-\U0001fffd\U00020000-\U0002fffd\U00030000-\U0003fffd"
    "\U00040000-\U0004fffd\U00050000-\U0005fffd\U00060000-\U0006fffd"
    "\U00070000-\U0007fffd\U00080000-\U0008fffd\U00090000-\U0009fffd"
    "\U000a0000-\U000afffd\U000b0000-\U000bfffd\U000c0000-\U000cfffd"
    "\U000d0000-\U000dfffd\U000e1000-\U000efffd"
    "-\U000f0000-\U000ffffd\U00100000-\U0010fffd"
)
_IRI = re.compile(r"<([^>]*)>")
_VALID_IRI = re.compile(r"^[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%" + _UCSCHAR + r"]*$")
# A literal's language tag at the end of the line: "..."@tag .
_LANG = re.compile(r'"@([^\s"]+)\s*\.\s*$')
_VALID_LANG = re.compile(r"^[A-Za-z]{1,8}(-[A-Za-z0-9]{1,8})*$")


def valid(line: str) -> bool:
    # IRIs before the first quote: subject, predicate, and an IRI object.
    if not all(_VALID_IRI.match(iri) for iri in _IRI.findall(line.split('"', 1)[0])):
        return False
    lang = _LANG.search(line)
    return lang is None or _VALID_LANG.match(lang.group(1)) is not None


def main() -> None:
    kept = dropped = 0
    out = sys.stdout
    for line in sys.stdin:
        if valid(line):
            out.write(line)
            kept += 1
        else:
            dropped += 1
    print(f"nt-filter: kept {kept}, dropped {dropped}", file=sys.stderr)


if __name__ == "__main__":
    sys.stdin.reconfigure(encoding="utf-8", errors="surrogateescape")
    sys.stdout.reconfigure(encoding="utf-8", errors="surrogateescape")
    try:
        main()
    except BrokenPipeError:
        # The consumer stopped reading (e.g. `head`): not an error.
        sys.stderr.close()
