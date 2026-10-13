"""NRESE in ann-benchmarks: the server in memory inside the algorithm's container, the
vectors loaded as `nrv:vector` literals, each query a SPARQL `SERVICE nrv:search` over HTTP.

The index is what a user of the server gets: built at the first search (on a thread past
50,000 vectors), with the HNSW parameters NRESE ships (M 16, ef_construction 200; the server
doesn't let a user set them). `fit` returns once the graph covers every vector, as Qdrant's
module waits for its collection to turn green. The query argument is `nrv:searchBudget` (the
beam, hnswlib's ef). The result cache is off: ann-benchmarks repeats the same queries.
"""
import os
import re
import subprocess
import tempfile
import time

import numpy as np
import requests

from ..base.module import BaseANN

PORT = 18080
PREFIX = "PREFIX nrv: <urn:nrese:vector:>\n"
CHUNK = 50_000
ANSI = re.compile(r"\x1b\[[0-9;]*m")  # the server's log is coloured


def literal(vector) -> str:
    return '"[' + ",".join(repr(float(x)) for x in vector) + ']"^^<urn:nrese:vector:vector>'


class Nrese(BaseANN):
    def __init__(self, metric):
        self._metric = {"angular": "cosine", "euclidean": "l2", "dot": "dot"}[metric]
        self._budget = None
        self._log = tempfile.NamedTemporaryFile(prefix="nrese-", suffix=".log", delete=False)
        env = {
            **os.environ,
            "NRESE_STORE_MODE": "in-memory",
            "NRESE_BIND_ADDR": f"127.0.0.1:{PORT}",
            "NRESE_QUERY_CACHE_BYTES": "0",
            "NRESE_READ_REQUESTS_PER_WINDOW": "100000000",
            "NRESE_WRITE_REQUESTS_PER_WINDOW": "100000000",
            "NRESE_MAX_RDF_UPLOAD_BYTES": str(16 << 30),
            "RUST_LOG": "info",
        }
        self._server = subprocess.Popen(["nrese-server"], env=env, stdout=self._log, stderr=subprocess.STDOUT)
        self._base = f"http://127.0.0.1:{PORT}"
        self._http = requests.Session()
        for _ in range(600):
            try:
                if self._http.get(f"{self._base}/readyz", timeout=2).ok:
                    break
            except requests.RequestException:
                pass
            time.sleep(0.1)
        self.name = "NRESE"

    def fit(self, X):
        n = X.shape[0]
        for start in range(0, n, CHUNK):
            body = "".join(f"<urn:i:{i}> <urn:e> {literal(X[i])} .\n" for i in range(start, min(n, start + CHUNK)))
            r = self._http.post(f"{self._base}/dataset/data?default", data=body.encode(),
                                headers={"Content-Type": "application/n-triples"}, timeout=3600)
            r.raise_for_status()
        # The first search builds the graph (on a thread past 50,000 vectors): wait for the
        # server's own record that it covers all of them.
        self._search(X[0], 1)
        deadline = time.time() + 6 * 3600
        while n > 20_000 and time.time() < deadline:
            log = ANSI.sub("", open(self._log.name, errors="replace").read())
            built = re.findall(r"vector graph built.*?vectors=(\d+)", log)
            if built and int(built[-1]) >= n:
                break
            time.sleep(1)

    def set_query_arguments(self, budget):
        self._budget = budget
        self.name = f"NRESE(searchBudget={budget})"

    def _search(self, q, n):
        budget = f" ; nrv:searchBudget {self._budget}" if self._budget else ""
        query = (PREFIX + "SELECT ?i WHERE { ?i <urn:e> ?v . SERVICE nrv:search { ?v nrv:near "
                 f"{literal(q)} ; nrv:k {n} ; nrv:metric \"{self._metric}\"{budget} ; nrv:rank ?r }} }} ORDER BY ?r")
        r = self._http.post(f"{self._base}/dataset/query", data={"query": query},
                            headers={"Accept": "text/tab-separated-values"}, timeout=600)
        r.raise_for_status()
        return [int(line[len("<urn:i:"):-1]) for line in r.text.splitlines()[1:] if line.startswith("<urn:i:")]

    def query(self, q, n):
        return np.array(self._search(q, n))

    def get_memory_usage(self):
        try:
            with open(f"/proc/{self._server.pid}/status") as f:
                return next(int(line.split()[1]) for line in f if line.startswith("VmRSS:"))
        except (OSError, StopIteration):
            return None

    def done(self):
        self._server.terminate()
        try:
            self._server.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self._server.kill()
