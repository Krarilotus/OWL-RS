# sparq's Nemo encoding (vendored unchanged)

`owl-rl.rls` is sparq's validated Nemo encoding of the OWL 2 RL closure it computes on
LUBM, copied byte for byte:

- **Source:** github.com/sparq-org/sparq, commit `674f50b5a3f12e17b912f5a004c5ab377147f735` (2026-09-27), path `bench/reason-encodings/nemo/owl-rl.rls`.
- **Licence:** MIT, Copyright (c) 2026 Jesse Wright ([LICENSE](LICENSE)).

**What it is.** It is LUBM-tailored. Its own header says it encodes RDFS plus exactly the
OWL rules the LUBM TBox exercises: `prp-inv`, `prp-trp`, `cls-svf1`, `cls-int1`, `scm-int`,
`scm-dom1`/`rng1`, `scm-svf2`, and domain and range propagation along subPropertyOf and
inverseOf. It has no equality, cardinality, union, oneOf, hasValue, chains or keys, and no
consistency rules. sparq validated it set for set against its own closure of LUBM(1). It is
**not** the full OWL 2 RL rule set that NRESE, owlrl and our two general encodings
compute, so its closure is compared with that difference stated.

**How the suite runs it** (`nemo-sparq` in `benches/suite/systems.toml`). The file stays
unchanged; at run time the adapter, on a copy:
- replaces its `@@DATA@@` placeholder with the input file;
- drops its `@export closed ...` line;
- appends the suite's output rule: `INFERRED(?s, ?p, ?o) :- closed(?s, ?p, ?o), ~triple(?s, ?p, ?o) .`, exported as `inferred.nt`, the same contract as our encodings.
