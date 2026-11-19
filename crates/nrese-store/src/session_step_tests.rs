//! The scheduler models only facts that cross a lock or affect a later answer.
use super::*;
use nrese_exec::lifecycle_explorer::{Model, Stats, Trace, explore};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

const ORDINARY_DEPTH: usize = 5;
const ORDINARY_WORLDS: usize = 100_000;
const WITNESS_DEPTH: usize = 8;
const DEEP_DEPTH: usize = 16;
const DEEP_WORLDS: usize = 50_000_000;
const GOOD: u8 = 0;
const UNKNOWN_ROLLBACK: u8 = 1;
const JOIN_ANSWER: u8 = 2;
const EARLY_RELEASE: u8 = 3;
const STALE_LOOKUP: u8 = 4;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Op {
    Write,
    Read,
    Commit,
    Rollback,
    Ping,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Request {
    op: Op,
    rights: bool,
}

type ReportedAnswer = (Option<Cell>, Answer);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Phase {
    Pending,
    Claimed,
    Ended(Outcome),
    Recorded(Outcome),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Task {
    switch: bool,
    bit: u8,
    scratch: u8,
    pin: Option<Pin>,
    guard: bool,
    awaiting: bool,
    phase: Phase,
}

impl Task {
    fn observe(self) -> Observation {
        match self.phase {
            Phase::Recorded(outcome) => Observation::Ended(outcome),
            Phase::Ended(Outcome::Committed) => Observation::Published,
            Phase::Ended(outcome @ Outcome::Failed(Problem::NoSession | Problem::Status(408))) => {
                Observation::Ended(outcome)
            }
            _ => Observation::InProgress,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
struct Store {
    tags: [u8; 3],
    revision: u8,
    epoch: u8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Pin {
    revision: u8,
    epoch: u8,
    protected: bool,
    rights: bool,
}

impl Pin {
    fn capture(store: Store, rights: bool) -> Self {
        Self {
            revision: store.revision,
            epoch: store.epoch,
            protected: store.tags.iter().any(|tags| tags & 2 != 0),
            rights,
        }
    }

    fn is_current(self, store: Store) -> bool {
        (self.revision, self.epoch) == (store.revision, store.epoch)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Read {
    first: bool,
    pin: Pin,
    private: u8,
    request_rights: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Commit {
    pinned: bool,
    rights: bool,
    awaiting: bool,
    published: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Repeat {
    rights: bool,
    record: Observation,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Session {
    state: State,
    mapped: bool,
    waiting: Option<Request>,
    rollback_waiting: Option<Request>,
    repeat: Option<Repeat>,
    read: Option<Read>,
    tasks: [Option<Task>; 2],
    commit: Option<Commit>,
    terminal: Option<Outcome>,
    pin: Option<Pin>,
    evidence_rights: bool,
    pending: u8,
    expected: u8,
    acknowledged: u8,
    issued: u8,
    past_idle_limit: bool,
    expired: bool,
    stale_success: bool,
}

impl Session {
    fn new(state: State) -> Self {
        Self {
            state,
            mapped: state == State::U,
            waiting: None,
            rollback_waiting: None,
            repeat: None,
            read: None,
            tasks: [None; 2],
            commit: None,
            terminal: None,
            pin: None,
            evidence_rights: false,
            pending: 0,
            expected: 0,
            acknowledged: 0,
            issued: 0,
            past_idle_limit: false,
            expired: false,
            stale_success: false,
        }
    }

    fn held(&self) -> bool {
        self.tasks.iter().flatten().any(|task| task.guard)
    }

    fn next_write(&mut self) -> u8 {
        let bit = 1 << self.issued;
        self.issued += 1;
        bit
    }

    fn action(&self, op: Op, rights: bool) -> Action {
        let same_rights = match self.state {
            State::E(_) => self.evidence_rights == rights,
            _ => self.pin.is_none_or(|pin| pin.rights == rights),
        };
        match op {
            Op::Write => Action::Write { same_rights },
            Op::Read => Action::Read { same_rights },
            Op::Commit => Action::Commit {
                same_rights,
                empty: self.pending == 0,
            },
            Op::Rollback => Action::Rollback,
            Op::Ping => Action::Ping,
        }
    }

    fn can_settle(&self) -> bool {
        self.terminal.is_some() && matches!(self.state, State::S | State::Pw | State::C)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Violation {
    OutsideLock,
    DetachedChange,
    DetachedAnswer,
    ReleasedEarly,
    BusyAnswer,
    RecordAnswer,
    Unsettled,
    InvalidTransition,
    InvalidPlacement,
    Disclosure,
    LostWrite,
    DoublePublish,
    RollbackUnknown,
    PublishedFailure,
    StaleSuccess,
}

#[derive(Clone, Debug)]
struct World<const N: usize> {
    sessions: [Session; N],
    store: Store,
    serial: Store,
    external_commit: bool,
    rights: bool,
    rights_changed: bool,
    retired: bool,
    swept: bool,
    restarted: bool,
    violation: Option<Violation>,
}

impl<const N: usize> World<N> {
    fn identity(&self) -> impl Eq + Hash {
        // A tag belongs to its client. Sort those together, keeping the IDs in
        // the actual world so a trace still names the same client at every step.
        let mut clients = std::array::from_fn::<_, N, _>(|id| {
            (
                &self.sessions[id],
                self.store.tags[id],
                self.serial.tags[id],
            )
        });
        clients.sort_unstable();
        (
            clients,
            (self.store.revision, self.store.epoch),
            (self.serial.revision, self.serial.epoch),
            self.external_commit,
            self.rights,
            self.rights_changed,
            self.retired,
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
    CancelForRollback(usize, usize),
    DropWaiting(usize),
    DropRollback(usize),
    DropTaskRequest(usize, usize),
    DropCommitRequest(usize),
    DropRepeat(usize),
    DropRead(usize),
    Claim(usize, usize),
    Swap(usize, usize),
    Fail(usize, usize),
    DropGuard(usize, usize, Outcome),
    TaskReady(usize, usize),
    TaskDeadline(usize, usize),
    PublishCommit(usize),
    CommitExit(usize, Exit),
    CommitGrace(usize),
    RepeatReady(usize),
    Stream(usize, StreamEnd),
    // One try_lock visit by the periodic sweep, whose interval derives from the idle limit.
    Sweep(usize),
    // Jump past the idle deadline, independent of the sweep cadence.
    IdleLimitPassed,
    LifeClock,
    OtherCommit,
    RulesChange,
    RightsChange,
    NoOpChange,
    ReplaceRepository,
    Restart,
}

struct Sessions<const N: usize, const MUTATION: u8>;

impl<const N: usize> World<N> {
    fn normal_commit_move(&self, id: usize, commit: Commit) -> Move {
        if commit.published {
            Move::CommitExit(id, Exit::Published { panicked: false })
        } else if commit.pinned
            && self.sessions[id]
                .pin
                .is_some_and(|pin| !pin.is_current(self.store))
        {
            Move::CommitExit(id, Exit::Conflict)
        } else if commit.rights != self.rights {
            Move::CommitExit(id, Exit::RightsChanged)
        } else {
            Move::PublishCommit(id)
        }
    }

    fn violate<const MUTATION: u8>(&mut self, violation: Violation) {
        // Mutation runs keep searching until the externally visible regression.
        let target = match MUTATION {
            UNKNOWN_ROLLBACK => Some(Violation::RollbackUnknown),
            JOIN_ANSWER => Some(Violation::PublishedFailure),
            EARLY_RELEASE => Some(Violation::LostWrite),
            STALE_LOOKUP => Some(Violation::StaleSuccess),
            _ => None,
        };
        if target.is_none_or(|expected| expected == violation) {
            self.violation.get_or_insert(violation);
        }
    }
}

fn transition<const MUTATION: u8>(state: State, event: Event) -> Step {
    let mut result = step(&state, &event);
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
        }
        (STALE_LOOKUP, State::Gone, Event::Action(Action::Write { .. })) => {
            result.answer = Answer::WriteOk;
        }
        _ => {}
    }
    result
}

impl<const N: usize> World<N> {
    fn event<const MUTATION: u8>(
        &mut self,
        id: usize,
        event: Event,
        locked: bool,
        report: &mut impl FnMut(ReportedAnswer),
    ) -> Step {
        let before = self.sessions[id];
        let result = transition::<MUTATION>(before.state, event);
        if result.next != before.state || matches!(result.effect, Effect::Append | Effect::Swap) {
            if !locked {
                self.violate::<MUTATION>(Violation::OutsideLock);
            }
            if !before.mapped || self.retired || before.state == State::Gone {
                self.violate::<MUTATION>(Violation::DetachedChange);
            }
        }
        if result.effect == Effect::LogBadTransition {
            self.violate::<MUTATION>(Violation::InvalidTransition);
        }
        let detached_action = match event {
            Event::Action(action) if before.state == State::Gone => Some(action),
            Event::Detached(action) => Some(action),
            _ => None,
        };
        if let Some(action) = detached_action {
            let expected = if matches!(action, Action::Commit { .. }) {
                Problem::ReadBeforeRetry
            } else {
                Problem::NoSession
            };
            if result.answer != Answer::Error(expected) {
                self.violate::<MUTATION>(Violation::DetachedAnswer);
                if matches!(action, Action::Write { .. }) && result.answer == Answer::WriteOk {
                    self.sessions[id].stale_success = true;
                }
            }
        }
        if result.answer == Answer::NoContent && before.state == State::E(Outcome::Unknown) {
            self.violate::<MUTATION>(Violation::RollbackUnknown);
        }
        self.sessions[id].state = result.next;
        report((Some(result.cell), result.answer));
        result
    }

    fn settle<const MUTATION: u8>(&mut self, id: usize, report: &mut impl FnMut(ReportedAnswer)) {
        let session = self.sessions[id];
        if let Some(outcome) = session.terminal.filter(|_| session.can_settle()) {
            let result = self.event::<MUTATION>(id, Event::Settled(outcome), true, report);
            let session = &mut self.sessions[id];
            session.terminal = None;
            if result.effect == Effect::StampEnd {
                session.pin = None;
                session.past_idle_limit = false;
                session.expired = false;
            }
        }
        if matches!(self.sessions[id].state, State::S | State::Pw | State::C)
            && self.sessions[id].commit.is_none()
            && !self.sessions[id].held()
        {
            self.violate::<MUTATION>(Violation::Unsettled);
        }
    }

    fn section<const MUTATION: u8>(
        &mut self,
        id: usize,
        request: Option<Request>,
        report: &mut impl FnMut(ReportedAnswer),
    ) {
        let rights = request.map_or(self.rights, |request| request.rights);
        let action = request.map(|request| self.sessions[id].action(request.op, rights));
        if self.retired {
            if let Some(action) = action {
                self.event::<MUTATION>(id, Event::Detached(action), true, report);
            }
            return;
        }
        self.settle::<MUTATION>(id, report);
        let before = self.sessions[id];
        let event = if before.expired && before.state == State::P {
            Event::LongestLife
        } else if before.past_idle_limit
            && matches!(before.state, State::U | State::P | State::E(_))
        {
            let result = self.event::<MUTATION>(id, Event::IdleLimit, true, report);
            if result.effect == Effect::Remove {
                self.sessions[id].mapped = false;
                self.sessions[id].pin = None;
            }
            if let Some(action) = action {
                self.event::<MUTATION>(id, Event::Action(action), true, report);
            }
            return;
        } else if let Some(request) = request {
            Event::Action(before.action(request.op, rights))
        } else {
            return;
        };
        let result = self.event::<MUTATION>(id, event, true, report);
        if let Answer::Recorded { evidence: true, .. } = result.answer
            && before.evidence_rights
            && !rights
        {
            self.violate::<MUTATION>(Violation::Disclosure);
        }
        let session = &mut self.sessions[id];
        match result.effect {
            Effect::Append => {
                let bit = session.next_write();
                session.pending |= bit;
                session.expected |= bit;
                session.acknowledged |= bit;
                session.past_idle_limit = false;
            }
            Effect::Touch => session.past_idle_limit = false,
            Effect::StartSwitch | Effect::StartWrite => {
                let switch = result.effect == Effect::StartSwitch;
                if switch {
                    session.pin = Some(Pin::capture(self.store, rights));
                }
                let bit = if switch { 0 } else { session.next_write() };
                let slot = session
                    .tasks
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .unwrap();
                *slot = Some(Task {
                    switch,
                    bit,
                    scratch: session.pending | bit,
                    pin: if switch { session.pin } else { None },
                    guard: true,
                    awaiting: true,
                    phase: Phase::Pending,
                });
                session.past_idle_limit = false;
            }
            Effect::TakeSnapshot => {
                session.read = Some(Read {
                    first: false,
                    pin: session.pin.unwrap(),
                    private: session.pending,
                    request_rights: rights,
                });
                session.past_idle_limit = false;
            }
            Effect::ReplayIntents | Effect::CommitDelta => {
                session.commit = Some(Commit {
                    pinned: result.effect == Effect::CommitDelta,
                    rights,
                    awaiting: true,
                    published: false,
                });
                session.evidence_rights = rights;
            }
            Effect::WaitOutcome => {
                let record = if session.commit.is_some_and(|commit| commit.published) {
                    Observation::Published
                } else {
                    Observation::InProgress
                };
                session.repeat = Some(Repeat { rights, record });
            }
            Effect::Remove => {
                session.mapped = false;
                session.pin = None;
            }
            Effect::StampEnd => {
                session.pin = None;
                session.expired = false;
                session.past_idle_limit = false;
                session.evidence_rights = rights;
            }
            _ => {}
        }
    }

    fn answer_task<const MUTATION: u8>(
        &mut self,
        id: usize,
        slot: usize,
        report: &mut impl FnMut(ReportedAnswer),
    ) {
        let task = self.sessions[id].tasks[slot].unwrap();
        let record = task.observe();
        let result = self.event::<MUTATION>(
            id,
            Event::TaskReady {
                switch: task.switch,
                record,
            },
            false,
            report,
        );
        let (expected, effect) = match record {
            Observation::Published | Observation::Ended(Outcome::Committed) if task.switch => {
                (Answer::Pending, Effect::TakeSnapshot)
            }
            Observation::Published | Observation::Ended(Outcome::Committed) => {
                (Answer::WriteOk, Effect::None)
            }
            Observation::Ended(Outcome::Failed(Problem::NoSession)) => {
                (Answer::Error(Problem::NoSession), Effect::None)
            }
            Observation::Ended(Outcome::Refused(problem) | Outcome::Failed(problem)) => (
                Answer::TaskError {
                    problem,
                    source: if task.switch { Cell::S2 } else { Cell::P4 },
                },
                Effect::None,
            ),
            Observation::Ended(Outcome::Unknown) => (
                Answer::Error(Problem::Status(500)),
                Effect::LogBadTransition,
            ),
            Observation::InProgress => (Answer::Error(Problem::WorkInProgress), Effect::None),
        };
        let recorded_orphan = task.phase == Phase::Ended(Outcome::Failed(Problem::NoSession));
        if (result.answer, result.effect) != (expected, effect)
            || (result.effect == Effect::TakeSnapshot && task.guard)
            || (recorded_orphan && result.answer != Answer::Error(Problem::NoSession))
        {
            self.violate::<MUTATION>(Violation::RecordAnswer);
        }
        if result.answer == Answer::WriteOk && self.sessions[id].pending & task.bit == 0 {
            self.violate::<MUTATION>(Violation::LostWrite);
        }
        let session = &mut self.sessions[id];
        if result.effect == Effect::TakeSnapshot {
            let pin = task.pin.unwrap();
            session.read = Some(Read {
                first: true,
                pin,
                private: task.scratch,
                request_rights: pin.rights,
            });
        }
        if result.answer == Answer::WriteOk {
            session.acknowledged |= task.bit;
        }
        session.tasks[slot] = if matches!(task.phase, Phase::Recorded(_)) {
            None
        } else {
            Some(Task {
                awaiting: false,
                ..task
            })
        };
    }

    fn do_move<const MUTATION: u8>(
        &mut self,
        action: Move,
        mut report: impl FnMut(ReportedAnswer),
    ) {
        match action {
            Move::Lookup(id, op) => {
                let request = Request {
                    op,
                    rights: self.rights,
                };
                let session = self.sessions[id];
                if session.mapped && !self.retired {
                    if session.waiting.is_some() {
                        debug_assert_eq!(op, Op::Rollback);
                        self.sessions[id].rollback_waiting = Some(request);
                    } else {
                        self.sessions[id].waiting = Some(request);
                    }
                } else {
                    let result = transition::<MUTATION>(
                        State::G,
                        Event::Action(session.action(op, request.rights)),
                    );
                    report((Some(result.cell), result.answer));
                    if op == Op::Commit
                        && session.stale_success
                        && result.answer == Answer::Error(Problem::ReadBeforeRetry)
                    {
                        self.violate::<MUTATION>(Violation::StaleSuccess);
                    }
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
                if matches!(action, Move::RollbackTimeout(_)) {
                    self.sessions[id].rollback_waiting = None;
                } else {
                    self.sessions[id].waiting = None;
                }
                let answer = lock_timeout();
                report((None, answer));
                if answer != Answer::Error(Problem::LockBusy) {
                    self.violate::<MUTATION>(Violation::BusyAnswer);
                }
            }
            Move::CancelForRollback(id, slot) | Move::DropTaskRequest(id, slot) => {
                let dropped = matches!(action, Move::DropTaskRequest(..));
                if dropped
                    && matches!(
                        self.sessions[id].tasks[slot].unwrap().phase,
                        Phase::Recorded(_)
                    )
                {
                    self.sessions[id].tasks[slot] = None;
                    return;
                }
                let event = if dropped {
                    Event::Dropped
                } else {
                    Event::Action(Action::Rollback)
                };
                let result = self.event::<MUTATION>(id, event, false, &mut report);
                let task = self.sessions[id].tasks[slot].as_mut().unwrap();
                if dropped {
                    task.awaiting = false;
                }
                if matches!(
                    result.effect,
                    Effect::TryCancel | Effect::CancelThenWaitLock
                ) && task.phase == Phase::Pending
                {
                    task.phase = Phase::Ended(Outcome::Failed(Problem::Status(408)));
                }
                if result.next == State::P
                    && task.guard
                    && !task.switch
                    && !matches!(task.phase, Phase::Ended(_))
                {
                    task.guard = false;
                    self.violate::<MUTATION>(Violation::ReleasedEarly);
                }
            }
            Move::DropWaiting(id) => self.sessions[id].waiting = None,
            Move::DropRollback(id) => self.sessions[id].rollback_waiting = None,
            Move::DropRead(id) => self.sessions[id].read = None,
            Move::DropRepeat(id) => self.sessions[id].repeat = None,
            Move::DropCommitRequest(id) => {
                self.event::<MUTATION>(id, Event::Dropped, false, &mut report);
                self.sessions[id].commit.as_mut().unwrap().awaiting = false;
            }
            Move::Claim(id, slot) => {
                self.sessions[id].tasks[slot].as_mut().unwrap().phase = Phase::Claimed;
            }
            Move::Swap(id, slot) | Move::Fail(id, slot) => {
                let session = self.sessions[id];
                let task = session.tasks[slot].unwrap();
                let (event, outcome) = if self.retired {
                    (Event::OrphanEnded, Outcome::Failed(Problem::NoSession))
                } else if matches!(action, Move::Fail(..)) {
                    (
                        Event::EvaluationFailed(Problem::Status(403)),
                        Outcome::Failed(Problem::Status(403)),
                    )
                } else if session.expired {
                    (Event::LongestLife, Outcome::Failed(Problem::LongestLife))
                } else {
                    (Event::Swapped, Outcome::Committed)
                };
                let result = if task.guard {
                    self.event::<MUTATION>(id, event, true, &mut report)
                } else {
                    // A stale task in the old implementation still has its scratch transaction.
                    transition::<MUTATION>(State::Pw, event)
                };
                if result.answer != Answer::None
                    || (self.retired && result.effect != Effect::Record(outcome))
                {
                    self.violate::<MUTATION>(Violation::RecordAnswer);
                }
                let session = &mut self.sessions[id];
                // The lifecycle state and publish mark change in this one move.
                session.tasks[slot].as_mut().unwrap().phase = Phase::Ended(outcome);
                if task.guard && result.effect == Effect::StampEnd {
                    session.pin = None;
                    session.past_idle_limit = false;
                    session.expired = false;
                }
                if matches!(result.effect, Effect::Swap | Effect::SwapAndRead) {
                    session.pending = task.scratch;
                    session.expected |= task.bit;
                    if session.acknowledged & !session.pending != 0 {
                        self.violate::<MUTATION>(Violation::LostWrite);
                    }
                }
            }
            Move::DropGuard(id, slot, outcome) => {
                let session = &mut self.sessions[id];
                let task = session.tasks[slot].unwrap();
                session.terminal = Some(outcome);
                if task.guard && matches!(session.state, State::U | State::S | State::P | State::Pw)
                {
                    session.past_idle_limit = false;
                }
                session.tasks[slot] = task.awaiting.then_some(Task {
                    guard: false,
                    phase: Phase::Recorded(outcome),
                    ..task
                });
                if !self.retired && !self.sessions[id].can_settle() {
                    self.sessions[id].terminal = None;
                }
            }
            Move::TaskReady(id, slot) => self.answer_task::<MUTATION>(id, slot, &mut report),
            Move::TaskDeadline(id, slot) => {
                self.sessions[id].tasks[slot].as_mut().unwrap().phase =
                    Phase::Ended(Outcome::Failed(Problem::Status(408)));
                self.answer_task::<MUTATION>(id, slot, &mut report);
            }
            Move::PublishCommit(id) => {
                let session = self.sessions[id];
                let commit = session.commit.unwrap();
                if commit.pinned && session.pin.is_none_or(|pin| !pin.is_current(self.store)) {
                    self.violate::<MUTATION>(Violation::InvalidPlacement);
                }
                if commit.published || self.store.tags[id] != 0 {
                    self.violate::<MUTATION>(Violation::DoublePublish);
                }
                self.store.tags[id] = session.pending;
                self.serial.tags[id] = session.expected;
                self.store.revision += u8::from(session.pending != 0);
                self.serial.revision += u8::from(session.expected != 0);
                let session = &mut self.sessions[id];
                session.commit.as_mut().unwrap().published = true;
                if let Some(repeat) = &mut session.repeat {
                    repeat.record = Observation::Published;
                }
            }
            Move::CommitExit(id, exit) => {
                let commit = self.sessions[id].commit.unwrap();
                let result =
                    self.event::<MUTATION>(id, Event::WorkerExit(exit), false, &mut report);
                if let Effect::Record(outcome) = result.effect {
                    if (outcome == Outcome::Committed) != commit.published
                        || (commit.published
                            && commit.awaiting
                            && result.answer
                                != (Answer::Recorded {
                                    outcome,
                                    evidence: true,
                                }))
                    {
                        self.violate::<MUTATION>(Violation::PublishedFailure);
                    }
                    let session = &mut self.sessions[id];
                    session.terminal = Some(outcome);
                    if let Some(repeat) = &mut session.repeat {
                        repeat.record = Observation::Ended(outcome);
                    }
                }
                self.sessions[id].commit = None;
            }
            Move::CommitGrace(id) => {
                let commit = self.sessions[id].commit.unwrap();
                let record = if commit.published {
                    Observation::Published
                } else {
                    Observation::InProgress
                };
                let result =
                    self.event::<MUTATION>(id, Event::CommitGrace(record), false, &mut report);
                if commit.published
                    && result.answer
                        != (Answer::Recorded {
                            outcome: Outcome::Committed,
                            evidence: true,
                        })
                {
                    self.violate::<MUTATION>(Violation::PublishedFailure);
                }
                self.sessions[id].commit.as_mut().unwrap().awaiting = false;
            }
            Move::RepeatReady(id) => {
                let repeat = self.sessions[id].repeat.take().unwrap();
                let same_rights = repeat.rights == self.sessions[id].evidence_rights;
                let result = self.event::<MUTATION>(
                    id,
                    Event::RepeatCommitReady {
                        record: repeat.record,
                        same_rights,
                    },
                    false,
                    &mut report,
                );
                let expected = match repeat.record {
                    Observation::Published => Some(Outcome::Committed),
                    Observation::Ended(outcome) => Some(outcome),
                    Observation::InProgress => None,
                };
                if let Some(outcome) = expected
                    && result.answer
                        != (Answer::Recorded {
                            outcome,
                            evidence: same_rights,
                        })
                {
                    self.violate::<MUTATION>(Violation::RecordAnswer);
                }
            }
            Move::Stream(id, end) => {
                let read = self.sessions[id].read.take().unwrap();
                let result = self.event::<MUTATION>(
                    id,
                    Event::ReadEnded {
                        first: read.first,
                        end,
                    },
                    false,
                    &mut report,
                );
                if matches!(
                    result.answer,
                    Answer::Read(StreamEnd::Data | StreamEnd::Aborted)
                ) {
                    // The top bit represents a protected statement in this abstract store.
                    let may_disclose_protected =
                        (read.pin.protected || read.private & 2 != 0) && read.pin.rights;
                    if may_disclose_protected && !read.request_rights {
                        self.violate::<MUTATION>(Violation::Disclosure);
                    }
                }
            }
            Move::Sweep(id) => {
                self.swept = true;
                self.section::<MUTATION>(id, None, &mut report);
            }
            Move::IdleLimitPassed => {
                for session in &mut self.sessions {
                    if session.mapped {
                        session.past_idle_limit = true;
                    }
                }
            }
            Move::LifeClock => {
                for session in &mut self.sessions {
                    if matches!(session.state, State::P | State::Pw) {
                        session.expired = true;
                    }
                }
            }
            Move::OtherCommit => {
                self.external_commit = true;
                self.store.revision += 1;
                self.serial.revision += 1;
            }
            Move::RulesChange => {
                self.store.epoch += 1;
                self.serial.epoch += 1;
            }
            Move::RightsChange => {
                self.rights_changed = true;
                self.rights = !self.rights;
            }
            Move::NoOpChange => {}
            Move::ReplaceRepository => {
                for id in 0..N {
                    self.event::<MUTATION>(id, Event::RepositoryReplaced, false, &mut report);
                }
                self.retired = true;
            }
            Move::Restart => {
                self.restarted = true;
                // Process exit destroys the map and its handles as a unit.
                self.sessions.fill(Session::new(State::G));
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
                self.event::<MUTATION>(id, Event::Changed(change), false, &mut report);
            }
        }
    }
}

impl<const N: usize, const MUTATION: u8> Model for Sessions<N, MUTATION> {
    type World = World<N>;
    type Move = Move;
    const DRAIN_BOUND: usize = 16 * N;

    fn start() -> World<N> {
        let begin = step(&State::G, &Event::Begin { valid_level: true });
        let mut world = World {
            sessions: [Session::new(begin.next); N],
            store: Store::default(),
            serial: Store::default(),
            external_commit: false,
            rights: true,
            rights_changed: false,
            retired: false,
            swept: false,
            restarted: false,
            violation: None,
        };
        // Begin and one completed first read are fixture setup. This leaves an
        // observed client for the two-write witness and U clients for the switch.
        let pinned = N - 1;
        for action in [
            Move::Lookup(pinned, Op::Read),
            Move::SessionLock(pinned),
            Move::Swap(pinned, 0),
            Move::DropGuard(pinned, 0, Outcome::Committed),
            Move::TaskReady(pinned, 0),
            Move::Stream(pinned, StreamEnd::Data),
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
        if MUTATION == GOOD {
            moves.push(Move::Restart);
        }
        if MUTATION == GOOD && !world.retired {
            moves.extend([Move::ReplaceRepository, Move::NoOpChange]);
            if !world.external_commit {
                moves.push(Move::OtherCommit);
            }
            if world.store.epoch == 0 {
                moves.push(Move::RulesChange);
            }
            if !world.rights_changed {
                moves.push(Move::RightsChange);
            }
            if world
                .sessions
                .iter()
                .any(|s| s.mapped && !s.past_idle_limit)
            {
                moves.push(Move::IdleLimitPassed);
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
            // Red runs cover the affected client's request interleavings.
            // Other clients and unrelated stimuli run in the green search.
            if MUTATION != GOOD && id != if MUTATION == EARLY_RELEASE { N - 1 } else { 0 } {
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
                if s.mapped
                    && !world.retired
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
                && s.mapped
                && !world.retired
                && !s.held()
                && (s.past_idle_limit || s.expired || s.can_settle())
            {
                moves.push(Move::Sweep(id));
            }
            for (slot, task) in s.tasks.iter().enumerate() {
                let Some(task) = task else { continue };
                if task.awaiting {
                    moves.push(Move::DropTaskRequest(id, slot));
                }
                let panic = Outcome::Failed(Problem::Unpublished);
                match task.phase {
                    Phase::Pending => {
                        moves.extend([
                            Move::Claim(id, slot),
                            Move::Swap(id, slot),
                            Move::Fail(id, slot),
                            Move::DropGuard(id, slot, panic),
                            Move::DropGuard(id, slot, Outcome::Failed(Problem::NotRun)),
                        ]);
                        if s.waiting.is_some_and(|request| request.op == Op::Rollback)
                            || s.rollback_waiting.is_some()
                        {
                            moves.push(Move::CancelForRollback(id, slot));
                        }
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
                        if task.awaiting && (!task.switch || outcome != Outcome::Committed) {
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
                    if s.pending != 0 {
                        moves.push(Move::CommitExit(id, Exit::Published { panicked: true }));
                    }
                } else {
                    moves.extend([
                        Move::CommitExit(id, Exit::Failed(Problem::Status(403))),
                        Move::CommitExit(id, Exit::Unavailable),
                        Move::CommitExit(id, Exit::NeverRan),
                        Move::CommitExit(id, Exit::Claimed),
                    ]);
                    if matches!(normal, Move::PublishCommit(_)) && s.pending != 0 {
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
                    Move::Stream(id, StreamEnd::Data),
                    Move::Stream(id, StreamEnd::Error(Problem::Status(408))),
                    Move::Stream(id, StreamEnd::Aborted),
                    Move::DropRead(id),
                ]);
            }
        }
        moves
    }

    fn apply(world: &World<N>, action: &Move) -> World<N> {
        let mut next = world.clone();
        next.do_move::<MUTATION>(*action, |_| {});
        next
    }

    fn check(world: &World<N>) -> Result<(), String> {
        if let Some(violation) = world.violation {
            return Err(format!("{violation:?}"));
        }
        if MUTATION == GOOD && world.store != world.serial {
            return Err("published store differs from the serial execution".into());
        }
        Ok(())
    }

    fn quiet(world: &World<N>) -> bool {
        world.sessions.iter().all(|s| {
            s.waiting.is_none()
                && s.rollback_waiting.is_none()
                && s.repeat.is_none()
                && s.read.is_none()
                && s.tasks.iter().all(Option::is_none)
                && s.commit.is_none()
                && (world.retired
                    || !s.mapped
                    || !matches!(s.state, State::S | State::Pw | State::C))
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
                return Some(Move::Stream(id, StreamEnd::Data));
            }
            if s.waiting.is_some() {
                return Some(Move::SessionLock(id));
            }
            if s.rollback_waiting.is_some() {
                return Some(Move::RollbackLock(id));
            }
            if !world.retired && s.can_settle() {
                return Some(Move::Sweep(id));
            }
        }
        None
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
    let outcome = explore::<Sessions<2, MUTATION>>(WITNESS_DEPTH, ORDINARY_WORLDS);
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
}

// Spec §2b E3b: ROLLBACK after an unknown publication must not claim a rollback.
#[test]
fn explorer_rejects_rollback_after_unknown() {
    red::<UNKNOWN_ROLLBACK>("RollbackUnknown");
}

// Spec §2b C1: even a panic after publication answers from the committed record.
#[test]
fn explorer_rejects_published_failure() {
    red::<JOIN_ANSWER>("PublishedFailure");
}

// Spec §2b Pw3/P16: a dropped handler cannot let an older scratch overwrite an acknowledged write.
#[test]
fn explorer_rejects_lost_second_write() {
    red::<EARLY_RELEASE>("LostWrite");
}

// Spec §2b X7: a write waiting on a removed transaction must not answer 200 then miss its COMMIT.
#[test]
fn explorer_rejects_success_after_removal() {
    red::<STALE_LOOKUP>("StaleSuccess");
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
    for state in [State::S, State::Pw] {
        let result = step(&state, &Event::Settled(Outcome::Committed));
        assert_eq!(result.answer, Answer::Error(Problem::Status(500)));
        assert_eq!(result.effect, Effect::LogBadTransition);
        assert_eq!(result.next, state);
    }
}

// Spec §2b: the task touches its session on release, so expiry uses that touch.
#[test]
fn sweep_uses_idle_deadline_after_guard_release() {
    let mut live = Sessions::<2, GOOD>::start();
    for action in [
        Move::Lookup(1, Op::Write),
        Move::SessionLock(1),
        Move::IdleLimitPassed,
        Move::Swap(1, 0),
        Move::DropGuard(1, 0, Outcome::Committed),
    ] {
        live.do_move::<GOOD>(action, |_| {});
    }
    let mut expired = live.clone();
    live.do_move::<GOOD>(Move::Sweep(1), |_| {});
    assert!(live.sessions[1].mapped);
    expired.do_move::<GOOD>(Move::IdleLimitPassed, |_| {});
    expired.do_move::<GOOD>(Move::Sweep(1), |_| {});
    assert!(!expired.sessions[1].mapped);
}

fn deep_limit(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |value| value.parse().expect(name))
}

fn green<const N: usize>() {
    let deep = std::env::var_os("NRESE_SESSION_EXPLORER_DEEP").is_some();
    let (depth, worlds) = if deep {
        (
            deep_limit("NRESE_SESSION_EXPLORER_DEEP_DEPTH", DEEP_DEPTH),
            deep_limit("NRESE_SESSION_EXPLORER_DEEP_WORLDS", DEEP_WORLDS),
        )
    } else {
        (ORDINARY_DEPTH, ORDINARY_WORLDS)
    };
    eprintln!(
        "Stimulus budget: one of each server change and one sweep per path, two admitted writes per client, one waiting request per session plus one racing ROLLBACK. Depth N means every schedule of N moves within this budget, not every schedule of N events."
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
