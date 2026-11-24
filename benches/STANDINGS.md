# Standings: NRESE against every contender

Generated 2026-11-21 by `python benches/suite/suite.py standings --write` from the run records' `versus` entries ([runs/](runs/README.md)). Don't edit it by hand.

Per case and contender, the newest comparison on each host: answers first (each system against the reference systems; a wrong, missing or unchecked answer is never timed), then NRESE's time over the queries both answered right, as NRESE / contender with its 95 % bootstrap interval (below 1: NRESE faster). **win**: faster beyond max(10 %, the spread) and 2 ms, the interval below 1; **loss**: the other way round; **tie**: neither; **no-verdict**: nothing both answered right. A rule engine's time is its materialisation (`reason`), timed only where its derived relations' counts passed their checks. Load and memory (serve peak, with its kind) as the same ratio. A case marked ★ is the contender's home turf ([cases/](cases/README.md) `home_turf`).

† before the reset of 25 Oct 2026 (engine-v2 re-based on 59e5e24c): history, not the current engine's result: a row or newest date marked so. 0 of the 0 run records are.

## Summary

| Contender | Home turf: win / tie / loss / no verdict | All cases: win / tie / loss / no verdict | Answers not ok | Newest |
|---|---|---|---|---|
| qlever | - | - | - | not measured yet |
| oxigraph | - | - | - | not measured yet |
| virtuoso | - | - | - | not measured yet |
| jena | - | - | - | not measured yet |
| millenniumdb | - | - | - | not measured yet |
| rdf4j | - | - | - | not measured yet |
| nemo | - | - | - | not measured yet |
| souffle | - | - | - | not measured yet |
| vlog | - | - | - | not measured yet |
| glog | - | - | - | not measured yet |
| owlrl | - | - | - | not measured yet |
| hermit | - | - | - | not measured yet |
| openllet | - | - | - | not measured yet |
| konclude | - | - | - | not measured yet |
| elk | - | - | - | not measured yet |
| graphdb | restricted | restricted | - | not run |
| rdfox | refused | refused | - | no evaluation licence (12 Oct 2026): its published numbers only, as reported; 72 published numbers below |
| allegrograph | refused | refused | - | no evaluation licence (12 Oct 2026): its published numbers only, as reported; 0 published numbers below |
| stardog | restricted | restricted | - | not run |
| anzograph | restricted | restricted | - | not run |
| qdrant | - | - | - | not measured yet |
| milvus | - | - | - | not measured yet |
| pgvector | - | - | - | not measured yet |
| lancedb | - | - | - | not measured yet |
| topbraid | - | - | - | not measured yet |
| pyshacl | - | - | - | not measured yet |
| maplib | - | - | - | not measured yet |
| corese | - | - | - | not measured yet |

## Home turf, per contender

The owner's bar: beat each competitor on its home turf. Per contender of [suite/systems.toml](suite/systems.toml): its home-turf cases ([cases/](cases/README.md) `home_turf`), whether they can run here, and its newest verdict on them.

| Contender | Home-turf cases | Runnable here | Newest verdict on its home turf |
|---|---|---|---|
| qlever | q-large-result, text-search, clients-sweep, cache-replay, sparqloscope-dblp | yes; cache-replay: no content check (not timed); sparqloscope-dblp: declared memory-limit; sparqloscope-dblp: no content check (not timed) | q-large-result: not measured; text-search: not measured; clients-sweep: not measured; cache-replay: not measured; sparqloscope-dblp: not measured |
| oxigraph | open-first-query, watdiv-10m | yes; watdiv-10m: no content check (not timed) | open-first-query: not measured; watdiv-10m: not measured |
| virtuoso | clients-lookups, writes-under-readers, bsbm-explore-100m, bsbm-bi-100m | yes; bsbm-explore-100m: no content check (not timed); bsbm-bi-100m: no content check (not timed) | clients-lookups: not measured; writes-under-readers: not measured; bsbm-explore-100m: not measured; bsbm-bi-100m: not measured |
| jena | horst-lubm10, load-entities, watdiv-100m | yes; horst-lubm10: no content check (not timed); watdiv-100m: no content check (not timed) | horst-lubm10: not measured; load-entities: not measured; watdiv-100m: not measured |
| millenniumdb | **gap: none defined** | yes (docker, rootfs) | - |
| rdf4j | commits-plain | yes | commits-plain: not measured |
| nemo | custom-tc | yes | custom-tc: not measured |
| souffle | **gap: none defined** | yes (kit) | - |
| vlog | **gap: none defined** | yes (kit) | - |
| glog | **gap: none defined** | yes (kit) | - |
| owlrl | **gap: none defined** | yes (docker, apptainer) | - |
| hermit | **gap: none defined** | yes (kit) | - |
| openllet | dl-transitive | yes | dl-transitive: not measured |
| konclude | dl-transitive-400, horn-classify | yes; dl-transitive-400: declared gave-up; horn-classify: no content check (not timed) | dl-transitive-400: not measured; horn-classify: not measured |
| elk | el-classify, el-classify-irregular | yes | el-classify: not measured; el-classify-irregular: not measured |
| graphdb | **gap: none defined** | licensed: runs, its results stay local | - |
| rdfox | **gap: none defined** | no: refused (no evaluation licence (12 Oct 2026): its published numbers only, as reported) | - |
| allegrograph | **gap: none defined** | no: refused (no evaluation licence (12 Oct 2026): its published numbers only, as reported) | - |
| stardog | **gap: none defined** | no: nothing here starts it | - |
| anzograph | **gap: none defined** | licensed: runs, its results stay local | - |
| qdrant | **gap: none defined** | yes (kit) | - |
| milvus | **gap: none defined** | yes (kit) | - |
| pgvector | **gap: none defined** | yes (kit) | - |
| lancedb | **gap: none defined** | yes (kit) | - |
| topbraid | **gap: none defined** | yes (kit) | - |
| pyshacl | **gap: none defined** | yes (kit) | - |
| maplib | **gap: none defined** | yes (kit) | - |
| corese | **gap: none defined** | yes (kit) | - |

Gaps, no home-turf case defined (19 of 28): millenniumdb, souffle, vlog, glog, owlrl, hermit, graphdb, rdfox, allegrograph, stardog, anzograph, qdrant, milvus, pgvector, lancedb, topbraid, pyshacl, maplib, corese.
Home turfs of systems not in suite/systems.toml (no runner here): glog-vlog.

## rdfox: reported, not measured by us

As the source reports it, on the source's hardware: not comparable with our measurements as if it were one ([competitors/reported.toml](competitors/reported.toml), [reasoning/reported](reasoning/reported/README.md)).

| Benchmark | Tier | Metric | Value | Threads | Hardware | Source | Date |
|---|---|---|---|---|---|---|---|
| claros | l | load import-sequential | 61.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): import without materialisation, sequential version | 2014 |
| claros | l | load import-parallel | 58.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): import without materialisation, parallel version | 2014 |
| claros | l | reason threads-sequential | 1907.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation, sequential version; 95.5 M triples after, 4.2 GB after; average of 3 runs; the 2014 program with owl:sameAs axiomatised (2,174 rules) | 2014 |
| claros | l | reason threads-1 | 2477.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 1 thread; 95.5 M triples after; average of 3 runs | 2014 |
| claros | l | reason threads-2 | 1177.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 2 threads | 2014 |
| claros | l | reason threads-8 | 333.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 8 threads | 2014 |
| claros | l | reason threads-16 | 179.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 16 threads | 2014 |
| claros | l | reason threads-24 | 139.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 24 threads | 2014 |
| claros | l | reason threads-32 | 127.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 32 threads (16 cores, hyperthreading) | 2014 |
| claros | le | load import-sequential | 57.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): import without materialisation, sequential version | 2014 |
| claros | le | load import-parallel | 58.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): import without materialisation, parallel version | 2014 |
| claros | le | reason threads-sequential | 4128.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation, sequential version; 555.1 M triples after, 18.0 GB after; average of 3 runs; the 2014 program with owl:sameAs axiomatised (2,223 rules) | 2014 |
| claros | le | reason threads-1 | 4989.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 1 thread; 555.1 M triples after | 2014 |
| claros | le | reason threads-2 | 2543.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 2 threads | 2014 |
| claros | le | reason threads-8 | 773.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 8 threads | 2014 |
| claros | le | reason threads-16 | 415.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 16 threads | 2014 |
| claros | le | reason threads-24 | 313.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 24 threads | 2014 |
| claros | le | reason threads-32 | 285.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 32 threads (16 cores, hyperthreading) | 2014 |
| lubm | rdfox-l-1000 | load import-sequential | 421.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): LUBM 01K (133.6 M triples; ours, LUBM(1000), has 138.3 M), import without materialisation, sequential version | 2014 |
| lubm | rdfox-l-1000 | load import-parallel | 415.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): import without materialisation, parallel version | 2014 |
| lubm | rdfox-l-1000 | reason threads-sequential | 68.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): LUBM_L on LUBM 01K, materialisation, sequential version; 182.4 M triples after, 9.3 GB after; average of 3 runs | 2014 |
| lubm | rdfox-l-1000 | reason threads-1 | 73.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 1 thread | 2014 |
| lubm | rdfox-l-1000 | reason threads-2 | 35.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 2 threads | 2014 |
| lubm | rdfox-l-1000 | reason threads-8 | 14.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 8 threads | 2014 |
| lubm | rdfox-l-1000 | reason threads-16 | 8.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 16 threads | 2014 |
| lubm | rdfox-l-1000 | reason threads-24 | 7.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 24 threads | 2014 |
| lubm | rdfox-l-1000 | reason threads-32 | 7.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 32 threads (16 cores, hyperthreading) | 2014 |
| lubm | rdfox-le-1000 | load import-sequential | 408.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): LUBM 01K (133.6 M triples; ours has 138.3 M), import without materialisation, sequential version | 2014 |
| lubm | rdfox-le-1000 | load import-parallel | 415.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): import without materialisation, parallel version | 2014 |
| lubm | rdfox-le-1000 | reason threads-sequential | 913.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): LUBM_LE on LUBM 01K, materialisation, sequential version; 332.6 M triples after, 13.8 GB after; average of 3 runs | 2014 |
| lubm | rdfox-le-1000 | reason threads-1 | 947.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 1 thread | 2014 |
| lubm | rdfox-le-1000 | reason threads-2 | 514.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 2 threads | 2014 |
| lubm | rdfox-le-1000 | reason threads-8 | 155.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 8 threads | 2014 |
| lubm | rdfox-le-1000 | reason threads-16 | 88.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 16 threads | 2014 |
| lubm | rdfox-le-1000 | reason threads-24 | 77.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 24 threads | 2014 |
| lubm | rdfox-le-1000 | reason threads-32 | 71.0 s | - | 2x Xeon E5-2650, 16 cores / 32 threads, 128 GB, RHEL 6.3 | RDFox (AAAI 2014); motik2014parallel Fig. 2(a): materialisation at 32 threads (16 cores, hyperthreading) | 2014 |
| claros | le | load import | 168.0 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: import at 1 thread (one input file); 18.8 M triples, 80.5 B/triple after import; ClarosLE with owl:sameAs handled by rewriting, not axiomatised | 2015 |
| claros | le | reason threads-1 | 10000.0 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 1 thread (10.0k s); 539.2 M triples, 36.9 B/triple after; owl:sameAs by rewriting | 2015 |
| claros | le | reason threads-16 | 906.0 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 16 threads | 2015 |
| claros | le | reason threads-32 | 583.3 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 32 threads | 2015 |
| claros | le | reason threads-128 | 225.7 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 128 threads | 2015 |
| claros | le | reason threads-1024 | 124.9 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 1024 threads (the paper's fastest Claros run) | 2015 |
| lubm | rdfox-l-50k | load import | 6800.0 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: LUBM-50K (6.7 G triples, 37 GB compressed), import at 11 threads (6.8k s), 124.1 B/triple after import; data we don't hold | 2015 |
| lubm | rdfox-l-50k | reason threads-1 | 27000.0 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: LUBM_L on LUBM-50K at 1 thread (27.0k s); 9.2 G triples, 101.0 B/triple after (89.7 without preallocation) | 2015 |
| lubm | rdfox-l-50k | reason threads-16 | 1700.0 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 16 threads (1.7k s) | 2015 |
| lubm | rdfox-l-50k | reason threads-32 | 1100.0 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 32 threads (1.1k s) | 2015 |
| lubm | rdfox-l-50k | reason threads-128 | 387.4 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Table 1: materialisation at 128 threads (the most that fitted in memory) | 2015 |
| lubm | rdfox-l-50k | update delete-5000 | 0.49 s | - | SPARC T5-8, 128 cores / 1024 threads, 4 TB, Solaris 11.1 | RDFox (ISWC 2015); nenov2015rdfox Section 5: incremental update after deleting 5,000 triples (8,525.8 deleted in all on average), average of 5 subsets; fastest 420 ms, slowest 600 ms | 2015 |
| lubm | rdfox-l-125 | reason threads-1 | 23.0 s | - | Intel i7 PC, 94.1 GiB, Ubuntu 16.04 | RDFox (GLog's VLDB 2021 study); tsamoura2021triggergraphs Table 4: LUBM-L, single thread, Skolem chase; memory the peak at reasoning in MB as published; the LUBM size isn't stated in the table, the authors' package runs 125 universities | 2021 |
| lubm | rdfox-le-125 | reason threads-1 | 116.6 s | - | Intel i7 PC, 94.1 GiB, Ubuntu 16.04 | RDFox (GLog's VLDB 2021 study); tsamoura2021triggergraphs Table 4: LUBM-LE, single thread; memory in MB as published | 2021 |
| claros | l | reason threads-1 | 2512.0 s | - | Intel i7 PC, 94.1 GiB, Ubuntu 16.04 | RDFox (GLog's VLDB 2021 study); tsamoura2021triggergraphs Table 4: Claros-L, single thread; memory in MB as published | 2021 |
| claros | le | reason threads-1 | timeout  | - | Intel i7 PC, 94.1 GiB, Ubuntu 16.04 | RDFox (GLog's VLDB 2021 study); tsamoura2021triggergraphs Table 4: Claros-LE, single thread: timeout after 1 h | 2021 |
| claros | l-noeq | update delete-1000-uniform:CNT:unstratified | 3.17 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: deleting 1,000 facts by uniform sampling, single thread, mean over 30 samples (95% CI +-0.06 s); Claros_L with equality off (1,310 rules, 73.8 M facts after materialisation) | 2019 |
| claros | l-noeq | update delete-1000-uniform:CNT:stratified | 2.84 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: as above, rules stratified by levels (95% CI +-0.04 s) | 2019 |
| claros | l-noeq | update delete-1000-uniform:DRed:unstratified | 0.36 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: DRed (95% CI +-0.32 s) | 2019 |
| claros | l-noeq | update delete-1000-uniform:DRed:stratified | 0.04 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: DRed, stratified (95% CI +-0.00 s) | 2019 |
| claros | l-noeq | update delete-1000-uniform:FBF:unstratified | 0.06 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: FBF (95% CI +-0.00 s) | 2019 |
| claros | l-noeq | update delete-1000-uniform:FBF:stratified | 0.05 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: FBF, stratified (95% CI +-0.00 s) | 2019 |
| claros | le-noeq | update delete-1000-uniform:CNT:unstratified | 26.15 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: deleting 1,000 facts by uniform sampling, single thread, mean over 30 samples (95% CI +-0.81 s); Claros_LE with equality off (1,337 rules, 533.3 M facts after materialisation) | 2019 |
| claros | le-noeq | update delete-1000-uniform:CNT:stratified | 26.31 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: as above, stratified (95% CI +-0.80 s) | 2019 |
| claros | le-noeq | update delete-1000-uniform:DRed:unstratified | 5450.0 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: DRed (5.45k s, 95% CI +-1.29k s): the cliques of relatedPlaces | 2019 |
| claros | le-noeq | update delete-1000-uniform:DRed:stratified | 1810.0 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: DRed, stratified (1.81k s, 95% CI +-997.80 s) | 2019 |
| claros | le-noeq | update delete-1000-uniform:FBF:unstratified | 582.89 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: FBF (95% CI +-458.10 s) | 2019 |
| claros | le-noeq | update delete-1000-uniform:FBF:stratified | 472.54 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 3: FBF, stratified (95% CI +-374.46 s) | 2019 |
| claros | l-noeq | update add-1000-uniform:CNT:unstratified | 3.01 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: adding 1,000 facts by uniform sampling, single thread, mean over 30 samples (95% CI +-0.02 s) | 2019 |
| claros | l-noeq | update add-1000-uniform:CNT:stratified | 2.72 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: stratified (95% CI +-0.02 s) | 2019 |
| claros | l-noeq | update add-1000-uniform:DRed-FBF:unstratified | 0.02 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: DRed and FBF (insertion is the same in both; 95% CI +-0.00 s) | 2019 |
| claros | l-noeq | update add-1000-uniform:DRed-FBF:stratified | 0.02 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: stratified (95% CI +-0.00 s) | 2019 |
| claros | le-noeq | update add-1000-uniform:CNT:unstratified | 26.7 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: adding 1,000 facts by uniform sampling, single thread, mean over 30 samples (95% CI +-1.10 s) | 2019 |
| claros | le-noeq | update add-1000-uniform:CNT:stratified | 25.84 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: stratified (95% CI +-0.79 s) | 2019 |
| claros | le-noeq | update add-1000-uniform:DRed-FBF:unstratified | 0.77 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: DRed and FBF (95% CI +-0.51 s) | 2019 |
| claros | le-noeq | update add-1000-uniform:DRed-FBF:stratified | 0.71 s | - | 2x Xeon E5-2640 2.6 GHz, 16 cores / 32 threads, 512 GB, Fedora 24 | RDFox rev. 2725 and its counting branch (AIJ 2019); motik2019maintenance Table 6: stratified (95% CI +-0.41 s) | 2019 |
