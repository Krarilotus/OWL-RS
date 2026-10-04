#!/usr/bin/env python3
"""Check the actual Linux image in disposable, capped containers; emit a receipt.

Uses only Python's standard library and Docker. Never touches existing containers,
volumes, ports or stores. The receipt describes this fixture, not production scale.
"""

import argparse
import hashlib
import json
import re
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

CONFIG = """[server]
bind_address = "0.0.0.0:8080"
deployment_posture = "read-only-demo"
[store]
mode = "on-disk"
data_dir = "/var/lib/nrese/data"
map_checkpoints = true
[reasoner]
mode = "disabled"
[budgets]
query_memory = "256MiB"
total_query_memory = "512MiB"
bulk_load_memory = "512MiB"
query_timeout = "10s"
query_text = "16KiB"
upload_size = "8MiB"
result_cache = "64MiB"
[policy.exposure]
operator_ui = false
metrics = false
[auth]
mode = "none"
local_logins = false
[ai]
enabled = false
[federation]
allow = []
"""

FIXTURE = """@prefix ex: <https://example.org/image-smoke/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
ex:subject ex:predicate ex:object .
ex:assertion rdf:reifies <<( ex:subject ex:predicate ex:object )>> ;
    prov:wasDerivedFrom ex:source .
ex:source a prov:Entity .
"""
ASK = "ASK { <https://example.org/image-smoke/subject> ?p ?o }"
PROVENANCE = """PREFIX ex: <https://example.org/image-smoke/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX prov: <http://www.w3.org/ns/prov#>
SELECT ?source WHERE {
  ?assertion rdf:reifies <<( ex:subject ex:predicate ex:object )>> ;
      prov:wasDerivedFrom ?source .
}"""
UPDATE = "INSERT DATA { <urn:smoke:forbidden> <urn:smoke:p> <urn:smoke:o> }"
CONFIG_PATH = "/var/lib/nrese/data/image-smoke.toml"
DATA_PATH = "/var/lib/nrese/data/image-smoke.ttl"
CAP = 2 * 1024**3


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def check_image(image, context, revision):
    docker = ["docker"] + (["--context", context] if context else [])

    def command(*args, input_text=None):
        result = subprocess.run(
            [*docker, *args],
            input=input_text,
            text=True,
            capture_output=True,
            encoding="utf-8",
            errors="replace",
            timeout=180,
            check=False,
        )
        require(result.returncode == 0, f"docker {args[0]} failed: {result.stderr}")
        return result.stdout.strip()

    metadata = json.loads(command("image", "inspect", image))[0]
    require(
        metadata["Os"] == "linux" and metadata["Architecture"] == "amd64",
        "Expected a Linux amd64 image",
    )
    labels = metadata["Config"].get("Labels") or {}
    source_revision = labels.get("org.opencontainers.image.revision", "")
    require(
        re.fullmatch(r"[0-9a-f]{40}", source_revision),
        "Missing full source revision label",
    )
    if revision:
        require(
            source_revision == revision,
            "Image source revision does not match --revision",
        )
    require(
        labels.get("io.nrese.target-cpu") == "portable", "Expected portable CPU build"
    )
    require(
        metadata["Config"]["User"] in ("nrese", "10001"),
        "Expected unprivileged image user",
    )
    image_id = metadata[
        "Id"
    ]  # All checks use immutable identity, even if the tag moves.
    name = "nrese-image-smoke-" + uuid.uuid4().hex
    volume = command("volume", "create", name)
    require(volume == name, "Unexpected Docker volume identity")
    containers = []
    mount = f"type=volume,source={volume},target=/var/lib/nrese/data"
    limits = [
        "--memory",
        str(CAP),
        "--memory-swap",
        str(CAP),
        "--cpus",
        "1",
        "--pids-limit",
        "128",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=16m",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--mount",
        mount,
        "-e",
        "RAYON_NUM_THREADS=1",
    ]
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    checks = []

    def create(*args):
        container = command("create", *limits, *args)
        containers.append(container)
        return container

    def foreground(*args, input_text=None):
        container = create("--network", "none", *args)
        output = command(
            "start", "--attach", "--interactive", container, input_text=input_text
        )
        state = json.loads(command("inspect", container))[0]["State"]
        require(
            state["ExitCode"] == 0 and not state["OOMKilled"],
            f"Container failed: {state}",
        )
        return output

    def request(base, path, *, method="GET", body=None, media=None):
        headers = {"Accept": "application/sparql-results+json"}
        if media:
            headers["Content-Type"] = media
        req = urllib.request.Request(
            base + path,
            data=body.encode() if body else None,
            headers=headers,
            method=method,
        )
        try:
            with opener.open(req, timeout=15) as response:
                return response.status, response.read().decode()
        except urllib.error.HTTPError as error:
            return error.code, error.read().decode()

    def ready(base, container):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            try:
                status, body = request(base, "/readyz")
                if status == 200 and json.loads(body).get("status") == "ready":
                    result = json.loads(body)
                    require(
                        result["quad_count"] == 4, f"Unexpected fixture count: {result}"
                    )
                    return result
            except (OSError, ValueError):
                pass
            state = json.loads(command("inspect", container))[0]["State"]
            require(state["Running"], "Server exited: " + command("logs", container))
            time.sleep(0.5)
        raise RuntimeError("Server never ready: " + command("logs", container))

    def query_checks(base):
        for method in ("GET", "POST"):
            path = "/dataset/sparql"
            kwargs = {}
            if method == "GET":
                path += "?" + urllib.parse.urlencode({"query": ASK})
            else:
                kwargs = {
                    "method": "POST",
                    "body": ASK,
                    "media": "application/sparql-query",
                }
            status, body = request(base, path, **kwargs)
            require(
                status == 200 and json.loads(body).get("boolean") is True,
                f"{method} ASK failed: {status} {body}",
            )
        status, body = request(
            base,
            "/dataset/sparql",
            method="POST",
            body=PROVENANCE,
            media="application/sparql-query",
        )
        require(status == 200, f"Provenance query failed: {status} {body}")
        require(
            json.loads(body)["results"]["bindings"]
            == [
                {
                    "source": {
                        "type": "uri",
                        "value": "https://example.org/image-smoke/source",
                    }
                }
            ],
            "RDF 1.2 provenance was not preserved",
        )

    def address(container):
        inspected = json.loads(command("inspect", container))[0]
        binding = inspected["NetworkSettings"]["Ports"]["8080/tcp"][0]
        require(binding["HostIp"] == "127.0.0.1", "Port must be loopback only")
        return "http://127.0.0.1:" + binding["HostPort"]

    try:
        for path, content in ((CONFIG_PATH, CONFIG), (DATA_PATH, FIXTURE)):
            foreground(
                "-i",
                "--entrypoint",
                "/bin/sh",
                image_id,
                "-c",
                f"cat > {path}",
                input_text=content,
            )
        foreground(image_id, "check-config", "--config", CONFIG_PATH)
        foreground(image_id, "load", "--config", CONFIG_PATH, DATA_PATH)
        checks.extend(["config-validation", "serial-offline-load"])
        server = create(
            "--publish", "127.0.0.1::8080", image_id, "--config", CONFIG_PATH
        )
        command("start", server)
        inspected = json.loads(command("inspect", server))[0]
        host = inspected["HostConfig"]
        require(
            host["Memory"] == CAP and host["MemorySwap"] == CAP,
            "Hard memory cap missing",
        )
        base = address(server)
        initial_ready = ready(base, server)
        status, body = request(base, "/version")
        require(status == 200, "Version endpoint failed")
        version = json.loads(body)
        require(
            version["deployment_posture"] == "read-only-demo",
            "Wrong deployment posture",
        )
        require(
            version["store_mode"] == "on-disk" and version["durable_storage_available"],
            "Durable storage unavailable",
        )
        for flag in (
            "graph_write_enabled",
            "sparql_update_enabled",
            "tell_enabled",
            "federated_service_enabled",
            "admin_surface_enabled",
            "operator_surface_enabled",
            "metrics_enabled",
            "ai_query_suggestions_enabled",
        ):
            require(version[flag] is False, f"Unexpected exposed capability: {flag}")
        for key, value in {
            "query_memory": 256 * 1024**2,
            "total_query_memory": 512 * 1024**2,
            "query_text": 16 * 1024,
            "query_timeout_ms": 10000,
        }.items():
            require(version["budgets"][key] == value, f"Unexpected budget: {key}")
        query_checks(base)
        checks.extend(
            ["ready-and-effective-budgets", "get-post-ask", "rdf12-provenance-select"]
        )
        for path, media, body in (
            ("/dataset/update", "application/sparql-update", UPDATE),
            ("/dataset/sparql", "application/sparql-update", UPDATE),
            ("/dataset/data?default", "text/turtle", FIXTURE),
        ):
            status, response = request(
                base, path, method="POST", body=body, media=media
            )
            require(
                status == 404,
                f"Write surface was not hidden: {path}: {status} {response}",
            )
        checks.append("update-and-graph-write-denied")
        # Only this check's container is stopped; the store survives in its own volume.
        command("stop", "--time", "15", server)
        stopped = json.loads(command("inspect", server))[0]["State"]
        # The server currently uses the default SIGTERM disposition (143). Reject
        # forced SIGKILL (137) and OOM, while checking recovery of acknowledged data.
        require(
            stopped["ExitCode"] in (0, 143) and not stopped["OOMKilled"],
            f"Unexpected shutdown: {stopped}",
        )
        command("start", server)
        base = address(server)
        recovered_ready = ready(base, server)
        require(
            recovered_ready["revision"] == initial_ready["revision"],
            "Revision changed on reopen",
        )
        query_checks(base)
        checks.append("durable-reopen")
        command("stop", "--time", "15", server)
        final_state = json.loads(command("inspect", server))[0]["State"]
        require(
            final_state["ExitCode"] in (0, 143) and not final_state["OOMKilled"],
            f"Unexpected final shutdown: {final_state}",
        )
        return {
            "schema": "nrese-linux-image-smoke-v1",
            "status": "passed",
            "tested_at": datetime.now(timezone.utc).isoformat(),
            "source_revision": source_revision,
            "source_url": labels.get("org.opencontainers.image.source"),
            "checker_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "image_id": image_id,
            "platform": "linux/amd64",
            "target_cpu": "portable",
            "memory_limit_bytes": CAP,
            "fixture_sha256": hashlib.sha256(FIXTURE.encode()).hexdigest(),
            "config_sha256": hashlib.sha256(CONFIG.encode()).hexdigest(),
            "checks": checks,
            "version": version,
            "ready": recovered_ready,
            "shutdown_exit_codes": [stopped["ExitCode"], final_state["ExitCode"]],
            "scope": "Four-quad fixture; not a production capacity or full conformance certificate.",
        }
    finally:
        cleanup_errors = []
        for container in reversed(containers):
            try:
                command("rm", "--force", container)
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                cleanup_errors.append(str(error))
        try:
            command("volume", "rm", volume)
        except (RuntimeError, subprocess.TimeoutExpired) as error:
            cleanup_errors.append(str(error))
        require(not cleanup_errors, "Cleanup failed: " + "; ".join(cleanup_errors))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--docker-context")
    parser.add_argument("--revision", help="Require this full source commit")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    receipt = check_image(args.image, args.docker_context, args.revision)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(f"PASS {receipt['image_id']}: {', '.join(receipt['checks'])}")


if __name__ == "__main__":
    main()
