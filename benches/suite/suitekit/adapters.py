"""One adapter per system: how it loads (and reasons), serves, and is reached.

The contract (`Adapter`):

- `regimes`: the entailment regimes the system runs, each with its own setting (none,
  rdfs, owl-horst, owl2-rl, owl2-ql); a workload runs under the first regime of its
  preference the system has.
- `images(ctx)` / `builds()`: the images it needs (pulled) and the ones this repository
  builds (benches/<dir>).
- `supports(ctx, plan, regime)`: None, or why it can't run this workload here (a licence
  missing, an input format it doesn't read, a runtime it has no path for).
- `load(ctx, store, inputs, regime) -> Step`: bulk load into a fresh store, reasoning
  included where the system reasons at load; wall time, peak memory, the statement counts
  it reports.
- `serve(ctx, store, regime) -> Endpoint`: the server on the loaded store, until its
  endpoint answers (the driver times this as the restart).
- `stop(ctx) -> peak MiB`: stops what serve started.

Systems that serve from memory load in `load` and keep that server (`serves_from_load`).
Systems without SPARQL (Nemo, the owlrl oracle) have their closure answered by Oxigraph
(`answers_only`: the answers count, their times don't).

The start commands are those of the kits (benches/competitors/scorecard.sh,
benches/reasoning/reasoning-scorecard.sh, benches/integration/run.sh). What no kit has run
yet is marked UNVERIFIED: the first run with a licence confirms it (RDFox; GraphDB with a
ruleset; AnzoGraph through the suite).
"""
from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

from .runtime import PREFIX, DockerRuntime, Measured, Mount, Runtime, Server, Spec

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


@dataclass
class Context:
    runtime: Runtime
    root: Path  # the repository
    work: Path  # this run's scratch directory (host), mounted at /work
    logs: Path  # this run's logs (host)
    data: Mount  # the datasets, at /data
    extra: list[Mount]  # a workload's own inputs (the integration workload's checkout at /rggs)
    timeout_s: int
    settings: dict  # the environment: images, licences, heap sizes
    run_id: str
    tier_small: bool = False

    @property
    def dry(self) -> bool:
        return self.runtime.dry

    def name(self, part: str) -> str:
        return f"{PREFIX}{self.run_id}-{part}"

    def mounts(self, *more: Mount) -> list[Mount]:
        return [self.data, Mount(str(self.work), "/work", readonly=False), *self.extra, *more]

    def listen(self, port: int, fixed: bool = False) -> int:
        """The port a server listens on: its usual one in a container (Docker maps the
        suite's port to it); the suite's port on the host, unless the system can't move."""
        if fixed or isinstance(getattr(self.runtime, "inner", self.runtime), DockerRuntime):
            return port
        return self.runtime.port

    def setting(self, key: str, default: str = "") -> str:
        return self.settings.get(key) or default

    @property
    def heap(self) -> str:
        return self.setting("JAVA_HEAP", "16g")

    @property
    def memory(self) -> str | None:
        return self.setting("DOCKER_MEMORY") or None


@dataclass
class Step:
    measured: Measured
    asserted: int | None = None
    inferred: int | None = None
    reason_ms: float | None = None  # when the system reports its reasoning time apart
    note: str = ""


@dataclass
class Endpoint:
    query: str
    update: str | None = None
    write_graph: str | None = None
    ready: str | None = None  # a GET URL that answers 200 when ready (else ASK {} on query)
    server: Server | None = None
    restart_ms: float | None = None


# --- HTTP -----------------------------------------------------------------------------------

def post(url: str, data: dict, accept: str = "application/sparql-results+json", timeout: float = 30) -> tuple[int, str]:
    body = urllib.parse.urlencode(data).encode()
    request = urllib.request.Request(url, data=body, headers={"Accept": accept})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except (urllib.error.URLError, ConnectionError, TimeoutError, OSError):
        return 0, ""


def get(url: str, timeout: float = 10) -> int:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            return response.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, ConnectionError, TimeoutError, OSError):
        return 0


def wait_ready(ctx: Context, endpoint: Endpoint, timeout_s: float) -> bool:
    if ctx.dry:
        return True
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if endpoint.server is not None and not ctx.runtime.alive(endpoint.server):
            return False
        if endpoint.ready:
            if get(endpoint.ready) == 200:
                return True
        elif post(endpoint.query, {"query": "ASK {}"}, timeout=10)[0] == 200:
            return True
        time.sleep(0.3)
    return False


def count_all(ctx: Context, endpoint: Endpoint) -> tuple[int | None, float, str]:
    """Every statement the endpoint answers with (asserted and inferred): count, ms, error."""
    if ctx.dry:
        return None, 0.0, ""
    started = time.monotonic()
    status, body = post(endpoint.query, {"query": "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"},
                        timeout=ctx.timeout_s)
    ms = (time.monotonic() - started) * 1000
    if status != 200:
        return None, ms, f"HTTP {status}: {body[:200]}"
    try:
        binding = json.loads(body)["results"]["bindings"][0]
        return int(next(iter(binding.values()))["value"]), ms, ""
    except (ValueError, KeyError, IndexError, StopIteration) as e:
        return None, ms, f"unreadable count: {e}"


def read_text(path: Path) -> str:
    try:
        return ANSI.sub("", path.read_text(encoding="utf-8", errors="replace"))
    except OSError:
        return ""


def last_int(pattern: str, text: str) -> int | None:
    found = re.findall(pattern, text)
    return int(found[-1]) if found else None


def image_tag(image: str) -> str:
    return image.rsplit(":", 1)[1] if ":" in image.rsplit("/", 1)[-1] else "latest"


def all_ntriples(inputs: list[str]) -> bool:
    return all(i.endswith(".nt") for i in inputs)


# --- the contract ---------------------------------------------------------------------------

class Adapter:
    key = ""
    regimes: dict[str, str] = {"none": ""}
    runtimes = {"docker", "apptainer"}
    answers_only = False
    serves_from_load = False
    persistent = True  # the store is on disk (a size and a restart are measured)

    def __init__(self):
        self.server: Server | None = None

    def lazy(self, regime: str) -> bool:
        """Whether the system reasons when first asked (the count), not at load."""
        return False

    def in_memory(self, regime: str) -> bool:
        """Whether `load` leaves the server running: it serves from memory, and has no
        store size or restart to measure."""
        return self.serves_from_load

    def images(self, ctx: Context) -> list[str]:
        return []

    def builds(self) -> list[tuple[str, str]]:
        """(image, build context under benches/) for images this repository builds."""
        return []

    def supports(self, ctx: Context, inputs: list[str], regime: str) -> str | None:
        if ctx.runtime.kind not in self.runtimes:
            return f"no {ctx.runtime.kind} path (runs: {', '.join(sorted(self.runtimes))})"
        return None

    def load(self, ctx: Context, store: str, inputs: list[str], regime: str) -> Step:
        raise NotImplementedError

    def serve(self, ctx: Context, store: str, regime: str) -> Endpoint:
        raise NotImplementedError

    def start(self, ctx: Context, spec: Spec, endpoint: Endpoint, timeout_s: float | None = None) -> Endpoint:
        """Starts a server and waits until its endpoint answers; sets restart_ms."""
        started = time.monotonic()
        self.server = ctx.runtime.start(spec, ctx.logs / "server.log")
        endpoint.server = self.server
        if not wait_ready(ctx, endpoint, timeout_s or max(ctx.timeout_s, 1800)):
            raise RuntimeError(f"{self.key}: the server didn't answer; see {ctx.logs / 'server.log'}")
        endpoint.restart_ms = (time.monotonic() - started) * 1000
        return endpoint

    def stop(self, ctx: Context) -> int | None:
        if self.server is None:
            return None
        peak = ctx.runtime.stop(self.server)
        self.server = None
        return peak

    def version(self, ctx: Context, endpoint: Endpoint | None) -> str:
        images = self.images(ctx)
        return image_tag(images[0]) if images else "-"


# --- NRESE ----------------------------------------------------------------------------------

class Nrese(Adapter):
    key = "nrese"
    regimes = {"none": "disabled", "rdfs": "rdfs", "owl-horst": "owl-horst",
               "owl2-rl": "owl2-rl", "owl2-ql": "owl2-ql"}
    runtimes = {"docker", "apptainer", "process"}

    def rust_image(self, ctx: Context) -> str:
        toolchain = (ctx.root / "rust-toolchain.toml").read_text()
        channel = re.search(r'channel = "(.*)"', toolchain).group(1)
        return ctx.setting("RUST_IMAGE", f"rust:{channel}-bookworm")

    def images(self, ctx):
        # Docker runs the build in the Rust image; elsewhere NRESE is a host process.
        docker = ctx.runtime is not None and ctx.runtime.kind == "docker"
        return [self.rust_image(ctx)] if docker else []

    def binary(self, ctx: Context) -> str:
        if ctx.setting("NRESE_BIN"):
            return ctx.setting("NRESE_BIN")
        target = Path(ctx.setting("CARGO_TARGET_DIR", str(ctx.root / "target")))
        return str(target / "release" / ("nrese-server.exe" if os.name == "nt" else "nrese-server"))

    def spec(self, ctx: Context, name: str, store: str, command: list[str], env: dict) -> Spec:
        env = {"NRESE_STORE_MODE": "on-disk", "NRESE_DATA_DIR": "/store", "RUST_LOG": "info", **env}
        if ctx.runtime.kind == "docker":
            return Spec(name, self.rust_image(ctx), ["/target/release/nrese-server", *command], env,
                        ctx.mounts(Mount("nrese-target", "/target"), Mount(store, "/store", readonly=False)),
                        memory=ctx.memory)
        # Apptainer and processes: the host's release build, on the host.
        return Spec(name, None, [self.binary(ctx), *command], env,
                    ctx.mounts(Mount(store, "/store", readonly=False)))

    def load(self, ctx, store, inputs, regime):
        spec = self.spec(ctx, ctx.name("load"), store, ["load", *inputs],
                         {"NRESE_REASONING_MODE": self.regimes[regime]})
        log = ctx.logs / "load.log"
        measured = ctx.runtime.run(spec, log, ctx.timeout_s)
        text = read_text(log)
        return Step(measured,
                    asserted=last_int(r"bulk load complete.*?inserted=(\d+)", text),
                    inferred=last_int(r"inferred stack rematerialised.*?inferred=(\d+)", text),
                    note="reasoning included in the load" if regime != "none" else "")

    def serve(self, ctx, store, regime):
        port = ctx.listen(8080)
        host = "0.0.0.0" if ctx.runtime.kind == "docker" else "127.0.0.1"
        env = {
            "NRESE_REASONING_MODE": self.regimes[regime],
            "NRESE_BIND_ADDR": f"{host}:{port}",
            "NRESE_QUERY_TIMEOUT_MS": str(ctx.timeout_s * 1000),
            # No rate limits and no result cache: repeated runs measure evaluation.
            "NRESE_READ_REQUESTS_PER_WINDOW": "100000000",
            "NRESE_QUERY_CACHE_BYTES": "0",
        }
        if ctx.setting("QUERY_MEMORY_MIB"):
            env["NRESE_MAX_QUERY_MEMORY_BYTES"] = str(int(ctx.setting("QUERY_MEMORY_MIB")) * 1048576)
        spec = self.spec(ctx, ctx.name("serve"), store, [], env)
        spec.port = port
        base = ctx.runtime.url(spec)
        return self.start(ctx, spec, Endpoint(f"{base}/dataset/query", f"{base}/dataset/update",
                                              ready=f"{base}/readyz"))

    def version(self, ctx, endpoint):
        if endpoint is None or ctx.dry:
            return "-"
        base = endpoint.query.rsplit("/dataset/", 1)[0]
        try:
            with urllib.request.urlopen(f"{base}/version", timeout=10) as response:
                return json.loads(response.read()).get("version", "-")
        except (OSError, ValueError):
            return "-"


# --- the other SPARQL stores ------------------------------------------------------------------

class Qlever(Adapter):
    key = "qlever"

    def images(self, ctx):
        return [ctx.setting("QLEVER_IMAGE", "adfreiburg/qlever:latest")]

    def load(self, ctx, store, inputs, regime):
        fmt = "nt" if all_ntriples(inputs) else "ttl"
        # One input stream: the Turtle files concatenated are one Turtle document.
        script = ('cat "$@" | /qlever/qlever-index -i /index/idx -f /dev/stdin -F ' + fmt +
                  ' -p true --stxxl-memory ' + ctx.setting("QLEVER_SORT_MEMORY", "8G"))
        spec = Spec(ctx.name("load"), self.images(ctx)[0], ["sh", "-c", script, "sh", *inputs],
                    mounts=ctx.mounts(Mount(store, "/index", readonly=False)), workdir="/index",
                    user="root", memory=ctx.memory)
        return Step(ctx.runtime.run(spec, ctx.logs / "load.log", ctx.timeout_s))

    def serve(self, ctx, store, regime):
        port = ctx.listen(7001)
        # The result cache is capped at 1 MB: repeated runs measure evaluation.
        spec = Spec(ctx.name("serve"), self.images(ctx)[0],
                    ["/qlever/qlever-server", "-i", "/index/idx", "-p", str(port), "-n", "-j", "8",
                     "-m", ctx.setting("QLEVER_MEMORY", "16G"), "-c", "1MB", "-e", "1MB",
                     "-s", f"{ctx.timeout_s}s"],
                    mounts=ctx.mounts(Mount(store, "/index", readonly=False)), port=port,
                    workdir="/index", user="root", memory=ctx.memory)
        base = ctx.runtime.url(spec) + "/"
        return self.start(ctx, spec, Endpoint(base, base))


class Oxigraph(Adapter):
    key = "oxigraph"

    def images(self, ctx):
        return [ctx.setting("OXIGRAPH_IMAGE", "ghcr.io/oxigraph/oxigraph:0.5.11")]

    def load(self, ctx, store, inputs, regime, log_name="load.log"):
        files = [a for i in inputs for a in ("--file", i)]
        spec = Spec(ctx.name("load"), self.images(ctx)[0], ["load", "--location", "/store", *files],
                    mounts=ctx.mounts(Mount(store, "/store", readonly=False)), entrypoint=True,
                    memory=ctx.memory)
        return Step(ctx.runtime.run(spec, ctx.logs / log_name, ctx.timeout_s))

    def serve(self, ctx, store, regime):
        port = ctx.listen(7878)
        spec = Spec(ctx.name("serve"), self.images(ctx)[0],
                    ["serve", "--location", "/store", "--bind", f"0.0.0.0:{port}"],
                    mounts=ctx.mounts(Mount(store, "/store", readonly=False)), port=port,
                    entrypoint=True, memory=ctx.memory)
        base = ctx.runtime.url(spec)
        return self.start(ctx, spec, Endpoint(f"{base}/query", f"{base}/update"))


# Jena's rule reasoners by regime (the reasoning kit's default for OWL is OWL Micro; set
# JENA_OWL_REASONER=OWLMiniFBRuleReasoner or OWLFBRuleReasoner for more of OWL).
JENA_REASONERS = {"rdfs": "RDFSExptRuleReasoner", "owl-horst": None}


class Jena(Adapter):
    key = "jena"
    regimes = {"none": "", "rdfs": "rdfs", "owl-horst": "owl"}

    def images(self, ctx):
        return []

    def builds(self):
        return [("nrese-bench/jena:6.2.0", "competitors/jena")]

    def image(self, ctx):
        return ctx.setting("JENA_IMAGE", "nrese-bench/jena:6.2.0")

    def in_memory(self, regime):
        return regime != "none"

    def lazy(self, regime):
        return regime != "none"

    def load(self, ctx, store, inputs, regime):
        env = {"JVM_ARGS": f"-Xmx{ctx.heap}"}
        if regime == "none":
            spec = Spec(ctx.name("load"), self.image(ctx),
                        ["tdb2.tdbloader", "--loader=parallel", "--loc", "/store/db", *inputs], env,
                        ctx.mounts(Mount(store, "/store", readonly=False)), memory=ctx.memory)
            return Step(ctx.runtime.run(spec, ctx.logs / "load.log", ctx.timeout_s))
        # A rule reasoner over an in-memory model: Fuseki reads the data at start-up, the
        # rules run when the model is first asked (the count).
        reasoner = JENA_REASONERS[regime] or ctx.setting("JENA_OWL_REASONER", "OWLMicroFBRuleReasoner")
        content = " , ".join(f"[ ja:externalContent <file://{i}> ]" for i in inputs)
        (ctx.work / "fuseki.ttl").write_text(f"""\
@prefix fuseki: <http://jena.apache.org/fuseki#> .
@prefix ja: <http://jena.hpl.hp.com/2005/11/Assembler#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
<#service> a fuseki:Service ; fuseki:name "ds" ;
  fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "query" ] ,
                  [ fuseki:operation fuseki:update ; fuseki:name "update" ] ;
  fuseki:dataset <#dataset> .
<#dataset> a ja:RDFDataset ; ja:defaultGraph <#inferred> .
<#inferred> a ja:InfModel ;
  ja:reasoner [ ja:reasonerURL <http://jena.hpl.hp.com/2003/{reasoner}> ] ;
  ja:baseModel <#asserted> .
<#asserted> a ja:MemoryModel ; ja:content {content} .
""", encoding="utf-8")
        self.endpoint = self.serve_config(ctx, env)
        return Step(Measured(self.endpoint.restart_ms or 0.0, None, 0),
                    note=f"in memory, {reasoner}; the rules run at the first query (count)")

    def serve_config(self, ctx, env):
        port = ctx.listen(3030)
        spec = Spec(ctx.name("serve"), self.image(ctx),
                    ["fuseki-server", "--config=/work/fuseki.ttl", f"--port={port}"], env,
                    ctx.mounts(), port=port, workdir="/work", memory=ctx.memory)
        base = ctx.runtime.url(spec)
        return self.start(ctx, spec, Endpoint(f"{base}/ds/query", f"{base}/ds/update"))

    def serve(self, ctx, store, regime):
        if regime != "none":
            return self.endpoint
        port = ctx.listen(3030)
        spec = Spec(ctx.name("serve"), self.image(ctx),
                    ["fuseki-server", "--update", "--tdb2", "--loc=/store/db", f"--port={port}", "/ds"],
                    {"JVM_ARGS": f"-Xmx{ctx.heap}"}, ctx.mounts(Mount(store, "/store", readonly=False)),
                    port=port, memory=ctx.memory)
        base = ctx.runtime.url(spec)
        return self.start(ctx, spec, Endpoint(f"{base}/ds/query", f"{base}/ds/update"))

    def version(self, ctx, endpoint):
        return image_tag(self.image(ctx))


VIRTUOSO_ENV = {
    "DBA_PASSWORD": "bench",
    "VIRT_PARAMETERS_DIRSALLOWED": "., /data, /rggs, /work, ../vad, /usr/share/proj",
    "VIRT_PARAMETERS_NUMBEROFBUFFERS": "1360000",
    "VIRT_PARAMETERS_MAXDIRTYBUFFERS": "1000000",
    # Complete results: no row cap, no cost-based rejection.
    "VIRT_SPARQL_RESULTSETMAXROWS": "1000000000",
    "VIRT_SPARQL_MAXQUERYEXECUTIONTIME": "0",
    "VIRT_SPARQL_MAXQUERYCOSTESTIMATIONTIME": "0",
}


class Virtuoso(Adapter):
    """Loads through its running server: ld_dir, six loaders, checkpoint (start-up excluded)."""

    key = "virtuoso"

    def images(self, ctx):
        return [ctx.setting("VIRTUOSO_IMAGE", "openlink/virtuoso-opensource-7:7.2.17")]

    def spec(self, ctx, name, store, port=None):
        return Spec(name, self.images(ctx)[0], [], dict(VIRTUOSO_ENV),
                    ctx.mounts(Mount(store, "/database", readonly=False)), port=port, entrypoint=True,
                    memory=ctx.memory)

    def isql(self, ctx, command: str) -> tuple[int, str]:
        return ctx.runtime.exec(self.server, ["isql", "1111", "dba", "bench", f"exec={command}"], ctx.timeout_s)

    def load(self, ctx, store, inputs, regime):
        self.server = ctx.runtime.start(self.spec(ctx, ctx.name("load"), store), ctx.logs / "load-server.log")
        deadline = time.monotonic() + 600
        while not ctx.dry and "Server online at 1111" not in server_log(ctx, self.server):
            if time.monotonic() > deadline or not ctx.runtime.alive(self.server):
                self.stop(ctx)
                return Step(Measured(0.0, None, 1), note="the server didn't start")
            time.sleep(0.3)
        started = time.monotonic()
        output = []
        for path in inputs:
            directory, file = path.rsplit("/", 1)
            output.append(self.isql(ctx, f"ld_dir('{directory}', '{file}', 'http://bench');")[1])
        with ThreadPoolExecutor(6) as pool:
            output += [r[1] for r in pool.map(lambda _: self.isql(ctx, "rdf_loader_run();"), range(6))]
        rc, text = self.isql(ctx, "checkpoint;")
        output.append(text)
        ms = (time.monotonic() - started) * 1000
        (ctx.logs / "load.log").write_text("\n".join(output), encoding="utf-8")
        peak = self.stop(ctx)
        return Step(Measured(ms, peak, rc), note="server start-up excluded")

    def serve(self, ctx, store, regime):
        port = ctx.listen(8890, fixed=True)
        spec = self.spec(ctx, ctx.name("serve"), store, port)
        base = ctx.runtime.url(spec)
        endpoint = self.start(ctx, spec, Endpoint(f"{base}/sparql", f"{base}/sparql", write_graph="http://bench"))
        self.isql(ctx, "GRANT SPARQL_UPDATE TO \"SPARQL\"; DB.DBA.RDF_DEFAULT_USER_PERMS_SET('nobody', 7);")
        return endpoint


def server_log(ctx: Context, server: Server) -> str:
    runtime = getattr(ctx.runtime, "inner", ctx.runtime)
    if isinstance(runtime, DockerRuntime):
        return runtime.logs(server)
    return read_text(server.log) if server.log else ""


GRAPHDB_RULESETS = {"none": "empty", "rdfs": "rdfs", "owl-horst": "owl-horst",
                    "owl2-rl": "owl2-rl", "owl2-ql": "owl2-ql"}


class Graphdb(Adapter):
    """importrdf into a fresh home (preload without reasoning; load with a ruleset, which
    infers while loading: UNVERIFIED until a licensed run). Queries need GRAPHDB_LICENSE."""

    key = "graphdb"
    regimes = GRAPHDB_RULESETS

    def images(self, ctx):
        return [ctx.setting("GRAPHDB_IMAGE", "ontotext/graphdb:11.5.1")]

    def supports(self, ctx, inputs, regime):
        if not ctx.setting("GRAPHDB_LICENSE"):
            return "needs GRAPHDB_LICENSE=/path/graphdb.license (it answers no queries without one)"
        return super().supports(ctx, inputs, regime)

    def env(self, ctx, options: str) -> dict:
        return {"GDB_HEAP_SIZE": ctx.heap, "GDB_JAVA_OPTS": options}

    def load(self, ctx, store, inputs, regime):
        template = (ctx.root / "benches/competitors/graphdb/repo-owl2-rl.ttl").read_text(encoding="utf-8")
        config = template.replace('graphdb:ruleset "owl2-rl"', f'graphdb:ruleset "{self.regimes[regime]}"')
        (ctx.work / "repo.ttl").write_text(config, encoding="utf-8")
        mode = ["preload", "-f"] if regime == "none" else ["load", "-f", "-m", "parallel"]
        spec = Spec(ctx.name("load"), self.images(ctx)[0],
                    ["/opt/graphdb/dist/bin/importrdf", *mode, "-c", "/work/repo.ttl", *inputs],
                    self.env(ctx, "-Dgraphdb.home=/opt/graphdb/home"),
                    ctx.mounts(Mount(store, "/opt/graphdb/home", readonly=False)), memory=ctx.memory)
        return Step(ctx.runtime.run(spec, ctx.logs / "load.log", ctx.timeout_s),
                    note="" if regime == "none" else "UNVERIFIED: importrdf load with a ruleset")

    def serve(self, ctx, store, regime):
        port = ctx.listen(7200, fixed=True)
        licence = Mount(ctx.setting("GRAPHDB_LICENSE"), "/license/graphdb.license")
        spec = Spec(ctx.name("serve"), self.images(ctx)[0], [],
                    self.env(ctx, "-Dgraphdb.license.file=/license/graphdb.license"),
                    ctx.mounts(licence, Mount(store, "/opt/graphdb/home", readonly=False)), port=port,
                    entrypoint=True, memory=ctx.memory)
        base = ctx.runtime.url(spec)
        return self.start(ctx, spec, Endpoint(f"{base}/repositories/bench",
                                              f"{base}/repositories/bench/statements"))


class Rdfox(Adapter):
    """UNVERIFIED until a licensed run: an in-memory sandbox that imports the inputs (and,
    for OWL 2 RL, their OWL axioms as rules: importaxioms), then serves its endpoint."""

    key = "rdfox"
    regimes = {"none": "", "owl2-rl": "importaxioms"}
    serves_from_load = True
    persistent = False

    def images(self, ctx):
        return [ctx.setting("RDFOX_IMAGE", "oxfordsemantic/rdfox:7.6b")]

    def supports(self, ctx, inputs, regime):
        if not ctx.setting("RDFOX_LICENSE"):
            return "needs RDFOX_LICENSE=/path/RDFox.lic"
        return super().supports(ctx, inputs, regime)

    def load(self, ctx, store, inputs, regime):
        port = ctx.listen(12110)
        script = ["dstore create bench", "active bench", *(f"import {i}" for i in inputs)]
        if regime == "owl2-rl":
            script.append("importaxioms")
        script += [f"set endpoint.port {port}", "endpoint start"]
        (ctx.work / "load.rdfox").write_text("\n".join(script) + "\n", encoding="utf-8")
        spec = Spec(ctx.name("serve"), self.images(ctx)[0],
                    ["-license-file", "/license/RDFox.lic", "sandbox", "/work", "exec /work/load.rdfox"],
                    mounts=ctx.mounts(Mount(ctx.setting("RDFOX_LICENSE"), "/license/RDFox.lic")),
                    port=port, entrypoint=True, keep_stdin=True, memory=ctx.memory)
        base = ctx.runtime.url(spec) + "/datastores/bench/sparql"
        self.endpoint = self.start(ctx, spec, Endpoint(base, base), timeout_s=ctx.timeout_s)
        return Step(Measured(self.endpoint.restart_ms or 0.0, None, 0),
                    note="in memory; start-up included; UNVERIFIED")

    def serve(self, ctx, store, regime):
        return self.endpoint


class Anzograph(Adapter):
    """In memory (at most 8 GB unregistered); loads with SPARQL LOAD into the running
    server, start-up excluded. Docker only."""

    key = "anzograph"
    serves_from_load = True
    persistent = False
    runtimes = {"docker"}

    def images(self, ctx):
        return [ctx.setting("ANZOGRAPH_IMAGE", "cambridgesemantics/anzograph:3.5.0")]

    def load(self, ctx, store, inputs, regime):
        port = ctx.listen(7070, fixed=True)
        # Its own files live in /data, so the datasets are mounted at /bench.
        mounts = [Mount(ctx.data.source, "/bench"), *ctx.extra]
        spec = Spec(ctx.name("serve"), self.images(ctx)[0], [], mounts=mounts, port=port, entrypoint=True)
        base = ctx.runtime.url(spec) + "/sparql"
        self.endpoint = self.start(ctx, spec, Endpoint(base, base), timeout_s=900)
        started, rc, output = time.monotonic(), 0, []
        for path in inputs:
            path = "/bench/" + path[len("/data/"):] if path.startswith("/data/") else path
            if ctx.dry:
                ctx.runtime.echo(f"    [http] LOAD WITH 'global' <file:{path}>")
                continue
            status, body = post(base, {"update": f"LOAD WITH 'global' <file:{path}>"}, timeout=ctx.timeout_s)
            output.append(f"{path}: HTTP {status} {body[:500]}")
            rc = rc or (0 if status == 200 else 1)
        ms = (time.monotonic() - started) * 1000
        (ctx.logs / "load.log").write_text("\n".join(output), encoding="utf-8")
        peak = self.server.sampler.peak if self.server and self.server.sampler else None
        return Step(Measured(ms, peak, rc), note="server start-up excluded; in memory")

    def serve(self, ctx, store, regime):
        return self.endpoint


# --- reasoners without SPARQL: their closure answered by Oxigraph -----------------------------

class ClosureAdapter(Adapter):
    answers_only = True
    persistent = False  # the store and its restart are Oxigraph's

    def serve(self, ctx, store, regime):
        """Oxigraph over the inputs and the closure (its load isn't measured). Statements
        with a literal subject (generalised RDF, which the reference closure contains) are
        left out: no store holds them, and no query can ask for them."""
        closure = ctx.work / "closure.nt"
        if not ctx.dry and closure.exists():
            with open(closure, encoding="utf-8", errors="replace") as source, \
                    open(ctx.work / "closure.rdf.nt", "w", encoding="utf-8") as target:
                target.writelines(line for line in source if not line.startswith('"'))
        oxigraph = Oxigraph()
        step = oxigraph.load(ctx, store, [*self.inputs, "/work/closure.rdf.nt"], "none", "answer-store.log")
        if not step.measured.ok:
            raise RuntimeError(f"{self.key}: Oxigraph couldn't load the closure")
        endpoint = oxigraph.serve(ctx, store, "none")
        self.server = oxigraph.server
        endpoint.update = None
        return endpoint

    def images(self, ctx):
        return Oxigraph().images(ctx)

    def closure_size(self, ctx) -> int | None:
        path = ctx.work / "closure.nt"
        if ctx.dry or not path.exists():
            return None
        with open(path, "rb") as f:
            return sum(1 for _ in f)


class Nemo(ClosureAdapter):
    key = "nemo"
    regimes = {"owl2-rl": "benches/reasoning/nemo/owl2rl.rls"}

    def builds(self):
        return [("nrese-bench/nemo", "reasoning/nemo")]

    def supports(self, ctx, inputs, regime):
        if not all_ntriples(inputs):
            return "its rules read one N-Triples file; the inputs aren't N-Triples"
        return super().supports(ctx, inputs, regime)

    def load(self, ctx, store, inputs, regime):
        self.inputs = inputs
        script = ('mkdir -p /tmp/in /tmp/nemo && cat "$@" > /tmp/in/input.nt && '
                  'nmo -I /tmp/in -D /tmp/nemo -o --report short /nemo/owl2rl.rls && '
                  'cp /tmp/nemo/inferred.nt /work/closure.nt')
        spec = Spec(ctx.name("load"), "nrese-bench/nemo", ["sh", "-c", script, "sh", *inputs],
                    mounts=ctx.mounts(), memory=ctx.memory)
        log = ctx.logs / "load.log"
        measured = ctx.runtime.run(spec, log, ctx.timeout_s)
        reasoning = last_int(r"Reasoning: +(\d+)ms", read_text(log))
        return Step(measured, inferred=self.closure_size(ctx), reason_ms=reasoning,
                    note="load time includes concatenating the inputs")


class Owlrl(ClosureAdapter):
    """The reference OWL 2 RL closure (pure Python): small tiers only."""

    key = "owlrl"
    regimes = {"rdfs": "rdfs", "owl2-rl": "owl2-rl"}

    def builds(self):
        return [("nrese-bench/owlrl-oracle", "reasoning/oracle")]

    def supports(self, ctx, inputs, regime):
        if not ctx.tier_small:
            return "the reference closure runs on the small tiers only"
        if not all_ntriples(inputs):
            return "reads N-Triples only"
        return super().supports(ctx, inputs, regime)

    def load(self, ctx, store, inputs, regime):
        self.inputs = inputs
        spec = Spec(ctx.name("load"), "nrese-bench/owlrl-oracle",
                    ["python", "/oracle/owlrl_materialise.py", "--profile", self.regimes[regime],
                     "--out", "/work/closure.nt", *inputs], mounts=ctx.mounts(), memory=ctx.memory)
        log = ctx.logs / "load.log"
        measured = ctx.runtime.run(spec, log, ctx.timeout_s)
        seconds = re.findall(r"inferred triples in ([0-9.]+) s", read_text(log))
        return Step(measured, asserted=last_int(r"asserted: (\d+)", read_text(log)),
                    inferred=self.closure_size(ctx),
                    reason_ms=float(seconds[-1]) * 1000 if seconds else None)


ADAPTERS = {a.key: a for a in (Nrese, Qlever, Oxigraph, Jena, Virtuoso, Graphdb, Rdfox, Anzograph, Nemo, Owlrl)}


def adapter(key: str) -> Adapter | None:
    cls = ADAPTERS.get(key)
    return cls() if cls else None


def warn(text: str):
    print(text, file=sys.stderr)
