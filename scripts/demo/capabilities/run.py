"""Five existing-capability demos. Linux fleet only; see README.md."""
import argparse
import contextlib
import itertools
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from urllib.parse import urlencode

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE.parents[1] / "verify"), str(HERE.parents[2] / "benches/suite")]
from verifykit import client
from verifykit.server import Server

EVIDENCE = None
REQUESTS = itertools.count(1)
QUERY = "SELECT ?version WHERE { <urn:demo:contract> <urn:demo:component> ?part . ?part <urn:demo:version> ?version }"


def request(url, method="GET", body=None, media="application/sparql-update"):
    assert EVIDENCE is not None
    path = EVIDENCE / f"{next(REQUESTS):03}"
    record = client.exchange({"path": "", "method": method, "params": {},
        "headers": {"Content-Type": media, "Accept": "application/sparql-results+json"},
        "payload": ("body", body) if body is not None else (None, None),
        "timeout_s": 10, "read_limit": 1 << 20}, url, path, 1 << 20)
    payload = record.pop("body")
    path.with_suffix(".json").write_text(json.dumps(record, indent=2))
    assert record["error"] is None, record
    assert 200 <= record["response"]["status"] < 300, record
    assert record["response"]["body"]["saved_complete"], record
    return {k.lower(): v for k, v in record["response"]["headers"]}, payload


def document(url, **kwargs):
    return json.loads(request(url, **kwargs)[1])


def versions(url, tx=False):
    if tx:
        result = document(url + "?action=QUERY", method="PUT", body=QUERY,
                          media="application/sparql-query")
    else:
        result = document(url + "?" + urlencode({"query": QUERY}))
    return sorted(row["version"]["value"] for row in result["results"]["bindings"])


def change(value):
    return ('DELETE WHERE { <urn:demo:brake> <urn:demo:version> ?v }; '
            f'INSERT DATA {{ <urn:demo:brake> <urn:demo:version> "{value}" }}')


def until(read, accept):
    deadline = time.monotonic() + 30
    last = None
    while time.monotonic() < deadline:
        try:
            last = read()
            if accept(last):
                return last
        except TimeoutError as error:
            last = str(error)
        time.sleep(0.1)
    raise RuntimeError(f"30-second deadline: {last}")


def environment(data):
    # Don't inherit a real server's settings or credentials (main() bars an ancestor's .env).
    return {**{k: v for k, v in os.environ.items() if not k.startswith("NRESE_")},
            "NRESE_DATA_DIR": str(data), "NRESE_STORE_MODE": "on-disk",
            "NRESE_DEPLOYMENT_POSTURE": "open-workbench", "NRESE_AUTH_MODE": "none",
            "NRESE_REASONING_MODE": "none", "NRESE_WAL_ARCHIVE": "true",
            "RAYON_NUM_THREADS": "2", "TOKIO_WORKER_THREADS": "2"}


@contextlib.contextmanager
def server(binary, root, name, primary=None):
    env = environment(root / name / "data")
    env.update(NRESE_REPLICATION_MODE="replica" if primary else "primary")
    if primary:
        env.update(NRESE_REPLICATION_PRIMARY=primary, NRESE_REPLICATION_POLL_MS="100",
                   NRESE_REPLICATION_IMAGE_TIMEOUT_MS="30000",
                   NRESE_REPLICATION_ROUND_TIMEOUT_MS="30000")
    instance = Server([binary, "--config", str(root / "empty.toml")], {}, env,
                      root / f"{name}.log", start_timeout=30)
    instance.root = root / name
    instance.root.mkdir(exist_ok=True)
    try:
        instance.start()
        yield instance.base
    finally:
        cleanup = instance.cleanup(keep=True)
        (root / f"{name}-cleanup.json").write_text(json.dumps(cleanup))
        assert cleanup["stopped"] and not cleanup["error"], cleanup


def main():
    global EVIDENCE
    if not __debug__:
        sys.exit("run.py checks every answer with assert: run it without -O or PYTHONOPTIMIZE")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--snapshot", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)  # never reuse another run's state
    # Every server and the restore start at or below root, and the server loads the first .env
    # from its working directory upwards: this empty one ends that search before any ancestor's.
    (root / ".env").write_text("")
    EVIDENCE = root / "http"
    (root / "empty.toml").write_text("# Isolated demo configuration\n")
    binary = str(args.server.resolve())
    fixture = (HERE / "contract.nt").read_text()
    print(subprocess.check_output([str(args.snapshot.resolve())], cwd=root,
                                  timeout=30, text=True), end="", flush=True)
    with server(binary, root, "primary") as primary:
        query = primary + "/dataset/query"
        update = primary + "/dataset/update"
        request(update, "POST", "INSERT DATA {" + fixture + "}")
        assert versions(query) == ["approved"]

        headers, _ = request(primary + "/repositories/nrese/transactions", "POST")
        tx = headers["location"]
        if tx.startswith("/"):
            tx = primary + tx
        request(tx + "?action=UPDATE", "PUT", change("proposed"))
        private, committed = versions(tx, tx=True), versions(query)
        assert private == ["proposed"] and committed == ["approved"]
        request(tx, "DELETE")
        after = versions(query)
        assert after == ["approved"]
        print(f"private what-if: private={private}, committed={committed}, after rollback={after}", flush=True)
        print("LIMIT: request/session speculation; not durable branches or merge", flush=True)

        image = document(primary + "/api/v1/repositories/nrese/image", method="POST")
        cut0 = image["manifest"]["revision"]
        with server(binary, root, "replica", primary=primary) as replica:
            until(lambda: versions(replica + "/dataset/query"), lambda v: v == ["approved"])
            request(update, "POST", change("revised"))
            headers, feed = request(primary + f"/api/v1/replication/log?after={cut0}")
            (root / "feed.bin").write_bytes(feed)
            progress = {key: headers[key] for key in (
                "x-nrese-records", "x-nrese-last-revision", "x-nrese-latest-revision")}
            (root / "feed.json").write_text(json.dumps(progress, indent=2))
            assert int(progress["x-nrese-records"]) >= 1 and feed
            cut = int(progress["x-nrese-last-revision"])
            status = until(lambda: document(replica + "/api/v1/replication/status"),
                           lambda s: s["revision"] == s["primary_revision"] == cut
                           and s["records_applied"] >= 1)
            current = document(primary + "/api/v1/replication/status")["revision"]
            live, replayed = versions(query), versions(replica + "/dataset/query")
            (root / "replica-status.json").write_text(json.dumps(status, indent=2))
            assert current == int(progress["x-nrese-latest-revision"]) == cut, (current, progress)
            assert live == replayed == ["revised"] and not status.get("last_error")
            print(f"replication feed: {progress}, bytes={len(feed)}, primary={live}, primary_revision={current}, replica={replayed}, replica_revision={status['revision']}, applied={status['records_applied']}", flush=True)
            print("LIMIT: raw WAL replay; not Kafka Change+Latest, HA or all-catalogue replication", flush=True)

        request(update, "POST", change("later"))
        assert versions(query) == ["later"]
    # Stop the source before consuming its retained logs; restore only into new state.
    restore = [binary, "restore", image["directory"], "--config", str(root / "empty.toml")]
    for name in ("wal-archive", "wal"):
        directory = root / "primary" / "data" / name
        if directory.is_dir():
            restore += ["--wal", str(directory)]
    restore += ["--until-revision", str(cut)]
    with (root / "restore.log").open("wb") as log:
        subprocess.run(restore, cwd=root, env=environment(root / "restored" / "data"),
                       stdout=log, stderr=log, timeout=30, check=True)
    with server(binary, root, "restored") as restored:
        answer = versions(restored + "/dataset/query")
        revision = document(restored + "/api/v1/replication/status")["revision"]
        assert answer == ["revised"] and revision == cut and cut > cut0
        print(f"image restore: image_revision={cut0}, restored_revision={revision}, answer={answer}, later source=['later'] excluded", flush=True)
        print("LIMIT: image + retained WAL cut; not queryable durable history", flush=True)

        other = restored + "/api/v1/repositories/demo-other"
        request(other, "PUT", '{"title":"Other contract"}', "application/json")
        request(other + "/update", "POST", "INSERT DATA {" + fixture + "}")
        default, separate = versions(restored + "/dataset/query"), versions(other + "/query")
        assert default == ["revised"] and separate == ["approved"]
        print(f"multiple repositories: nrese={default}, demo-other={separate} (same query)", flush=True)
        print("LIMIT: independent repositories; not configuration composition or cross-repository transactions", flush=True)
    print("PASS: all five demonstrations; release-identity rehearsal remains for 15 Nov", flush=True)


if __name__ == "__main__":
    main()
