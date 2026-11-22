"""Publication and launch boundaries for the Python benchmark runners.

Scan the suite, fast, verify and the reasoning and breadth runners that share
their output or launch contracts. Standalone converters, probes, container
plugins, shell scripts and test fixtures have their own entry points.
"""
from __future__ import annotations

import ast
from pathlib import Path
import unittest

from boundary_exceptions import STARTS, WRITES

ROOT = Path(__file__).resolve().parents[3]
DIRECTORIES = ("benches/suite", "benches/fast", "scripts/verify", "benches/reasoning/datalog",
               "benches/reasoning/maintenance", "benches/reasoning/spb")
RUNNERS = ("benches/reasoning/dl/nrese.py", "benches/reasoning/dl/reference.py",
           "benches/breadth/kit.py", "benches/breadth/era-shacl/run.py", "benches/breadth/vectors/ann.py")


def argument(call, index, key):
    return next((kw.value for kw in call.keywords if kw.arg == key),
                call.args[index] if len(call.args) > index else None)


def spelling(node):
    return ast.unparse(node) if node is not None else ""


def destination(call, name):
    member = name.rsplit(".", 1)[-1]
    if name in {"open", "io.open", "gzip.open", "bz2.open", "lzma.open", "os.fdopen"}:
        return argument(call, 0, "file") if writable(argument(call, 1, "mode")) else None
    if name in {"os.rename", "os.replace", "os.link", "os.symlink", "shutil.copy", "shutil.copy2",
                "shutil.copyfile", "shutil.copytree", "shutil.move"}:
        return argument(call, 1, "dst")
    if name == "os.open":
        flags = spelling(argument(call, 1, "flags"))
        read_only = {"0", "os.O_RDONLY", "os.O_CLOEXEC", "os.O_NOFOLLOW"}
        return None if set(flags.split(" | ")) <= read_only else argument(call, 0, "path")
    if not isinstance(call.func, ast.Attribute):
        return None
    if member in {"write_text", "write_bytes", "touch"}:
        return call.func.value
    if member == "extractall":
        return argument(call, 0, "path") or ast.Constant(".")
    if member == "extract":
        return argument(call, 1, "path") or ast.Constant(".")
    if member == "open" and writable(argument(call, 0, "mode")):
        return call.func.value
    if member in {"rename", "replace"}:
        # Path(...).rename/replace. String substitutions have two positional operands.
        return argument(call, 0, "target") if len(call.args) <= 1 else None
    return None


def writable(mode):
    return mode is not None and (not isinstance(mode, ast.Constant)
                                 or any(flag in str(mode.value) for flag in "wax+"))


def checked(path):
    return isinstance(path, ast.Call) and spelling(path.func) in {
        "hosts.evidence_path", "hosts_registry.evidence_path", "evidence_path",
        "rg.hosts_registry.evidence_path", "fast.hosts.evidence_path", "kit.hosts.evidence_path",
    }


def subprocess_start(name):
    return (name in {"subprocess.run", "subprocess.Popen", "subprocess.call", "subprocess.check_call",
                     "subprocess.check_output", "subprocess.getoutput", "subprocess.getstatusoutput"}
            or name.startswith(("os.exec", "os.spawn", "os.posix_spawn", "asyncio.create_subprocess"))
            or name in {"os.system", "os.popen", "multiprocessing.Process"})


def fixed_tool_query(call):
    command = argument(call, 0, "args")
    if not isinstance(command, (ast.List, ast.Tuple)) or len(command.elts) < 2:
        return False
    argv = command.elts
    executable = spelling(argv[0])
    verb = argv[3] if len(argv) > 3 and spelling(argv[1]) == "'-C'" else argv[1]
    # Git object and ref reads do no benchmark work and use no overlay settings.
    if executable == "'git'" and isinstance(verb, ast.Constant):
        return verb.value in {"archive", "cat-file", "grep", "ls-remote", "ls-tree", "merge-base", "rev-parse", "show"}
    # Docker lifecycle and accounting reads act on an already selected container.
    if executable in {"'docker'", "ns.executable()"} and isinstance(verb, ast.Constant):
        return verb.value in {"inspect", "rm", "stats", "stop", "wait"} or (
            verb.value == "image" and len(argv) > 2 and spelling(argv[2]) == "'inspect'")
    return False


class Audit(ast.NodeVisitor):
    def __init__(self, path):
        self.path = path
        self.scope = []
        self.imports = {}
        self.writes = []
        self.starts = []

    def visit_Import(self, node):
        for name in node.names:
            if name.asname:
                self.imports[name.asname] = name.name

    def visit_ImportFrom(self, node):
        if node.module in {"subprocess", "os", "shutil", "io", "gzip", "bz2", "lzma", "builtins",
                           "asyncio", "multiprocessing"}:
            for name in node.names:
                self.imports[name.asname or name.name] = f"{node.module}.{name.name}".removeprefix("builtins.")

    def visit_FunctionDef(self, node):
        self.scope.append(node.name)
        self.generic_visit(node)
        self.scope.pop()

    visit_AsyncFunctionDef = visit_ClassDef = visit_FunctionDef

    def visit_Call(self, node):
        name = spelling(node.func)
        head, dot, tail = name.partition(".")
        name = self.imports.get(head, head) + dot + tail
        path = destination(node, name)
        owner = ".".join(self.scope)
        signature = spelling(node)
        site = f"{self.path}:{node.lineno} {owner} {signature}"
        write = (self.path, owner, name.rsplit(".", 1)[-1], spelling(path))
        if path is not None and not checked(path) and write not in WRITES:
            self.writes.append(site)
        start = (self.path, owner, name, spelling(argument(node, 0, "args")))
        if subprocess_start(name) and start not in STARTS and not fixed_tool_query(node):
            self.starts.append(site)
        if (name.endswith((".host_command", ".launch")) or name in {"kit.run", "ns.docker"}) and any(
                kw.arg == "env" for kw in node.keywords):
            self.starts.append(site)
        self.generic_visit(node)


def audit():
    paths = [ROOT / path for path in RUNNERS]
    paths.extend(path for folder in DIRECTORIES for path in (ROOT / folder).rglob("*.py"))
    for path in sorted(paths):
        rel = path.relative_to(ROOT)
        if "tests" in rel.parts:
            continue
        visitor = Audit(rel.as_posix())
        visitor.visit(ast.parse(path.read_text(encoding="utf-8"), filename=str(path)))
        yield visitor


class BoundaryOwners(unittest.TestCase):
    def test_outputs_open_the_checked_concrete_destination(self):
        # Spec: every file publication goes through the destination owner, including append and replacement.
        sites = [site for visitor in audit() for site in visitor.writes]
        if sites:
            self.fail("\n".join(sites))

    def test_child_starts_use_the_replacement_launcher(self):
        # Spec: benchmark commands inherit one frozen replacement environment, through the shared launcher.
        sites = [site for visitor in audit() for site in visitor.starts]
        if sites:
            self.fail("\n".join(sites))
