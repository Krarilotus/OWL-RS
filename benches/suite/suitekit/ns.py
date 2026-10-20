"""The job namespace: the one owner of the names of what a benchmark job keeps in Docker or in a
shared directory (scripts/lib/ns.sh is its shell twin; scripts/check-names.sh fails on a name
built anywhere else).

On a shared host several jobs run at once or back to back, and fleet checks every job out into a
directory named `work`: names derived from the checkout's directory collided, so jobs timed each
other's builds and one job's cleanup removed another's volumes (office-pc, 19 Oct 2026). Here:

    namespace(root)      the job's namespace: FLEET_JOB (the fleet job's id) when set, else a
                         digest of the checkout's path; every name below carries it
    name(kind, root)     a container's or volume's name: nrese-<kind>-<namespace>-<checkout>,
                         the checkout's own digest telling apart two checkouts of one job (an
                         A-B-B-A job's base and head)
    labels(root, owner)  docker --label arguments: nrese.ns, nrese.checkout (the checkout's path:
                         the host's sweep leaves a resource whose checkout still exists) and
                         nrese.owner (the tool that made it: a tool's own cleanup)
    name_args(...)       `--name NAME` with the labels, for `docker run`
    volume(action, ...)  a `docker volume` command: create (labelled), rm, inspect
    directory(base, kind, root)   a job's own directory under a shared one (a kit's binaries,
                         scratch, build context)
    shared(kind)         a host-wide resource every job shares on purpose and none removes:
                         the datasets' and the fast suite's data volumes, the Cargo registry
    clean(root, owner, everything)   removes this namespace's containers, volumes and networks:
                         the checkout's own (or, `everything`, the whole job's), of one owner or all
    lock(path)           the host lock (lock.py: an OS lock the kernel releases when its holder
                         dies); lock_dir() where the named ones live
    pin_images(tags, root)   at the job's start, before its first timed step: every image its
                         measured containers will run, pulled if missing and resolved to its ID,
                         kept for the whole job (every later step reuses the ID, never resolves
                         again). A tag that can't be resolved stops the job (ImageError).
    image_id(tag, root)  the pinned ID to run; a tag the job didn't pin is an error, never run
                         by its tag. pinned(root): the pins, for the result
    docker(argv)         runs Docker (NRESE_DOCKER names another executable: the tests' stub)

    python ns.py pin ROOT TAG...    pin_images for a shell script (scripts/lib/ns.sh ns_pin_images)
"""
from __future__ import annotations

import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

try:
    from .lock import lock
except ImportError:  # run as a script, or imported beside it (runtime.py's scope entry)
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from lock import lock

LABEL_NS, LABEL_CHECKOUT, LABEL_OWNER = "nrese.ns", "nrese.checkout", "nrese.owner"
SHARED = {"cargo": ("NRESE_CARGO_VOLUME", "nrese-cargo")}


def _slug(text: str) -> str:
    return re.sub(r"[^a-z0-9_.-]+", "-", text.lower()).strip(".-") or "x"


def checkout_path(root: Path | str) -> str:
    """The checkout's path as every tool spells it (forward slashes; on Windows the drive form)."""
    return Path(root).resolve().as_posix()


def _digest(text: str, n: int) -> str:
    return hashlib.sha1(text.encode()).hexdigest()[:n]


def namespace(root: Path | str, environ: dict | None = None) -> str:
    env = os.environ if environ is None else environ
    job = env.get("FLEET_JOB", "")
    return _slug(job) if job else "c" + _digest(checkout_path(root), 10)


def name(kind: str, root: Path | str, environ: dict | None = None) -> str:
    return f"nrese-{_slug(kind)}-{namespace(root, environ)}-{_digest(checkout_path(root), 6)}"


def labels(root: Path | str, owner: str, environ: dict | None = None) -> list[str]:
    return ["--label", f"{LABEL_NS}={namespace(root, environ)}", "--label", f"{LABEL_CHECKOUT}={checkout_path(root)}",
            "--label", f"{LABEL_OWNER}={owner}"]


def name_args(container: str, root: Path | str, owner: str, environ: dict | None = None) -> list[str]:
    return ["--name", container, *labels(root, owner, environ)]


def volume(action: str, volume_name: str, root: Path | str | None = None, owner: str = "",
           environ: dict | None = None) -> list[str]:
    if action == "create":
        if root is None:
            raise ValueError("a volume is created with its labels: its checkout and owner")
        return ["volume", "create", *labels(root, owner, environ), volume_name]
    if action not in ("rm", "inspect"):
        raise ValueError(action)
    return ["volume", action, volume_name]


def directory(base: Path, kind: str, root: Path | str, environ: dict | None = None) -> Path:
    return Path(base) / _slug(kind) / name("dir", root, environ).removeprefix("nrese-dir-")


def shared(kind: str) -> str:
    variable, default = SHARED[kind]
    return os.environ.get(variable) or default


def executable() -> str:
    found = os.environ.get("NRESE_DOCKER") or "docker"
    return shutil.which(found) or found


def docker(argv: list[str], **kw) -> subprocess.CompletedProcess:
    kw.setdefault("capture_output", True)
    kw.setdefault("text", True)
    return subprocess.run([executable(), *argv], **kw)


def clean(root: Path | str, owner: str | None = None, everything: bool = False,
          environ: dict | None = None, run=docker) -> list[str]:
    """Removes this namespace's containers (running ones too: they are this job's) and volumes;
    returns what it removed. Never anything without this namespace's label."""
    filters = ["--filter", f"label={LABEL_NS}={namespace(root, environ)}"]
    if not everything:
        filters += ["--filter", f"label={LABEL_CHECKOUT}={checkout_path(root)}"]
    if owner:
        filters += ["--filter", f"label={LABEL_OWNER}={owner}"]
    removed = []
    for item in run(["ps", "-a", "-q", *filters]).stdout.split():
        if run(["rm", "-f", "-v", item]).returncode == 0:
            removed.append(item)
    for item in run(["volume", "ls", "-q", *filters]).stdout.split():
        if run(["volume", "rm", item]).returncode == 0:
            removed.append(item)
    for item in run(["network", "ls", "-q", *filters]).stdout.split():
        if run(["network", "rm", item]).returncode == 0:
            removed.append(item)
    return removed


def lock_dir() -> Path:
    return Path(os.environ.get("NRESE_LOCK_DIR") or Path.home() / ".nrese-locks")


def pins_file(root: Path | str, environ: dict | None = None) -> Path:
    return lock_dir() / "pins" / f"{namespace(root, environ)}.tsv"


def pinned(root: Path | str, environ: dict | None = None) -> dict[str, str]:
    path = pins_file(root, environ)
    lines = path.read_text(encoding="utf-8").splitlines() if path.is_file() else []
    return dict(line.split("\t", 1) for line in lines if "\t" in line)


class ImageError(RuntimeError):
    """An image the job can't run by ID: not resolvable at its start, or not pinned then."""


def pin_images(tags, root: Path | str, environ: dict | None = None, run=None) -> dict[str, str]:
    run = run or docker
    path = pins_file(root, environ)
    with lock(f"{path}.lock"):
        pins = pinned(root, environ)
        new = {}
        for tag in dict.fromkeys(t for t in tags if t and not t.startswith("sha256:")):
            if tag in pins:
                continue  # pinned at the job's start: the same ID for the whole job
            if run(["image", "inspect", tag]).returncode != 0 and run(["pull", "-q", tag]).returncode != 0:
                raise ImageError(f"{tag}: neither on this host nor pullable; the job stops before it times anything")
            image = run(["image", "inspect", "-f", "{{.Id}}", tag]).stdout.strip()
            if not image.startswith("sha256:"):
                raise ImageError(f"{tag}: no image ID ({image!r}); the job stops before it times anything")
            new[tag] = image
        if new:
            path.parent.mkdir(parents=True, exist_ok=True)
            with open(path, "a", encoding="utf-8", newline="\n") as out:
                out.writelines(f"{tag}\t{image}\n" for tag, image in new.items())
        return {**pins, **new}


def image_id(tag: str, root: Path | str, environ: dict | None = None) -> str:
    if tag.startswith("sha256:"):
        return tag
    found = pinned(root, environ).get(tag)
    if not found:
        raise ImageError(f"{tag}: not pinned at the job's start (ns.pin_images); never run by its tag")
    return found


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "pin":
        try:
            pin_images(sys.argv[3:], sys.argv[2])
        except ImageError as error:
            sys.exit(str(error))
    else:
        sys.exit("usage: ns.py pin ROOT TAG...")
