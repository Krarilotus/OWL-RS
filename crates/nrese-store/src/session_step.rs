//! Transaction session transitions. Observations come from the caller, effects go back as values.
#![deny(clippy::wildcard_enum_match_arm)]

#[cfg(test)]
#[path = "session_step_tests.rs"]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "the mutations override individual cells"
)]
mod session_step_tests;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum State {
    U,
    S,
    P,
    Pw,
    C,
    E(Outcome),
    Gone,
    G,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Outcome {
    Committed,
    Refused(Problem),
    Failed(Problem),
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Problem {
    Status(u16),
    RightsChanged,
    Conflict,
    LongestLife,
    CommitUnavailable,
    NotRun,
    Unpublished,
    CommitInProgress {
        retry: bool,
    },
    EndedWhileCommitting,
    Committed,
    OutcomeUnknown,
    Ended(EndKind),
    NoSession,
    ReadBeforeRetry,
    /// HTTP 503 with Retry-After: 1.
    LockBusy,
    WorkInProgress,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum EndKind {
    Committed,
    Refused,
    Failed,
    Unknown,
}

impl Outcome {
    fn kind(self) -> EndKind {
        match self {
            Self::Committed => EndKind::Committed,
            Self::Refused(_) => EndKind::Refused,
            Self::Failed(_) => EndKind::Failed,
            Self::Unknown => EndKind::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Action {
    Write { same_rights: bool },
    WriteRefused(Problem),
    Read { same_rights: bool },
    ReadRefused(Problem),
    Ping,
    Commit { same_rights: bool, empty: bool },
    Rollback,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Exit {
    Published { panicked: bool },
    Conflict,
    RightsChanged,
    Failed(Problem),
    Cancelled,
    Unavailable,
    Entered,
    NeverRan,
    Claimed,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Observation {
    Ended(Outcome),
    Published,
    InProgress,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum StreamEnd {
    Data,
    Error(Problem),
    Aborted,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Change {
    RevisionOrRules,
    Rights,
    NoOp,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Event {
    Begin {
        valid_level: bool,
    },
    Action(Action),
    Foreign(Action),
    Changed(Change),
    IdleLimit,
    LongestLife,
    Dropped,
    Restart,
    GuardRefused(Problem),
    OwnerKeyChanged(Action),
    RepositoryReplaced,
    OrphanEnded,
    NativeWriteRefused(Problem),
    DefaultUnavailable,
    Detached(Action),
    Swapped,
    EvaluationFailed(Problem),
    TaskReady {
        switch: bool,
        record: Observation,
    },
    Settled(Outcome),
    WorkerExit(Exit),
    CommitGrace(Observation),
    RepeatCommitReady {
        record: Observation,
        same_rights: bool,
    },
    ReadEnded {
        first: bool,
        end: StreamEnd,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Answer {
    None,
    Pending,
    Created,
    WriteOk,
    Read(StreamEnd),
    PingOk,
    NoContent,
    Error(Problem),
    TaskError { problem: Problem, source: Cell },
    Recorded { outcome: Outcome, evidence: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Effect {
    None,
    Create,
    Touch,
    Append,
    StartSwitch,
    StartWrite,
    TakeSnapshot,
    Swap,
    SwapAndRead,
    RestoreIntents,
    Record(Outcome),
    StampEnd,
    ReplayIntents,
    CommitDelta,
    Remove,
    Forget,
    RetireMap,
    WaitLock,
    CancelThenWaitLock,
    TryCancel,
    WaitOutcome,
    LogBadTransition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub answer: Answer,
    pub effect: Effect,
    pub next: State,
    pub cell: Cell,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Cell {
    U1,
    U2,
    U3,
    U4,
    U5,
    U6,
    U7,
    U8,
    U9,
    U10,
    U11,
    U12,
    U13,
    U14,
    U15,
    U16,
    U17,
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    S12,
    S13,
    S14,
    P1,
    P2,
    P3,
    P4,
    P5,
    P6,
    P7,
    P8,
    P9,
    P9a,
    P10,
    P11,
    P12,
    P13,
    P14,
    P15,
    P16,
    P17,
    P18,
    P19,
    P20,
    Pw1,
    Pw2,
    Pw3,
    Pw4,
    Pw5,
    Pw6,
    Pw7,
    Pw8,
    Pw9,
    Pw10,
    C1,
    C2,
    C3,
    C4,
    C5,
    C5b,
    C6,
    C7,
    C8,
    C9,
    C10,
    C11,
    C12,
    C13,
    C14,
    C15,
    C16,
    C17,
    C18,
    E1,
    E2,
    E3,
    E3b,
    E4,
    E5,
    E6,
    E7,
    E8,
    E9,
    G1,
    G2,
    G3,
    X1,
    X2,
    X3,
    X4,
    X5,
    X6,
    X7,
}

fn missing(action: Action) -> (Cell, Answer) {
    match action {
        Action::Commit { .. } => (Cell::G1, Answer::Error(Problem::ReadBeforeRetry)),
        Action::Write { .. }
        | Action::WriteRefused(_)
        | Action::Read { .. }
        | Action::ReadRefused(_)
        | Action::Ping
        | Action::Rollback
        | Action::Unknown => (Cell::G2, Answer::Error(Problem::NoSession)),
    }
}

fn recorded(outcome: Outcome, evidence: bool) -> Answer {
    Answer::Recorded { outcome, evidence }
}

fn observed(record: Observation, evidence: bool) -> Answer {
    match record {
        Observation::Ended(outcome) => recorded(outcome, evidence),
        Observation::Published => recorded(Outcome::Committed, evidence),
        Observation::InProgress => Answer::Error(Problem::CommitInProgress { retry: true }),
    }
}

// U15, S4, P19, Pw2 and E9 share one answer before the session lock is held.
pub(crate) fn lock_timeout() -> Answer {
    Answer::Error(Problem::LockBusy)
}

pub(crate) fn step(state: &State, event: &Event) -> Step {
    use Answer::{Created, Error, NoContent, Pending, PingOk, WriteOk};
    use Cell::*;
    use Effect::{
        Append, CancelThenWaitLock, CommitDelta, Create, Forget, Record, Remove, ReplayIntents,
        RestoreIntents, RetireMap, StampEnd, StartSwitch, StartWrite, Swap, SwapAndRead,
        TakeSnapshot, Touch, TryCancel, WaitLock, WaitOutcome,
    };
    use Event::{
        Action as Request, Begin, Changed, CommitGrace, DefaultUnavailable, Detached, Dropped,
        EvaluationFailed, Foreign, GuardRefused, IdleLimit, LongestLife, NativeWriteRefused,
        OrphanEnded, OwnerKeyChanged, ReadEnded, RepeatCommitReady, RepositoryReplaced, Restart,
        Settled, Swapped, TaskReady, WorkerExit,
    };
    use Outcome::{Committed, Failed, Refused};
    use State::*;

    let go = |cell, answer, effect, next| Step {
        answer,
        effect,
        next,
        cell,
    };
    let stay = |cell, answer, effect| go(cell, answer, effect, *state);
    let reply = |cell, answer| stay(cell, answer, Effect::None);
    let noop = |cell| reply(cell, Answer::None);
    let bug = |cell| stay(cell, Error(Problem::Status(500)), Effect::LogBadTransition);
    let end = |cell, problem| go(cell, Error(problem), StampEnd, E(Failed(problem)));
    let rights = |cell| {
        let problem = Problem::RightsChanged;
        go(cell, Error(problem), StampEnd, E(Refused(problem)))
    };
    let remove = |cell, answer| go(cell, answer, Remove, Gone);
    let forget = |cell| go(cell, Answer::None, Forget, G);

    match *event {
        Begin { valid_level: true } => {
            // Create refers to a fresh id. A map entry that already exists is independent.
            go(G3, Created, Create, if *state == G { U } else { *state })
        }
        Begin { valid_level: false } => reply(G3, Error(Problem::Status(400))),
        GuardRefused(problem) => reply(X1, Error(problem)),
        OwnerKeyChanged(action) => reply(X2, missing(action).1),
        RepositoryReplaced => stay(X4, Answer::None, RetireMap),
        OrphanEnded => stay(X4, Answer::None, Record(Failed(Problem::NoSession))),
        NativeWriteRefused(problem) => reply(X5, Error(problem)),
        DefaultUnavailable => reply(X6, Error(Problem::Status(503))),
        Detached(action) => reply(X7, missing(action).1),
        ReadEnded { first, end } => reply(
            match (first, end) {
                (true, StreamEnd::Data) => S1,
                (true, StreamEnd::Error(_) | StreamEnd::Aborted) => S3,
                (false, StreamEnd::Data) => P5,
                (false, StreamEnd::Error(_) | StreamEnd::Aborted) => P7,
            },
            Answer::Read(end),
        ),
        CommitGrace(record) => reply(C5b, observed(record, true)),
        RepeatCommitReady {
            record,
            same_rights,
        } => reply(C8, observed(record, same_rights)),
        TaskReady { switch, record } => {
            let cell = if switch { S14 } else { Pw10 };
            match record {
                Observation::Published | Observation::Ended(Committed) => {
                    if switch {
                        stay(cell, Pending, TakeSnapshot)
                    } else {
                        reply(cell, WriteOk)
                    }
                }
                Observation::Ended(Failed(Problem::NoSession)) => {
                    reply(cell, Error(Problem::NoSession))
                }
                Observation::Ended(Refused(problem) | Failed(problem)) => reply(
                    cell,
                    Answer::TaskError {
                        problem,
                        source: if switch { S2 } else { P4 },
                    },
                ),
                Observation::Ended(Outcome::Unknown) => bug(cell),
                Observation::InProgress => reply(cell, Error(Problem::WorkInProgress)),
            }
        }
        Request(action) => match (*state, action) {
            (U, Action::Write { .. }) => stay(U1, WriteOk, Append),
            (U, Action::WriteRefused(problem)) => stay(U2, Error(problem), Touch),
            (U, Action::Read { .. }) => go(U3, Pending, StartSwitch, S),
            (U, Action::ReadRefused(problem)) => reply(U4, Error(problem)),
            (U, Action::Ping) => stay(U5, PingOk, Touch),
            (U, Action::Commit { .. }) => go(U6, Pending, ReplayIntents, C),
            (U, Action::Rollback) => remove(U7, NoContent),
            (U | P, Action::Unknown) => stay(X3, Error(Problem::Status(400)), Touch),
            (S, Action::Rollback) => stay(S13, Pending, CancelThenWaitLock),
            (Pw, Action::Rollback) => stay(Pw9, Pending, CancelThenWaitLock),
            (
                S | Pw,
                Action::Write { .. }
                | Action::WriteRefused(_)
                | Action::Read { .. }
                | Action::ReadRefused(_)
                | Action::Ping
                | Action::Commit { .. }
                | Action::Unknown,
            ) => stay(if *state == S { S4 } else { Pw2 }, Pending, WaitLock),
            (P, Action::Write { same_rights: true }) => go(P1, Pending, StartWrite, Pw),
            (P, Action::Write { same_rights: false }) => rights(P2),
            (P, Action::WriteRefused(problem)) => stay(P3, Error(problem), Touch),
            (P, Action::Read { same_rights: true }) => stay(P5, Pending, TakeSnapshot),
            (P, Action::Read { same_rights: false }) => rights(P6),
            (P, Action::ReadRefused(problem)) => reply(P7, Error(problem)),
            (P, Action::Ping) => stay(P8, PingOk, Touch),
            (
                P,
                Action::Commit {
                    same_rights: true,
                    empty: true,
                },
            ) => go(P9a, recorded(Committed, true), StampEnd, E(Committed)),
            (
                P,
                Action::Commit {
                    same_rights: true,
                    empty: false,
                },
            ) => go(P9, Pending, CommitDelta, C),
            (
                P,
                Action::Commit {
                    same_rights: false, ..
                },
            ) => rights(P10),
            (P, Action::Rollback) => remove(P11, NoContent),
            (C, Action::Commit { .. }) => stay(C8, Pending, WaitOutcome),
            (C, Action::Rollback) => reply(C9, Error(Problem::CommitInProgress { retry: false })),
            (
                C,
                Action::Write { .. }
                | Action::WriteRefused(_)
                | Action::Read { .. }
                | Action::ReadRefused(_)
                | Action::Ping
                | Action::Unknown,
            ) => reply(C10, Error(Problem::EndedWhileCommitting)),
            (E(outcome), Action::Commit { same_rights, .. }) => {
                reply(E1, recorded(outcome, same_rights))
            }
            (E(Committed), Action::Rollback) => reply(E2, Error(Problem::Committed)),
            (E(Refused(_) | Failed(_)), Action::Rollback) => reply(E3, NoContent),
            (E(Outcome::Unknown), Action::Rollback) => reply(E3b, Error(Problem::OutcomeUnknown)),
            (
                E(outcome),
                Action::Write { .. }
                | Action::WriteRefused(_)
                | Action::Read { .. }
                | Action::ReadRefused(_)
                | Action::Ping
                | Action::Unknown,
            ) => reply(E4, Error(Problem::Ended(outcome.kind()))),
            (Gone, action) => reply(X7, missing(action).1),
            (G, action) => {
                let (cell, answer) = missing(action);
                reply(cell, answer)
            }
        },
        Foreign(action) => {
            let cell = match state {
                U => U14,
                S => S11,
                P => P18,
                Pw => Pw6,
                C => C14,
                E(_) => E7,
                Gone => X7,
                G => missing(action).0,
            };
            reply(cell, missing(action).1)
        }
        IdleLimit => match state {
            U => remove(U8, Answer::None),
            S => bug(S8),
            P => remove(P12, Answer::None),
            Pw => bug(Pw8),
            C => bug(C11),
            E(_) => remove(E5, Answer::None),
            Gone => noop(X7),
            G => noop(G2),
        },
        LongestLife => match state {
            U => bug(U9),
            S => bug(S10),
            P => end(P13, Problem::LongestLife),
            Pw => go(Pw4, Answer::None, StampEnd, E(Failed(Problem::LongestLife))),
            C => bug(C11),
            E(_) => noop(E8),
            Gone => noop(X7),
            G => noop(G2),
        },
        Changed(change) => noop(match state {
            U => match change {
                Change::Rights => U11,
                Change::NoOp => U16,
                Change::RevisionOrRules => U10,
            },
            S => match change {
                Change::Rights => S9,
                Change::NoOp | Change::RevisionOrRules => S5,
            },
            P => match change {
                Change::RevisionOrRules => P14,
                Change::Rights | Change::NoOp => P15,
            },
            Pw => Pw5,
            C => C16,
            E(_) => E8,
            Gone => X7,
            G => G2,
        }),
        Dropped => match state {
            U => noop(U12),
            S => stay(S6, Answer::None, TryCancel),
            P => noop(P7),
            Pw => stay(P16, Answer::None, TryCancel),
            C => noop(C12),
            E(_) => noop(E8),
            Gone => noop(X7),
            G => noop(G2),
        },
        Restart => match state {
            U => forget(U13),
            S => forget(S7),
            P => forget(P17),
            Pw => forget(Pw7),
            C => forget(C13),
            E(_) => forget(E6),
            Gone => noop(X7),
            G => noop(G2),
        },
        Settled(outcome) => match state {
            S if outcome == Committed => bug(S12),
            Pw if outcome == Committed => bug(Pw1),
            S => go(S12, Answer::None, RestoreIntents, U),
            Pw => go(Pw1, Answer::None, StampEnd, E(outcome)),
            C => go(C18, Answer::None, StampEnd, E(outcome)),
            U => bug(U17),
            P => bug(P20),
            E(_) => noop(E8),
            Gone => noop(X7),
            G => noop(G2),
        },
        Swapped => match state {
            S => go(S1, Answer::None, SwapAndRead, P),
            Pw => go(Pw3, Answer::None, Swap, P),
            U => bug(U17),
            P => bug(P20),
            C => bug(C11),
            E(_) => noop(E8),
            Gone => noop(X7),
            G => noop(G2),
        },
        EvaluationFailed(problem) => match state {
            S => go(S2, Answer::None, RestoreIntents, U),
            Pw => go(Pw3, Answer::None, StampEnd, E(Failed(problem))),
            U => bug(U17),
            P => bug(P20),
            C => bug(C11),
            E(_) => noop(E8),
            Gone => noop(X7),
            G => noop(G2),
        },
        WorkerExit(exit) => match state {
            C => {
                let (cell, outcome) = match exit {
                    Exit::Published { .. } => (C1, Committed),
                    Exit::Conflict => (C2, Refused(Problem::Conflict)),
                    Exit::RightsChanged => (C3, Refused(Problem::RightsChanged)),
                    Exit::Failed(problem) => (C4, Failed(problem)),
                    Exit::Cancelled => (C5, Failed(Problem::Status(408))),
                    Exit::Unavailable => (C6, Failed(Problem::CommitUnavailable)),
                    Exit::Entered => (C7, Outcome::Unknown),
                    Exit::NeverRan => (C15, Failed(Problem::NotRun)),
                    Exit::Claimed => (C17, Failed(Problem::Unpublished)),
                };
                stay(cell, recorded(outcome, true), Record(outcome))
            }
            U => bug(U17),
            P => bug(P20),
            S => bug(S8),
            Pw => bug(Pw8),
            E(_) => noop(E8),
            Gone => noop(X7),
            G => noop(G2),
        },
    }
}
