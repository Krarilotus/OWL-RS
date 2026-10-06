# Reasoning: supported semantics

This is the contract for what NRESE's reasoning computes, as implemented on 30 September 2026 (`SEMANTICS_VERSION` 2). A mode name alone doesn't state it; this page does. The source of truth is `crates/nrese-reasoner/src/rulesets.rs` (the rule text) and `lists.rs` (list axioms).

## Modes

`reasoner.mode` / `NRESE_REASONING_MODE`: `disabled`, `rdfs`, `rdfs-full`, `rdfs-plus`, `owl-horst`, `owl2-ql`, `owl2-rl` or `custom`. The profiles between `rdfs` and `owl2-rl` are the RDFS rules plus named OWL 2 RL rules, as GraphDB's rulesets of the same names are. User rules in Notation3 (`reasoner.rules`) are added to any of them, or are the whole program with `custom` (see "User rules" below).

| Mode | Rules |
|---|---|
| `rdfs` | the 6-rule subset below |
| `rdfs-full` | rdfD2, rdfs2 to rdfs13 (rdfs4a/b included) and 50 axiomatic triples: the RDF and RDFS axioms without the infinitely many about `rdf:_1`, `rdf:_2`, …, and rdfs1 for `rdf:langString`, `rdf:HTML`, `rdf:XMLLiteral` and `xsd:string`. rdfD1 (a blank node per literal) is left out. Axioms seed every closure and no deletion retracts them |
| `rdfs-plus` | `rdfs` + eq-sym, eq-trans, eq-rep-s/p/o, prp-fp, prp-ifp, prp-symp, prp-trp, prp-eqp1/2, prp-inv1/2, cax-eqc1/2, scm-eqc1/2, scm-eqp1/2 |
| `owl-horst` | `rdfs-plus` + cls-hv1/2, cls-svf1/2, cls-avf |
| `owl2-ql` | prp-dom, prp-rng, prp-spo1, prp-eqp1/2, prp-inv1/2, prp-symp, cax-sco, cax-eqc1/2, cls-svf2, the schema rules scm-cls, sco, eqc1/2, op, dp, spo, eqp1/2, dom1/2, rng1/2, and the checks prp-asyp, prp-irp, prp-pdw, cls-nothing2, cax-dw. Existentials on the right of `SubClassOf` are not materialised (they would invent individuals); queries get the answers through them by tree-witness rewriting (`reasoner.ql_rewriting = "auto"`, the default; [design](../design/ql-rewriting.md)), each answer saying whether it is complete |
| `owl2-rl` | below. Its standard semantics: the OWL 2 RL/RDF rules' closure, no answers through existentials on the right, so answer counts equal other systems' `owl2-rl`. A repository may add the QL rewriting (`ql_rewriting = "on"`, per repository or `reasoner.ql_rewriting`); `owl2-dl` mode gets those answers through its bounds |

### Unnamed union classes

With `reasoner.unnamed_classes = "skip"`, an unnamed class that has an `owl:unionOf` and occurs only as the object of `rdfs:domain`, `rdfs:range`, `rdf:type`, `rdfs:subClassOf` or `owl:allValuesFrom`, or in its own definition, gets no inferred members: no rule consumes them and no query can name the class. A membership of such a class declared `owl:Class` becomes an `owl:Thing` membership where the ruleset has scm-cls. The closure is otherwise the full one (property test over every ruleset).

### `rdfs`: a 6-rule RDFS subset

| Rule | Derives |
|---|---|
| rdfs2, rdfs3 | domain and range typing |
| rdfs5, rdfs11 | transitive `subPropertyOf`, `subClassOf` |
| rdfs7, rdfs9 | subproperty and subclass propagation |

**Not included:**
- the remaining RDFS entailment rules (rdfs1, rdfs4a/b, rdfs6, rdfs8, rdfs10, rdfs12, rdfs13 and the `rdf:` rules);
- axiomatic triples.

This matches GraphDB's `rdfs` with *partialRDFS*. Full RDFS entailment is `rdfs-full`.

### `owl2-rl`: OWL 2 RL/RDF rules

**Included:**
- **Fixed rules (58):** W3C OWL 2 Profiles §4.3 tables 4 (equality), 5 (properties), 6 (classes), 7 (class axioms) and 9 (schema vocabulary), with the W3C rule names.
- **One extra `eq-diff1`:** `x owl:differentFrom x` is inconsistent (see `eq-ref` below).
- **List axioms, instantiated per axiom:** `prp-spo2` (property chains), `prp-key`, `cls-int1/2`, `cls-uni`, `cls-oo`, `scm-int`, `scm-uni`, `cax-adc`, `eq-diff2/3`, `prp-adp`.

**Omitted, and what that means:**

| Omitted | Consequence |
|---|---|
| `eq-ref` (`x sameAs x` for every term) | Not materialised (it would double the store). Its consistency consequences are kept: `x differentFrom x`, and AllDifferent lists naming one individual twice, are inconsistent. Queries for `?x owl:sameAs ?x` don't return reflexive pairs. |
| Datatype rules, table 8, in part | `dt-not-type` and `dt-diff` are checked: a value outside a property's range datatype (a string where the range is `xsd:integer`, an integer where it is `xsd:double`, whose value spaces are disjoint in OWL 2, 300 for `xsd:byte`) and two different data values made the same (a functional data property with two values) are inconsistencies, on materialisation and on commits. Not done: `dt-type1/2` and `dt-eq` (literals aren't typed or equated by value, so `"1"^^xsd:integer` and `"01"^^xsd:integer` stay two terms in answers, though the checks treat them as one value); ill-typed literals and datatypes outside OWL 2's list aren't judged. |
| Axiomatic triples | Not materialised (as W3C allows for the RL/RDF rules). |

**List limits:**
- Lists of any length are instantiated. `owl:AllDifferent`, `owl:AllDisjointClasses` and `owl:AllDisjointProperties` over more than 100 members become one rule each whose pairs are checked through an index of the members (a rule per pair would be quadratic); shorter ones a rule per pair. The cost differs by kind: the long `owl:AllDifferent` rule starts from `owl:sameAs` statements and the long `owl:AllDisjointClasses` rule from an individual's types, but the long `owl:AllDisjointProperties` rule (`prp-adp`) joins every statement with every other statement of the same subject and object, whatever the properties: on a large store, a full materialisation with such an axiom is markedly slower (commits only check their new statements). Disjoint property sets of over 100 members are rare; split into axioms of at most 100, they are a rule per pair again.
- A property chain or a key with more than 250 properties can't be one rule (the rule format's variables) and isn't instantiated (`list-too-long`).
- A list axiom is instantiated for at most 64 member sequences (when `sameAs` makes several nodes one list).
- Those, malformed (a node without `rdf:first`/`rdf:rest`) and cyclic lists are not instantiated, never truncated. Each one is reported as a typed diagnostic (`malformed-list`, `cyclic-list`, `list-too-long`, `too-many-list-variants`) with the axiom and node decoded:
  - full materialisations (load, startup) report all of them: in the log, and under `last_materialisation` in `/ops/api/diagnostics/reasoning`;
  - a commit reports those it introduced: in the log, and under `last_run`.

**Generalised triples:**
- Full materialisation may derive intermediate triples RDF can't store: a literal subject, or a non-IRI predicate.
- Only storable facts are persisted. Commit-path reasoning reads persisted facts, so it doesn't see the dropped ones.
- This only affects rules that would turn such triples back into storable facts, for example `owl:inverseOf` on a datatype property, which OWL 2 doesn't allow.

**Completeness:** W3C states the conditions under which the RL/RDF rules are complete for OWL 2 RL ontologies. Datasets labelled EL, QL or DL are reasoned with these RL rules, and results are RL entailments, not EL/QL/DL reasoning.

## Graph scope

- Rules match over the **union of all graphs** (default and named).
- Inferred statements go to the **default graph**.
- An inferred statement is suppressed while it is asserted in any graph.
- A named graph is **not** a reasoning boundary: there's no per-graph isolation or inferred provenance yet (planned: R9, per-graph placement).

## Reads

- **Default:** asserted plus inferred (`Materialised`).
- **Per request:**
  - `infer=false`, or `FROM <http://www.ontotext.com/explicit>`: asserted only;
  - `FROM <http://www.ontotext.com/implicit>`: inferred only.
- Graph Store reads use the default.

## Consistency

- A commit whose closure violates a consistency rule is **rejected**. The response carries an explanation: the rule, its premises (asserted or inferred) and the likely commit-local trigger.
- Commit-path checking examines what the commit adds, so it relies on a consistent baseline. When a full materialisation finds violations (data imported without reasoning, or a ruleset switched on later), the store is **quarantined**:
  - `/readyz` answers 503 with status `quarantined` and the violation count;
  - reads work;
  - consistent commits are accepted and each one revalidates the whole store, until the data is repaired.
- `nrese-server load` fails when the loaded data is inconsistent (the data stays loaded).

## Freshness

The reasoning state (`reasoning.state` in the data directory) records:
- the ruleset;
- its **semantic fingerprint** (the rule text and `SEMANTICS_VERSION`);
- the violation count.

Startup skips rematerialisation only if the ruleset and the fingerprint match. A new build that changes what a ruleset derives rebuilds once. `nrese-server check-config` prints the fingerprint. Writes that don't maintain the inferred stack (bulk load, ungated tools, commits with reasoning off, a store changed while opening) drop the state, and the next reasoning write or startup rematerialises.

## Maintenance

- Commits maintain the inferred stack incrementally: DRed with backward/forward proofs, transitive and equality modules.
- It's equal to full rematerialisation on 84,000 random changes and 3,000 random engine commits (differential tests).

## User rules (Notation3)

`reasoner.rules` / `NRESE_REASONING_RULES` names a Notation3 file. Its rules compile into the same rule IR as the built-in rulesets (`crates/nrese-reasoner/src/n3.rs`), so they are materialised, maintained on every commit, and checked for consistency the same way.

| N3 | Meaning |
|---|---|
| `{ ?x :p ?y . ?y :p ?z } => { ?x :q ?z } .` | a rule; `?x` are its variables, and so are blank nodes in the premises |
| `{ head } <= { body } .` | the same rule written the other way round |
| `{ … } => false .` | a consistency rule: commits that make it hold are rejected |
| `?x log:notEqualTo ?y` | a guard |
| `?x log:equalTo ?y` | the two are one (a variable takes the other's place) |
| plain triples | facts that hold whatever the data, never retracted |

The rules' name in the materialisation state and on `/version` is `custom:<file>` (`owl2-rl+custom:<file>` when added to a ruleset). Its fingerprint covers the file's text, so a changed file rebuilds the closure.

Not supported, with a startup error naming the rule: other builtins (`math:`, `string:`, `list:`, `time:`, and the rest of `log:`), blank nodes in conclusions (existentials), formulas as terms and rules inside formulas. W3C N3 Community Group reasoner tests: all 13 within this scope pass (`crates/nrese-reasoner/tests/w3c_n3_reasoner`). Of the other 76, 49 use cwm options other than "rules to a fixpoint, then the data", 13 need builtins, and the rest need formulas or generalised triples.
