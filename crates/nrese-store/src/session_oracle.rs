//! Observable client receipts and store operations for the bounded session model.

#[allow(dead_code)]
mod names {
    pub(super) enum State {}
    pub(super) enum Action {}
    pub(super) enum Event {}
    pub(super) enum Outcome {}
    pub(super) enum Effect {}
    pub(super) enum Step {}
    pub(super) enum Cell {}
}
#[allow(unused_imports)]
use names::{Action, Cell, Effect, Event, Outcome, State, Step};

pub(super) type Version = (u8, u8);
pub(super) type Id = usize;
const SLOTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum Op {
    Write,
    Read,
    Commit,
    Rollback,
    Ping,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum Failure {
    Status(u16),
    Rights,
    Conflict,
    LongestLife,
    Busy,
    NotRun,
    Unpublished,
    CommitBusy(bool),
    WriteBusy,
    EndedWhileCommitting,
    AlreadyCommitted,
    OutcomeUnknown,
    Missing(bool),
    LockBusy,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum End {
    Committed,
    Unknown,
    Refused(Failure),
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum Stream {
    Data,
    Aborted,
    Error(Failure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Seen {
    Silent,
    Pending,
    WriteOk,
    Stream(Stream),
    Ping,
    RolledBack,
    Ended(End),
    Error(Failure),
    Commit(End, bool),
    Created,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct Sent {
    pub op: Op,
    pub rights: bool,
    pub tag: u8,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Receipt {
    pub id: Id,
    pub sent: Sent,
    pub seen: Seen,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ClientRecord {
    pub transaction: Id,
    pub owed: [Option<Sent>; SLOTS],
    pub first_shown: Option<Version>,
    pub acknowledged: u8,
    pub refused: u8,
    pub receipt: Option<Receipt>,
}

impl ClientRecord {
    pub fn key(&self) -> impl Ord + Hash {
        (self.owed, self.first_shown, self.acknowledged, self.refused)
    }

    pub fn send(&mut self, sent: Sent) -> Id {
        let id = self
            .owed
            .iter()
            .position(Option::is_none)
            .expect("request slots");
        self.owed[id] = Some(sent);
        id
    }

    pub fn receive(&mut self, id: Id, seen: Seen, delivery: Delivery) {
        let sent = self.owed[id].expect("answer to an outstanding request");
        if matches!(seen, Seen::Stream(Stream::Data | Stream::Aborted))
            && let Delivery::Read { version, .. } = delivery
        {
            self.first_shown.get_or_insert(version);
        }
        if sent.op == Op::Write {
            match seen {
                Seen::WriteOk => self.acknowledged |= sent.tag,
                Seen::Error(Failure::WriteBusy) => {}
                Seen::Ended(_) | Seen::Error(_) => self.refused |= sent.tag,
                _ => {}
            }
        }
        if !matches!(seen, Seen::Silent | Seen::Pending) {
            self.owed[id] = None;
        }
        self.receipt = Some(Receipt { id, sent, seen });
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum Producer {
    Lock,
    Task { slot: usize, switch: bool },
    Read,
    Commit,
    Repeat,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Delivery {
    Request {
        missing: bool,
        running: bool,
        prior_end: Option<End>,
        rights_equal: bool,
    },
    LockTimeout,
    Task {
        slot: usize,
        answer: Seen,
    },
    Read {
        version: Version,
        tags: u8,
        end: Stream,
    },
    CommitExit,
    CommitGrace,
    Repeat,
}

impl Delivery {
    fn answers(self, producer: Option<Producer>) -> bool {
        match self {
            Self::Request { .. } => true,
            Self::LockTimeout => producer == Some(Producer::Lock),
            Self::Task { slot, .. } => {
                matches!(producer, Some(Producer::Task { slot: owner, .. }) if owner == slot)
            }
            Self::Read { .. } => producer == Some(Producer::Read),
            Self::CommitExit | Self::CommitGrace => producer == Some(Producer::Commit),
            Self::Repeat => producer == Some(Producer::Repeat),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TerminalWrite {
    pub expected: End,
    pub recorded: Option<End>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct Publication {
    pub before: Version,
    pub after: Version,
    pub tags: u8,
    pub order: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct Transaction {
    pub mapped: bool,
    pub pinned: bool,
    pub pin_rights: Option<bool>,
    pub first_capture: Option<Version>,
    pub idle: bool,
    pub pending: u8,
    pub applied: u8,
    pub publication: Option<Publication>,
    pub published_twice: bool,
    pub read_only: bool,
    pub end: Option<End>,
    pub checked_at: Option<Version>,
    pub evidence_rights: bool,
    pub producers: [Option<Producer>; SLOTS],
    pub guards: u8,
    pub settling: bool,
    pub worker: bool,
}

impl Default for Transaction {
    fn default() -> Self {
        Self {
            mapped: true,
            pinned: false,
            pin_rights: None,
            first_capture: None,
            idle: false,
            pending: 0,
            applied: 0,
            publication: None,
            published_twice: false,
            read_only: false,
            end: None,
            checked_at: None,
            evidence_rights: false,
            producers: [None; SLOTS],
            guards: 0,
            settling: false,
            worker: false,
        }
    }
}

impl Transaction {
    pub fn request_delivery(self, retired: bool, rights: bool) -> Delivery {
        Delivery::Request {
            missing: retired || !self.mapped,
            running: self.worker,
            prior_end: self.end,
            rights_equal: self.pin_rights.is_none_or(|captured| captured == rights),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct StoreRecord<const N: usize> {
    pub transactions: [Transaction; N],
    pub version: Version,
    pub retired: bool,
    pub external_publication: Option<Publication>,
    pub delivery: Option<Delivery>,
    pub terminal_write: Option<TerminalWrite>,
    pub at_swap: Option<Id>,
    pub write: Option<(bool, bool, bool)>,
}

impl<const N: usize> Default for StoreRecord<N> {
    fn default() -> Self {
        Self {
            transactions: [Transaction::default(); N],
            version: (0, 0),
            retired: false,
            external_publication: None,
            delivery: None,
            terminal_write: None,
            at_swap: None,
            write: None,
        }
    }
}

impl<const N: usize> StoreRecord<N> {
    fn transaction(&self, client: &ClientRecord) -> &Transaction {
        &self.transactions[client.transaction]
    }

    fn publication(&mut self, tags: u8, changed: bool) -> Publication {
        let before = self.version;
        self.version.0 += u8::from(changed);
        let order = self
            .transactions
            .iter()
            .filter(|txn| txn.publication.is_some())
            .count()
            + usize::from(self.external_publication.is_some());
        Publication {
            before,
            after: self.version,
            tags,
            order,
        }
    }

    pub fn publish(&mut self, id: usize, tags: u8) {
        if self.transactions[id].publication.is_some() {
            self.transactions[id].published_twice = true;
            return;
        }
        let publication = self.publication(tags, tags != 0);
        self.transactions[id].publication = Some(publication);
        self.transactions[id].end = Some(End::Committed);
    }

    pub fn other_commit(&mut self) {
        let publication = self.publication(0, true);
        self.external_publication = Some(publication);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Invariant {
    I1,
    I2,
    I3,
    I4,
    I5,
    I6,
    I7,
    I8,
    I9,
    InvalidTransition,
}

// §2 invariant 1 and §0a: publication validates the revision actually shown.
pub(super) fn i1<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    if let (Some(first), Some(receipt), Some(Delivery::Read { version, .. })) =
        (client.first_shown, client.receipt, store.delivery)
        && receipt.sent.op == Op::Read
        && matches!(receipt.seen, Seen::Stream(Stream::Data | Stream::Aborted))
        && first != version
    {
        return false;
    }
    match (client.first_shown, store.transaction(client).publication) {
        (Some(shown), Some(publication)) => shown == publication.before,
        _ => true,
    }
}

// §0a, P15 and P21: only a data/rules conflict justifies a conflict refusal.
pub(super) fn i2<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    let txn = store.transaction(client);
    !client.receipt.is_some_and(|receipt| {
        matches!(
            receipt.seen,
            Seen::Commit(End::Refused(Failure::Conflict), _)
        )
    }) || client
        .first_shown
        .or(txn.first_capture)
        .is_some_and(|shown| shown != txn.checked_at.unwrap_or(store.version))
}

// §2 invariant 3 and lifecycle rule 3: answers agree with observed store operations.
pub(super) fn i3<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    let (Some(receipt), Some(delivery)) = (client.receipt, store.delivery) else {
        return true;
    };
    let txn = store.transaction(client);
    let Receipt { sent, seen, .. } = receipt;
    match delivery {
        Delivery::LockTimeout => return seen == Seen::Error(Failure::LockBusy),
        Delivery::Read { end, .. } => return seen == Seen::Stream(end) && sent.op == Op::Read,
        Delivery::Task { answer, .. } if seen != answer => return false,
        Delivery::Request {
            missing,
            running,
            prior_end,
            rights_equal,
        } => {
            if missing {
                return seen == Seen::Error(Failure::Missing(sent.op == Op::Commit));
            }
            if running {
                return seen
                    == match sent.op {
                        Op::Commit => Seen::Pending,
                        Op::Rollback => Seen::Error(Failure::CommitBusy(false)),
                        _ => Seen::Error(Failure::EndedWhileCommitting),
                    };
            }
            if let Some(end) = prior_end {
                return match sent.op {
                    Op::Commit => matches!(seen, Seen::Commit(answer, _) if answer == end),
                    Op::Rollback => {
                        seen == match end {
                            End::Committed => Seen::Error(Failure::AlreadyCommitted),
                            End::Unknown => Seen::Error(Failure::OutcomeUnknown),
                            End::Refused(_) | End::Failed(_) => Seen::RolledBack,
                        }
                    }
                    _ => seen == Seen::Ended(end),
                };
            }
            if !rights_equal && matches!(sent.op, Op::Commit | Op::Write | Op::Read) {
                return seen == Seen::Error(Failure::Rights)
                    && txn.end == Some(End::Refused(Failure::Rights));
            }
            return match sent.op {
                Op::Write => {
                    txn.mapped
                        && (seen == Seen::Pending
                            || (seen == Seen::WriteOk && txn.pending & sent.tag != 0))
                }
                Op::Read => txn.mapped && seen == Seen::Pending,
                Op::Commit => {
                    seen == Seen::Pending
                        || (matches!(seen, Seen::Commit(End::Committed, _)) && txn.read_only)
                }
                Op::Rollback => seen == Seen::RolledBack && !txn.mapped && !txn.pinned,
                Op::Ping => txn.mapped && seen == Seen::Ping,
            };
        }
        _ => {}
    }
    match seen {
        Seen::Commit(end, _) => {
            sent.op == Op::Commit
                && match end {
                    End::Committed => txn.publication.is_some() || txn.read_only,
                    End::Unknown => txn.end == Some(End::Unknown),
                    End::Refused(_) | End::Failed(_) => {
                        matches!(txn.end, Some(End::Refused(_) | End::Failed(_)))
                    }
                }
        }
        Seen::Error(Failure::Missing(read_first)) => {
            (!txn.mapped || store.retired) && read_first == (sent.op == Op::Commit)
        }
        Seen::WriteOk => sent.op == Op::Write && (txn.pending | txn.applied) & sent.tag != 0,
        Seen::Pending | Seen::Error(_) if matches!(delivery, Delivery::Task { .. }) => true,
        Seen::Error(Failure::CommitBusy(true)) => txn.worker && txn.end.is_none(),
        _ => false,
    }
}

// §2 invariant 3, Pw3 and P16: the applied or published tags agree with write receipts.
pub(super) fn i4<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    let txn = store.transaction(client);
    let valid = |tags| client.acknowledged & !tags == 0 && client.refused & tags == 0;
    !txn.published_twice
        && (!(store.at_swap == Some(client.transaction)
            || (txn.mapped && !store.retired && txn.end.is_none() && !txn.worker))
            || valid(txn.pending))
        && txn
            .publication
            .is_none_or(|publication| valid(publication.tags))
        && (txn.publication.is_some() || txn.end != Some(End::Committed) || valid(0))
}

// Lifecycle rule 8 and §2d: every outstanding request can be answered by its own work.
pub(super) fn i5<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    let txn = store.transaction(client);
    if let (Some(receipt), Some(delivery)) = (client.receipt, store.delivery)
        && !delivery.answers(txn.producers[receipt.id])
    {
        return false;
    }
    client
        .owed
        .iter()
        .zip(txn.producers)
        .all(|(owed, producer)| match owed {
            None => true,
            Some(sent) => match producer {
                Some(Producer::Lock) => true,
                Some(Producer::Task { switch: false, .. }) => sent.op == Op::Write,
                Some(Producer::Task { switch: true, .. } | Producer::Read) => sent.op == Op::Read,
                Some(Producer::Commit | Producer::Repeat) => sent.op == Op::Commit,
                None => false,
            },
        })
}

pub(super) fn no_work_owed<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    let txn = store.transaction(client);
    client.owed.iter().all(Option::is_none) && txn.guards == 0 && !txn.worker && !txn.settling
}

// Round 7 F1 and lifecycle rule 6: idle expiry releases abandoned private state.
pub(super) fn i6<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    let txn = store.transaction(client);
    store.retired || !txn.mapped || (!txn.pinned && !txn.idle)
}

// §2 invariant 4 and N3: protected data and evidence respect the rights on arrival.
pub(super) fn i7<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    let (Some(receipt), Some(delivery)) = (client.receipt, store.delivery) else {
        return true;
    };
    match delivery {
        Delivery::Read { tags, .. } if tags & 2 != 0 => receipt.sent.rights,
        _ => match receipt.seen {
            Seen::Commit(_, evidence) => {
                evidence == (receipt.sent.rights == store.transaction(client).evidence_rights)
            }
            _ => true,
        },
    }
}

// Lifecycle rules 4, 7 and 12: mutations hold the session lock and require a live map entry.
pub(super) fn i8<const N: usize>(_client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    store
        .write
        .is_none_or(|(locked, mapped, retired)| locked && mapped && !retired)
}

// C2-C6, C15, C17: a final COMMIT answer agrees with the worker's independently observed exit.
pub(super) fn i9<const N: usize>(client: &ClientRecord, store: &StoreRecord<N>) -> bool {
    if let Some(write) = store.terminal_write {
        return write.recorded == Some(write.expected);
    }
    let Some(receipt) = client.receipt else {
        return true;
    };
    if !matches!(
        store.delivery,
        Some(Delivery::CommitExit | Delivery::CommitGrace | Delivery::Repeat)
    ) {
        return true;
    }
    let txn = store.transaction(client);
    match (txn.end, receipt.seen) {
        (Some(expected), Seen::Commit(answer, _)) => expected == answer,
        (None, Seen::Error(Failure::CommitBusy(true))) => txn.worker,
        _ => false,
    }
}
