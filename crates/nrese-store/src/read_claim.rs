use nrese_sparql::Completeness;

use super::{PreparedQuery, StoreService, context_outside_dl, dl_context, status_for_its_reader};
use crate::ConsistencyStatus;
use crate::dl::{
    Verdict,
    query::{DL_CLASH, Revision},
};
use crate::entailment::Entailment;

const QUARANTINE: &str = "the store is in quarantine (the consistency rules found violations and \
                          have not found the data clean since), and as inconsistent data entail \
                          every answer, no read is certified complete until it leaves";
const CLASH: &str = "the data is inconsistent, so it entails every answer";
const RULES_HOLD: &str =
    "the store is in quarantine after the materialisation rules found consistency violations";
const RULES_CLASH: &str = "the materialisation rules found a consistency clash";

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
///
/// ```compile_fail
/// use nrese_store::Claim;
/// fn raise(mut claim: Claim) {
///     claim.complete = true;
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
    fn lower(&mut self, source: &'static str, reason: &str) {
        self.status.complete = false;
        self.note(source, reason);
    }

    fn note(&mut self, source: &'static str, reason: &str) {
        self.reported = true;
        // Keep the primary reason for an undecided answer, with room for the
        // mandatory reason among the first entries that the header can show.
        let at = usize::from(!self.status.reasons.is_empty());
        let reason = match self
            .status
            .reasons
            .iter()
            .position(|r| r.source == source && r.text == reason)
        {
            Some(current) if current <= at => return,
            Some(current) => self.status.reasons.remove(current),
            None => nrese_sparql::completeness::Reason {
                source,
                text: reason.to_owned(),
            },
        };
        self.status.reasons.insert(at, reason);
    }

    /// The non-disclosing reason for a clash or a quarantine on this read's publication.
    pub fn consistency_reason(&self) -> Option<&str> {
        self.status
            .reasons
            .iter()
            .find(|r| {
                matches!(
                    r.text.as_str(),
                    QUARANTINE | CLASH | RULES_HOLD | RULES_CLASH | DL_CLASH
                )
            })
            .map(|r| r.text.as_str())
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
}

pub(crate) enum Evidence<'a> {
    Query {
        prepared: &'a PreparedQuery,
        route: Option<Completeness>,
        rewritten: bool,
        cache: Option<&'a mut Vec<u8>>,
    },
    DlQuery {
        prepared: &'a PreparedQuery,
        route: Completeness,
        verdict: Option<Verdict>,
        cache: Option<(crate::DlAnswers, &'a mut Vec<u8>)>,
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
        let printing = match &evidence {
            Evidence::Printed { query, .. } => Some(*query),
            _ => None,
        };
        let dl_query = matches!(&evidence, Evidence::DlQuery { .. });
        let query = match &evidence {
            Evidence::Query { prepared, .. } | Evidence::DlQuery { prepared, .. } => {
                Some(prepared.reads())
            }
            _ => None,
        };
        let mut entailment = None;
        let mut dl_answer = None;
        let mut header_cache = None;
        let (status, consistency, dl_verdict) = match evidence {
            Evidence::Query {
                prepared,
                route,
                rewritten,
                cache,
            } => {
                let status = self.compose_query_status(prepared, at, route);
                if let Some(cache) = cache {
                    *cache = context_outside_dl(status.as_ref(), rewritten);
                    header_cache = Some(cache);
                }
                let told = status_for_its_reader(
                    prepared,
                    status,
                    rewritten,
                    self.config.equality_compact,
                );
                (told, ConsistencyStatus::Unknown, None)
            }
            Evidence::DlQuery {
                prepared,
                route,
                verdict,
                cache,
            } => {
                let status = self
                    .compose_query_status(prepared, at, Some(route))
                    .unwrap_or_else(Completeness::not_certified);
                if let Some((mode, cache)) = cache {
                    *cache = dl_context(mode, &status);
                    header_cache = Some(cache);
                }
                let told = status_for_its_reader(
                    prepared,
                    Some(status),
                    false,
                    self.config.equality_compact,
                );
                let verdict = verdict.filter(|_| prepared.access().is_none());
                (told, ConsistencyStatus::Unknown, verdict)
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
        let mut claim = Claim {
            reported: status.is_some(),
            status: status.unwrap_or_default(),
            consistency,
            dl_verdict,
            entailment,
            dl_answer,
        };
        let base = super::Certificate::assessment(at.base(), self.config.replica);
        let clash = matches!(base.rules, ConsistencyStatus::Inconsistent { .. });
        if clash {
            claim.consistency = base.rules;
        }
        let dl_clash = matches!(claim.dl_verdict, Some(Verdict::Inconsistent));
        let lowers = claim.reported && (query.is_some() || printing.is_some());
        let consistency_reason = if query.is_some() && !claim.reported {
            None
        } else if dl_clash && (dl_query || !lowers) {
            Some(DL_CLASH)
        } else if base.held {
            Some(if !lowers || (dl_query && clash) {
                RULES_HOLD
            } else {
                QUARANTINE
            })
        } else if clash && (dl_query || !lowers) {
            Some(RULES_CLASH)
        } else if clash || dl_clash {
            Some(CLASH)
        } else {
            None
        };
        if let Some(reason) = consistency_reason {
            let source = if reason == DL_CLASH && dl_query {
                "dl"
            } else {
                "consistency"
            };
            // A clash can leave query rows missing. It can also be the complete
            // answer of a service that decides whether the ontology has a model.
            if lowers {
                let newly_incomplete = claim.complete;
                claim.lower(source, reason);
                if newly_incomplete {
                    let reads = crate::dl::query::Reads::default();
                    if let Some(query) = query.or(printing.map(|query| (query, &reads))) {
                        crate::dl::query::negation_rule(&mut claim.status, query, "consistency");
                    }
                }
            } else {
                claim.note(source, reason);
            }
        }
        if let Some(cache) = header_cache {
            cache.push(match consistency_reason {
                Some(QUARANTINE) => 2,
                Some(RULES_HOLD) => 3,
                Some(RULES_CLASH) => 4,
                Some(DL_CLASH) => 5,
                Some(_) => 1,
                None => 0,
            });
        }
        claim
    }
}
