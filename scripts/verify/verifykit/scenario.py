"""A scenario file (TOML; the schema is ../README.md): the server settings it needs and the HTTP
steps it drives, read and checked before anything starts.

Every key is known: a misspelt one is an error that names it and the keys allowed there, so a typo
can't turn an expectation into nothing. Every request step states the status it expects. A
variable (`${name}`) must be one the runner gives (`base`, `port`), one of `[vars]`, or one an
earlier step captures; anything else is an error before the server starts.
"""
from __future__ import annotations

import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

from . import checks

TOP = ("name", "description", "server", "vars", "step")
SERVER = ("set", "env")
STEP = ("name", "method", "path", "params", "headers", "auth", "body", "body_file", "json", "form",
        "timeout_s", "read_limit_mib", "expect", "capture", "restart")
PAYLOADS = ("body", "body_file", "json", "form")
EXPECT = ("status", "headers", "body", "json", "rows")
BODY_PREDICATES = ("equals", "contains", "not_contains", "matches", "length")
CAPTURE_SOURCES = ("json", "header", "body")
METHODS = ("GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS")
BUILTINS = ("base", "port")
# The launcher's: every run has its own port and data directory.
OWNED_KEYS = ("server.bind_address", "server.bind_addr", "store.data_dir")
OWNED_ENV = ("NRESE_BIND_ADDR", "NRESE_DATA_DIR", "NRESE_CONFIG_PATH")
ENV_NAME = re.compile(r"^(NRESE_[A-Z0-9_]+|RUST_LOG|RUST_BACKTRACE)$")
VAR = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")
NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


class ScenarioError(Exception):
    """A scenario that can't run as written; the message names the file, the step and the key."""


@dataclass
class Scenario:
    path: Path | None
    name: str
    description: str
    settings: dict[str, str]
    env: dict[str, str]
    vars: dict[str, str]
    steps: list[dict] = field(default_factory=list)
    text: str = ""


def load(path: Path) -> Scenario:
    text = path.read_text(encoding="utf-8")
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise ScenarioError(f"{path}: {e}") from None
    scenario = from_dict(data, path.stem, path)
    scenario.text = text
    return scenario


def from_dict(data: dict, default_name: str, path: Path | None = None) -> Scenario:
    where = str(path) if path else default_name
    _keys(data, TOP, where)
    name = data.get("name", default_name)
    if not isinstance(name, str) or not NAME.match(name):
        raise ScenarioError(f"{where}: name {name!r}: letters, digits, '.', '_' and '-' (it names a directory)")
    server = data.get("server", {})
    _keys(server, SERVER, f"{where}: [server]")
    settings = {}
    for key, value in server.get("set", {}).items():
        if key in OWNED_KEYS:
            raise ScenarioError(f"{where}: [server] set {key}: the launcher sets it (a fresh one per run)")
        settings[key] = _scalar(value, f"{where}: [server] set {key}")
    env = {}
    for key, value in server.get("env", {}).items():
        if not ENV_NAME.match(key) or key in OWNED_ENV:
            raise ScenarioError(f"{where}: [server] env {key}: only NRESE_* (not {', '.join(OWNED_ENV)}), "
                                "RUST_LOG and RUST_BACKTRACE")
        env[key] = _scalar(value, f"{where}: [server] env {key}")
    variables = {k: _scalar(v, f"{where}: [vars] {k}") for k, v in data.get("vars", {}).items()}
    for key in variables:
        if not VAR.fullmatch("${" + key + "}") or key in BUILTINS:
            raise ScenarioError(f"{where}: [vars] {key}: a name of letters, digits and '_', not {', '.join(BUILTINS)}")
    raw = data.get("step")
    if not isinstance(raw, list) or not raw:
        raise ScenarioError(f"{where}: no [[step]]")
    known = {*BUILTINS, *variables}
    steps = []
    for index, step in enumerate(raw, 1):
        label = f"{where}: step {index}" + (f" ({step.get('name')!r})" if isinstance(step, dict) else "")
        steps.append(_step(step, index, label, known, path))
    return Scenario(path, name, data.get("description", ""), settings, env, variables, steps)


def _step(step: dict, index: int, label: str, known: set[str], path: Path | None) -> dict:
    _keys(step, STEP, label)
    if not isinstance(step.get("name"), str) or not step["name"].strip():
        raise ScenarioError(f"{label}: no name (the summary lists each step by it)")
    out = {"index": index, "name": step["name"]}
    if step.get("restart"):
        if set(step) - {"name", "restart"}:
            raise ScenarioError(f"{label}: a restart step has a name and `restart = true`, nothing else")
        out["restart"] = True
        return out
    path_ = step.get("path")
    if not isinstance(path_, str) or not path_.startswith("/"):
        raise ScenarioError(f"{label}: path must start with / (the server's base comes from the runner)")
    method = str(step.get("method", "GET")).upper()
    if method not in METHODS:
        raise ScenarioError(f"{label}: method {method}: one of {', '.join(METHODS)}")
    payloads = [k for k in PAYLOADS if k in step]
    if len(payloads) > 1:
        raise ScenarioError(f"{label}: one payload at most, not {' and '.join(payloads)}")
    out.update(method=method, path=path_, params=_strings(step.get("params", {}), f"{label}: params"),
               headers=_strings(step.get("headers", {}), f"{label}: headers"),
               timeout_s=float(step.get("timeout_s", 60)),
               read_limit=int(float(step.get("read_limit_mib", 16)) * 1024 * 1024))
    if "auth" in step:
        auth = step["auth"]
        if not (isinstance(auth, dict) and len(auth) == 1 and (
                (isinstance(auth.get("basic"), list) and len(auth["basic"]) == 2)
                or isinstance(auth.get("bearer"), str))):
            raise ScenarioError(f"{label}: auth is {{ basic = [user, password] }} or {{ bearer = token }}")
        out["auth"] = auth
    if payloads:
        kind = payloads[0]
        value = step[kind]
        if kind == "body_file":
            if path is None:
                raise ScenarioError(f"{label}: body_file needs a scenario file")
            file = (path.parent / value).resolve()
            if not file.is_file():
                raise ScenarioError(f"{label}: body_file {value}: no such file ({file})")
            kind, value = "body", file.read_text(encoding="utf-8")
        elif kind == "form":
            value = _strings(value, f"{label}: form")
        elif kind == "body" and not isinstance(value, str):
            raise ScenarioError(f"{label}: body is text (json = {{...}} for a JSON body)")
        out["payload"] = (kind, value)
    out["expect"] = _expect(step.get("expect"), label)
    _variables(out, label, known)
    captures = step.get("capture", {})
    for var, spec in captures.items():
        where = f"{label}: capture {var}"
        if not VAR.fullmatch("${" + var + "}") or var in BUILTINS:
            raise ScenarioError(f"{where}: a name of letters, digits and '_', not {', '.join(BUILTINS)}")
        _keys(spec, (*CAPTURE_SOURCES, "regex"), where)
        sources = [k for k in CAPTURE_SOURCES if k in spec]
        if len(sources) != 1:
            raise ScenarioError(f"{where}: one source: json = \"$.path\", header = \"name\" or body = true")
        if "json" in spec:
            _path(spec["json"], where)
        if "regex" in spec:
            _regex(spec["regex"], where)
    out["capture"] = captures
    known.update(captures)
    return out


def _expect(expect, label: str) -> dict:
    if not isinstance(expect, dict) or "status" not in expect:
        raise ScenarioError(f"{label}: no expect.status (every request step states the status it expects)")
    _keys(expect, EXPECT, f"{label}: expect")
    status = expect["status"]
    status = status if isinstance(status, list) else [status]
    if not status or not all(isinstance(s, int) and 100 <= s <= 599 for s in status):
        raise ScenarioError(f"{label}: expect.status is a status code or a list of them")
    out = {"status": status, "headers": {}, "body": [], "json": [], "rows": expect.get("rows")}
    for name, predicate in expect.get("headers", {}).items():
        out["headers"][name.lower()] = _predicate(predicate, f"{label}: expect.headers {name}", checks.PREDICATES)
    bodies = expect.get("body", [])
    for i, predicate in enumerate(bodies if isinstance(bodies, list) else [bodies], 1):
        if not isinstance(predicate, dict):
            raise ScenarioError(f"{label}: expect.body is a table such as {{ contains = \"…\" }}, or a list of them")
        out["body"].append(_predicate(predicate, f"{label}: expect.body", BODY_PREDICATES))
    for json_path, predicate in expect.get("json", {}).items():
        where = f"{label}: expect.json {json_path}"
        _path(json_path, where)
        out["json"].append((json_path, _predicate(predicate, where, checks.PREDICATES, bare_any=True)))
    if out["rows"] is not None and not (isinstance(out["rows"], int) and out["rows"] >= 0):
        raise ScenarioError(f"{label}: expect.rows is a count")
    return out


def _predicate(value, where: str, allowed: tuple[str, ...], bare_any: bool = False) -> dict:
    """A predicate table, or a bare value meaning `equals` (a string for headers; for JSON any
    value but a table, which would read as a predicate: write { equals = {...} } for an object)."""
    if not isinstance(value, dict):
        if not bare_any and not isinstance(value, str):
            raise ScenarioError(f"{where}: a string or a table such as {{ contains = \"…\" }}")
        return {"equals": value}
    if not value:
        raise ScenarioError(f"{where}: an empty predicate checks nothing")
    _keys(value, allowed, where)
    if "matches" in value:
        _regex(value["matches"], where)
    if "length" in value and not isinstance(value["length"], int):
        raise ScenarioError(f"{where}: length is a count")
    return value


def _variables(step: dict, label: str, known: set[str]) -> None:
    for name in sorted(set(VAR.findall(repr(step)))):
        if name not in known:
            raise ScenarioError(f"{label}: ${{{name}}} is neither the runner's ({', '.join(BUILTINS)}), "
                                "in [vars], nor captured by an earlier step")


def substitute(value, variables: dict[str, str]):
    """`value` with each ${name} replaced, through strings, lists and tables."""
    if isinstance(value, str):
        return VAR.sub(lambda m: variables[m.group(1)], value)
    if isinstance(value, list):
        return [substitute(v, variables) for v in value]
    if isinstance(value, tuple):
        return tuple(substitute(v, variables) for v in value)
    if isinstance(value, dict):
        return {k: substitute(v, variables) for k, v in value.items()}
    return value


def _keys(table, allowed: tuple[str, ...], where: str) -> None:
    if not isinstance(table, dict):
        raise ScenarioError(f"{where}: expected a table")
    unknown = [k for k in table if k not in allowed]
    if unknown:
        raise ScenarioError(f"{where}: unknown key {', '.join(map(repr, unknown))} (known: {', '.join(allowed)})")


def _scalar(value, where: str) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (str, int, float)):
        return str(value)
    raise ScenarioError(f"{where}: a string, number or boolean")


def _strings(table, where: str) -> dict[str, str]:
    if not isinstance(table, dict):
        raise ScenarioError(f"{where}: expected a table")
    return {str(k): _scalar(v, f"{where} {k}") for k, v in table.items()}


def _path(path, where: str) -> None:
    try:
        checks.parse_path(path)
    except checks.PathError as e:
        raise ScenarioError(f"{where}: {e}") from None


def _regex(pattern, where: str) -> None:
    try:
        re.compile(pattern)
    except (re.error, TypeError) as e:
        raise ScenarioError(f"{where}: regex {pattern!r}: {e}") from None
