"""benches/hosts.toml: the benchmark hosts, their roles and what they have, and the verified
shared inputs on hosts that hold the preserved assets.

    current()            the host this process runs on: (key, entry), or None
    input_path(kind)     an input's path here: its environment variable, else hosts.toml
    lacking(needs)       what of `needs` this registered host doesn't have (an unregistered
                         host isn't described, so nothing is refused there)
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
