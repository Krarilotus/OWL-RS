# LUBM queries through existentials

Conjunctive queries over LUBM whose unprojected variables match the existentials of
`univ-bench.owl` (`Employee ⊑ ∃worksFor.Organization`, `Student ⊑ ∃takesCourse.Course`, …):
the measured workload of the OWL 2 QL rewriting (docs/design/ql-rewriting.md;
docs/design/performance.md §6). LUBM's own fourteen queries project every variable.
