//! Classification and realisation (docs/design/owl2-dl.md §7, package 3.4): never one test
//! per pair of classes.
//!
//! - **Dispatch:** an ontology the context core's Horn stage takes is classified there
//!   completely ([`crate::context`]); every other goes to the hypertableau driver below.
//! - **Known subsumers** `K(C)`: the Horn part's (the context core on the Horn clauses, a
//!   lower bound: fewer clauses, fewer consequences) and the deterministic part of the
//!   label of `C`'s model in the hypertableau ([`known`]).
//! - **Possible subsumers** `P(C)`: the label of `C`'s model, cut down by every label `C` is
//!   part of in any model the driver sees (HermiT's pruning; Glimm, Horrocks, Motik,
//!   Shearer and Stoilos, *A Novel Approach to Ontology Classification*, JWS 2012). Only
//!   `P(C) \ K(C)` is tested, most general first: a non-subsumption rules out every known
//!   subclass of the candidate, and its model's label rules out what it leaves out
//!   ([`driver`]).
//! - **Parallel:** satisfiability tests run in waves over every core, a class seen in a
//!   model needs none; candidate tests run per class over every core; only the merging of
//!   results is serialised.
//! - **Realisation** is the same over individuals, with the consistency run's labels as
//!   known and possible types and every refutation's model pruning the others
//!   ([`realise`]).
//!
//! The output is the context core's [`Classification`] (whose `canonical` is the DL lab's
//! taxonomy format) and a [`Realisation`] in the reference runner's format; both report
//! whether they are complete and, if not, why.

mod driver;
pub mod known;
mod profile;
pub mod realise;

use std::time::{Duration, Instant};

use nrese_owl::{Normalised, Ontology, Term, normalise_with};

pub use crate::context::Classification;
use crate::tableau;
pub use profile::Profile;
pub use realise::{Realisation, realise};

/// How to classify; every optimisation has a switch ("on" and "off" give the same
/// taxonomy).
#[derive(Debug, Clone)]
pub struct Options {
    /// Workers; 1 runs everything on the calling thread.
    pub threads: usize,
    /// Classify Horn ontologies with the context core (else the tableau driver only).
    pub context_core: bool,
    /// Known subsumers from the context core on the Horn part of a non-Horn ontology.
    pub horn_lower_bound: bool,
    /// Cut possible subsumers by every model's labels (else only the class's own model).
    pub model_pruning: bool,
    /// A class seen in a model needs no satisfiability test of its own.
    pub skip_seen: bool,
    /// Classify without the individuals where they can't matter (no nominals): their
    /// consistency is checked once, then the tests run on the terminology alone.
    pub tbox_only: bool,
    /// Each hypertableau test's budgets (its memory budget is per worker).
    pub tableau: tableau::Config,
    /// A deadline for the whole classification; what isn't decided by then is reported.
    pub timeout: Option<Duration>,
    /// How `nrese-owl` normalises.
    pub normalise: nrese_owl::Options,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threads: 1,
            context_core: true,
            horn_lower_bound: true,
            model_pruning: true,
            skip_seen: true,
            tbox_only: true,
            tableau: tableau::Config {
                max_memory: 1 << 30,
                ..tableau::Config::default()
            },
            timeout: None,
            normalise: nrese_owl::Options::default(),
        }
    }
}

/// A classification with whether it is complete.
#[derive(Debug, Clone, Default)]
pub struct Taxonomy {
    pub classification: Classification,
    /// Why it isn't complete (empty: it is): budgets that ran out, parts of the ontology
    /// left out. What it does contain is entailed.
    pub incomplete: Vec<String>,
    pub profile: Profile,
}

impl Taxonomy {
    pub fn complete(&self) -> bool {
        self.incomplete.is_empty()
    }
}

/// The deadline of a run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deadline(Option<Instant>);

impl Deadline {
    pub(crate) fn new(timeout: Option<Duration>) -> Self {
        Self(timeout.map(|t| Instant::now() + t))
    }

    pub(crate) fn passed(&self) -> bool {
        self.0.is_some_and(|d| Instant::now() >= d)
    }

    /// The tableau's per-test configuration within the deadline.
    pub(crate) fn config(&self, base: &tableau::Config) -> tableau::Config {
        let mut c = base.clone();
        if let Some(d) = self.0 {
            let left = d.saturating_duration_since(Instant::now());
            c.timeout = Some(c.timeout.map_or(left, |t| t.min(left)));
        }
        c
    }
}

/// Runs `f` on a pool of `threads` workers (on the calling thread for one).
pub(crate) fn with_pool<T: Send>(threads: usize, f: impl FnOnce() -> T + Send) -> T {
    if threads <= 1 {
        return f();
    }
    match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(f),
        Err(_) => f(),
    }
}

/// Classifies `ontology`.
pub fn classify(ontology: &Ontology, options: &Options) -> Taxonomy {
    let started = Instant::now();
    let deadline = Deadline::new(options.timeout);
    let classes = crate::context::signature(ontology);
    if options.context_core {
        let core = crate::context::Options {
            threads: options.threads,
            proofs: false,
            normalise: options.normalise,
            ..crate::context::Options::default()
        };
        if let Ok((classification, p)) = crate::context::classify(ontology, &core) {
            let profile = Profile {
                path: "context-core",
                normalise: p.normalise,
                lower_bound: p.compile + p.saturate + p.assemble,
                classes: classes.len(),
                total: started.elapsed(),
                threads: options.threads,
                ..Profile::default()
            };
            return Taxonomy {
                classification,
                incomplete: Vec::new(),
                profile,
            };
        }
    }
    let t = Instant::now();
    let ontology = tableau::prepared(ontology);
    let normalised = normalise_with(&ontology, options.normalise);
    let normalise = t.elapsed();
    let mut taxonomy = classify_normalised(&ontology, &normalised, &classes, options, deadline);
    taxonomy.profile.normalise += normalise;
    taxonomy.profile.total = started.elapsed();
    taxonomy
}

/// Classifies `normalised` (of `ontology`, prepared) over `classes` with the tableau driver.
pub(crate) fn classify_normalised(
    ontology: &Ontology,
    normalised: &Normalised,
    classes: &[Term],
    options: &Options,
    deadline: Deadline,
) -> Taxonomy {
    driver::Driver::new(ontology, normalised, classes, options, deadline).classify()
}
