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
    overlay_identity()   which overlay this process reads (its commit or tree), or None
    overlay_job(job, o)  a fleet job that reads the overlay `o` (overlay_env()) on its host
    floor(host)          a host's A-A' noise floor: the job's NRESE_BENCH_FLOORS, else the folder's
    fleet(name)          a list of host keys the overlay's [fleet] table gives OWL-RS's scripts

    python benches/suite/suitekit/hosts.py fleet NAME   that list, space-separated (shell scripts)
"""
from __future__ import annotations

import os
import socket
import subprocess
import tomllib
from pathlib import Path

HOSTS = Path(__file__).resolve().parents[2] / "hosts.toml"
ROOT = HOSTS.parents[1]
# The private overlay's folder: its hosts.toml adds the hosts, [assets] and the inputs' paths.
# Unset, no host is registered, as for an unregistered host.
PRIVATE = "NRESE_BENCH_PRIVATE"
# A fleet job's overlay, from the machine that writes the job (overlay_env()): the overlay's
# hosts.toml as text, and the commit of the private bench inputs repository it was taken from,
# for a run's identity. The fleet stages one git input, the job's checkout, so the overlay
# travels in the job's environment. Set together with NRESE_BENCH_PRIVATE, it is an error.
HOSTS_TEXT, OVERLAY_REV = "NRESE_BENCH_HOSTS", "NRESE_BENCH_OVERLAY_REV"
# The overlay's A-A' noise floors (noise-floor/HOST.toml) in a fleet job, from the same commit:
# one TOML text, a table per host (floors_text()). Set together with NRESE_BENCH_PRIVATE, an error.
FLOORS_TEXT = "NRESE_BENCH_FLOORS"


def git(where: Path, *args: str) -> str | None:
    try:
        done = subprocess.run(["git", "-C", str(where), *args], capture_output=True, text=True, encoding="utf-8",
                              errors="replace", timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def private_dir(environ: dict | None = None) -> Path | None:
    """The overlay folder NRESE_BENCH_PRIVATE names, None when unset."""
    environ = os.environ if environ is None else environ
    return Path(environ[PRIVATE]) if environ.get(PRIVATE) else None


def overlay_identity(environ: dict | None = None) -> str | None:
    """Which overlay this process reads, None without one: in a fleet job the commit its hosts.toml
    was taken from (`rev:SHA`); from a folder, the folder's git tree (`tree:SHA`, with "+changes"
    where the folder differs from its commit), "unknown" where git doesn't say."""
    environ = os.environ if environ is None else environ
    if environ.get(HOSTS_TEXT):
        return "rev:" + (environ.get(OVERLAY_REV) or "unknown")
    folder = private_dir(environ)
    if folder is None:
        return None
    tree = git(folder, "rev-parse", "HEAD:./")
    if tree is None:
        return "unknown"
    return "tree:" + tree + ("+changes" if git(folder, "status", "--porcelain", "--", ".") else "")


def overlay_env(environ: dict | None = None) -> dict[str, str]:
    """What a fleet job carries of the private overlay, made on the machine that writes the job:
    the overlay's hosts.toml as committed (its comment lines left out, its tables as they are),
    and that commit: of the checkout NRESE_BENCH_PRIVATE's folder is in, else of the private bench
    inputs repository (benches/hosts.toml [sources] bench-inputs: its variable). So a change to
    the overlay reaches a job once committed. Stops the writer where there is none, or its HEAD has
    no hosts.toml there: the job would run without its hosts. The hosts' A-A' noise floors of that
    commit come along (floors_text()), where it has any."""
    environ = os.environ if environ is None else environ
    source = load(environ=environ).get("sources", {}).get("bench-inputs", {})
    named = environ.get(source.get("env") or "BENCH_INPUTS_GIT") or source.get("git")
    folder = private_dir(environ)
    repo, at = (folder, "./") if folder else (Path(os.path.expanduser(named)) if named else None, "overlay/")
    found = repo is not None and repo.is_dir()
    rev = git(repo, "rev-parse", "--verify", "HEAD^{commit}") if found else None
    text = git(repo, "show", f"{rev}:{at}hosts.toml") if rev else None
    if not text:
        raise SystemExit(f"the private bench overlay isn't here ({repo or 'no repository named'}, or its HEAD has "
                         f"no overlay/hosts.toml): a fleet job would run without its hosts. Set {PRIVATE} to the "
                         f"overlay/ folder of a checkout of the private bench inputs repository, or "
                         f"{source.get('env') or 'BENCH_INPUTS_GIT'} to that repository")
    kept = "\n".join(line for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")) + "\n"
    if tomllib.loads(kept) != tomllib.loads(text):
        raise SystemExit(f"the overlay's hosts.toml at {rev[:12]} reads otherwise without its comment lines")
    floors = floors_text(repo, f"{rev}:{at}noise-floor")
    return {HOSTS_TEXT: kept, OVERLAY_REV: rev, **({FLOORS_TEXT: floors} if floors else {})}


def floors_text(repo: Path, at: str) -> str:
    """The floors' files in git tree `at` (noise-floor/, one HOST.toml each) as one TOML text, a
    table per host, their comment lines left out; "" where there are none. Checked to read as the
    files do."""
    lines, want = [], {}
    # --full-tree: from a subfolder (an overlay folder's checkout), ls-tree lists only its paths.
    for name in sorted((git(repo, "ls-tree", "--full-tree", "--name-only", at) or "").splitlines()):
        if not name.endswith(".toml"):
            continue
        host, text = name[:-len(".toml")], git(repo, "show", f"{at}/{name}") or ""
        want[host], key = tomllib.loads(text), '"' + host + '"'
        lines.append(f"[{key}]")
        for line in text.splitlines():
            bare = line.strip()
            if not bare or bare.startswith("#"):
                continue
            if bare.startswith("["):  # a table of the file: the host's
                brackets = "[[" if bare.startswith("[[") else "["
                line = f"{brackets}{key}.{bare[len(brackets):]}"
            lines.append(line)
    out = "\n".join(lines) + "\n" if lines else ""
    if tomllib.loads(out) != want:
        raise SystemExit(f"the overlay's noise floors at {at} read otherwise as one text")
    return out


def floor(host: str, environ: dict | None = None) -> tuple[str, dict] | None:
    """Host `host`'s A-A' noise floor (release_gate.py floor) as its file reads, with the file's
    name; None where none was measured. In a fleet job, from NRESE_BENCH_FLOORS (overlay_env());
    else the overlay folder's noise-floor/; else benches/fast/noise-floor/."""
    environ = os.environ if environ is None else environ
    text, folder = environ.get(FLOORS_TEXT), private_dir(environ)
    if text and folder:
        raise ValueError(f"both {PRIVATE} and {FLOORS_TEXT} are set: one overlay at a time")
    name = f"{host}.toml"
    if text:
        found = tomllib.loads(text).get(host)
        return (name, found) if found is not None else None
    path = (folder or ROOT / "benches" / "fast") / "noise-floor" / name
    return (name, tomllib.loads(path.read_text(encoding="utf-8"))) if path.is_file() else None


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
    environ = os.environ if environ is None else environ
    with open(path, "rb") as f:
        registry = tomllib.load(f)
    folder, text = private_dir(environ), environ.get(HOSTS_TEXT)
    if folder and text:
        raise ValueError(f"both {PRIVATE} and {HOSTS_TEXT} are set: one overlay at a time")
    if text:
        return merged(registry, tomllib.loads(text))
    if folder is None:
        return registry
    try:
        with open(folder / "hosts.toml", "rb") as f:
            return merged(registry, tomllib.load(f))
    except FileNotFoundError:
        raise FileNotFoundError(f"{PRIVATE} names {environ[PRIVATE]}, which has no hosts.toml") from None


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
    # The overlay its host's entry came from (manifest.machine()), where one was read.
    overlay = {"overlay": machine["overlay"]} if machine.get("overlay") else {}
    return {"host": key, "config": config["since"] if config else "-", "governor": machine.get("governor"), **overlay}


def fleet(name: str, registry: dict | None = None) -> list[str]:
    """The overlay's [fleet] list `name` (host keys), empty without one."""
    registry = load() if registry is None else registry
    return [str(h) for h in registry.get("fleet", {}).get(name, [])]


if __name__ == "__main__":
    import sys
    if len(sys.argv) != 3 or sys.argv[1] != "fleet":
        sys.exit("usage: hosts.py fleet NAME")
    print(" ".join(fleet(sys.argv[2])))
