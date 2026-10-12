# Standings: NRESE against every contender

Generated 2026-10-12 by `python benches/suite/suite.py standings --write` from the run records' `versus` entries ([runs/](runs/README.md)). Don't edit it by hand.

Per case and contender, the newest comparison on each host: answers first (each system against the reference systems; a wrong, missing or unchecked answer is never timed), then NRESE's time over the queries both answered right, as NRESE / contender with its 95 % bootstrap interval (below 1: NRESE faster). **win**: faster beyond max(10 %, the spread) and 2 ms, the interval below 1; **loss**: the other way round; **tie**: neither; **no-verdict**: nothing both answered right. A rule engine's time is its materialisation (`reason`), timed only where its derived relations' counts passed their checks. Load and memory (serve peak, with its kind) as the same ratio. A case marked ★ is the contender's home turf ([cases/](cases/README.md) `home_turf`).

## Summary

| Contender | Home turf: win / tie / loss / no verdict | All cases: win / tie / loss / no verdict | Answers not ok | Newest |
|---|---|---|---|---|
| qlever | 2 / 0 / 0 / 0 | 14 / 4 / 0 / 0 | 2 | 2026-10-10 |
| oxigraph | - | 13 / 5 / 0 / 0 | 3 | 2026-10-10 |
| virtuoso | 0 / 2 / 0 / 0 | 8 / 7 / 1 / 0 | 3 | 2026-10-10 |
| jena | - | 17 / 0 / 0 / 0 | 3 | 2026-10-10 |
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
| graphdb | restricted | restricted | - | ran in 7 runs; results stay local |
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

## qlever

| Case | Semantics | Answers | Timed | Time NRESE/them [95 %] | Verdict | Load | Memory | Run |
|---|---|---|---|---|---|---|---|---|
| clients-sweep ★ (plain-clients-sweep-capacity-r3) | plain | ok | 4/4 | 0.005 [0.005, 0.006] | **win** | 0.342 | 1.55 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-sweep ★ (plain-clients-sweep-r3) | plain | ok | 4/4 | 0.005 [0.005, 0.005] | **win** | 0.349 | 7.65 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-lookups (plain-clients-lookups-capacity-r3) | plain | ok | 4/4 | 0.005 [0.005, 0.006] | **win** | 0.344 | 1.07 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-lookups (plain-clients-lookups-r3) | plain | ok | 4/4 | 0.005 [0.005, 0.005] | **win** | 0.344 | 5.46 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| index-join-keys | plain | ok | 2/2 | 0.034 [0.032, 0.088] | **win** | 0.377 | 4.21 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| kernel-intersect | plain | ok | 2/2 | 0.096 [0.092, 1.85] | **tie** | 0.451 | 0.549 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-dates | plain | ok | 3/3 | 0.087 [0.08, 0.096] | **win** | 0.349 | 2.78 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-group | plain | ok | 3/3 | 0.18 [0.169, 0.205] | **win** | 0.355 | 1.87 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-negation | plain | ok | 2/2 | 0.059 [0.055, 0.087] | **win** | 0.345 | 4.37 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-optional | plain | ok | 2/2 | 1.04 [1, 3.83] | **tie** | 0.343 | 4.14 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-strings | plain | ok | 3/3 | 0.028 [0.023, 0.031] | **win** | 0.346 | 5.76 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-chain | plain | ok | 2/2 | 0.026 [0.022, 0.122] | **win** | 0.347 | 1.53 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-cycles | plain | ok | 2/2 | 0.078 [0.077, 5.11] | **tie** | 0.35 | 0.088 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-paths | plain | follows-closure-count incomplete (qlever); follows-reach incomplete (qlever); tag-descendants incomplete (qlever) | 1/4 | 0.007 [0.005, 0.008] | **win** | 0.345 | 1.22 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-sideways | plain | limit-join unchecked (nrese); limit-join unchecked (qlever) | 2/3 | 0.045 [0.037, 0.064] | **win** | 0.346 | 0.21 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-snowflake | plain | ok | 1/1 | 0.019 [0.017, 0.022] | **win** | 0.342 | 0.853 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-star | plain | ok | 2/2 | 0.102 [0.093, 0.237] | **win** | 0.344 | 4.38 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-topk | plain | ok | 2/2 | 0.285 [0.269, 9.1] | **tie** | 0.344 | 1.11 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |

## oxigraph

| Case | Semantics | Answers | Timed | Time NRESE/them [95 %] | Verdict | Load | Memory | Run |
|---|---|---|---|---|---|---|---|---|
| clients-lookups (plain-clients-lookups-capacity-r3) | plain | ok | 4/4 | 0.859 [0.774, 0.972] | **tie** | 0.383 | 1.47 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-lookups (plain-clients-lookups-r3) | plain | ok | 4/4 | 0.838 [0.769, 0.904] | **tie** | 0.391 | 1.38 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-sweep (plain-clients-sweep-capacity-r3) | plain | ok | 4/4 | 0.868 [0.73, 1.08] | **tie** | 0.377 | 1.65 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-sweep (plain-clients-sweep-r3) | plain | ok | 4/4 | 0.873 [0.8, 0.944] | **tie** | 0.393 | 1.72 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| index-join-keys | plain | ok | 2/2 | 0.015 [0.014, 0.017] | **win** | 0.479 | 1.27 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| kernel-intersect | plain | ok | 2/2 | 0.004 [0.004, 0.004] | **win** | 0.559 | 6.82 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-dates | plain | ok | 3/3 | 0.029 [0.027, 0.032] | **win** | 0.392 | 0.832 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-group | plain | ok | 3/3 | 0.008 [0.008, 0.009] | **win** | 0.394 | 0.976 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-negation | plain | ok | 2/2 | 0.006 [0.005, 0.007] | **win** | 0.391 | 0.767 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-optional | plain | ok | 2/2 | 0.026 [0.026, 0.027] | **win** | 0.38 | 3.37 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-strings | plain | ok | 3/3 | 0.014 [0.012, 0.016] | **win** | 0.386 | 1.2 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-chain | plain | ok | 2/2 | 0.149 [0.125, 0.189] | **win** | 0.393 | 0.471 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-cycles | plain | four-cycles incomplete (oxigraph) | 1/2 | 0.003 [0.003, 0.003] | **win** | 0.395 | 1.29 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-paths | plain | follows-closure-count incomplete (oxigraph) | 3/4 | 0.126 [0.123, 0.129] | **win** | 0.389 | 0.021 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-sideways | plain | limit-join unchecked (nrese); exists incomplete (oxigraph); limit-join unchecked (oxigraph) | 1/3 | 0.44 [0.289, 0.53] | **tie** | 0.39 | 0.679 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-snowflake | plain | ok | 1/1 | 0.074 [0.067, 0.088] | **win** | 0.388 | 0.431 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-star | plain | ok | 2/2 | 0.003 [0.002, 0.003] | **win** | 0.387 | 2.56 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-topk | plain | ok | 2/2 | 0.001 [0.001, 0.001] | **win** | 0.39 | 0.272 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |

## virtuoso

| Case | Semantics | Answers | Timed | Time NRESE/them [95 %] | Verdict | Load | Memory | Run |
|---|---|---|---|---|---|---|---|---|
| clients-lookups ★ (plain-clients-lookups-capacity-r3) | plain | ok | 4/4 | 0.325 [0.305, 0.373] | **tie** | 0.067 | 0.027 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-lookups ★ (plain-clients-lookups-r3) | plain | ok | 4/4 | 0.292 [0.253, 0.33] | **tie** | 0.066 | 0.026 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-sweep (plain-clients-sweep-capacity-r3) | plain | ok | 4/4 | 0.317 [0.286, 0.377] | **tie** | 0.066 | 0.034 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-sweep (plain-clients-sweep-r3) | plain | ok | 4/4 | 0.305 [0.276, 0.338] | **tie** | 0.067 | 0.035 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-dates | plain | ok | 3/3 | 0.629 [0.576, 0.718] | **win** | 0.067 | 0.013 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-group | plain | ok | 3/3 | 0.082 [0.077, 0.088] | **win** | 0.068 | 0.025 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-negation | plain | ok | 2/2 | 0.108 [0.098, 0.134] | **win** | 0.066 | 0.035 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-optional | plain | ok | 2/2 | 2.54 [2.42, 2.71] | **loss** | 0.066 | 0.056 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-strings | plain | ok | 3/3 | 0.01 [0.008, 0.012] | **win** | 0.066 | 0.033 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-chain | plain | ok | 2/2 | 0.064 [0.053, 0.081] | **win** | 0.067 | 0.009 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-cycles | plain | ok | 2/2 | 0.016 [0.014, 0.017] | **win** | 0.066 | 0.029 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-paths | plain | follows-closure-count incomplete (virtuoso); follows-reach incomplete (virtuoso) | 2/4 | 0.441 [0.365, 0.487] | **tie** | 0.066 | 0.028 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-sideways | plain | limit-join unchecked (nrese); limit-join unchecked (virtuoso) | 2/3 | 0.769 [0.684, 0.894] | **tie** | 0.067 | 0.011 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-snowflake | plain | ok | 1/1 | 0.324 [0.302, 0.407] | **tie** | 0.066 | 0.007 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-star | plain | ok | 2/2 | 0.261 [0.238, 0.29] | **win** | 0.067 | 0.04 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-topk | plain | top-scores wrong (virtuoso) | 1/2 | 0.008 [0.008, 0.009] | **win** | 0.066 | 0.013 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |

## jena

| Case | Semantics | Answers | Timed | Time NRESE/them [95 %] | Verdict | Load | Memory | Run |
|---|---|---|---|---|---|---|---|---|
| clients-lookups (plain-clients-lookups-capacity-r3) | plain | ok | 4/4 | 0.033 [0.031, 0.038] | **win** | 0.062 | 0.033 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-lookups (plain-clients-lookups-r3) | plain | ok | 4/4 | 0.032 [0.028, 0.036] | **win** | 0.061 | 0.033 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-sweep (plain-clients-sweep-capacity-r3) | plain | ok | 4/4 | 0.032 [0.03, 0.038] | **win** | 0.06 | 0.025 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| clients-sweep (plain-clients-sweep-r3) | plain | ok | 4/4 | 0.031 [0.029, 0.034] | **win** | 0.061 | 0.026 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| kernel-intersect | plain | ok | 2/2 | 0.013 [0.012, 0.014] | **win** | 0.133 | 0.48 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-dates | plain | ok | 3/3 | 0.013 [0.012, 0.015] | **win** | 0.062 | 0.071 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-group | plain | ok | 3/3 | 0.017 [0.016, 0.018] | **win** | 0.062 | 0.148 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-negation | plain | ok | 2/2 | 0.009 [0.008, 0.011] | **win** | 0.061 | 0.175 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-optional | plain | ok | 2/2 | 0.037 [0.036, 0.039] | **win** | 0.06 | 0.145 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| op-strings | plain | ok | 3/3 | 0.033 [0.026, 0.036] | **win** | 0.061 | 0.195 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-chain | plain | ok | 2/2 | 0.041 [0.033, 0.057] | **win** | 0.061 | 0.068 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-cycles | plain | four-cycles incomplete (jena) | 1/2 | 0.009 [0.009, 0.01] | **win** | 0.061 | 0.191 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-paths | plain | follows-closure-count incomplete (jena); follows-reach incomplete (jena) | 2/4 | 0.068 [0.056, 0.072] | **win** | 0.06 | 0.224 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-sideways | plain | limit-join unchecked (nrese); limit-join unchecked (jena) | 2/3 | 0.003 [0.002, 0.003] | **win** | 0.061 | 0.045 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-snowflake | plain | ok | 1/1 | 0.029 [0.025, 0.041] | **win** | 0.06 | 0.058 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-star | plain | ok | 2/2 | 0.008 [0.007, 0.008] | **win** | 0.061 | 0.27 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |
| q-topk | plain | ok | 2/2 | 0.009 [0.008, 0.009] | **win** | 0.061 | 0.065 (sampled) | [2026-10-10-competitors-dc2114a](runs/2026-10-10-competitors-dc2114a.toml) (office-pc, dc2114a) |

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
