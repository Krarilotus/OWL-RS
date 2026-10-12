# Standings: NRESE against every contender

Generated 2026-10-12 by `python benches/suite/suite.py standings --write` from the run records' `versus` entries ([runs/](runs/README.md)). Don't edit it by hand.

Per case and contender, the newest comparison on each host: answers first (each system against the reference systems; a wrong, missing or unchecked answer is never timed), then NRESE's time over the queries both answered right, as NRESE / contender with its 95 % bootstrap interval (below 1: NRESE faster). **win**: faster beyond max(10 %, the spread) and 2 ms, the interval below 1; **loss**: the other way round; **tie**: neither; **no-verdict**: nothing both answered right. Load and memory (serve peak, with its kind) as the same ratio. A case marked ★ is the contender's home turf ([cases/](cases/README.md) `home_turf`).

## Summary

| Contender | Home turf: win / tie / loss / no verdict | All cases: win / tie / loss / no verdict | Answers not ok | Newest |
|---|---|---|---|---|
| qlever | 2 / 0 / 0 / 0 | 14 / 4 / 0 / 0 | 2 | 2026-10-10 |
| oxigraph | - | 13 / 5 / 0 / 0 | 3 | 2026-10-10 |
| virtuoso | 0 / 2 / 0 / 0 | 8 / 7 / 1 / 0 | 3 | 2026-10-10 |
| jena | - | 17 / 0 / 0 / 0 | 3 | 2026-10-10 |
| rdf4j | - | - | - | not measured yet |
| nemo | - | - | - | not measured yet |
| owlrl | - | - | - | not measured yet |
| hermit | - | - | - | not measured yet |
| openllet | - | - | - | not measured yet |
| konclude | - | - | - | not measured yet |
| elk | - | - | - | not measured yet |
| graphdb | restricted | restricted | - | ran in 7 runs; results stay local |
| rdfox | refused | refused | - | no evaluation licence (12 Oct 2026): its published numbers only, as reported; 0 published numbers below |
| allegrograph | refused | refused | - | no evaluation licence (12 Oct 2026): its published numbers only, as reported; 0 published numbers below |
| stardog | restricted | restricted | - | not run |
| anzograph | restricted | restricted | - | not run |

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
