#!/usr/bin/env python3
"""Regenerates crates/rdf/nrese-xsd/src/owl/regular/blocks.rs from Unicode's Blocks.txt.

    curl -sSO https://www.unicode.org/Public/UCD/latest/ucd/Blocks.txt
    python3 scripts/gen-unicode-blocks.py Blocks.txt

XML Schema names a block by its name with white space removed (`\\p{IsBasicLatin}`).
"""

import re
import sys
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "crates/rdf/nrese-xsd/src/owl/regular/blocks.rs"


def main(source: str) -> None:
    text = Path(source).read_text(encoding="utf-8")
    version = re.search(r"Blocks-([\d.]+)\.txt", text)
    rows = []
    for line in text.splitlines():
        line = line.split("#")[0].strip()
        if not line:
            continue
        span, name = line.split(";")
        lo, hi = span.split("..")
        rows.append((re.sub(r"\s+", "", name.strip()), int(lo, 16), int(hi, 16)))
    rows.sort()
    out = [
        f"//! Unicode blocks (`Blocks.txt` of Unicode {version.group(1) if version else '?'}), for the `\\p{{IsBlock}}` escapes of",
        "//! XML Schema regular expressions: names with white space removed, sorted for a binary",
        "//! search. Generated from the Unicode Character Database; regenerate with",
        "//! `scripts/gen-unicode-blocks.py`.",
        "",
        "#[rustfmt::skip]",
        "pub(super) const BLOCKS: &[(&str, u32, u32)] = &[",
    ]
    out += [f'    ("{n}", 0x{lo:04X}, 0x{hi:04X}),' for n, lo, hi in rows]
    out.append("];")
    OUT.write_text("\n".join(out) + "\n", encoding="utf-8", newline="\n")
    print(f"{len(rows)} blocks -> {OUT}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "Blocks.txt")
