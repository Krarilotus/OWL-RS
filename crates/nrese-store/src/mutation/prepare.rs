use std::sync::{Arc, OnceLock, Weak};

use nrese_engine::engine::PublicationListener;
use nrese_engine::{Snapshot, TermId, Transaction};
use nrese_exec::cell::{Build, BuildCell, Claim, End, Ended, Joined, Kind};
use nrese_exec::terminal::{Guard, terminal};
use nrese_reasoner::{RuleProgram, eval::GroundProgram};
use parking_lot::{Mutex, ReentrantMutex};

use super::ticket::MutationTicket;
use crate::reasoning::Program;
use crate::{StoreError, StoreService};

pub(super) type GroundKey = (u64, Option<TermId>);
type Entry = Option<(GroundKey, Arc<GroundProgram>)>;

pub(super) fn ground_key(snapshot: &Snapshot) -> GroundKey {
    let identity = snapshot.identity();
    (identity.revision, identity.equality)
}

pub(super) fn cached(entry: &Entry, key: GroundKey) -> Option<&Arc<GroundProgram>> {
    entry.as_ref().filter(|(at, _)| *at == key).map(|(_, p)| p)
}

#[derive(Clone)]
enum Handoff {
    Covered,
    Joined(Arc<Build<GroundProgram>>),
    Failed,
    Retired,
}

pub(super) struct Preparation {
    store: Weak<StoreService>,
    pub(super) rules: Option<RuleProgram>,
    program: OnceLock<Program>,
    pub(super) entry: Mutex<Entry>,
    builds: BuildCell<(), GroundProgram>,
    pub(super) retired: Mutex<bool>,
    // Only this thread defers its own commit notification until its entry is tagged.
    pub(super) tagging: ReentrantMutex<()>,
}

impl std::fmt::Debug for Preparation {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Preparation").finish_non_exhaustive()
    }
}

impl Preparation {
    pub(super) fn new(store: &Arc<StoreService>, rules: Option<RuleProgram>) -> Self {
        Self {
            store: Arc::downgrade(store),
            rules,
            program: OnceLock::new(),
            entry: Mutex::default(),
            builds: BuildCell::default(),
            retired: Mutex::default(),
            tagging: ReentrantMutex::default(),
        }
    }

    pub(super) fn register(self: &Arc<Self>, store: &StoreService) {
        if self.rules.is_some() && !store.config().replica {
            let listener: Arc<dyn PublicationListener> = self.clone();
            store.engine().listen_publications(&listener);
            self.wake();
        }
    }

    pub(super) fn compiled(
        &self,
        store: &StoreService,
        rules: &RuleProgram,
        tx: &Transaction<'_>,
    ) -> Result<&Program, StoreError> {
        if let Some(program) = self.program.get() {
            return Ok(program);
        }
        let config = store.config();
        let program = Program::compile(rules, &|term| tx.intern(term))
            .hiding_unnamed_classes(config.hide_unnamed_classes)
            .map_err(|error| store.interruption_refusal(error))?
            .by_representatives(config.equality_by_representatives)
            .storing_representatives(config.equality_compact);
        Ok(self.program.get_or_init(|| program))
    }

    pub(super) fn join(
        self: &Arc<Self>,
        store: &StoreService,
        ticket: &MutationTicket,
    ) -> Result<(), StoreError> {
        let stop = || ticket.stop_requested() || *self.retired.lock();
        let (handoff, reply) = terminal(|| Handoff::Failed);
        self.launch(Some(reply))?;
        let outcome = handoff.wait_stopped(&stop);
        let build = match outcome {
            Some(Handoff::Joined(build)) => build,
            Some(Handoff::Failed) => return Err(abandoned()),
            Some(Handoff::Covered | Handoff::Retired) | None => return Ok(()),
        };
        match build.join(None, &stop) {
            Joined::Told(Ended::End(End::Empty)) => {
                Err(store.interruption_refusal(Default::default()))
            }
            Joined::Told(Ended::Abandoned) => Err(abandoned()),
            _ => Ok(()),
        }
    }

    fn launch(self: &Arc<Self>, reply: Option<Guard<Handoff>>) -> std::io::Result<()> {
        let prep = Arc::clone(self);
        std::thread::Builder::new()
            .name("nrese-write-program".into())
            .spawn(move || prep.run(reply))
            .map(drop)
    }

    fn run(&self, mut reply: Option<Guard<Handoff>>) {
        let requested = reply.is_some();
        while let Some(store) = self.store.upgrade().filter(|_| !*self.retired.lock()) {
            let key = ground_key(&store.engine().snapshot());
            if cached(&self.entry.lock(), key).is_some() {
                if let Some(reply) = reply {
                    reply.finish(Handoff::Covered);
                }
                return;
            }
            let rules = self.rules.as_ref().expect("preparation requires rules");
            if !requested && !store.reasoning_is_current(rules) {
                return;
            }
            let (build, owner) = match self.builds.claim((), Kind::Opaque, || {
                let entry = self.entry.try_lock()?;
                cached(&entry, key).cloned().map(Some)
            }) {
                Claim::Wait(build) => (build, None),
                Claim::Build(builder) => (builder.handle(), Some(builder)),
            };
            if let Some(reply) = reply.take() {
                reply.finish(Handoff::Joined(build.clone()));
            }
            let completed = owner.map(|owner| (owner, self.build(&store, rules)));
            drop(store);
            if let Some((owner, end)) = completed {
                owner.end(end);
            }
            if requested
                || !matches!(
                    build.join(None, &|| *self.retired.lock()),
                    Joined::Told(Ended::End(End::Kept(_) | End::Later))
                )
            {
                return;
            }
        }
        if let Some(reply) = reply {
            reply.finish(Handoff::Retired);
        }
    }

    fn build(&self, store: &StoreService, rules: &RuleProgram) -> End<GroundProgram> {
        let (base, program) = {
            let tx = store.engine().transaction();
            if *self.retired.lock()
                || !store.reasoning_is_current_held(tx.writer().expect("writer"), rules)
            {
                return End::Later;
            }
            let base = tx.base();
            if let Some(program) = cached(&self.entry.lock(), ground_key(base)).cloned() {
                return End::Kept(program);
            }
            let Ok(program) = self.compiled(store, rules, &tx) else {
                return End::Empty;
            };
            (base.clone(), program)
        };
        let key = ground_key(&base);
        let moved = || ground_key(&store.engine().snapshot()) != key;
        let memory = store.memory_stop();
        let built = program.ground_program_until(&base, &|| {
            *self.retired.lock() || moved() || memory.exceeded()
        });
        let mut entry = self.entry.lock();
        let retired = self.retired.lock();
        if *retired || moved() {
            return End::Later;
        }
        let Ok(built) = built else { return End::Empty };
        let built = Arc::new(built);
        *entry = Some((key, Arc::clone(&built)));
        End::Kept(built)
    }

    fn wake(self: &Arc<Self>) {
        if let Err(error) = self.launch(None) {
            tracing::error!(%error, "starting write program preparation failed");
        }
    }
}

impl PublicationListener for Preparation {
    fn published(self: Arc<Self>, revision: u64, reading: Option<TermId>) {
        if !self.tagging.is_owned_by_current_thread()
            && !*self.retired.lock()
            && !self
                .entry
                .try_lock()
                .is_some_and(|entry| cached(&entry, (revision, reading)).is_some())
        {
            self.wake();
        }
    }
}

fn abandoned() -> StoreError {
    std::io::Error::other("write program preparation ended without a result").into()
}
