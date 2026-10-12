# Datalog programs on the rule engines' home turf

The same Datalog program on the same facts, on NRESE and on the engines whose strength it
is: Soufflé, Nemo, VLog and GLog (RDFox when a licence exists). Every engine runs in its own
language and its own documented configuration; every answer is checked before a time
counts.

```sh
python3 datalog.py list                                 # the cases, and who can express each
python3 datalog.py prepare --cases tc-G10K,claros-l     # fetch (pinned SHA-256), unpack, convert
python3 datalog.py translate cspa-httpd /tmp/cspa       # the program in every engine's language
NRESE_BIN=.../nrese-server python3 datalog.py run --cases cspa-httpd --threads 4 --runs 1 --qualify --results DIR
NRESE_BIN=.../nrese-server python3 datalog.py run --cases cspa-httpd --threads 4 --runs 3 --results DIR
```

| File | What |
|---|---|
| `cases.toml` | the cases: program, pinned data, expected counts, the published numbers to reproduce, home turf |
| `program.py` | one program read once (FlowLog's syntax, or VLog's over triples) and written for each engine |
| `datalog.py` | the driver: prepare, translate, run; rows in the suite's schema |
| `programs/flowlog/` | FlowLog's benchmark programs, vendored (Apache-2.0, `flowlog-rs/vldb26-artifact` at `abf3e47`) |
| `souffle/`, `vlog/` | the images: Soufflé 2.5 from the checksummed release; VLog 1.3.7 and GLog from pinned commits, their Trident and KOGNAC pinned too |

## The cases

- **`datalog-programs`**, FlowLog's suite (VLDB 2026): transitive closure and same
  generation on BigDatalog's graphs, CSPA and CSDA on Graspan's httpd, Linux and PostgreSQL
  graphs, reachability on LiveJournal, Galen, and Doop on DaCapo's batik. Soufflé's own
  counts from the FlowLog reproduction logs are the expected answers; FlowLog's Table 1 has
  Soufflé's times at 4 and 64 threads.
- **`materialisation-programs`**, RDFox's benchmark programs over RDF (Claros, LUBM; L is the
  OWL 2 RL part of the ontology as rules, LE adds rules that build large cliques), as GLog's
  VLDB 2021 package has them. Each relation is one triple pattern, so NRESE runs them over
  its store as they are. The authors' own logs in that package are the numbers to reproduce
  (single-threaded VLog `mat`, GLog `tgchase --querycont 1 --edbcheck 1`).

## How each engine runs

| Engine | Input | Command | Threads |
|---|---|---|---|
| NRESE | N-Triples (each fact a triple: `<n:1> <r:Arc> <n:2>`) | `nrese-server load` with `NRESE_REASONING_MODE=custom` and the program as Notation3 rules | all the container has |
| Soufflé 2.5 | CSV (programs over triples: a table of N-Triples terms) | `souffle -o` (compiled; compilation timed apart), then the binary with `-j N` | N |
| Nemo 0.10.1 | CSV, or the N-Triples | `nmo -e none` | one, by design |
| VLog 1.3.7 | CSV in memory, or a Trident store (`vlog load`) | `vlog mat` | one (its default; `--vlog-threads` runs `--multithreaded 1 --nthreads N`) |
| GLog | as VLog | `vlog tgchase --querycont 1 --edbcheck 1 --rewritecliques 0 --delProofs 0` (`GLOG_OPTIONS`) | one |
| RDFox 7.6 (UNVERIFIED until a licensed run) | the N-Triples, as NRESE | sandbox: `threads N`, import the facts, import the rules (RDFox's triple rules, the same as NRESE's) | N |

VLog and GLog print no count of what they derived: every run stores the materialisation
(`--storemat`) and the files are counted afterwards; the storing time VLog logs comes off the
wall time. RDFox without `RDFox.lic` is a skipped row naming the licence; its rows carry
`publish = permission`.

Every engine runs in a container capped at the same CPUs (`--threads`, as `DOCKER_CPUS`) and
memory (`DOCKER_MEMORY`, default 28g), as the invoking user, from fresh scratch; the
repetitions are interleaved across the engines, each round in an order rotated by one.

**What is timed.** `load` is the wall time from the input files to the complete
materialisation, as each engine is used: for NRESE that includes parsing, the bulk load
into its on-disk store and writing the closure there; for Soufflé running the compiled
program (reading CSV included). `reason` is the time the engine reports for reasoning
alone (NRESE's rematerialisation, Nemo's "Reasoning", VLog's "Runtime materialization").
Peak memory is sampled from `docker stats`.

**What is checked.** `--qualify` (a run before the timed ones) counts every derived
relation: NRESE by a SPARQL count over its closure, Soufflé by `.printsize`, Nemo and VLog
by the exported relation. Each count is a `query` row `count:REL`, compared with the
published count where there is one and across the engines (the suite's disputes). A timed
run's total of derived facts must equal the published total or the engine's own qualified
one (`qualified.json`); otherwise the row is `wrong`, never a time.

**What an engine can't express is reported, never approximated.** NRESE's user rules are
over triples, so relations of arity 3 or more (Galen, Doop) and negation (Doop) are `not
expressible`; VLog has no comparisons (same generation's `x != y`), and the wildcard in
Doop is left out until a run confirms its negation. Each such pair is a `skipped` row with
the reason.

## Licences

The vendored programs are Apache-2.0. The datasets (FlowLog's, Oxford's Claros) and GLog's
program files state no licence: they are fetched by pinned hash into `DATALOG_DOWNLOADS`
and never committed; ask their authors before publishing results on them
([../../CATALOG.md](../../CATALOG.md) §3.4).
