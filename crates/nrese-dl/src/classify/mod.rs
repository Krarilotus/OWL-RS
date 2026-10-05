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

mod consistency;
mod driver;
pub mod inline;
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
    /// Eliminate the fresh names resolution can eliminate ([`inline`]).
    pub inline: bool,
    /// Known subsumers from the context core on the Horn part of a non-Horn ontology.
    pub horn_lower_bound: bool,
    /// Where that part is exact (nothing left out but data clauses no value needs), take
    /// its taxonomy without testing a class (else the hypertableau checks every class).
    pub exact_lower_bound: bool,
    /// Cut possible subsumers by every model's labels (else only the class's own model).
    pub model_pruning: bool,
    /// A class seen in a model needs no satisfiability test of its own where it has no
    /// candidates left (else every class gets one).
    pub skip_seen: bool,
    /// Classify without the individuals where they can't matter (no nominals): their
    /// consistency is checked once, then the tests run on the terminology alone.
    pub tbox_only: bool,
    /// Each hypertableau test's budgets (its memory budget is per worker: 256 MiB).
    pub tableau: tableau::Config,
    /// A deadline for the whole classification; what isn't decided by then is reported.
    pub timeout: Option<Duration>,
    /// The longest the Horn lower bound may take (it is only an optimisation).
    pub lower_bound_timeout: Duration,
    /// The most conclusions one join of the context core may produce, per run.
    pub max_join: usize,
    /// How `nrese-owl` normalises.
    pub normalise: nrese_owl::Options,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threads: 1,
            context_core: true,
            inline: true,
            horn_lower_bound: true,
            exact_lower_bound: true,
            model_pruning: true,
            skip_seen: true,
            tbox_only: true,
            // A class test that needs more is lost for any deadline worth having, and the
            // engine checks its budget only every few thousand steps: a lower budget keeps
            // an overshoot small (on ore_ont_9724 one test overshot 1 GiB to a 5 GiB
            // allocation).
            tableau: tableau::Config {
                max_memory: 256 << 20,
                ..tableau::Config::default()
            },
            timeout: None,
            lower_bound_timeout: Duration::from_secs(2),
            max_join: 1 << 20,
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

    /// The context core's budget within the deadline (and `limit` from now, if given).
    pub(crate) fn budget(
        &self,
        options: &Options,
        limit: Option<Duration>,
    ) -> crate::context::Budget {
        let limit = limit.map(|l| Instant::now() + l);
        let deadline = match (self.0, limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        crate::context::Budget {
            deadline,
            max_join: Some(options.max_join),
        }
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

/// With `NRESE_CLASSIFY_TRACE` set, the phases as they start, on stderr (for the lab).
pub(crate) fn trace(what: &str) {
    static ON: std::sync::OnceLock<Option<Instant>> = std::sync::OnceLock::new();
    if let Some(start) =
        ON.get_or_init(|| std::env::var_os("NRESE_CLASSIFY_TRACE").map(|_| Instant::now()))
    {
        eprintln!(
            "trace {:>9.1} ms {what}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// The workers of a run: the calling thread alone for one, else a pool of their own
/// (never rayon's global pool, so a run uses the threads it was given).
pub(crate) struct Workers(Option<rayon::ThreadPool>);

impl Workers {
    pub(crate) fn new(threads: usize) -> Self {
        if threads <= 1 {
            return Self(None);
        }
        Self(
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .ok(),
        )
    }

    /// `f` of each item, in order.
    pub(crate) fn map<T: Sync, R: Send>(
        &self,
        items: &[T],
        f: impl Fn(&T) -> R + Sync + Send,
    ) -> Vec<R> {
        use rayon::prelude::*;
        match &self.0 {
            Some(pool) => pool.install(|| items.par_iter().map(&f).collect()),
            None => items.iter().map(f).collect(),
        }
    }
}

/// Classifies `ontology`.
pub fn classify(ontology: &Ontology, options: &Options) -> Taxonomy {
    let started = Instant::now();
    let deadline = Deadline::new(options.timeout);
    let classes = crate::context::signature(ontology);
    let t = Instant::now();
    let ontology = tableau::prepared(ontology);
    trace("normalise");
    let (normalised, inlined) = clauses(&ontology, options);
    let normalise = t.elapsed();
    trace("context core");
    if options.context_core {
        let core = crate::context::Options {
            threads: options.threads,
            proofs: false,
            normalise: options.normalise,
            budget: deadline.budget(options, None),
            ..crate::context::Options::default()
        };
        let t = Instant::now();
        if let Ok(saturated) = crate::context::saturate_normalised(&normalised, &classes, &core) {
            let classification = saturated.classification();
            let profile = Profile {
                path: "context-core",
                normalise,
                inlined,
                lower_bound: t.elapsed(),
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
    trace("driver");
    let mut taxonomy = classify_normalised(&ontology, &normalised, &classes, options, deadline);
    taxonomy.profile.normalise += normalise;
    taxonomy.profile.inlined = inlined;
    taxonomy.profile.total = started.elapsed();
    taxonomy
}

/// The clauses the engines get: `nrese-owl`'s normalisation of `ontology` (prepared),
/// with the fresh names eliminated that can be ([`inline`]).
pub(crate) fn clauses(ontology: &Ontology, options: &Options) -> (Normalised, inline::Inlined) {
    let normalised = normalise_with(ontology, options.normalise);
    if options.inline {
        inline::eliminate(&normalised)
    } else {
        (normalised, inline::Inlined::default())
    }
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
