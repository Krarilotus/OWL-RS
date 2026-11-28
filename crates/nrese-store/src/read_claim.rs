use nrese_sparql::{Completeness, completeness::Reason};

use super::{PreparedQuery, StoreService, status_for_its_reader};
use crate::ConsistencyStatus;
use crate::dl::{Verdict, query::Revision};
use crate::entailment::Entailment;

/// The store's assessment of an answer, composed once from the publication and its route.
/// A route supplies evidence ([`Completeness`]); only `StoreService::assess` issues a claim.
///
/// ```compile_fail
/// use nrese_sparql::Completeness;
/// use nrese_store::Claim;
/// fn issue_without_assessment(evidence: Completeness) -> Claim {
///     evidence.into()
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    status: Completeness,
    reported: bool,
    consistency: ConsistencyStatus,
    dl_verdict: Option<Verdict>,
    entailment: Option<Entailment>,
    dl_answer: Option<crate::dl::entailment::Entailed>,
}

impl std::ops::Deref for Claim {
    type Target = Completeness;

    fn deref(&self) -> &Self::Target {
        &self.status
    }
}

impl Claim {
    /// Orders or encoders may lose answers during delivery.
    pub fn lower(&mut self, reason: Reason) {
        self.status.incomplete(reason.source, reason.text);
        self.reported = true;
    }

    /// The rules' consistency assessment of the publication.
    pub fn consistency(&self) -> ConsistencyStatus {
        self.consistency
    }

    /// The Direct Semantics verdict, where the route has one.
    pub fn dl_verdict(&self) -> Option<&Verdict> {
        self.dl_verdict.as_ref()
    }

    pub(crate) fn into_report(self) -> Option<Self> {
        self.reported.then_some(self)
    }

    pub(crate) fn entailment(&self) -> crate::StoreResult<Entailment> {
        self.entailment.ok_or_else(|| {
            let why = self
                .status
                .reasons
                .first()
                .map_or("entailment was not decided", |reason| reason.text.as_str());
            crate::StoreError::Undecided(why.to_owned())
        })
    }

    pub(crate) fn dl_answer(&self) -> Option<&crate::dl::entailment::Entailed> {
        self.dl_answer.as_ref()
    }

    pub(super) fn cache_status(&self) -> Option<&Completeness> {
        self.reported.then_some(&self.status)
    }
}

pub(crate) enum Evidence<'a> {
    Query {
        prepared: &'a PreparedQuery,
        route: Option<Completeness>,
        rewritten: bool,
    },
    Service {
        status: Completeness,
        verdict: Option<Verdict>,
    },
    Classification {
        incomplete: &'a [String],
        verdict: Verdict,
        outside: Option<nrese_sparql::Outside>,
    },
    Printed {
        query: &'a nrese_sparql_syntax::Query,
        status: Completeness,
    },
    Publication(Option<Verdict>),
    RulesEntailment(RulesEntailment),
    DlEntailment {
        premise: Verdict,
        answer: &'a crate::dl::entailment::Entailed,
    },
}

pub(crate) enum RulesEntailment {
    Closure(bool),
    InconsistentPremise,
    Undecided(String),
}

impl StoreService {
    pub(crate) fn assess(&self, at: Revision<'_>, evidence: Evidence<'_>) -> Claim {
        let snapshot = at.snapshot();
        let mut entailment = None;
        let mut dl_answer = None;
        let (status, consistency, dl_verdict) = match evidence {
            Evidence::Query {
                prepared,
                route,
                rewritten,
            } => {
                let status = self.compose_query_status(prepared, at, route);
                let told = status_for_its_reader(
                    prepared,
                    status,
                    rewritten,
                    self.config.equality_compact,
                );
                (told, ConsistencyStatus::Unknown, None)
            }
            Evidence::Service { status, verdict } => {
                (Some(status), ConsistencyStatus::Unknown, verdict)
            }
            Evidence::Classification {
                incomplete,
                verdict,
                outside,
            } => {
                let mut status = Completeness::under(nrese_sparql::Regime::Owl2Dl);
                for why in incomplete {
                    status.incomplete("dl", why.clone());
                }
                status.outside = outside;
                (Some(status), ConsistencyStatus::Unknown, Some(verdict))
            }
            Evidence::Printed { query, mut status } => {
                status.regime = Some(nrese_sparql::Regime::Owl2Rl);
                let reads = crate::dl::query::Reads::default();
                crate::dl::query::certify_report(
                    Some(&mut status),
                    (query, &reads),
                    Some(snapshot),
                    self,
                );
                (Some(status), ConsistencyStatus::Unknown, None)
            }
            Evidence::Publication(verdict) => (None, self.consistency_at(snapshot, None), verdict),
            Evidence::RulesEntailment(evidence) => {
                let consistency =
                    super::Certificate::assessment(snapshot, self.config.replica).rules;
                let regime = super::Certificate::record(snapshot)
                    .map_or(nrese_sparql::Regime::Rdfs, |record| {
                        nrese_sparql::Regime::of_ruleset(&record.state.ruleset)
                    });
                let mut status = Completeness::under(regime);
                entailment = match evidence {
                    RulesEntailment::Closure(true) => Some(Entailment::Entailed),
                    RulesEntailment::Closure(false) => Some(Entailment::NotEntailed),
                    RulesEntailment::InconsistentPremise => Some(Entailment::InconsistentPremise),
                    RulesEntailment::Undecided(why) => {
                        status.incomplete("rules", why);
                        None
                    }
                };
                (Some(status), consistency, None)
            }
            Evidence::DlEntailment { premise, answer } => {
                use crate::dl::entailment::Entailed;
                let mut status = Completeness::under(nrese_sparql::Regime::Owl2Dl);
                entailment = match (&premise, answer) {
                    (Verdict::Inconsistent, Entailed::Yes) => Some(Entailment::InconsistentPremise),
                    (_, Entailed::Yes) => Some(Entailment::Entailed),
                    (_, Entailed::No) => Some(Entailment::NotEntailed),
                    (_, Entailed::Unknown(why)) => {
                        status.incomplete("dl", format!("under OWL 2 DL: {why}"));
                        None
                    }
                };
                dl_answer = Some(answer.clone());
                (Some(status), ConsistencyStatus::Unknown, Some(premise))
            }
        };
        Claim {
            reported: status.is_some(),
            status: status.unwrap_or_default(),
            consistency,
            dl_verdict,
            entailment,
            dl_answer,
        }
    }
}
