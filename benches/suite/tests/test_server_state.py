"""Where a server the suite starts keeps its own state.

Spec (Context.mounts; Runtime.new_store and remove_store): a run's work directory (/work) is a host directory the
suite removes with its own rights, while its store is the runtime's own (a Docker volume, else a directory in the
suite's scratch) and is removed by the runtime. A container writes into the work directory as its own user, which on
Docker is root: a directory tree it makes there holds entries the suite can't remove, and its cleanup ends the run
with exit 1 (Jena's rule regimes on 15 to 17 Nov: every row good, a `run/` left behind). So a server's own state
goes into the run's store or onto no mount at all, never into the work directory.

Fuseki keeps its state in FUSEKI_BASE, by default `$PWD/run` (fuseki-server); its working directory is the spec's.
Nothing here starts a container: the spec Jena would start is read.
"""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
from suitekit import adapters  # noqa: E402
from suitekit.runtime import Mount  # noqa: E402


def holder(spec, path: str):
    """The mount `path` lies on inside the container (the deepest), or None."""
    on = [m for m in spec.mounts if path == m.target or path.startswith(m.target.rstrip("/") + "/")]
    return max(on, key=lambda m: len(m.target), default=None)


class ServerState(unittest.TestCase):
    def test_fuseki_in_a_rule_regime_keeps_its_state_out_of_the_work_directory(self):
        seen = []
        runtime = SimpleNamespace(kind="docker", dry=False, port=3030, url=lambda spec: "http://localhost:3030")
        with tempfile.TemporaryDirectory() as tmp, \
                patch.object(adapters.Jena, "start", lambda self, ctx, spec, endpoint, timeout_s=None:
                             seen.append(spec) or endpoint):
            ctx = adapters.Context(runtime, ROOT, Path(tmp), Path(tmp), Mount("/host/data", "/data"), [], 60,
                                   {"DOCKER_MEMORY": "0"}, "t")
            for regime in ("rdfs", "owl-horst"):
                with self.subTest(regime=regime):
                    seen.clear()
                    adapters.Jena().load(ctx, "run-store", ["/data/lubm-1.nt"], regime)
                    spec = seen[0]
                    base = spec.env.get("FUSEKI_BASE") or f"{(spec.workdir or '/').rstrip('/')}/run"
                    on = holder(spec, base)
                    self.assertTrue(on is None or on.source == "run-store",
                                    f"Fuseki's state {base} is on {on}, not in the run's store or on no mount")
                    self.assertFalse(on is not None and on.readonly, f"Fuseki's state {base} is on a read-only mount")


if __name__ == "__main__":
    unittest.main()
