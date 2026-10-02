"""Answers queries over a materialised closure, for systems without their own SPARQL engine.

    answer_queries.py --queries DIR file.nt...

Loads the files (asserted plus inferred N-Triples) into pyoxigraph and prints
"<query><TAB><answer count>" per query. The counts show whether a closure is complete for
the queries. The timing is Oxigraph's, so it's never reported as the system's.
"""

import argparse
from pathlib import Path

import pyoxigraph


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--queries", required=True)
    parser.add_argument("files", nargs="+")
    args = parser.parse_args()
    store = pyoxigraph.Store()
    for path in args.files:
        store.bulk_load(path=path, format=pyoxigraph.RdfFormat.N_TRIPLES)
    for query in sorted(Path(args.queries).glob("*.rq")):
        rows = len(list(store.query(query.read_text(encoding="utf-8"))))
        print(f"{query.stem}\t{rows}")


if __name__ == "__main__":
    main()
