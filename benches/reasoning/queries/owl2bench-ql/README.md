# OWL2Bench QL queries through existentials

One conjunctive query per existential axiom of OWL2Bench's QL TBox (`Chair ⊑ ∃isHeadOf.Department`,
`… ⊑ Person ⊓ ∃worksFor.Organization`, …), its filler unprojected: the measured workload of
the OWL 2 QL rewriting (docs/design/ql-rewriting.md; docs/design/performance.md §6).
OWL2Bench's own 22 queries are in `../owl2bench`.
