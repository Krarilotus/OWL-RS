"""benches/hosts.toml: the benchmark hosts, their roles and what they have, and the verified
shared inputs on hosts that hold the preserved assets.

    current()            the host this process runs on: (key, entry), or None
    input_path(kind)     an input's path here: its environment variable, else hosts.toml
    lacking(needs)       what of `needs` this registered host doesn't have (an unregistered
                         host isn't described, so nothing is refused there)
    volume(kind)         a Docker volume the drivers use: the datasets, the fast suite's data
    image(name)          a benchmark image by its original tag: the tag, or its verified
                         preserved alias on a host whose Docker has only that
    configuration(key)   a host's configuration in force at a time (hosts.toml `config`)
    run_identity(...)    a run's host, configuration and measured governor, for compare
"""
from __future__ import annotations

import os
import socket
import tomllib
from pathlib import Path

HOSTS = Path(__file__).resolve().parents[2] / "hosts.toml"


def load(path: Path = HOSTS) -> dict:
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
    registry = load() if registry is None else registry
    environ = os.environ if environ is None else environ
    entry = registry.get("inputs", {}).get(kind)
    if entry is None:
        raise KeyError(f"no input {kind!r} in hosts.toml")
    if entry.get("env") and environ.get(entry["env"]):
        return Path(environ[entry["env"]])
    found = current(registry, name)
    if found and found[1].get("assets"):
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
    registry = load() if registry is None else registry
    environ = os.environ if environ is None else environ
    entry = registry.get("volumes", {}).get(kind)
    if entry is None:
        raise KeyError(f"no volume {kind!r} in hosts.toml")
    return environ.get(entry.get("env", ""), "") or entry["name"]


def configuration(key: str, at: str | None = None, registry: dict | None = None) -> dict | None:
    """Host `key`'s configuration in force at `at` (an ISO time, the host's local time; now
    if None): the newest `config` entry whose `since` isn't after it, or None if hosts.toml
    lists none for the host (or `at` is before the first)."""
    import datetime
    registry = load() if registry is None else registry
    at = (at or datetime.datetime.now().isoformat(timespec="seconds"))[:19]
    entries = sorted(registry.get("host", {}).get(key, {}).get("config", []), key=lambda c: c["since"], reverse=True)
    return next((c for c in entries if c["since"][:19] <= at), None)


def run_identity(machine: dict, started: str, registry: dict | None = None) -> dict:
    """Where and under what a run was measured, from its report's `machine` and start time:
    the host's key (else its name), the configuration then in force (its `since`, "-" for a
    host without a recorded one), and the CPU governor the run itself read, if any."""
    registry = load() if registry is None else registry
    name = machine.get("host") or "?"
    found = current(registry, name)
    key = found[0] if found else name
    config = configuration(key, started, registry) if found else None
    return {"host": key, "config": config["since"] if config else "-", "governor": machine.get("governor")}
