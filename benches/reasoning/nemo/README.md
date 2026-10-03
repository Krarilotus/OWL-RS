# Nemo encodings

Nemo (TU Dresden's Rust datalog engine, pinned in the [Dockerfile](Dockerfile)) runs three
encodings. The suite treats each one as a system of its own (`benches/suite/systems.toml`),
so every Nemo number names the rules it ran. The image holds only the binary. The suite
mounts the rules from this directory at run time and records each file's SHA-256 in the
step's note.

| System | File | What it is | Paper label |
|---|---|---|---|
| `nemo` | [owl2rl.rls](owl2rl.rls) | Our translation of the W3C OWL 2 RL/RDF rule tables (Tables 4–7, 9), schema atoms written inline | general |
| `nemo-schemafirst` | [owl2rl-schemafirst.rls](owl2rl-schemafirst.rls) | The same rules; each rule's schema atoms are folded into a small relation first (sparq's technique, applied to the full rule set) | general |
| `nemo-sparq` | [sparq/owl-rl.rls](sparq/) | sparq's encoding, vendored unchanged (MIT, commit `674f50b`): RDFS plus the OWL rules the LUBM TBox uses | LUBM-tailored |

`nemo-sparq` runs only on LUBM: the suite skips it on inputs without `univ-bench.nt`.
Elsewhere its closure is incomplete by design and wouldn't be a result.

## Closure checks (3 October 2026, main PC)

These are set-for-set comparisons of the inferred statements, without timing.
- **Blank nodes** are compared up to renaming: each blank node gets a name from its
  statements with IRIs and literals.
- **owlrl comparisons** use the benchmark's normalisation (`../compare_inferred.py`).
- **Tools:** NRESE's set comes from `reason_query --inferred-out` (nrese-store example), and
  each Nemo set from the encoding run exactly as the suite runs it.

| Tier | NRESE | `nemo` | `nemo-schemafirst` | `nemo-sparq` |
|---|---|---|---|---|
| LUBM 1 | 67,253 | 67,221 | 67,221 | 49,751 |
| LUBM 10 | 830,003 | 829,971 | 829,971 | 622,249 |
| OWL2Bench RL-1 | 1,399,563 | 1,399,531 | (pending) | not run (LUBM only) |

**NRESE against the general encodings.** The sets are identical, blank nodes included,
except for **exactly 32 statements** on every tier. These are `dt rdf:type rdfs:Datatype`
for the 32 datatypes of the OWL 2 RL datatype map (`rdf:PlainLiteral`, `rdf:XMLLiteral`,
`rdfs:Literal` and 29 XSD types). Rule **dt-type1** (Table 8) derives them. Both general
encodings leave out Table 8 and the axiomatic triples, as their headers state.
`compare_inferred.py` drops statements about RDF/RDFS/XSD vocabulary, so the normalised
comparison shows no difference.

**`nemo` against `nemo-schemafirst`.** Identical on LUBM 1 and 10, blank nodes included;
OWL2Bench RL-1 is pending.

**Against the owlrl reference (LUBM 1, normalised).** NRESE, `nemo-schemafirst` and
`nemo-sparq` all have instance precision and recall 1.0 (37,935 statements each).

**`nemo-sparq`'s schema extras.** Its schema precision is 0.91: on LUBM 1 and 10 it
derives 6 statements that OWL 2 RL/RDF doesn't:
- `memberOf`, `worksFor` and `headOf` each get an `rdfs:domain Person` and an
  `rdfs:range Organization`;
- the source is lines 118–120 of `sparq/owl-rl.rls`, which swap domain and range across
  `owl:inverseOf` (`member` has domain Organization and range Person);
- the file labels them "scm-dom2/scm-rng2", but those rules propagate along
  `rdfs:subPropertyOf`, and no OWL 2 RL rule swaps across an inverse;
- they are sound under the OWL semantics, and every instance-level statement still
  matches.

## Times so far

These are single runs on the main PC, which was also building at the time: indicative
only. The benchmark runs on the office PC (batch B) decide.

| Tier | `nemo` | `nemo-schemafirst` | `nemo-sparq` | NRESE (closure) |
|---|---|---|---|---|
| LUBM 1 | 3.5 s | 2.9–3.5 s | 0.9 s | 0.04 s |
| LUBM 10 | 37.1 s | 40.0 s | 11.3 s | 0.32 s |
| OWL2Bench RL-1 | 1,881 s | over 36 min (still running at writing) | – | 0.59 s |

So far, folding the schema first doesn't make the general encoding faster. sparq's
encoding also does less work: it has no equality, cardinality, union, oneOf, hasValue,
chain or key rules, and no consistency rules. How much of the time difference each
omission accounts for hasn't been measured.

**sparq's 55.1 s** is Nemo v0.9.1's self-reported `Reasoning:` time with this encoding on
LUBM(100) (13,880,276 input triples, TBox included), on a dedicated AWS box, best of 3
(sparq `bench/canonical-competitor-results/2026-07-10/materialize-lubm100-*.json`). Batch
B measures the same encoding on our LUBM(100), under these conditions:
- **Nemo version:** v0.10.1 (theirs v0.9.1);
- **Input:** 13,405,674 triples (another generator run, so different data);
- **Hardware:** the office PC;
- **Recorded time:** the step's `reason_ms`, Nemo's self-reported reasoning time.
