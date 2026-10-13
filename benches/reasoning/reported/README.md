# Reported numbers: contenders that don't run here

RDFox's evaluation licence was refused (owner, 12 Oct 2026), so RDFox runs nowhere in the
suite, now or later. What its authors published stands beside our measurements as
**reported rows**: the suite's result schema ([../../suite/suitekit/schema.py](../../suite/suitekit/schema.py)),
`runtime = reported`, the paper's hardware in `host`, its year in `date`, and the source, table and
setup in `note`. They are never measured, never summed into a run's results, and never
compared as if they came from our hardware; a reader puts them beside our rows for the same
workload, tier and setup and reads the hardware apart.

| File | Rows from |
|---|---|
| `rdfox.csv` | Motik et al., AAAI 2014 (Fig. 2a: materialisation of Claros L/LE and LUBM L/LE 01K at 1 to 32 threads); Nenov et al., ISWC 2015 (Table 1: Claros-LE and LUBM-50K at 1 to 1,024 threads on a SPARC T5-8; 5,000 deletions on LUBM-50K); Tsamoura et al., VLDB 2021 (GLog's Table 4: RDFox single-threaded on LUBM L/LE, Claros L/LE); Motik et al., AIJ 2019 (Tables 3 and 6: deleting and adding 1,000 facts on Claros L/LE, single-threaded) |

## Where our runs match a paper's setup

| Paper | Its setup | Ours (benches/reasoning/datalog, maintenance) |
|---|---|---|
| AAAI 2014 | Claros (18.8 M triples) with the 2014 L and LE programs (owl:sameAs axiomatised); 1, 2, 8, 16, 24, 32 threads on 16 cores / 32 threads | the same data and programs (`claros` `l`, `le`); NRESE at 1, 2, 8, 16 and 32 CPUs on Office-PC's 16 cores / 32 threads; the closure's size checked against the paper's 95.5 M and 555.1 M |
| AAAI 2014 | LUBM 01K (133.6 M triples), LUBM L and LE | LUBM(1000) from our frozen data, 138.3 M triples (the same generator family, another version): close, not identical; the paper's 182.4 M and 332.6 M beside our sizes |
| VLDB 2021 (GLog) | single thread; LUBM with 125 universities, Claros L/LE | NRESE at 1 CPU beside VLog and GLog, which run single-threaded by design, on the same programs (`rdfox-l-125`, `rdfox-le-125`, `l`, `le`) |
| AIJ 2019 | single thread; Claros L/LE with equality off (owl:sameAs as an ordinary property, no axiomatisation); 1,000 facts deleted, and added, by uniform sampling, mean over 30 samples | the `l-noeq` and `le-noeq` programs (the 2014 programs without the 862 and 884 rules that read owl:sameAs: 1,312 and 1,339 rules against the paper's 1,310 and 1,337); NRESE at 1 CPU, 1,000-fact deletions and additions over SPARQL Update; the closure's size checked against the paper's 73.8 M and 533.3 M |
| ISWC 2015 | a 128-core SPARC with 4 TB; LUBM-50K | not matched: neither the machine nor the data; reported for completeness only |

The papers' RDFox also differs from today's: a research build of 2014–2017, in memory, with
its import (parsing) reported apart from materialisation; our NRESE times include parsing and
writing the closure to its on-disk store unless a row says otherwise.
