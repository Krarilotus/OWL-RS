//! What the bounds cost and how tight they are: the times of compiling U1 and of
//! evaluating L and U1, and the sizes of the program, the closures and the gap (package
//! 3.2's telemetry). The evaluations run in the caller's engine, so the caller fills in
//! their times and closure sizes.

use std::fmt;
use std::time::Duration;

use super::gap::GapReport;
use super::program::{Origin, Program};

/// The program's size, by where its rules came from and how they approximate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgramSize {
    pub rules: usize,
    /// Rules from clauses (with at-most rules), chains and keys.
    pub clause_rules: usize,
    pub chain_rules: usize,
    pub key_rules: usize,
    /// `⊥` rules: from clauses with empty heads, Skolem distinctness, assertions.
    pub bottom_rules: usize,
    /// Rules that approximate nothing.
    pub exact_rules: usize,
    pub split_rules: usize,
    pub skolem_rules: usize,
    pub at_most_rules: usize,
    /// Rules of at-least atoms collapsed to fewer constants.
    pub collapsed_rules: usize,
    /// Clauses whose datatype conditions U1 doesn't check ([`Program::unchecked`]).
    pub unchecked_clauses: usize,
    pub facts: usize,
    pub skolems: usize,
    pub fresh_classes: usize,
}

impl ProgramSize {
    pub fn of(program: &Program) -> Self {
        let mut s = Self {
            rules: program.rules.len(),
            facts: program.facts.len(),
            skolems: program.names.skolems.len(),
            fresh_classes: program.names.fresh.len(),
            unchecked_clauses: program.unchecked.len(),
            ..Self::default()
        };
        for rule in &program.rules {
            let a = rule.provenance.approximations;
            match rule.provenance.origin {
                Origin::Clause(_) => s.clause_rules += 1,
                Origin::Chain => s.chain_rules += 1,
                Origin::Key => s.key_rules += 1,
                _ => {}
            }
            s.bottom_rules += usize::from(a.bottom);
            s.exact_rules += usize::from(a.exact());
            s.split_rules += usize::from(a.split);
            s.skolem_rules += usize::from(a.skolem);
            s.at_most_rules += usize::from(a.at_most);
            s.collapsed_rules += usize::from(a.collapsed);
        }
        s
    }
}

/// One run of the bounds: times and sizes.
#[derive(Debug, Clone, Default)]
pub struct Telemetry {
    /// Normalising the ontology into clauses (`nrese-owl`).
    pub normalise: Duration,
    /// Compiling U1 from the clauses.
    pub compile: Duration,
    pub evaluate_lower: Duration,
    pub evaluate_upper: Duration,
    pub program: ProgramSize,
    /// The input facts, and the closures' sizes (input included).
    pub input: usize,
    pub lower_facts: usize,
    pub upper_facts: usize,
    /// The answers' bounds and their gap.
    pub gap: GapReport,
}

impl fmt::Display for Telemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let p = &self.program;
        writeln!(
            f,
            "times (ms): normalise {:.1}, compile {:.1}, evaluate L {:.1}, evaluate U1 {:.1}",
            ms(self.normalise),
            ms(self.compile),
            ms(self.evaluate_lower),
            ms(self.evaluate_upper)
        )?;
        writeln!(
            f,
            "program: {} rules ({} from clauses, {} chains, {} keys, {} bottom; {} exact, \
             {} split, {} skolem, {} at-most, {} collapsed), {} facts, {} skolem constants, \
             {} fresh classes, {} clauses with unchecked data",
            p.rules,
            p.clause_rules,
            p.chain_rules,
            p.key_rules,
            p.bottom_rules,
            p.exact_rules,
            p.split_rules,
            p.skolem_rules,
            p.at_most_rules,
            p.collapsed_rules,
            p.facts,
            p.skolems,
            p.fresh_classes,
            p.unchecked_clauses
        )?;
        writeln!(
            f,
            "closures: input {}, L {}, U1 {}",
            self.input, self.lower_facts, self.upper_facts
        )?;
        let g = &self.gap;
        write!(
            f,
            "answers: L {}, U1 {}, gap {}, open data values {}, L not in U1 {}; {} of {} predicate queries exact; \
             clashes {}, consistency proved: {}",
            g.lower,
            g.upper,
            g.gap,
            g.open,
            g.lower_only,
            g.exact_queries,
            g.predicates.len(),
            g.clashes,
            g.consistent
        )
    }
}
