"""benches/hosts.toml: what the bench drivers read about a host (inputs, volumes, caches), and,
from a private overlay, the hosts themselves and the verified inputs on those that hold them.

    load()               benches/hosts.toml, with the overlay's hosts.toml merged over it when
                         NRESE_BENCH_PRIVATE names an overlay folder, or a fleet job's
                         NRESE_BENCH_HOSTS holds its text
    current()            the host this process runs on: (key, entry), or None
    input_path(kind)     an input's path here: its environment variable, else hosts.toml
    lacking(needs)       what of `needs` this registered host doesn't have (an unregistered
                         host isn't described, so nothing is refused there)
    volume(kind)         a Docker volume the drivers use: the datasets, the fast suite's data
    image(name)          a benchmark image by its original tag: the tag, or its verified
                         preserved alias on a host whose Docker has only that
    configuration(key)   a host's configuration in force at a time (hosts.toml `config`)
    run_identity(...)    a run's host, configuration and measured governor, for compare
    private_dir()        the overlay's folder, or None
    overlay_identity()   the digest of the overlay this process reads, or None
    overlay_job(job, o)  a fleet job that reads the overlay `o` (overlay_env()) on its host
    floor(host)          a host's A-A' noise floor: the job's NRESE_BENCH_FLOORS, else the folder's
    fleet(name)          a list of host keys the overlay's [fleet] table gives OWL-RS's scripts

    python benches/suite/suitekit/hosts.py fleet NAME   that list, space-separated (shell scripts)
"""
from __future__ import annotations

import datetime
import hashlib
import json
import os
import re
import socket
import subprocess
import tomllib
from collections.abc import Iterable
from contextvars import ContextVar
from dataclasses import dataclass
from functools import wraps
from pathlib import Path

HOSTS = Path(__file__).resolve().parents[2] / "hosts.toml"
ROOT = HOSTS.parents[1]
# The private overlay's folder: its hosts.toml adds the hosts, [assets] and the inputs' paths.
# Unset, no host is registered, as for an unregistered host.
PRIVATE = "NRESE_BENCH_PRIVATE"
# A fleet job carries this envelope because fleet stages only the job's checkout.
HOSTS_TEXT, FLOORS_TEXT, OVERLAY_REV = "NRESE_BENCH_HOSTS", "NRESE_BENCH_FLOORS", "NRESE_BENCH_OVERLAY_REV"
CHANNELS = (HOSTS_TEXT, FLOORS_TEXT, OVERLAY_REV)
UNBOUND = object()
_run_overlay = ContextVar("run_overlay", default=UNBOUND)


def git(where: Path, *args: str) -> str:
    try:
        done = subprocess.run(["git", "-C", str(where), *args], capture_output=True, text=True,
                              encoding="utf-8", check=True, timeout=60)
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f"can't read the private bench overlay ({args[0]})") from error
    return done.stdout


def content_id(hosts: str, floors: str) -> str:
    payload = json.dumps([hosts, floors], ensure_ascii=False).encode("utf-8")
    return "sha256:" + hashlib.sha256(payload).hexdigest()


def valid_identity(identity: object) -> bool:
    return identity is None or (isinstance(identity, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", identity) is not None)


@dataclass(frozen=True)
class Overlay:
    folder: Path | None
    hosts_text: str
    floors_text: str
    registry: dict
    floors: dict

    @property
    def identity(self) -> str:
        return content_id(self.hosts_text, self.floors_text)

    def environment(self) -> dict[str, str]:
        return dict(zip(CHANNELS, (self.hosts_text, self.floors_text, self.identity)))

    def floor(self, host: str) -> tuple[str, dict] | None:
        return (f"{host}.toml", self.floors[host]) if host in self.floors else None


def compact(text: str) -> str:
    """Drop blank and comment lines when they are insignificant, keeping jobs small."""
    kept = "\n".join(line for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#"))
    kept += "\n" if kept else ""
    return kept if tomllib.loads(kept) == tomllib.loads(text) else text


def floor_tables(files: Iterable[tuple[str, str]]) -> str:
    """The floor files carried together as TOML, with a table per host."""
    lines, want = [], {}
    for name, text in files:
        host = name.removesuffix(".toml")
        want[host], key = tomllib.loads(text), json.dumps(host, ensure_ascii=False)
        lines.append(f"[{key}]")
        for line in compact(text).splitlines():
            bare = line.strip()
            if bare.startswith("["):
                brackets = "[[" if bare.startswith("[[") else "["
                line = f"{brackets}{key}.{bare[len(brackets):]}"
            lines.append(line)
    out = "\n".join(lines) + "\n" if lines else ""
    if tomllib.loads(out) != want:
        raise ValueError("the private bench overlay's floors change when read as one TOML document")
    return out


def git_overlay(repo: Path) -> tuple[str, str]:
    """Read one pinned Git snapshot. Only a missing tree entry means no floors."""
    rev = git(repo, "rev-parse", "--verify", "HEAD^{commit}").strip()
    text = git(repo, "show", f"{rev}:overlay/hosts.toml")
    directory = "overlay/noise-floor"
    entry = git(repo, "ls-tree", "--full-tree", rev, "--", directory)
    if not entry:
        return text, ""
    if entry.split()[1] != "tree":
        raise ValueError("the private bench overlay's noise-floor path is not a directory")
    at = f"{rev}:{directory}"
    names = git(repo, "ls-tree", "--full-tree", "--name-only", "-z", at).split("\0")
    return text, floor_tables((n, git(repo, "show", f"{at}/{n}")) for n in sorted(names) if n.endswith(".toml"))


def resolve_overlay(environ: dict | None = None) -> Overlay | None:
    """Resolve an envelope, an explicit live folder, or the inputs repository's HEAD."""
    if environ is None and (bound := _run_overlay.get()) is not UNBOUND:
        return bound[0]
    env = os.environ if environ is None else environ
    explicit, repo = env.get(PRIVATE), env.get("BENCH_INPUTS_GIT")
    folder = None
    if any(key in env for key in CHANNELS):
        if explicit or repo or not all(key in env for key in CHANNELS):
            raise ValueError("the private bench overlay needs one complete source")
        text, floors, identity = (env[key] for key in CHANNELS)
        if identity != content_id(text, floors):
            raise ValueError("the private bench overlay identity doesn't match its contents")
    elif explicit:
        folder = Path(explicit).expanduser()
        path = folder / "hosts.toml"
        if not path.is_file():
            raise FileNotFoundError(f"{PRIVATE} names {explicit}, which has no hosts.toml")
        text = compact(path.read_text(encoding="utf-8"))
        directory = folder / "noise-floor"
        try:
            files = sorted(directory.iterdir())
        except FileNotFoundError:
            files = []
        floors = floor_tables((p.name, p.read_text(encoding="utf-8")) for p in files if p.suffix == ".toml")
    elif repo:
        checkout = Path(repo).expanduser()
        folder = checkout / "overlay"
        text, floors = git_overlay(checkout)
        text = compact(text)
    else:
        return None
    registry, readings = tomllib.loads(text), tomllib.loads(floors)
    if any(not isinstance(table, dict) for table in registry.values()) or any(
            not isinstance(table, dict) for table in readings.values()):
        raise ValueError("the private bench overlay must contain TOML tables")
    return Overlay(folder, text, floors, registry, readings)


def run_snapshot(run):
    """Every host lookup in a run, including its manifest, sees one overlay."""
    @wraps(run)
    def wrapped(*args, **kwargs):
        if _run_overlay.get() is not UNBOUND:
            return run(*args, **kwargs)
        overlay = resolve_overlay()
        registry = public_registry()
        if overlay:
            registry = merged(registry, overlay.registry)
        token = _run_overlay.set((overlay, datetime.datetime.now().isoformat(timespec="seconds"), registry))
        try:
            return run(*args, **kwargs)
        finally:
            _run_overlay.reset(token)
    return wrapped


def launch(start, command, *, environ=None, **kwargs):
    """Start a child with a complete environment and the active run's frozen overlay."""
    environment = dict(os.environ if environ is None else environ)
    if (bound := _run_overlay.get()) is not UNBOUND:
        for key in (*CHANNELS, PRIVATE, "BENCH_INPUTS_GIT"):
            environment.pop(key, None)
        if overlay := bound[0]:
            environment.update(overlay.environment())
    return start(command, env=environment, **kwargs)


def private_run(machine: dict) -> bool:
    return machine.get("overlay") is not None or bool(machine.get("fleet_host"))


def private_session() -> bool:
    return resolve_overlay() is not None or bool(os.environ.get("FLEET_HOST"))


def private_home() -> Path:
    return Path.home() / "owlrs" / "private"


def evidence_path(path: str | Path, private: bool | None = None, checkout: str | Path = ROOT) -> Path:
    """Resolve a private output file at the write, using saved privacy when supplied."""
    path = Path(path)
    if private is None:
        private = private_session()
    return private_output(path, checkout) if private else path


def private_dir(environ: dict | None = None) -> Path | None:
    overlay = resolve_overlay(environ)
    return overlay.folder if overlay else None


def overlay_identity(environ: dict | None = None) -> str | None:
    overlay = resolve_overlay(environ)
    return overlay.identity if overlay else None


def overlay_env(environ: dict | None = None) -> dict[str, str]:
    try:
        overlay = resolve_overlay(environ)
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from error
    if overlay is None:
        raise SystemExit(f"no private bench overlay: set {PRIVATE} or BENCH_INPUTS_GIT")
    return overlay.environment()


def floor(host: str, environ: dict | None = None) -> tuple[str, dict] | None:
    overlay = resolve_overlay(environ)
    if overlay is not None:
        return overlay.floor(host)
    return public_floor(host)


def public_floor(host: str) -> tuple[str, dict] | None:
    path = ROOT / "benches" / "fast" / "noise-floor" / f"{host}.toml"
    return (path.name, tomllib.loads(path.read_text(encoding="utf-8"))) if path.is_file() else None


def private_output(path: Path, checkout: str | Path = ROOT) -> Path:
    """Protect the writer's checkout and its selected checkout, including linked paths."""
    resolved = path.resolve()
    absolute = Path(os.path.abspath(path))
    if any(resolved.is_relative_to(root) or absolute.is_relative_to(root)
           for root in (ROOT.resolve(), Path(checkout).resolve())) or any(
           (parent / "benches" / "suite" / "suitekit" / "hosts.py").is_file()
           for candidate in (resolved, absolute) for parent in (candidate, *candidate.parents)):
        raise ValueError(f"private benchmark output belongs outside the public checkout: {path}")
    return resolved


def jobs_dir() -> Path:
    folder = private_dir()
    return folder / "jobs" if folder else Path.home() / "owlrs" / "private" / "bench-jobs"


def write_job(path: Path, text: str, checkout: str | Path, jobs: Iterable[dict]) -> Path:
    for job in jobs:
        if sum("git" in item.get("source", {}) for item in job.get("inputs", [])) > 1:
            raise ValueError(f"{job['id']}: fleet stages only the checkout Git input")
    path = private_output(path, checkout)
    path.parent.mkdir(parents=True, exist_ok=True)
    evidence_path(path, True, checkout).write_text(text, encoding="utf-8", newline="\n")
    return path


def overlay_job(job: dict, overlay: dict[str, str]) -> dict:
    """`job` reading the private overlay on its host: `overlay` (overlay_env()) in its environment.
    Its inputs, and so its checkout's tree, are as they were."""
    return {**job, "env": {**job.get("env", {}), **overlay}}


def merged(public: dict, overlay: dict) -> dict:
    """`overlay` over `public`, table by table: a key in both takes the overlay's value, except
    that two tables merge the same way."""
    out = dict(public)
    for key, value in overlay.items():
        out[key] = merged(out[key], value) if isinstance(value, dict) and isinstance(out.get(key), dict) else value
    return out


def load(path: Path = HOSTS, environ: dict | None = None) -> dict:
    if path == HOSTS and environ is None and (bound := _run_overlay.get()) is not UNBOUND:
        return bound[2]
    registry = public_registry(path)
    overlay = resolve_overlay(environ)
    return merged(registry, overlay.registry) if overlay else registry


def public_registry(path: Path = HOSTS) -> dict:
    with open(path, "rb") as f:
        return tomllib.load(f)


def current(registry: dict | None = None, name: str | None = None) -> tuple[str, dict] | None:
    """The entry whose `hostname` (or key, or ssh alias) is this machine's name, case aside."""
    registry = load() if registry is None else registry
    name = (name or socket.gethostname()).split(".")[0].lower()
    for key, host in registry.get("host", {}).items():
        if name in {key.lower(), str(host.get("hostname", "")).lower(), str(host.get("ssh", "")).lower()}:
            return key, host
    return None


def input_path(kind: str, registry: dict | None = None, name: str | None = None,
               environ: dict | None = None) -> Path | None:
    """Where input `kind` is on this host: its variable when set, else the assets' copy on a
    host that holds them, else None."""
    registry = load(environ=environ) if registry is None else registry
    environ = os.environ if environ is None else environ
    entry = registry.get("inputs", {}).get(kind)
    if entry is None:
        raise KeyError(f"no input {kind!r} in hosts.toml")
    if entry.get("env") and environ.get(entry["env"]):
        return Path(environ[entry["env"]])
    found = current(registry, name)
    if found and found[1].get("assets") and entry.get("path"):
        return Path(registry["assets"]["root"]) / entry["path"]
    return None


def lacking(needs: list[str], registry: dict | None = None, name: str | None = None) -> list[str]:
    found = current(registry, name)
    if found is None:
        return []
    has = set(found[1].get("has", []))
    return [n for n in needs if n not in has]


def docker_image_id(name: str) -> str | None:
    import subprocess
    try:
        done = subprocess.run(["docker", "image", "inspect", "--format", "{{.Id}}", name],
                              capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout.strip() if done.returncode == 0 and done.stdout.strip() else None


def image(name: str, registry: dict | None = None, host: str | None = None, inspect=docker_image_id) -> str:
    """A benchmark image by its original tag: the tag when Docker has it, else the verified
    alias it was loaded under on a host that holds the assets (assets.images: the alias whose
    image ID is the verified one), else the tag itself (for the caller to build or fail on)."""
    if inspect(name) is not None:
        return name
    tag = name if ":" in name.rsplit("/", 1)[-1] else name + ":latest"
    registry = load() if registry is None else registry
    here = current(registry, host)
    manifest = registry.get("assets", {}).get("images")
    if not (here and here[1].get("assets") and manifest):
        return name
    import json
    try:
        entries = json.loads((Path(registry["assets"]["root"]) / manifest).read_text(encoding="utf-8"))["images"]
    except (OSError, ValueError, KeyError):
        return name
    for entry in entries:
        if tag in entry.get("original_tags", []) and inspect(entry["preserved_tag"]) == entry.get("loaded_id"):
            return entry["preserved_tag"]
    return name


def volume(kind: str, registry: dict | None = None, environ: dict | None = None) -> str:
    """A Docker volume the drivers use (hosts.toml [volumes]): its variable when set, else its name."""
    registry = load(environ=environ) if registry is None else registry
    environ = os.environ if environ is None else environ
    entry = registry.get("volumes", {}).get(kind)
    if entry is None:
        raise KeyError(f"no volume {kind!r} in hosts.toml")
    return environ.get(entry.get("env", ""), "") or entry["name"]


def configuration(key: str, at: str | None = None, registry: dict | None = None) -> dict | None:
    """Host `key`'s configuration in force at `at` (an ISO time, the host's local time; now
    if None): the newest `config` entry whose `since` isn't after it, or None if hosts.toml
    lists none for the host (or `at` is before the first)."""
    registry = load() if registry is None else registry
    if not at:
        bound = _run_overlay.get()
        at = datetime.datetime.now().isoformat(timespec="seconds") if bound is UNBOUND else bound[1]
    at = at[:19]
    entries = sorted(registry.get("host", {}).get(key, {}).get("config", []), key=lambda c: c["since"], reverse=True)
    return next((c for c in entries if c["since"][:19] <= at), None)


def run_identity(machine: dict, started: str | None, registry: dict | None = None) -> dict:
    """Where and under what a run was measured, from its report's `machine` and start time:
    the host's key (else its name), the configuration then in force (its `since`, "-" for a
    host without a recorded one), and the CPU governor the run itself read, if any."""
    overlay = {"overlay": machine["overlay"]} if "overlay" in machine else {}
    key, config = machine.get("host_key"), machine.get("host_config")
    if key is None or config is None:
        # Reports from before host configurations were saved use the registry's dated entries.
        registry = load() if registry is None else registry
        name = machine.get("host") or "?"
        found = current(registry, name)
        key = found[0] if found else name
        entry = configuration(key, started, registry) if found else None
        config = entry["since"] if entry else "-"
    return {"host": key, "config": config, "governor": machine.get("governor"), **overlay}


def fleet(name: str) -> list[str] | None:
    """A configured fleet list, or None if its source or key is absent."""
    overlay = resolve_overlay()
    table = overlay.registry.get("fleet", {}) if overlay else {}
    if name not in table:
        return None
    keys = table[name]
    if not isinstance(keys, list) or any(not isinstance(key, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+", key)
                                        for key in keys):
        raise ValueError(f"invalid fleet host list: {name}")
    return keys


if __name__ == "__main__":
    import sys
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "fleet":
            keys = fleet(sys.argv[2])
            if keys is None:
                sys.exit(f"no private fleet policy for {sys.argv[2]}")
            print(" ".join(keys))
        elif len(sys.argv) >= 3 and sys.argv[1] == "evidence":
            if private_session() or os.environ.get("GATE_HOST"):
                for name in sys.argv[2:]:
                    private_output(Path(name))
        else:
            sys.exit("usage: hosts.py fleet NAME | evidence PATH...")
    except (OSError, ValueError) as error:
        sys.exit(str(error))
