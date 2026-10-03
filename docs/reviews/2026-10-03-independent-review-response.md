# Response to the independent review of 3 October 2026

The review was made at `5ccf8db`; its bundle is held outside the repository. This document
records, for each finding, what was done and where, or why it is still open. Each fix
comes with a test that fails without it (checked by reverting the fix) unless stated.

| Finding | Status | Commit | Test |
|---|---|---|---|
| B1–B5 evaluation contracts | fixed | 23861b2 | `benches/suite/tests/test_summary.py` |
| C1 / A1 retired pipeline publishes its closure | fixed | ff266d9 | `a_retired_pipeline_leaves_the_new_closure_alone` |
| C2 / A2 cache volatility by text search | fixed: decided on the parsed query | ff266d9 | `volatile_queries_and_a_zero_budget_are_not_cached` (`RAND ()`, `UUID ()`, names in strings) |
| C3 client headers missing on five methods | fixed: one request helper | 1da9c8b | `sends the configured headers with every request` |
| C4 CLI exit 0 on HTTP errors | fixed | 1da9c8b | `report.test.ts`, loopback 403 by hand |
| C5 stock runtime config masks the build URL | fixed: the stock file sets none; `""` still means own origin | 1da9c8b | `resolveApiBaseUrl` tests |
| C6 subset ruleset fingerprints miss rule bodies | fixed: one source list for compiling and hashing | 163d6b7 | `every_added_rule_is_in_the_owl2_rl_text`; fingerprints compared before and after (only rdfs-plus, owl-horst, owl2-ql change) |
| C7 explanation size overflow | fixed: saturating | 2506e2b | `a_proof_dag_with_shared_premises_does_not_overflow_its_size` |
| C8 | see A3, A4 | | |
| P1 merge join expands a group before the limit | fixed: charged per left row before expanding | 2506e2b | `join_limits.rs` (counting allocator: 42 MB before, under 256 KiB after) |
| P2 65-pattern BGP overflow | fixed: greedy path uses a vector; beyond 64 patterns 16 starts | 2506e2b | `bgps_beyond_64_patterns_get_an_order`, `a_bgp_of_70_patterns_runs` |
| P3 path traversal not cancellable | fixed | e22ca70 | `cancelled_traversals_stop_inside` |
| P3 scans build columns before the budget is charged | **open** | | |
| P4 stream chunk is a threshold | fixed: writes are split | d6a677f | `a_large_write_goes_out_in_chunks_of_at_most_64_kib` |
| P5 unread body keeps the producer | fixed: watchdog cancels at the deadline; sends wait until it at most | d6a677f | `an_unread_body_stops_its_producer_at_the_deadline` |
| P5 graph read runs on after its timeout | fixed: the read polls its token; the timeout cancels it | this commit | `a_cancelled_graph_read_stops` |
| P5 classification and SHACL run on after their timeout | **open**: neither has a stop hook yet; the classifier gets one in the DL work (roadmap phase 3), SHACL after | | |
| P6 bounded backward derivations exhaust the join | fixed: stoppable walk for derivations only | 10b3667 | `one_derivation_asked_for_scans_one_branch` |
| P7 reach-set order, first named graph | noted; workload-dependent, to measure in the benchmark extension (ROADMAP §3.2) | | |
| A3 graph writes check only their net delta | fixed: target checked first | a83a0e1 | `every_write_is_checked_against_its_requesters_scope` (zero-net cases) |
| A4 failed marker removal never retried | fixed | a83a0e1 | `a_failed_marker_removal_is_retried` |
| A5 saved-query IRIs with `|` | fixed: non-IRI bytes encoded, existing IRIs kept | a83a0e1 | `a_user_name_with_a_bar_saves_queries` |
| A6 update-wrapped evaluation errors misclassified | fixed | a83a0e1 | `an_update_s_evaluation_error_is_classified_as_a_query_s` |

**Performance.** P1, P3 and P6 add a check per left row, per traversal step and per
scan callback (an atomic load or an integer compare). These changes have not been A/B
measured on their own; the next interleaved suite run covers the paths they touch.
