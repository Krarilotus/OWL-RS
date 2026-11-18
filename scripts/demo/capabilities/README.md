# Held snapshots, private what-ifs, replication, image restore and multiple repositories

Three synthetic triples describe contract `1.2.3` and its brake component, initially
`approved`. The same fixture is embedded by the held-snapshot example and sent over HTTP.
No product functionality is added. Preparation run on engine-v2; rehearse the actual
release identity on **15 November**, separately.

Run **only in a Linux fleet job**, from the repository root:

```sh
bash scripts/demo/capabilities/fleet.sh
```

This builds one server and one thin example in a job-owned target directory. It records
head/tree, dirty state, toolchain, fixture hash, server/build logs, raw HTTP exchanges,
feed and observed answers under `$FLEET_OUT`. It also runs the existing verify
`repository-lifecycle` scenario against that build. It uses a fresh output root and loopback ports; never point
it at a real store. The server loads the first `.env` from its working directory upwards, so
the output root and verify's scratch each hold an empty one; the job plants a sentinel `.env`
above both that a server would refuse to start on. Run `run.py` without `-O`: it refuses. Fleet reclaims job directories. No browser, containers or tokens.

For already-built binaries, within a fleet job:

```sh
uv run --no-project --python 3.12 scripts/demo/capabilities/run.py \
  --server /absolute/path/nrese-server --snapshot /absolute/path/held_snapshot \
  --out "$FLEET_OUT/fresh-demo-state"
```

| Demonstration | Observable result | Explicit limit |
|---|---|---|
| Held snapshot | Old handle retains `approved` at its revision; fresh handle has `revised` one revision later | Handle lifetime; **not durable named history**. HTTP transactions cannot hold that handle across reads. |
| Private what-if | Session replaces `approved` with `proposed`; committed query stays `approved` before and after rollback | Per-read speculation; **not durable branches or merge** |
| Replication feed | Replica bootstraps at `approved`; an HTTP WAL batch then carries `revised`; replica answer/revision match primary and applied-record count advances | Raw WAL; **not Kafka Change+Latest**, decoded entity CDC, HA or all-catalogue replication |
| Image restore | Image at `approved` plus retained WAL restores `revised` at chosen revision, excluding later source `later` | Physical restore with retained logs; **not a queryable history catalogue** |
| Multiple repositories | Same component query: `nrese` returns `revised`, `demo-other` returns `approved` | Independent stores; **not configuration composition** or cross-repository transactions |

The component query is printed here for the presenter:

```sparql
SELECT ?version WHERE {
  <urn:demo:contract> <urn:demo:component> ?part .
  ?part <urn:demo:version> ?version
}
```

Reuses verifykit's server lifecycle and HTTP evidence/deadline client; the driver only
supplies the multi-server/offline-restore sequence. No toolkit code is changed.

Reused interfaces: [HTTP API](../../../docs/ops/http-api.md),
[replication](../../../docs/ops/replication.md), image backup/offline restore in
[server setup §5.4](../../../docs/ops/server-setup.md), and verify's
`scripts/verify/scenarios/repository-lifecycle.toml`. Engine `examples/image.rs`
writes a latest image; `perf_lab --replication` exercises feed/apply with generated
benchmark data. This driver uses their product interfaces with the common fixture.
