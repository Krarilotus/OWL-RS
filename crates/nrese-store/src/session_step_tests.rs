//! The scheduler models only facts that cross a lock or affect a later answer.
use super::*;
use nrese_exec::lifecycle_explorer::{Model, Stats, Trace, explore};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

#[path = "session_oracle.rs"]
mod session_oracle;
use session_oracle::{
    ClientRecord, Delivery, End, Failure, IDLE_TICKS, Id, Invariant, Op, Producer, Seen, Sent,
    StoreRecord, Stream, TerminalWrite, Version,
};

const ORDINARY_DEPTH: usize = 5;
const ORDINARY_WORLDS: usize = 100_000;
const WITNESS_DEPTH: usize = 8;
const DEEP_DEPTH: usize = 16;
const DEEP_WORLDS_TWO: usize = 70_000_000;
const DEEP_WORLDS_THREE: usize = 80_000_000;
const GOOD: u8 = 0;
const UNKNOWN_ROLLBACK: u8 = 1;
const JOIN_ANSWER: u8 = 2;
const EARLY_RELEASE: u8 = 3;
const STALE_LOOKUP: u8 = 4;
const REPLAY_OBSERVED: u8 = 5;
const ACK_WITHOUT_APPEND: u8 = 6;
const PUBLISHED_WITHOUT_RECORD: u8 = 7;
const REMOVED_WITHOUT_GONE: u8 = 8;
const DROP_TO_U: u8 = 9;
const NO_IDLE_EXPIRY: u8 = 10;
const IMMEDIATE_COMMIT: u8 = 11;
const GRACE_BEFORE_PUBLISH: u8 = 12;
const ROLLBACK_STAYS_LIVE: u8 = 13;
const READ_WITHOUT_SNAPSHOT: u8 = 14;
const WRONG_STREAM_ANSWER: u8 = 15;

const WRITE_STARTS_SWITCH: u8 = 16;
const FAILED_TOMBSTONE_COMMITS: u8 = 17;
const COMMITTED_ROLLBACK: u8 = 18;
const MISSING_COMMIT: u8 = 19;
const LOST_REPEAT: u8 = 20;
const WRITE_DURING_COMMIT: u8 = 21;
const STALE_REPEAT_EFFECT: u8 = 22;
const FAILED_ROLLBACK_REMOVES: u8 = 23;
const DROP_WITHOUT_CANCEL: u8 = 24;
const REPLACEMENT_WITHOUT_RETIRE: u8 = 25;
const COMMIT_DROP_CANCELS: u8 = 26;
const CHANGE_REMOVES: u8 = 27;
const READ_DROP_WITHOUT_CANCEL: u8 = 28;
const PING_WITHOUT_TOUCH: u8 = 29;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Request {
    id: Id,
    op: Op,
    rights: bool,
}

type ReportedAnswer = (Option<Cell>, Answer);

fn seen(answer: &Answer) -> Seen {
    let failure = |problem| match problem {
        Problem::Status(code) => Failure::Status(code),
        Problem::RightsChanged => Failure::Rights,
        Problem::Conflict => Failure::Conflict,
        Problem::LongestLife => Failure::LongestLife,
        Problem::CommitUnavailable => Failure::Busy,
        Problem::NotRun => Failure::NotRun,
        Problem::Unpublished => Failure::Unpublished,
        Problem::CommitInProgress { retry } => Failure::CommitBusy(retry),
        Problem::EndedWhileCommitting => Failure::EndedWhileCommitting,
        Problem::Committed => Failure::AlreadyCommitted,
        Problem::OutcomeUnknown => Failure::OutcomeUnknown,
        Problem::NoSession => Failure::Missing(false),
        Problem::ReadBeforeRetry => Failure::Missing(true),
        Problem::LockBusy => Failure::LockBusy,
        Problem::WorkInProgress => Failure::WriteBusy,
    };
    let end = |outcome| match outcome {
        Outcome::Committed => End::Committed,
        Outcome::Refused(problem) => End::Refused(failure(problem)),
        Outcome::Failed(problem) => End::Failed(failure(problem)),
        Outcome::Unknown => End::Unknown,
    };
    match *answer {
        Answer::None => Seen::Silent,
        Answer::Pending => Seen::Pending,
        Answer::Created => Seen::Created,
        Answer::WriteOk => Seen::WriteOk,
        Answer::PingOk => Seen::Ping,
        Answer::NoContent => Seen::RolledBack,
        Answer::Read(StreamEnd::Data) => Seen::Stream(Stream::Data),
        Answer::Read(StreamEnd::Aborted) => Seen::Stream(Stream::Aborted),
        Answer::Read(StreamEnd::Error(problem)) => Seen::Stream(Stream::Error(failure(problem))),
        Answer::Error(problem) | Answer::TaskError { problem, .. } => Seen::Error(failure(problem)),
        Answer::Ended(outcome) => Seen::Ended(end(outcome)),
        Answer::Recorded { outcome, evidence } => Seen::Commit(end(outcome), evidence),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Phase {
    Pending,
    Claimed,
    Ended(End),
    Recorded(End),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Task {
    request: Id,
    rights: bool,
    switch: bool,
    bit: u8,
    scratch: u8,
    pin: Option<Pin>,
    guard: bool,
    needs_settle: bool,
    awaiting: bool,
    phase: Phase,
}

impl Task {
    fn visible_end(self) -> Option<End> {
        match self.phase {
            Phase::Recorded(end)
            | Phase::Ended(
                end
                @ (End::Committed | End::Failed(Failure::Missing(false) | Failure::Status(408))),
            ) => Some(end),
            Phase::Pending | Phase::Claimed | Phase::Ended(_) => None,
        }
    }

    fn observe(self) -> Observation {
        match self.visible_end() {
            Some(End::Committed) if !matches!(self.phase, Phase::Recorded(_)) => {
                Observation::Published
            }
            Some(end) => Observation::Ended(task_outcome(end)),
            None => Observation::InProgress,
        }
    }
}

fn task_outcome(end: End) -> Outcome {
    match end {
        End::Committed => Outcome::Committed,
        End::Unknown => Outcome::Unknown,
        End::Refused(failure) => Outcome::Refused(model_problem(failure)),
        End::Failed(failure) => Outcome::Failed(model_problem(failure)),
    }
}

fn model_problem(failure: Failure) -> Problem {
    match failure {
        Failure::Status(code) => Problem::Status(code),
        Failure::Rights => Problem::RightsChanged,
        Failure::Conflict => Problem::Conflict,
        Failure::LongestLife => Problem::LongestLife,
        Failure::Unpublished => Problem::Unpublished,
        Failure::NotRun => Problem::NotRun,
        Failure::Missing(false) => Problem::NoSession,
        other => panic!("unexpected task or stream error: {other:?}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Pin {
    revision: u8,
    epoch: u8,
    protected: bool,
    rights: bool,
}

impl Pin {
    fn capture<const N: usize>(store: &StoreRecord<N>, rights: bool) -> Self {
        Self {
            revision: store.version.0,
            epoch: store.version.1,
            protected: store.transactions.iter().any(|txn| {
                txn.publication
                    .is_some_and(|publication| publication.tags & 2 != 0)
            }),
            rights,
        }
    }

    fn version(self) -> Version {
        (self.revision, self.epoch)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Read {
    request: Id,
    first: bool,
    pin: Pin,
    private: u8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Commit {
    request: Id,
    writes: u8,
    pinned: bool,
    rights: bool,
    awaiting: bool,
    published: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Repeat {
    request: Id,
    rights: bool,
    record: Observation,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Session {
    state: State,
    // The map entry exposes this slot without taking the session lock.
    ticket: Option<usize>,
    waiting: Option<Request>,
    rollback_waiting: Option<Request>,
    repeat: Option<Repeat>,
    read: Option<Read>,
    tasks: [Option<Task>; 2],
    commit: Option<Commit>,
    terminal: Option<Outcome>,
    pin: Option<Pin>,
    issued: u8,
    expired: bool,
}

impl Session {
    fn new(state: State) -> Self {
        Self {
            state,
            ticket: None,
            waiting: None,
            rollback_waiting: None,
            repeat: None,
            read: None,
            tasks: [None; 2],
            commit: None,
            terminal: None,
            pin: None,
            issued: 0,
            expired: false,
        }
    }

    fn held(&self) -> bool {
        self.tasks.iter().flatten().any(|task| task.guard)
    }

    fn try_cancel(&mut self, slot: usize) {
        let task = self.tasks[slot].as_mut().unwrap();
        if task.phase == Phase::Pending {
            task.phase = Phase::Ended(End::Failed(Failure::Status(408)));
        }
    }

    fn release_guard(&mut self, slot: usize) {
        self.tasks[slot].as_mut().unwrap().guard = false;
        if self.ticket == Some(slot) {
            self.ticket = None;
        }
    }

    fn next_write(&mut self) -> u8 {
        let bit = 1 << self.issued;
        self.issued += 1;
        bit
    }

    fn action(&self, op: Op, rights: bool, txn: &session_oracle::Transaction) -> Action {
        let same_rights = match self.state {
            State::E(_) => txn.evidence_rights == rights,
            _ => self.pin.is_none_or(|pin| pin.rights == rights),
        };
        match op {
            Op::Write => Action::Write { same_rights },
            Op::Read => Action::Read { same_rights },
            Op::Commit => Action::Commit {
                same_rights,
                empty: txn.pending == 0,
            },
            Op::Rollback => Action::Rollback,
            Op::Ping => Action::Ping,
        }
    }

    fn can_settle(&self) -> bool {
        self.terminal.is_some() && matches!(self.state, State::S | State::Pw | State::C)
    }
}

#[derive(Clone, Debug)]
struct World<const N: usize> {
    sessions: [Session; N],
    clients: [ClientRecord; N],
    store: StoreRecord<N>,
    rights: bool,
    rights_changed: bool,
    swept: bool,
    restarted: bool,
    violation: Option<Invariant>,
}

impl<const N: usize> World<N> {
    fn identity(&self) -> impl Eq + Hash {
        // A tag belongs to its client. Sort those together, keeping the IDs in
        // the actual world so a trace still names the same client at every step.
        let mut clients = std::array::from_fn::<_, N, _>(|id| {
            (
                &self.sessions[id],
                self.clients[id].key(),
                &self.store.transactions[id],
            )
        });
        clients.sort_unstable();
        (
            clients,
            self.store.version,
            self.store.tick,
            self.store.external_publication,
            self.rights,
            self.rights_changed,
            self.store.retired,
            self.swept,
            self.restarted,
            self.violation,
        )
    }
}

impl<const N: usize> PartialEq for World<N> {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl<const N: usize> Eq for World<N> {}

impl<const N: usize> Hash for World<N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

#[derive(Clone, Copy, Debug)]
enum Move {
    Lookup(usize, Op),
    SessionLock(usize),
    RollbackLock(usize),
    WaitTimeout(usize),
    RollbackTimeout(usize),
    DropWaiting(usize),
    DropRollback(usize),
    DropTaskRequest(usize, usize),
    DropCommitRequest(usize),
    DropRepeat(usize),
    DropRead(usize),
    Claim(usize, usize),
    Swap(usize, usize),
    Fail(usize, usize),
    DropGuard(usize, usize, End),
    TaskReady(usize, usize),
    TaskDeadline(usize, usize),
    PublishCommit(usize),
    CommitExit(usize, Exit),
    CommitGrace(usize),
    RepeatReady(usize),
    Stream(usize, Stream),
    // One try_lock visit by the periodic sweep, whose interval derives from the idle limit.
    Sweep(usize),
    // Advance idle time independently of the sweep cadence.
    IdleTick,
    LifeClock,
    OtherCommit,
    RulesChange,
    RightsChange,
    NoOpChange,
    ReplaceRepository,
    Restart,
}

struct Sessions<const N: usize, const MUTATION: u8, const FOCUS: u8 = GOOD>;

impl<const N: usize> World<N> {
    fn normal_commit_move(&self, id: usize, commit: Commit) -> Move {
        if commit.published {
            Move::CommitExit(id, Exit::Published { panicked: false })
        } else if commit.pinned
            && self.sessions[id]
                .pin
                .is_some_and(|pin| pin.version() != self.store.version)
        {
            Move::CommitExit(id, Exit::Conflict)
        } else if commit.rights != self.rights {
            Move::CommitExit(id, Exit::RightsChanged)
        } else {
            Move::PublishCommit(id)
        }
    }

    fn violate<const MUTATION: u8>(&mut self, violation: Invariant) {
        // Mutation runs keep searching until the externally visible regression.
        let target = match MUTATION {
            EARLY_RELEASE | ACK_WITHOUT_APPEND | WRITE_DURING_COMMIT => Some(Invariant::I4),
            REPLAY_OBSERVED | WRITE_STARTS_SWITCH => Some(Invariant::I1),
            DROP_TO_U | STALE_REPEAT_EFFECT => Some(Invariant::I8),
            READ_WITHOUT_SNAPSHOT | LOST_REPEAT => Some(Invariant::I5),
            NO_IDLE_EXPIRY | PING_WITHOUT_TOUCH => Some(Invariant::I6),
            DROP_WITHOUT_CANCEL | READ_DROP_WITHOUT_CANCEL => Some(Invariant::I10),
            REPLACEMENT_WITHOUT_RETIRE | COMMIT_DROP_CANCELS | CHANGE_REMOVES => {
                Some(Invariant::InvalidTransition)
            }
            UNKNOWN_ROLLBACK
            | JOIN_ANSWER
            | STALE_LOOKUP
            | PUBLISHED_WITHOUT_RECORD
            | REMOVED_WITHOUT_GONE
            | IMMEDIATE_COMMIT
            | GRACE_BEFORE_PUBLISH
            | ROLLBACK_STAYS_LIVE
            | WRONG_STREAM_ANSWER
            | FAILED_TOMBSTONE_COMMITS
            | COMMITTED_ROLLBACK
            | FAILED_ROLLBACK_REMOVES
            | MISSING_COMMIT => Some(Invariant::I3),
            _ => None,
        };
        if target.is_none_or(|expected| expected == violation) {
            self.violation.get_or_insert(violation);
        }
    }

    fn receive<const MUTATION: u8>(
        &mut self,
        id: usize,
        request: Id,
        answer: Answer,
        delivery: Delivery,
    ) {
        self.store.delivery = Some(delivery);
        self.clients[id].receive(request, seen(&answer), delivery);
        let client = &self.clients[id];
        let findings = [
            (session_oracle::i1(client, &self.store), Invariant::I1),
            (session_oracle::i2(client, &self.store), Invariant::I2),
            (session_oracle::i3(client, &self.store), Invariant::I3),
            (session_oracle::i5(client, &self.store), Invariant::I5),
            (session_oracle::i7(client, &self.store), Invariant::I7),
            (session_oracle::i9(client, &self.store), Invariant::I9),
        ];
        for (holds, invariant) in findings {
            if !holds {
                self.violate::<MUTATION>(invariant);
            }
        }
        self.store.delivery = None;
        self.clients[id].receipt = None;
        if self.clients[id].owed[request].is_none() {
            self.store.transactions[id].producers[request] = None;
        }
    }

    fn abandon(&mut self, id: usize, request: Id) {
        self.clients[id].abandon(request);
        self.store.transactions[id].producers[request] = None;
    }

    fn record_terminal<const MUTATION: u8>(
        &mut self,
        id: usize,
        expected: End,
        effect: Effect,
    ) -> Option<(Outcome, End)> {
        let recorded = if let Effect::Record(outcome) = effect {
            // Inspect the installed terminal using the same outcome view as a reply.
            let Seen::Commit(recorded, _) = seen(&Answer::Recorded {
                outcome,
                evidence: false,
            }) else {
                unreachable!("a terminal uses the recorded reply representation");
            };
            Some((outcome, recorded))
        } else {
            None
        };
        self.store.terminal_write = Some(TerminalWrite {
            expected,
            recorded: recorded.map(|(_, end)| end),
        });
        if !session_oracle::i9(&self.clients[id], &self.store) {
            self.violate::<MUTATION>(Invariant::I9);
        }
        self.store.terminal_write = None;
        recorded
    }
}

struct Transition {
    step: Step,
    release_guard: bool,
}

#[derive(Clone, Copy)]
enum StepUse {
    Begin,
    Request,
    Detached,
    Settle,
    Expiry,
    TaskDrop,
    TaskEnd,
    TaskReady,
    CommitExit { without_waiter: Option<End> },
    Reply,
    Notice,
    Replace,
    Restart,
}

impl StepUse {
    fn handles_effect(self, input: State, event: Event, effect: Effect) -> bool {
        use Effect::*;
        match self {
            Self::Begin => effect == Create,
            Self::Request => matches!(
                effect,
                None | Touch
                    | Append
                    | StartSwitch
                    | StartWrite
                    | TakeSnapshot
                    | ReplayIntents
                    | CommitDelta
                    | Remove
                    | StampEnd
                    | WaitOutcome
            ),
            // The detached handler rejects any requested effect through I8.
            Self::Detached => true,
            Self::Settle => match input {
                State::S => effect == RestoreIntents,
                State::Pw | State::C => effect == StampEnd,
                State::U | State::P | State::E(_) | State::G | State::Gone => false,
            },
            Self::Expiry => match event {
                Event::IdleLimit => matches!(effect, Remove | None),
                Event::LongestLife => matches!(effect, StampEnd | None),
                _ => false,
            },
            Self::TaskDrop => {
                effect == None || (effect == TryCancel && matches!(input, State::S | State::Pw))
            }
            Self::TaskEnd => match (input, event) {
                (State::S, Event::Swapped) => effect == SwapAndRead,
                (State::Pw, Event::Swapped) => effect == Swap,
                (State::S, Event::EvaluationFailed(_)) => effect == RestoreIntents,
                (State::Pw, Event::EvaluationFailed(_) | Event::LongestLife) => effect == StampEnd,
                (_, Event::OrphanEnded) => matches!(effect, Record(_) | None),
                _ => false,
            },
            Self::TaskReady => {
                effect == None
                    || (effect == TakeSnapshot
                        && matches!(
                            event,
                            Event::TaskReady {
                                switch: true,
                                record: Observation::Published
                                    | Observation::Ended(Outcome::Committed),
                            }
                        ))
            }
            Self::CommitExit { .. } => matches!(effect, Record(_) | None),
            Self::Reply | Self::Notice => effect == None,
            Self::Replace => effect == RetireMap,
            Self::Restart => {
                if input == State::G {
                    effect == None
                } else {
                    effect == Forget
                }
            }
        }
    }

    fn handles_answer(self, answer: Answer) -> bool {
        match self {
            Self::Begin => answer == Answer::Created,
            Self::Request
            | Self::Detached
            | Self::TaskReady
            | Self::Reply
            | Self::CommitExit {
                without_waiter: None,
            } => true,
            Self::CommitExit {
                without_waiter: Some(end),
            } => {
                // The exit still presents the terminal outcome even after its waiter leaves.
                seen(&answer) == Seen::Commit(end, true)
            }
            Self::Settle
            | Self::Expiry
            | Self::TaskDrop
            | Self::TaskEnd
            | Self::Notice
            | Self::Replace
            | Self::Restart => answer == Answer::None,
        }
    }
}

fn transition<const MUTATION: u8>(state: State, event: Event) -> Transition {
    let mut result = step(&state, &event);
    let mut release_guard = false;
    match (MUTATION, state, event) {
        (UNKNOWN_ROLLBACK, State::E(Outcome::Unknown), Event::Action(Action::Rollback)) => {
            result.answer = Answer::NoContent;
        }
        (JOIN_ANSWER, State::C, Event::WorkerExit(Exit::Published { panicked: true })) => {
            result.answer = Answer::Error(Problem::Status(500));
        }
        (EARLY_RELEASE, State::Pw, Event::Dropped) => {
            result.effect = Effect::None;
            result.next = State::P;
            release_guard = true;
        }
        (STALE_LOOKUP, State::Gone, Event::Action(Action::Write { .. })) => {
            result.answer = Answer::WriteOk;
        }
        (REPLAY_OBSERVED, State::P, Event::Action(Action::Commit { .. }))
            if result.cell == Cell::P9 =>
        {
            result.effect = Effect::ReplayIntents;
        }
        (ACK_WITHOUT_APPEND, State::U, Event::Action(Action::Write { .. })) => {
            result.effect = Effect::None;
        }
        (PUBLISHED_WITHOUT_RECORD, State::C, Event::WorkerExit(Exit::Published { .. })) => {
            result.answer = Answer::Error(Problem::Status(500));
            result.effect = Effect::None;
        }
        (REMOVED_WITHOUT_GONE, State::U, Event::IdleLimit) => result.next = State::U,
        (DROP_TO_U, State::Pw, Event::Dropped) => result.next = State::U,
        (NO_IDLE_EXPIRY, State::P, Event::IdleLimit) => {
            result.next = State::P;
            result.effect = Effect::None;
        }
        (IMMEDIATE_COMMIT, State::P, Event::Action(Action::Commit { .. }))
            if result.cell == Cell::P9 =>
        {
            result = step(
                &state,
                &Event::Action(Action::Commit {
                    same_rights: true,
                    empty: true,
                }),
            );
        }
        (GRACE_BEFORE_PUBLISH, State::C, Event::CommitGrace(Observation::InProgress)) => {
            result.answer = Answer::Recorded {
                outcome: Outcome::Committed,
                evidence: true,
            };
        }
        (ROLLBACK_STAYS_LIVE, State::U, Event::Action(Action::Rollback)) => {
            result.next = State::U;
            result.effect = Effect::None;
        }
        (READ_WITHOUT_SNAPSHOT, State::P, Event::Action(Action::Read { same_rights: true })) => {
            result.effect = Effect::None;
        }
        (
            WRONG_STREAM_ANSWER,
            _,
            Event::ReadEnded {
                first: true,
                end: StreamEnd::Aborted,
            },
        ) => result.answer = Answer::Read(StreamEnd::Data),
        (WRITE_STARTS_SWITCH, State::P, Event::Action(Action::Write { same_rights: true })) => {
            result.effect = Effect::StartSwitch;
        }
        (
            FAILED_TOMBSTONE_COMMITS,
            State::E(Outcome::Failed(_) | Outcome::Refused(_)),
            Event::Action(Action::Commit { .. }),
        )
        | (MISSING_COMMIT, State::G, Event::Action(Action::Commit { .. })) => {
            result.answer = Answer::Recorded {
                outcome: Outcome::Committed,
                evidence: false,
            };
        }
        (COMMITTED_ROLLBACK, State::E(Outcome::Committed), Event::Action(Action::Rollback)) => {
            result.answer = Answer::NoContent;
        }
        (LOST_REPEAT, State::C, Event::Action(Action::Commit { .. })) => {
            result.effect = Effect::None;
        }
        (WRITE_DURING_COMMIT, State::C, Event::Action(Action::Write { .. })) => {
            result.answer = Answer::WriteOk;
            result.effect = Effect::Append;
        }
        (STALE_REPEAT_EFFECT, State::Gone, Event::Action(Action::Commit { .. })) => {
            result.effect = Effect::WaitOutcome;
        }
        (
            FAILED_ROLLBACK_REMOVES,
            State::E(Outcome::Failed(_) | Outcome::Refused(_)),
            Event::Action(Action::Rollback),
        ) => result.effect = Effect::Remove,
        (DROP_WITHOUT_CANCEL, State::Pw, Event::Dropped) => result.effect = Effect::None,
        (REPLACEMENT_WITHOUT_RETIRE, _, Event::RepositoryReplaced) => result.effect = Effect::None,
        (COMMIT_DROP_CANCELS, State::C, Event::Dropped) => result.effect = Effect::TryCancel,
        (CHANGE_REMOVES, State::U, Event::Changed(Change::RevisionOrRules)) => {
            result.effect = Effect::Remove;
        }
        (READ_DROP_WITHOUT_CANCEL, State::S, Event::Dropped) => result.effect = Effect::None,
        (PING_WITHOUT_TOUCH, State::U, Event::Action(Action::Ping)) => result.effect = Effect::None,
        _ => {}
    }
    Transition {
        step: result,
        release_guard,
    }
}

impl<const N: usize> World<N> {
    fn checked_transition<const MUTATION: u8>(
        &mut self,
        input: State,
        event: Event,
        site: StepUse,
    ) -> Transition {
        let result = transition::<MUTATION>(input, event);
        if result.step.effect == Effect::LogBadTransition
            || !site.handles_effect(input, event, result.step.effect)
            || !site.handles_answer(result.step.answer)
        {
            self.violate::<MUTATION>(Invariant::InvalidTransition);
        }
        result
    }

    fn event<const MUTATION: u8>(
        &mut self,
        id: usize,
        event: Event,
        locked: bool,
        site: StepUse,
        report: &mut impl FnMut(ReportedAnswer),
    ) -> Step {
        let before = self.sessions[id];
        let Transition {
            step: result,
            release_guard,
        } = self.checked_transition::<MUTATION>(before.state, event, site);
        if result.next != before.state || release_guard {
            self.session_write::<MUTATION>(id, locked);
        }
        self.sessions[id].state = result.next;
        if release_guard && let Some(slot) = before.ticket {
            self.release_guard(id, slot);
        }
        report((Some(result.cell), result.answer));
        result
    }

    fn session_write<const MUTATION: u8>(&mut self, id: usize, locked: bool) {
        let txn = &self.store.transactions[id];
        self.store.write = Some((locked, txn.mapped, self.store.retired));
        if !session_oracle::i8(&self.clients[id], &self.store) {
            self.violate::<MUTATION>(Invariant::I8);
        }
        self.store.write = None;
    }

    fn answer_detached<const MUTATION: u8>(
        &mut self,
        id: usize,
        request: Request,
        result: Step,
        locked: bool,
    ) {
        if result.effect != Effect::None {
            self.session_write::<MUTATION>(id, locked);
        }
        let txn = &mut self.store.transactions[id];
        let delivery = txn.request_delivery(self.store.retired, request.rights);
        txn.producers[request.id] = None;
        self.receive::<MUTATION>(id, request.id, result.answer, delivery);
    }

    fn release_guard(&mut self, id: usize, slot: usize) {
        let session = &mut self.sessions[id];
        let txn = &mut self.store.transactions[id];
        if session.tasks[slot].unwrap().guard
            && txn.mapped
            && !self.store.retired
            && txn.end.is_none()
        {
            txn.touched_at = self.store.tick;
            txn.activity_at = self.store.tick;
        }
        txn.guards &= !(1 << slot);
        session.release_guard(slot);
    }

    fn release_pin(&mut self, id: usize) {
        self.sessions[id].pin = None;
        self.store.transactions[id].pinned = false;
        self.store.transactions[id].pin_rights = None;
    }

    fn restore_intents(&mut self, id: usize) {
        self.release_pin(id);
        let txn = &mut self.store.transactions[id];
        txn.first_capture = None;
        txn.cancelled_switch = None;
        txn.settling = false;
    }

    fn finish(&mut self, id: usize, end: End, rights: bool) {
        self.release_pin(id);
        self.sessions[id].expired = false;
        let txn = &mut self.store.transactions[id];
        txn.end = Some(end);
        txn.evidence_rights = rights;
        txn.pending = 0;
        txn.touched_at = self.store.tick;
        txn.activity_at = self.store.tick;
        txn.settling = false;
    }

    fn remove(&mut self, id: usize) {
        self.release_pin(id);
        let txn = &mut self.store.transactions[id];
        txn.mapped = false;
        txn.pending = 0;
    }

    fn settle<const MUTATION: u8>(&mut self, id: usize, report: &mut impl FnMut(ReportedAnswer)) {
        let session = self.sessions[id];
        if let Some(outcome) = session.terminal.filter(|_| session.can_settle()) {
            let result =
                self.event::<MUTATION>(id, Event::Settled(outcome), true, StepUse::Settle, report);
            self.sessions[id].terminal = None;
            match result.effect {
                Effect::RestoreIntents => {
                    self.session_write::<MUTATION>(id, true);
                    self.restore_intents(id);
                }
                Effect::StampEnd => {
                    self.session_write::<MUTATION>(id, true);
                    self.release_pin(id);
                    self.sessions[id].expired = false;
                    let txn = &mut self.store.transactions[id];
                    txn.pending = 0;
                    txn.touched_at = self.store.tick;
                    txn.activity_at = self.store.tick;
                    txn.settling = false;
                }
                _ => {}
            }
        }
    }

    fn section<const MUTATION: u8>(
        &mut self,
        id: usize,
        request: Option<Request>,
        report: &mut impl FnMut(ReportedAnswer),
    ) {
        let rights = request.map_or(self.rights, |request| request.rights);
        if self.store.retired {
            if let Some(request) = request {
                let txn = self.store.transactions[id];
                let action = self.sessions[id].action(request.op, rights, &txn);
                let result = self.event::<MUTATION>(
                    id,
                    Event::Detached(action),
                    true,
                    StepUse::Detached,
                    report,
                );
                self.answer_detached::<MUTATION>(id, request, result, true);
            }
            return;
        }
        self.settle::<MUTATION>(id, report);
        let session = self.sessions[id];
        let txn = self.store.transactions[id];
        if session.expired && session.state == State::P {
            let result =
                self.event::<MUTATION>(id, Event::LongestLife, true, StepUse::Expiry, report);
            if result.effect == Effect::StampEnd {
                self.session_write::<MUTATION>(id, true);
                self.finish(id, End::Failed(Failure::LongestLife), rights);
            }
        } else if txn.idle_due(self.store.tick)
            && txn.mapped
            && matches!(session.state, State::U | State::P | State::E(_))
        {
            let result =
                self.event::<MUTATION>(id, Event::IdleLimit, true, StepUse::Expiry, report);
            if result.effect == Effect::Remove {
                self.store.idle_removal = Some(id);
                if !session_oracle::i6(&self.clients[id], &self.store) {
                    self.violate::<MUTATION>(Invariant::I6);
                }
                self.store.idle_removal = None;
                self.session_write::<MUTATION>(id, true);
                self.remove(id);
            }
        }
        let Some(request) = request else { return };
        let txn = self.store.transactions[id];
        let action = self.sessions[id].action(request.op, rights, &txn);
        if !txn.mapped {
            let result =
                self.event::<MUTATION>(id, Event::Action(action), true, StepUse::Detached, report);
            self.answer_detached::<MUTATION>(id, request, result, true);
            return;
        }
        let delivery = txn.request_delivery(false, rights);
        self.store.transactions[id].producers[request.id] = None;
        if !txn.worker && txn.end.is_none() {
            self.store.transactions[id].activity_at = self.store.tick;
        }
        let bit = if request.op == Op::Write {
            let bit = if !txn.worker && txn.end.is_none() {
                self.sessions[id].next_write()
            } else {
                4
            };
            self.clients[id].owed[request.id].as_mut().unwrap().tag = bit;
            bit
        } else {
            0
        };
        let result =
            self.event::<MUTATION>(id, Event::Action(action), true, StepUse::Request, report);
        if matches!(
            result.effect,
            Effect::Append
                | Effect::Remove
                | Effect::StartSwitch
                | Effect::StartWrite
                | Effect::CommitDelta
                | Effect::ReplayIntents
                | Effect::StampEnd
                | Effect::Touch
                | Effect::TakeSnapshot
        ) {
            self.session_write::<MUTATION>(id, true);
        }
        let session = &mut self.sessions[id];
        let position = self.store.version;
        let pin = (result.effect == Effect::StartSwitch).then(|| Pin::capture(&self.store, rights));
        let txn = &mut self.store.transactions[id];
        match result.effect {
            Effect::Append => {
                txn.pending |= bit;
                txn.applied |= bit;
                txn.touched_at = self.store.tick;
            }
            Effect::Touch => txn.touched_at = self.store.tick,
            Effect::StartSwitch | Effect::StartWrite => {
                let switch = result.effect == Effect::StartSwitch;
                if switch {
                    session.pin = pin;
                    txn.first_capture.get_or_insert(position);
                    txn.pinned = true;
                    txn.pin_rights = Some(rights);
                }
                let slot = session.tasks.iter().position(Option::is_none).unwrap();
                session.ticket = Some(slot);
                session.tasks[slot] = Some(Task {
                    request: request.id,
                    rights,
                    switch,
                    bit,
                    scratch: txn.pending | bit,
                    pin: if switch { session.pin } else { None },
                    guard: true,
                    needs_settle: true,
                    awaiting: true,
                    phase: Phase::Pending,
                });
                txn.guards |= 1 << slot;
                txn.producers[request.id] = Some(Producer::Task { slot, switch });
                txn.touched_at = self.store.tick;
            }
            Effect::TakeSnapshot => {
                session.read = session.pin.map(|pin| Read {
                    request: request.id,
                    first: false,
                    pin,
                    private: txn.pending,
                });
                if session.read.is_some() {
                    txn.producers[request.id] = Some(Producer::Read);
                }
                txn.touched_at = self.store.tick;
            }
            Effect::ReplayIntents | Effect::CommitDelta => {
                session.commit = Some(Commit {
                    request: request.id,
                    writes: txn.pending,
                    pinned: result.effect == Effect::CommitDelta,
                    rights,
                    awaiting: true,
                    published: false,
                });
                txn.worker = true;
                txn.evidence_rights = rights;
                txn.producers[request.id] = Some(Producer::Commit);
            }
            Effect::WaitOutcome => {
                let record = if session.commit.is_some_and(|commit| commit.published) {
                    Observation::Published
                } else {
                    Observation::InProgress
                };
                session.repeat = Some(Repeat {
                    request: request.id,
                    rights,
                    record,
                });
                txn.producers[request.id] = Some(Producer::Repeat);
            }
            Effect::Remove => self.remove(id),
            Effect::StampEnd => {
                let end = if request.op == Op::Commit
                    && txn.pending == 0
                    && session.pin.is_some_and(|pin| pin.rights == rights)
                {
                    End::Committed
                } else {
                    End::Refused(Failure::Rights)
                };
                txn.read_only = end == End::Committed;
                self.finish(id, end, rights);
            }
            _ => {}
        }
        self.receive::<MUTATION>(id, request.id, result.answer, delivery);
    }

    fn answer_task<const MUTATION: u8>(
        &mut self,
        id: usize,
        slot: usize,
        report: &mut impl FnMut(ReportedAnswer),
    ) {
        let task = self.sessions[id].tasks[slot].unwrap();
        let sent = self.clients[id].owed[task.request].unwrap();
        let answer = match task.visible_end() {
            Some(End::Committed) if sent.op == Op::Read => Seen::Pending,
            Some(End::Committed) => Seen::WriteOk,
            Some(End::Refused(failure) | End::Failed(failure)) => Seen::Error(failure),
            Some(End::Unknown) => Seen::Error(Failure::Status(500)),
            None => Seen::Error(Failure::WriteBusy),
        };
        let result = self.event::<MUTATION>(
            id,
            Event::TaskReady {
                switch: task.switch,
                record: task.observe(),
            },
            false,
            StepUse::TaskReady,
            report,
        );
        let session = &mut self.sessions[id];
        let producer = if result.effect == Effect::TakeSnapshot {
            session.read = task.pin.map(|pin| Read {
                request: task.request,
                first: true,
                pin,
                private: task.scratch,
            });
            session.read.map(|_| Producer::Read)
        } else {
            None
        };
        session.tasks[slot] = if matches!(task.phase, Phase::Recorded(_)) {
            None
        } else {
            Some(Task {
                awaiting: false,
                ..task
            })
        };
        self.receive::<MUTATION>(
            id,
            task.request,
            result.answer,
            Delivery::Task { slot, answer },
        );
        self.store.transactions[id].producers[task.request] = producer;
    }

    fn do_move<const MUTATION: u8>(
        &mut self,
        action: Move,
        mut report: impl FnMut(ReportedAnswer),
    ) {
        self.store.at_swap = None;
        match action {
            Move::Lookup(id, op) => {
                let rights = self.rights;
                let request = Request {
                    id: self.clients[id].send(Sent { op, rights, tag: 0 }),
                    op,
                    rights,
                };
                let txn = &mut self.store.transactions[id];
                let session = &mut self.sessions[id];
                if txn.mapped && !self.store.retired {
                    if op == Op::Rollback
                        && let Some(slot) = session.ticket
                    {
                        session.try_cancel(slot);
                    }
                    txn.producers[request.id] = Some(Producer::Lock);
                    if session.waiting.is_some() {
                        debug_assert_eq!(op, Op::Rollback);
                        session.rollback_waiting = Some(request);
                    } else {
                        session.waiting = Some(request);
                    }
                } else {
                    let action = session.action(op, rights, txn);
                    let result = self
                        .checked_transition::<MUTATION>(
                            State::G,
                            Event::Action(action),
                            StepUse::Detached,
                        )
                        .step;
                    if result.next != State::G {
                        self.session_write::<MUTATION>(id, false);
                    }
                    report((Some(result.cell), result.answer));
                    self.answer_detached::<MUTATION>(id, request, result, false);
                }
            }
            Move::SessionLock(id) => {
                let request = self.sessions[id].waiting.take();
                self.section::<MUTATION>(id, request, &mut report);
            }
            Move::RollbackLock(id) => {
                let request = self.sessions[id].rollback_waiting.take();
                self.section::<MUTATION>(id, request, &mut report);
            }
            Move::WaitTimeout(id) | Move::RollbackTimeout(id) => {
                let request = if matches!(action, Move::RollbackTimeout(_)) {
                    self.sessions[id].rollback_waiting.take()
                } else {
                    self.sessions[id].waiting.take()
                }
                .unwrap();
                let answer = lock_timeout();
                report((None, answer));
                self.receive::<MUTATION>(id, request.id, answer, Delivery::LockTimeout);
            }
            Move::DropTaskRequest(id, slot) => {
                let task = self.sessions[id].tasks[slot].unwrap();
                if task.phase == Phase::Pending {
                    let txn = &mut self.store.transactions[id];
                    txn.dropped_unclaimed |= task.bit;
                    if task.switch {
                        txn.cancelled_switch = Some(slot);
                    }
                }
                self.abandon(id, task.request);
                if matches!(task.phase, Phase::Recorded(_)) {
                    self.sessions[id].tasks[slot] = None;
                    return;
                }
                let result = self.event::<MUTATION>(
                    id,
                    Event::Dropped,
                    false,
                    StepUse::TaskDrop,
                    &mut report,
                );
                let session = &mut self.sessions[id];
                session.tasks[slot].as_mut().unwrap().awaiting = false;
                if result.effect == Effect::TryCancel {
                    session.try_cancel(slot);
                }
            }
            Move::DropWaiting(id) => {
                let request = self.sessions[id].waiting.take().unwrap();
                self.abandon(id, request.id);
            }
            Move::DropRollback(id) => {
                let request = self.sessions[id].rollback_waiting.take().unwrap();
                self.abandon(id, request.id);
            }
            Move::DropRead(id) => {
                let read = self.sessions[id].read.take().unwrap();
                self.abandon(id, read.request);
            }
            Move::DropRepeat(id) => {
                let repeat = self.sessions[id].repeat.take().unwrap();
                self.abandon(id, repeat.request);
            }
            Move::DropCommitRequest(id) => {
                self.event::<MUTATION>(id, Event::Dropped, false, StepUse::Notice, &mut report);
                let commit = self.sessions[id].commit.as_mut().unwrap();
                commit.awaiting = false;
                let request = commit.request;
                self.abandon(id, request);
            }
            Move::Claim(id, slot) => {
                self.sessions[id].tasks[slot].as_mut().unwrap().phase = Phase::Claimed;
            }
            Move::Swap(id, slot) | Move::Fail(id, slot) => {
                let session = self.sessions[id];
                let task = session.tasks[slot].unwrap();
                let orphaned = self.store.retired;
                let (event, end) = if orphaned {
                    (Event::OrphanEnded, End::Failed(Failure::Missing(false)))
                } else if matches!(action, Move::Fail(..)) {
                    (
                        Event::EvaluationFailed(Problem::Status(403)),
                        End::Failed(Failure::Status(403)),
                    )
                } else if session.expired {
                    (Event::LongestLife, End::Failed(Failure::LongestLife))
                } else {
                    (Event::Swapped, End::Committed)
                };
                let result = if task.guard {
                    self.event::<MUTATION>(id, event, true, StepUse::TaskEnd, &mut report)
                } else {
                    // The lost-write mutation leaves an older scratch buffer running.
                    self.checked_transition::<MUTATION>(State::Pw, event, StepUse::TaskEnd)
                        .step
                };
                if orphaned {
                    if let Some((_, recorded)) =
                        self.record_terminal::<MUTATION>(id, end, result.effect)
                    {
                        let task = self.sessions[id].tasks[slot].as_mut().unwrap();
                        task.phase = Phase::Ended(recorded);
                        task.needs_settle = false;
                    }
                    return;
                }
                let task_slot = self.sessions[id].tasks[slot].as_mut().unwrap();
                task_slot.phase = Phase::Ended(end);
                task_slot.needs_settle = false;
                match result.effect {
                    Effect::StampEnd => {
                        self.session_write::<MUTATION>(id, task.guard);
                        self.finish(id, end, task.rights);
                    }
                    Effect::RestoreIntents => {
                        self.session_write::<MUTATION>(id, task.guard);
                        self.restore_intents(id);
                    }
                    Effect::Swap | Effect::SwapAndRead => {
                        self.session_write::<MUTATION>(id, task.guard);
                        let txn = &mut self.store.transactions[id];
                        txn.pending = task.scratch;
                        txn.applied |= task.scratch;
                        self.store.at_swap = Some((id, slot));
                    }
                    _ => {}
                }
            }
            Move::DropGuard(id, slot, end) => {
                let task = self.sessions[id].tasks[slot].unwrap();
                self.release_guard(id, slot);
                if task.guard && task.needs_settle && !self.store.retired {
                    let txn = &mut self.store.transactions[id];
                    txn.settling = true;
                    if !task.switch {
                        txn.end = Some(end);
                        txn.evidence_rights = task.rights;
                    }
                    self.sessions[id].terminal = Some(task_outcome(end));
                }
                self.sessions[id].tasks[slot] = task.awaiting.then_some(Task {
                    guard: false,
                    phase: Phase::Recorded(end),
                    ..task
                });
            }
            Move::TaskReady(id, slot) => self.answer_task::<MUTATION>(id, slot, &mut report),
            Move::TaskDeadline(id, slot) => {
                self.sessions[id].try_cancel(slot);
                self.answer_task::<MUTATION>(id, slot, &mut report);
            }
            Move::PublishCommit(id) => {
                let commit = self.sessions[id].commit.unwrap();
                self.store.publish(id, commit.writes);
                let session = &mut self.sessions[id];
                session.commit.as_mut().unwrap().published = true;
                if let Some(repeat) = &mut session.repeat {
                    repeat.record = Observation::Published;
                }
            }
            Move::CommitExit(id, exit) => {
                let commit = self.sessions[id].commit.take().unwrap();
                let end = match exit {
                    Exit::Published { .. } => End::Committed,
                    Exit::Conflict => End::Refused(Failure::Conflict),
                    Exit::RightsChanged => End::Refused(Failure::Rights),
                    Exit::Failed(Problem::Status(code)) => End::Failed(Failure::Status(code)),
                    Exit::Failed(other) => panic!("unexpected model commit failure: {other:?}"),
                    Exit::Cancelled => End::Failed(Failure::Status(408)),
                    Exit::Unavailable => End::Failed(Failure::Busy),
                    Exit::Entered => End::Unknown,
                    Exit::NeverRan => End::Failed(Failure::NotRun),
                    Exit::Claimed => End::Failed(Failure::Unpublished),
                };
                let result = self.event::<MUTATION>(
                    id,
                    Event::WorkerExit(exit),
                    false,
                    StepUse::CommitExit {
                        without_waiter: (!commit.awaiting).then_some(end),
                    },
                    &mut |reply| {
                        if commit.awaiting {
                            report(reply);
                        }
                    },
                );
                let recorded = self.record_terminal::<MUTATION>(id, end, result.effect);
                let txn = &mut self.store.transactions[id];
                txn.end = Some(end);
                txn.checked_at = Some(self.store.version);
                if let Some((outcome, _)) = recorded {
                    txn.worker = false;
                    txn.settling = txn.mapped && !self.store.retired;
                    let session = &mut self.sessions[id];
                    session.terminal = Some(outcome);
                    if let Some(repeat) = &mut session.repeat {
                        repeat.record = Observation::Ended(outcome);
                    }
                }
                if commit.awaiting {
                    self.receive::<MUTATION>(
                        id,
                        commit.request,
                        result.answer,
                        Delivery::CommitExit,
                    );
                }
            }
            Move::CommitGrace(id) => {
                let commit = self.sessions[id].commit.unwrap();
                let record = if commit.published {
                    Observation::Published
                } else {
                    Observation::InProgress
                };
                let result = self.event::<MUTATION>(
                    id,
                    Event::CommitGrace(record),
                    false,
                    StepUse::Reply,
                    &mut report,
                );
                self.sessions[id].commit.as_mut().unwrap().awaiting = false;
                self.receive::<MUTATION>(id, commit.request, result.answer, Delivery::CommitGrace);
            }
            Move::RepeatReady(id) => {
                let repeat = self.sessions[id].repeat.take().unwrap();
                let same_rights = repeat.rights == self.store.transactions[id].evidence_rights;
                let result = self.event::<MUTATION>(
                    id,
                    Event::RepeatCommitReady {
                        record: repeat.record,
                        same_rights,
                    },
                    false,
                    StepUse::Reply,
                    &mut report,
                );
                self.receive::<MUTATION>(id, repeat.request, result.answer, Delivery::Repeat);
            }
            Move::Stream(id, end) => {
                let read = self.sessions[id].read.take().unwrap();
                let stream_end = match end {
                    Stream::Data => StreamEnd::Data,
                    Stream::Aborted => StreamEnd::Aborted,
                    Stream::Error(failure) => StreamEnd::Error(model_problem(failure)),
                };
                let result = self.event::<MUTATION>(
                    id,
                    Event::ReadEnded {
                        first: read.first,
                        end: stream_end,
                    },
                    false,
                    StepUse::Reply,
                    &mut report,
                );
                let tags = if read.pin.rights && matches!(end, Stream::Data | Stream::Aborted) {
                    read.private | (u8::from(read.pin.protected) << 1)
                } else {
                    0
                };
                self.receive::<MUTATION>(
                    id,
                    read.request,
                    result.answer,
                    Delivery::Read {
                        version: read.pin.version(),
                        tags,
                        end,
                    },
                );
            }
            Move::Sweep(id) => {
                let txn = &self.store.transactions[id];
                let idle_due = txn.mapped
                    && !self.store.retired
                    && txn.expected_idle_due(self.store.tick)
                    && !txn.settling
                    && !txn.worker
                    && txn.guards == 0;
                self.swept = true;
                self.section::<MUTATION>(id, None, &mut report);
                if idle_due && !session_oracle::i6(&self.clients[id], &self.store) {
                    self.violate::<MUTATION>(Invariant::I6);
                }
            }
            Move::IdleTick => self.store.tick += 1,
            Move::LifeClock => {
                for session in &mut self.sessions {
                    if matches!(session.state, State::P | State::Pw) {
                        session.expired = true;
                    }
                }
            }
            Move::OtherCommit => self.store.other_commit(),
            Move::RulesChange => self.store.version.1 += 1,
            Move::RightsChange => {
                self.rights_changed = true;
                self.rights = !self.rights;
            }
            Move::NoOpChange => {}
            Move::ReplaceRepository => {
                let mut retire = true;
                for id in 0..N {
                    let result = self.event::<MUTATION>(
                        id,
                        Event::RepositoryReplaced,
                        false,
                        StepUse::Replace,
                        &mut report,
                    );
                    retire &= result.effect == Effect::RetireMap;
                }
                if retire {
                    self.store.retired = true;
                    for txn in &mut self.store.transactions {
                        txn.settling = false;
                    }
                }
            }
            Move::Restart => {
                self.restarted = true;
                for id in 0..N {
                    let result = self
                        .checked_transition::<MUTATION>(
                            self.sessions[id].state,
                            Event::Restart,
                            StepUse::Restart,
                        )
                        .step;
                    report((Some(result.cell), result.answer));
                    if result.effect != Effect::Forget {
                        continue;
                    }
                    self.sessions[id] = Session::new(result.next);
                    let client = &mut self.clients[id];
                    let txn = &mut self.store.transactions[id];
                    client.owed.fill(None);
                    txn.mapped = false;
                    txn.pinned = false;
                    txn.pin_rights = None;
                    txn.pending = 0;
                    txn.producers.fill(None);
                    txn.guards = 0;
                    txn.settling = false;
                    txn.worker = false;
                }
            }
        }
        let change = match action {
            Move::OtherCommit | Move::RulesChange => Some(Change::RevisionOrRules),
            Move::RightsChange => Some(Change::Rights),
            Move::NoOpChange => Some(Change::NoOp),
            _ => None,
        };
        if let Some(change) = change {
            for id in 0..N {
                self.event::<MUTATION>(
                    id,
                    Event::Changed(change),
                    false,
                    StepUse::Notice,
                    &mut report,
                );
            }
        }
    }
}

impl<const N: usize, const MUTATION: u8, const FOCUS: u8> Model for Sessions<N, MUTATION, FOCUS> {
    type World = World<N>;
    type Move = Move;
    const DRAIN_BOUND: usize = 16 * N;

    fn start() -> World<N> {
        let mut world = World {
            sessions: [Session::new(State::G); N],
            clients: std::array::from_fn(|id| ClientRecord {
                transaction: id,
                ..ClientRecord::default()
            }),
            store: StoreRecord::default(),
            rights: true,
            rights_changed: false,
            swept: false,
            restarted: false,
            violation: None,
        };
        let begin = world
            .checked_transition::<GOOD>(
                State::G,
                Event::Begin { valid_level: true },
                StepUse::Begin,
            )
            .step;
        if begin.effect != Effect::Create || world.violation.is_some() {
            return world;
        }
        world.sessions.fill(Session::new(begin.next));
        // Begin and one completed first read are fixture setup. This leaves an
        // observed client for the two-write witness and U clients for the switch.
        let pinned = N - 1;
        for action in [
            Move::Lookup(pinned, Op::Read),
            Move::SessionLock(pinned),
            Move::Swap(pinned, 0),
            Move::DropGuard(pinned, 0, End::Committed),
            Move::TaskReady(pinned, 0),
            Move::Stream(pinned, Stream::Data),
        ] {
            world.do_move::<GOOD>(action, |_| {});
        }
        world
    }

    fn moves(world: &World<N>) -> Vec<Move> {
        let mut moves = Vec::new();
        if world.restarted {
            return moves;
        }
        if FOCUS == GOOD {
            moves.push(Move::Restart);
        }
        if matches!(
            FOCUS,
            REPLAY_OBSERVED | WRITE_STARTS_SWITCH | CHANGE_REMOVES
        ) && world.store.external_publication.is_none()
        {
            moves.push(Move::OtherCommit);
        }
        if matches!(FOCUS, FAILED_TOMBSTONE_COMMITS | FAILED_ROLLBACK_REMOVES)
            && !world.sessions[N - 1].expired
            && world.store.transactions[N - 1].end.is_none()
        {
            moves.push(Move::LifeClock);
        }
        if matches!(
            FOCUS,
            REMOVED_WITHOUT_GONE | STALE_REPEAT_EFFECT | PING_WITHOUT_TOUCH
        ) && world.store.transactions[0].mapped
            && world.store.tick < IDLE_TICKS
        {
            moves.push(Move::IdleTick);
        }
        if FOCUS == REPLACEMENT_WITHOUT_RETIRE && !world.store.retired {
            moves.push(Move::ReplaceRepository);
        }
        if FOCUS == GOOD && !world.store.retired {
            moves.extend([Move::ReplaceRepository, Move::NoOpChange]);
            if world.store.external_publication.is_none() {
                moves.push(Move::OtherCommit);
            }
            if world.store.version.1 == 0 {
                moves.push(Move::RulesChange);
            }
            if !world.rights_changed {
                moves.push(Move::RightsChange);
            }
            if world.store.tick < IDLE_TICKS
                && world.store.transactions.iter().any(|txn| txn.mapped)
            {
                moves.push(Move::IdleTick);
            }
            if world
                .sessions
                .iter()
                .any(|s| matches!(s.state, State::P | State::Pw) && !s.expired)
            {
                moves.push(Move::LifeClock);
            }
        }

        for (id, s) in world.sessions.iter().enumerate() {
            let txn = &world.store.transactions[id];
            // Red runs cover the affected client's request interleavings.
            // Other clients and unrelated stimuli run in the green search.
            let focused_client = match FOCUS {
                EARLY_RELEASE
                | REPLAY_OBSERVED
                | DROP_TO_U
                | NO_IDLE_EXPIRY
                | IMMEDIATE_COMMIT
                | READ_WITHOUT_SNAPSHOT
                | WRITE_STARTS_SWITCH
                | FAILED_TOMBSTONE_COMMITS
                | FAILED_ROLLBACK_REMOVES
                | DROP_WITHOUT_CANCEL
                | COMMITTED_ROLLBACK => N - 1,
                _ => 0,
            };
            if FOCUS != GOOD && id != focused_client {
                continue;
            }
            if s.rollback_waiting.is_some() {
                moves.extend([Move::DropRollback(id), Move::RollbackTimeout(id)]);
                if !s.held() {
                    moves.push(Move::RollbackLock(id));
                }
            }
            if let Some(request) = s.waiting {
                // Timeout can also lose to a short, synchronous lock section.
                moves.extend([Move::DropWaiting(id), Move::WaitTimeout(id)]);
                if txn.mapped
                    && !world.store.retired
                    && s.rollback_waiting.is_none()
                    && request.op != Op::Rollback
                {
                    moves.push(Move::Lookup(id, Op::Rollback));
                }
                if !s.held() {
                    moves.push(Move::SessionLock(id));
                }
            } else {
                if s.repeat.is_none() {
                    moves.push(Move::Lookup(id, Op::Commit));
                }
                if s.rollback_waiting.is_none() {
                    moves.push(Move::Lookup(id, Op::Rollback));
                }
                if s.issued < 2 && s.tasks.iter().any(Option::is_none) {
                    moves.push(Move::Lookup(id, Op::Write));
                }
                if s.read.is_none() && !s.tasks.iter().flatten().any(|task| task.switch) {
                    moves.push(Move::Lookup(id, Op::Read));
                }
                moves.push(Move::Lookup(id, Op::Ping));
            }
            if !world.swept
                && txn.mapped
                && !world.store.retired
                && !s.held()
                && (txn.idle_due(world.store.tick)
                    || txn.expected_idle_due(world.store.tick)
                    || s.expired
                    || s.can_settle())
            {
                moves.push(Move::Sweep(id));
            }
            for (slot, task) in s.tasks.iter().enumerate() {
                let Some(task) = task else { continue };
                if task.awaiting {
                    moves.push(Move::DropTaskRequest(id, slot));
                }
                let panic = End::Failed(Failure::Unpublished);
                match task.phase {
                    Phase::Pending => {
                        moves.extend([
                            Move::Claim(id, slot),
                            Move::Swap(id, slot),
                            Move::Fail(id, slot),
                            Move::DropGuard(id, slot, panic),
                            Move::DropGuard(id, slot, End::Failed(Failure::NotRun)),
                        ]);
                        if task.awaiting {
                            moves.push(Move::TaskDeadline(id, slot));
                        }
                    }
                    Phase::Claimed => {
                        moves.extend([Move::Swap(id, slot), Move::DropGuard(id, slot, panic)]);
                        if task.awaiting {
                            moves.push(Move::TaskReady(id, slot));
                        }
                    }
                    Phase::Ended(outcome) => {
                        moves.push(Move::DropGuard(id, slot, outcome));
                        if task.awaiting && (!task.switch || outcome != End::Committed) {
                            moves.push(Move::TaskReady(id, slot));
                        }
                    }
                    Phase::Recorded(_) => moves.push(Move::TaskReady(id, slot)),
                }
            }
            if let Some(commit) = s.commit {
                let normal = world.normal_commit_move(id, commit);
                moves.push(normal);
                if commit.published {
                    if commit.writes != 0 {
                        moves.push(Move::CommitExit(id, Exit::Published { panicked: true }));
                    }
                } else {
                    moves.extend([
                        Move::CommitExit(id, Exit::Failed(Problem::Status(403))),
                        Move::CommitExit(id, Exit::Unavailable),
                        Move::CommitExit(id, Exit::NeverRan),
                        Move::CommitExit(id, Exit::Claimed),
                    ]);
                    if matches!(normal, Move::PublishCommit(_)) && commit.writes != 0 {
                        moves.push(Move::CommitExit(id, Exit::Entered));
                    }
                    if commit.awaiting {
                        moves.push(Move::CommitExit(id, Exit::Cancelled));
                    }
                }
                if commit.awaiting {
                    moves.extend([Move::DropCommitRequest(id), Move::CommitGrace(id)]);
                }
            }
            if s.repeat.is_some() {
                moves.extend([Move::RepeatReady(id), Move::DropRepeat(id)]);
            }
            if s.read.is_some() {
                moves.extend([
                    Move::Stream(id, Stream::Data),
                    Move::Stream(id, Stream::Error(Failure::Status(408))),
                    Move::Stream(id, Stream::Aborted),
                    Move::DropRead(id),
                ]);
            }
        }
        moves
    }

    fn apply(world: &World<N>, action: &Move) -> World<N> {
        let mut next = world.clone();
        next.do_move::<MUTATION>(*action, |_| {});
        for id in 0..N {
            let client = &next.clients[id];
            let checks = [
                (session_oracle::i1(client, &next.store), Invariant::I1),
                (session_oracle::i4(client, &next.store), Invariant::I4),
                (session_oracle::i5(client, &next.store), Invariant::I5),
                (session_oracle::i10(client, &next.store), Invariant::I10),
            ];
            for (holds, invariant) in checks {
                if !holds {
                    next.violate::<MUTATION>(invariant);
                }
            }
        }
        next.store.at_swap = None;
        next
    }

    fn check(world: &World<N>) -> Result<(), String> {
        if let Some(violation) = world.violation {
            return Err(format!("{violation:?}"));
        }
        Ok(())
    }

    fn quiet(world: &World<N>) -> bool {
        world.clients.iter().all(|client| {
            session_oracle::no_work_owed(client, &world.store)
                && session_oracle::i6(client, &world.store)
        })
    }

    fn drain_move(world: &World<N>) -> Option<Move> {
        for (id, s) in world.sessions.iter().enumerate() {
            if let Some((slot, task)) = s
                .tasks
                .iter()
                .enumerate()
                .find_map(|(slot, task)| task.map(|task| (slot, task)))
            {
                return Some(match task.phase {
                    Phase::Pending | Phase::Claimed => Move::Swap(id, slot),
                    Phase::Ended(outcome) => Move::DropGuard(id, slot, outcome),
                    Phase::Recorded(_) => Move::TaskReady(id, slot),
                });
            }
            if let Some(commit) = s.commit {
                return Some(world.normal_commit_move(id, commit));
            }
            if s.repeat.is_some() {
                return Some(Move::RepeatReady(id));
            }
            if s.read.is_some() {
                return Some(Move::Stream(id, Stream::Data));
            }
            if s.waiting.is_some() {
                return Some(Move::SessionLock(id));
            }
            if s.rollback_waiting.is_some() {
                return Some(Move::RollbackLock(id));
            }
            let txn = &world.store.transactions[id];
            if !world.store.retired
                && txn.mapped
                && (s.can_settle()
                    || txn.idle_due(world.store.tick)
                    || txn.expected_idle_due(world.store.tick)
                    || s.expired)
            {
                return Some(Move::Sweep(id));
            }
        }
        (!world.store.retired
            && world
                .store
                .transactions
                .iter()
                .any(|txn| txn.mapped && txn.pinned))
        .then_some(Move::IdleTick)
    }

    fn answer(world: &World<N>, action: &Move, _next: &World<N>) -> String {
        let mut cells = Vec::new();
        world
            .clone()
            .do_move::<MUTATION>(*action, |reply| cells.push(reply));
        format!("{cells:?}")
    }
}

fn print_trace(trace: &Trace<Move>) {
    for (prefix, path) in [("search", &trace.schedule), ("drain", &trace.drain)] {
        for (action, answer) in path {
            eprintln!("  {prefix} {action:?} => {answer}");
        }
    }
    eprintln!("{}", trace.violation);
}

fn print_stats(clients: usize, limit: usize, stats: Stats, elapsed: Duration) {
    eprintln!(
        "{clients} clients, depth limit {limit}, completed {}, {} worlds, collision bound {:.3e}, search {:?}, drain {:?}, total {:?}",
        stats.completed_depth,
        stats.worlds,
        stats.collision_bound(),
        elapsed.saturating_sub(stats.drain_time),
        stats.drain_time,
        elapsed,
    );
}

fn red<const MUTATION: u8>(expected: &str) {
    let start = Instant::now();
    let outcome = explore::<Sessions<2, MUTATION, MUTATION>>(WITNESS_DEPTH, ORDINARY_WORLDS);
    let elapsed = start.elapsed();
    let trace = match outcome {
        Err(trace) => trace,
        Ok(stats) => {
            print_stats(2, WITNESS_DEPTH, stats, elapsed);
            panic!("missing witness: {expected}");
        }
    };
    print_stats(2, WITNESS_DEPTH, trace.stats, elapsed);
    print_trace(&trace);
    assert_eq!(trace.violation.trim_start_matches("drain: "), expected);
    let stats = explore::<Sessions<2, GOOD, MUTATION>>(WITNESS_DEPTH, ORDINARY_WORLDS)
        .expect("the same stimulus passes with the real step");
    assert_eq!(stats.completed_depth, WITNESS_DEPTH);
    eprintln!("real step: {expected} absent through depth {WITNESS_DEPTH}");
}

// Spec §2b E3b: ROLLBACK after an unknown publication must not claim a rollback.
#[test]
fn explorer_rejects_rollback_after_unknown() {
    red::<UNKNOWN_ROLLBACK>("I3");
}

// Spec §2b C1: even a panic after publication answers from the committed record.
#[test]
fn explorer_rejects_published_failure() {
    red::<JOIN_ANSWER>("I3");
}

// Spec §2b Pw3/P16: a dropped handler cannot let an older scratch overwrite an acknowledged write.
#[test]
fn explorer_rejects_lost_second_write() {
    red::<EARLY_RELEASE>("I4");
}

// Spec §2b X7: a write waiting on a removed transaction must not answer 200 then miss its COMMIT.
#[test]
fn explorer_rejects_success_after_removal() {
    red::<STALE_LOOKUP>("I3");
}

// Spec §2b P9: a client that has read data can publish only at its current pin.
#[test]
fn explorer_rejects_replaying_observed_writes() {
    red::<REPLAY_OBSERVED>("I1");
}

// Spec §2b U1, invariant 3: a successful write survives a later publication.
#[test]
fn explorer_rejects_acknowledging_without_append() {
    red::<ACK_WITHOUT_APPEND>("I4");
}

// Spec §2b C1: publication answers success even if a worker omits the record.
#[test]
fn explorer_rejects_unrecorded_published_failure() {
    red::<PUBLISHED_WITHOUT_RECORD>("I3");
}

// Spec §2b U8/X7: a request waiting on an unmapped session gets the missing answer.
#[test]
fn explorer_rejects_idle_removal_without_gone() {
    red::<REMOVED_WITHOUT_GONE>("I3");
}

// Spec §2b P16/Pw3: dropping the handler cannot take an unfinished write out of Pw.
#[test]
fn explorer_rejects_drop_returning_to_unobserved() {
    red::<DROP_TO_U>("I8");
}

// Spec §2b P12: the server alone must release an abandoned pin after idle expiry.
#[test]
fn explorer_rejects_sweep_retaining_an_abandoned_pin() {
    red::<NO_IDLE_EXPIRY>("I6");
}

// Spec §2b P9/P9a: only an empty pinned delta can commit without publication.
#[test]
fn explorer_rejects_immediate_success_for_a_nonempty_commit() {
    red::<IMMEDIATE_COMMIT>("I3");
}

// Spec §2b C5b: the grace answer stays retryable until publication.
#[test]
fn explorer_rejects_success_during_unpublished_grace() {
    red::<GRACE_BEFORE_PUBLISH>("I3");
}

// Spec §2b U7/P11: success removes the session and releases its pin.
#[test]
fn explorer_rejects_rollback_retaining_a_live_session() {
    red::<ROLLBACK_STAYS_LIVE>("I3");
}

// Spec §2b P5: the accepted read captures a snapshot and runs outside the lock.
#[test]
fn explorer_rejects_read_without_a_snapshot() {
    red::<READ_WITHOUT_SNAPSHOT>("I5");
}

// Spec §2b S3: an aborted first read reports its abort even after showing data.
#[test]
fn explorer_rejects_completed_answer_for_an_aborted_stream() {
    red::<WRONG_STREAM_ANSWER>("I3");
}

// Spec §2 invariant 1: a new scratch task cannot replace the client’s first shown revision.
#[test]
fn explorer_rejects_repinning_an_observed_write() {
    red::<WRITE_STARTS_SWITCH>("I1");
}

// Spec §2b E1: a later COMMIT must repeat the failed outcome in the tombstone.
#[test]
fn explorer_rejects_commit_success_from_failed_tombstone() {
    red::<FAILED_TOMBSTONE_COMMITS>("I3");
}

// Spec §2b E2: ROLLBACK cannot undo a committed outcome.
#[test]
fn explorer_rejects_rollback_success_after_commit() {
    red::<COMMITTED_ROLLBACK>("I3");
}

// Spec §2b G1: an absent transaction cannot be reported committed.
#[test]
fn explorer_rejects_commit_success_without_session() {
    red::<MISSING_COMMIT>("I3");
}

// Spec §2b C8 and rule 8: a waiting repeat has its own outcome producer.
#[test]
fn explorer_rejects_pending_repeat_without_producer() {
    red::<LOST_REPEAT>("I5");
}

// Spec §2 invariant 3 and C10: no new write can be acknowledged once COMMIT took its delta.
#[test]
fn explorer_rejects_write_acknowledged_during_commit() {
    red::<WRITE_DURING_COMMIT>("I4");
}

// X7 and rule 12: a queued COMMIT still gets the missing answer without starting work.
#[test]
fn explorer_rejects_repeat_started_after_idle_removal() {
    red::<STALE_REPEAT_EFFECT>("I8");
}

// §2b E3 and E1: a successful rollback leaves a failed end available to COMMIT.
#[test]
fn explorer_rejects_rollback_removing_a_failed_tombstone() {
    red::<FAILED_ROLLBACK_REMOVES>("I3");
}

// Rule 3 and P16: a handler drop before claim prevents the write from applying.
#[test]
fn explorer_rejects_an_unclaimed_write_after_handler_drop() {
    red::<DROP_WITHOUT_CANCEL>("I10");
}

// X4: replacement retires the old map through its returned effect.
#[test]
fn explorer_rejects_replacement_without_retirement() {
    red::<REPLACEMENT_WITHOUT_RETIRE>("InvalidTransition");
}

// C12: dropping the COMMIT handler leaves the worker running.
#[test]
fn explorer_rejects_commit_handler_cancel() {
    red::<COMMIT_DROP_CANCELS>("InvalidTransition");
}

// U10: a revision notification leaves the session alone.
#[test]
fn explorer_rejects_removal_on_revision_notice() {
    red::<CHANGE_REMOVES>("InvalidTransition");
}

// S6: dropping the first read before claim cannot leave its pin in use.
#[test]
fn explorer_rejects_unclaimed_read_after_handler_drop() {
    red::<READ_DROP_WITHOUT_CANCEL>("I10");
}

// U5: PING advances the idle deadline, so the next tick cannot expire the session.
#[test]
fn explorer_rejects_ping_without_idle_refresh() {
    red::<PING_WITHOUT_TOUCH>("I6");
}

#[test]
fn oracle_source_uses_only_its_records() {
    let source = include_str!("session_oracle.rs");
    for name in ["session_step", "crate::", "super::"] {
        assert!(
            !source.contains(name),
            "unexpected lifecycle reference: {name}"
        );
    }
}

// C2 and X4: the terminal must record the observed end even after the client left.
#[test]
fn oracle_requires_the_terminal_record_without_a_waiter() {
    for (observed, written) in [
        (
            End::Refused(Failure::Conflict),
            Effect::Record(Outcome::Failed(Problem::Conflict)),
        ),
        (End::Failed(Failure::Missing(false)), Effect::None),
    ] {
        let mut world = Sessions::<2, GOOD>::start();
        world.record_terminal::<GOOD>(0, observed, written);
        assert_eq!(world.violation, Some(Invariant::I9));
    }
}

// C1 and C8: identical outcomes do not make two waiting requests interchangeable.
#[test]
fn oracle_rejects_a_commit_answer_delivered_to_the_repeat() {
    let mut world = Sessions::<2, GOOD>::start();
    for action in [
        Move::Lookup(0, Op::Commit),
        Move::SessionLock(0),
        Move::Lookup(0, Op::Commit),
        Move::SessionLock(0),
        Move::PublishCommit(0),
    ] {
        world.do_move::<GOOD>(action, |_| {});
    }
    let repeat = world.sessions[0].repeat.unwrap();
    world.receive::<GOOD>(
        0,
        repeat.request,
        Answer::Recorded {
            outcome: Outcome::Committed,
            evidence: true,
        },
        Delivery::CommitExit,
    );
    assert_eq!(world.violation, Some(Invariant::I5));
}

// C1 and E1: the evidence bit reports whether the request holds the recorded rights.
#[test]
fn oracle_requires_evidence_for_the_commit_requester() {
    let mut world = Sessions::<2, GOOD>::start();
    for action in [
        Move::Lookup(0, Op::Commit),
        Move::SessionLock(0),
        Move::PublishCommit(0),
    ] {
        world.do_move::<GOOD>(action, |_| {});
    }
    let request = world.sessions[0].commit.unwrap().request;
    world.receive::<GOOD>(
        0,
        request,
        Answer::Recorded {
            outcome: Outcome::Committed,
            evidence: false,
        },
        Delivery::CommitExit,
    );
    assert_eq!(world.violation, Some(Invariant::I7));
}

// Spec §2c v8 N3: a request that arrives with the pinned view keeps it while waiting.
#[test]
fn read_uses_arrival_rights_after_lock_wait() {
    let mut world = Sessions::<2, GOOD>::start();
    let mut replies = Vec::new();
    for action in [
        Move::Lookup(1, Op::Read),
        Move::RightsChange,
        Move::SessionLock(1),
    ] {
        world.do_move::<GOOD>(action, |reply| replies.push(reply));
    }
    assert_eq!(replies.last(), Some(&(Some(Cell::P5), Answer::Pending)));
}

// Spec §2c v8 F3: a committed record without the swap is a logged 500.
#[test]
fn committed_record_without_swap_is_an_internal_error() {
    for (state, cell) in [(State::S, Cell::S17), (State::Pw, Cell::Pw13)] {
        let result = step(&state, &Event::Settled(Outcome::Committed));
        assert_eq!(result.answer, Answer::Error(Problem::Status(500)));
        assert_eq!(result.effect, Effect::LogBadTransition);
        assert_eq!(result.next, state);
        assert_eq!(result.cell, cell);
    }
}

// Spec §2b E4 and P13: a request after longest life learns the recorded reason.
#[test]
fn ended_answer_carries_the_recorded_problem() {
    let outcome = Outcome::Failed(Problem::LongestLife);
    let result = step(&State::E(outcome), &Event::Action(Action::Ping));
    assert_eq!(result.answer, Answer::Ended(outcome));
}

// Spec §2b: the task touches its session on release, so expiry uses that touch.
#[test]
fn sweep_uses_idle_deadline_after_guard_release() {
    let mut live = Sessions::<2, GOOD>::start();
    for action in [
        Move::Lookup(1, Op::Write),
        Move::SessionLock(1),
        Move::IdleTick,
        Move::IdleTick,
        Move::Swap(1, 0),
        Move::DropGuard(1, 0, End::Committed),
    ] {
        live.do_move::<GOOD>(action, |_| {});
    }
    let mut expired = live.clone();
    live.do_move::<GOOD>(Move::Sweep(1), |_| {});
    assert!(live.store.transactions[1].mapped);
    expired.do_move::<GOOD>(Move::IdleTick, |_| {});
    expired.do_move::<GOOD>(Move::Sweep(1), |_| {});
    assert!(expired.store.transactions[1].mapped);
    expired.do_move::<GOOD>(Move::IdleTick, |_| {});
    expired.do_move::<GOOD>(Move::Sweep(1), |_| {});
    assert!(!expired.store.transactions[1].mapped);
}

// Spec §2b v9 P13/E3: a ROLLBACK after longest life succeeds with or without a sweep.
#[test]
fn rollback_after_longest_life_uses_the_tombstone() {
    let mut world = Sessions::<2, GOOD>::start();
    world.do_move::<GOOD>(Move::LifeClock, |_| {});
    for sweep_first in [false, true] {
        let mut session = world.clone();
        if sweep_first {
            session.do_move::<GOOD>(Move::Sweep(1), |_| {});
        }
        session.do_move::<GOOD>(Move::Lookup(1, Op::Rollback), |_| {});
        let mut answers = Vec::new();
        session.do_move::<GOOD>(Move::SessionLock(1), |(_, answer)| {
            if answer != Answer::None {
                answers.push(answer);
            }
        });
        for action in [Move::Lookup(1, Op::Commit), Move::SessionLock(1)] {
            session.do_move::<GOOD>(action, |(_, answer)| {
                if answer != Answer::None {
                    answers.push(answer);
                }
            });
        }
        assert_eq!(
            answers,
            [
                Answer::NoContent,
                Answer::Recorded {
                    outcome: Outcome::Failed(Problem::LongestLife),
                    evidence: true,
                },
            ]
        );
    }
}

// Spec §2b Pw9: ROLLBACK can cancel only the ticket present at its lookup.
#[test]
fn rollback_cancels_only_the_ticket_visible_at_lookup() {
    for sees_ticket in [false, true] {
        let mut world = Sessions::<2, GOOD>::start();
        world.do_move::<GOOD>(Move::Lookup(1, Op::Write), |_| {});
        if !sees_ticket {
            world.do_move::<GOOD>(Move::Lookup(1, Op::Rollback), |_| {});
        }
        world.do_move::<GOOD>(Move::SessionLock(1), |_| {});
        let writer_answer = if sees_ticket {
            world.do_move::<GOOD>(Move::Lookup(1, Op::Rollback), |_| {});
            Answer::TaskError {
                problem: Problem::Status(408),
                source: Cell::P4,
            }
        } else {
            Answer::WriteOk
        };
        let mut answers = Vec::new();
        for _ in 0..Sessions::<2, GOOD>::DRAIN_BOUND {
            if Sessions::<2, GOOD>::quiet(&world) {
                break;
            }
            let action = Sessions::<2, GOOD>::drain_move(&world).expect("a server exit");
            world.do_move::<GOOD>(action, |(_, answer)| {
                if answer != Answer::None {
                    answers.push(answer);
                }
            });
        }
        assert_eq!(answers, [writer_answer, Answer::NoContent]);
        assert!(Sessions::<2, GOOD>::quiet(&world));
    }
}

fn deep_limit(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |value| value.parse().expect(name))
}

fn green<const N: usize>() {
    let deep = std::env::var_os("NRESE_SESSION_EXPLORER_DEEP").is_some();
    let required_depth = if N == 2 { 12 } else { 10 };
    let deep_worlds = if N == 2 {
        DEEP_WORLDS_TWO
    } else {
        DEEP_WORLDS_THREE
    };
    let (depth, worlds) = if deep {
        (
            deep_limit("NRESE_SESSION_EXPLORER_DEEP_DEPTH", DEEP_DEPTH),
            deep_limit("NRESE_SESSION_EXPLORER_DEEP_WORLDS", deep_worlds),
        )
    } else {
        (ORDINARY_DEPTH, ORDINARY_WORLDS)
    };
    eprintln!(
        "Stimulus budget: one of each server change and one sweep per path, two idle ticks per search path, two write attempts under the lock per live session, one waiting request per session plus one racing ROLLBACK. Depth N means every schedule of N moves within this budget, not every schedule of N events."
    );
    eprintln!(
        "Server drain may go beyond search depth, up to {} moves.",
        Sessions::<N, GOOD>::DRAIN_BOUND,
    );
    let start = Instant::now();
    let outcome = explore::<Sessions<N, GOOD>>(depth, worlds);
    let elapsed = start.elapsed();
    match outcome {
        Ok(stats) => {
            print_stats(N, depth, stats, elapsed);
            if stats.budget_hit {
                eprintln!(
                    "World budget exhausted; depth {} did not complete.",
                    stats.completed_depth + 1
                );
                assert!(deep, "ordinary search hit its world budget");
            }
            if deep {
                assert!(
                    stats.completed_depth >= required_depth,
                    "{N}-client deep run requires depth {required_depth}"
                );
            }
        }
        Err(trace) => {
            print_stats(N, depth, trace.stats, elapsed);
            print_trace(&trace);
            panic!("{N}-client lifecycle check failed");
        }
    }
}

#[test]
fn session_lifecycle_two_clients() {
    green::<2>();
}

#[test]
fn session_lifecycle_three_clients() {
    green::<3>();
}
