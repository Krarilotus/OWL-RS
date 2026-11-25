use super::error::{RejectKind, UndecidedCause};
use crate::{StoreService, WhyUndecided};

/// What the consistency gate does when the active profile cannot decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndecidedPolicy {
    Mark,
    Refuse,
}

impl UndecidedPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mark => "mark",
            Self::Refuse => "refuse",
        }
    }
}

pub(crate) enum Route {
    Commit,
    Published,
}

#[derive(PartialEq, Eq)]
pub(crate) enum Disposition {
    Marked,
    Refused(UndecidedCause),
    Quarantined,
}

impl Disposition {
    pub(crate) fn refusal(self) -> Option<RejectKind> {
        match self {
            Self::Refused(cause) => Some(RejectKind::Undecided(cause)),
            _ => None,
        }
    }

    pub(crate) fn published_status(self) -> &'static str {
        match self {
            Self::Marked => "marked",
            Self::Quarantined => "quarantined",
            Self::Refused(_) => unreachable!("a published verdict cannot refuse a commit"),
        }
    }
}

impl StoreService {
    pub(crate) fn dispose(&self, why: WhyUndecided, route: Route) -> Disposition {
        use Route::{Commit, Published};
        use UndecidedPolicy::{Mark, Refuse};
        use WhyUndecided::NoQlDecider;

        match (why, self.config().gates_undecided, route) {
            (NoQlDecider, Mark, _) => Disposition::Marked,
            (NoQlDecider, Refuse, Commit) => Disposition::Refused(UndecidedCause::NoDecider),
            (_, _, Commit) => Disposition::Refused(UndecidedCause::ReadsDifferently),
            (_, _, Published) => Disposition::Quarantined,
        }
    }
}
