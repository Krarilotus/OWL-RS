//! Long operations run as jobs: an import of large files answers at once with a job, which
//! reports how far it is, ends with a report or an error, and can be cancelled. The last
//! [`KEPT`] finished jobs stay listed.
//!
//! A job ends once. Its [`Job`] is its worker's: one dropped without [`Job::finish`] (its
//! worker panicked, or its task was dropped before it ran) ends the job failed, with words
//! that claim only what the worker knew ([`Job::running`], [`Job::loaded`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime};

use crate::LoadProgress;

/// Finished jobs kept for listing.
pub const KEPT: usize = 100;

/// Where a job is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Why a job did not complete.
#[derive(Debug, PartialEq, Eq)]
pub enum JobFailure {
    /// The operation stopped in response to cancellation.
    Cancelled,
    /// The operation failed with a message for the client.
    Failed(String),
}

impl JobFailure {
    /// How a load's error ends its job. Only a load that stopped for its cancellation ends
    /// cancelled. Every other error is a failure with its message, and an unpublished load
    /// whose quarantine record couldn't be kept says how to `retry_action` it again.
    pub fn of_load(
        error: crate::StoreError,
        retry_action: &str,
        data_dir: Option<&std::path::Path>,
    ) -> Self {
        match error {
            crate::StoreError::LoadCancelled => Self::Cancelled,
            crate::StoreError::LoadQuarantineNotRecorded(error) => {
                Self::Failed(error.unpublished_message(retry_action, data_dir))
            }
            error => Self::Failed(error.to_string()),
        }
    }
}

/// A job as listed.
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobView {
    pub id: u64,
    /// What it does (`import`).
    pub kind: String,
    pub repository: String,
    /// What it works on (the files of an import).
    pub description: String,
    pub state: JobState,
    /// Its phase while running (`loading`, `reasoning`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Files done and in all, statements parsed so far.
    pub files_done: u64,
    pub files: u64,
    pub parsed: u64,
    /// When it started, in seconds since the Unix epoch, and how long it ran.
    pub started: u64,
    pub elapsed_ms: u64,
    /// Its report when done.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub report: Option<serde_json::Value>,
    /// Why it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How a job ended, as recorded ([`Jobs::end`]); put into words when the job is read.
#[derive(Debug)]
enum Outcome {
    Done(serde_json::Value),
    Cancelled,
    Failed(String),
    /// Its [`Job`] was dropped without an end, its worker at `stage`.
    Stopped {
        stage: Stage,
        retry: &'static str,
    },
}

/// How far a job's worker knows its import got.
#[derive(Debug, Clone, Copy)]
enum Stage {
    /// The worker hasn't started.
    Queued,
    /// The worker started; its load hasn't returned.
    Running,
    /// The load returned, published at `revision`.
    Loaded { revision: u64 },
}

impl Stage {
    /// Why the job failed, with how to `retry` its data (`send`, `load`): only what the
    /// worker knew when it stopped.
    fn words(self, retry: &str) -> String {
        match self {
            Self::Queued => format!(
                "the server stopped before the import started; nothing of it ran. It's safe to {retry} it again."
            ),
            Self::Running => format!(
                "the import stopped (an internal error) before it finished; whether any of it took effect is unknown. Check the data and the quarantine list before you {retry} it again."
            ),
            Self::Loaded { revision } => format!(
                "the load was published at revision {revision}, then the import stopped (an internal error); reasoning may not be up to date. Don't {retry} it again."
            ),
        }
    }

    /// The report of a job stopped here: the revision its load published.
    fn report(self) -> Option<serde_json::Value> {
        match self {
            Self::Loaded { revision } => Some(serde_json::json!({ "revision": revision })),
            Self::Queued | Self::Running => None,
        }
    }
}

struct Entry {
    kind: String,
    repository: String,
    description: String,
    phase: Option<String>,
    progress: Arc<LoadProgress>,
    started: SystemTime,
    since: Instant,
    /// Set once ([`Jobs::end`]); running while `None`.
    end: Option<(Outcome, Instant)>,
}

/// The jobs of a server.
#[derive(Default)]
pub struct Jobs {
    inner: Mutex<(u64, BTreeMap<u64, Entry>)>,
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs").finish_non_exhaustive()
    }
}

/// A running job, for the worker that runs it: made before the worker is submitted, and
/// moved into it, so that the job ends however the worker does.
pub struct Job {
    jobs: Arc<Jobs>,
    id: u64,
    progress: Arc<LoadProgress>,
    /// How the client gives the data again (`send`, `load`).
    retry: &'static str,
    stage: Stage,
}

impl Job {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// What the job's work reports to (and is cancelled through).
    pub fn progress(&self) -> &LoadProgress {
        &self.progress
    }

    /// Names the job's phase.
    pub fn phase(&self, phase: &str) {
        self.jobs
            .update(self.id, |entry| entry.phase = Some(phase.to_owned()));
    }

    /// Its worker has started: the worker's first statement.
    pub fn running(&mut self) {
        self.stage = Stage::Running;
    }

    /// Its load returned, published at `revision`: as soon as it returns, before anything
    /// after it.
    pub fn loaded(&mut self, revision: u64) {
        self.stage = Stage::Loaded { revision };
    }

    /// Ends the job with the operation's outcome, preserving a genuine failure
    /// even when a cancel request arrived before this call.
    pub fn finish(self, outcome: Result<serde_json::Value, JobFailure>) {
        let outcome = match outcome {
            Ok(report) => Outcome::Done(report),
            Err(JobFailure::Cancelled) => Outcome::Cancelled,
            Err(JobFailure::Failed(error)) => Outcome::Failed(error),
        };
        // The drop that follows finds the end recorded and changes nothing.
        self.jobs.end(self.id, outcome);
    }
}

impl Drop for Job {
    /// A job its worker didn't finish ends failed, at the stage the worker had reached.
    fn drop(&mut self) {
        let (stage, retry) = (self.stage, self.retry);
        self.jobs.end(self.id, Outcome::Stopped { stage, retry });
    }
}

impl Jobs {
    /// Starts a job whose data the client gives again by `retry` (`send`, `load`).
    pub fn start(
        self: &Arc<Self>,
        kind: &str,
        repository: &str,
        description: &str,
        retry: &'static str,
    ) -> Job {
        let progress = Arc::new(LoadProgress::default());
        let mut inner = self.lock();
        inner.0 += 1;
        let id = inner.0;
        inner.1.insert(
            id,
            Entry {
                kind: kind.to_owned(),
                repository: repository.to_owned(),
                description: description.to_owned(),
                phase: None,
                progress: Arc::clone(&progress),
                started: SystemTime::now(),
                since: Instant::now(),
                end: None,
            },
        );
        Job {
            jobs: Arc::clone(self),
            id,
            progress,
            retry,
            stage: Stage::Queued,
        }
    }

    /// The registry, also after a panic under its lock: every change of it is one step, so
    /// none is left half done.
    fn lock(&self) -> MutexGuard<'_, (u64, BTreeMap<u64, Entry>)> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, id: u64, change: impl FnOnce(&mut Entry)) {
        if let Some(entry) = self.lock().1.get_mut(&id) {
            change(entry);
        }
    }

    /// Records job `id`'s end, unless it has one (the first end stands), then forgets the
    /// oldest ended jobs beyond [`KEPT`].
    fn end(&self, id: u64, outcome: Outcome) {
        let mut inner = self.lock();
        let Some(entry) = inner.1.get_mut(&id) else {
            return;
        };
        if entry.end.is_some() {
            return;
        }
        entry.phase = None;
        entry.end = Some((outcome, Instant::now()));
        let ended: Vec<u64> = inner
            .1
            .iter()
            .filter(|(_, entry)| entry.end.is_some())
            .map(|(&id, _)| id)
            .collect();
        for id in ended.iter().take(ended.len().saturating_sub(KEPT)) {
            inner.1.remove(id);
        }
    }

    fn view(id: u64, entry: &Entry) -> JobView {
        let (files_done, files) = entry.progress.files();
        let (state, report, error) = match entry.end.as_ref().map(|(outcome, _)| outcome) {
            None => (JobState::Running, None, None),
            Some(Outcome::Done(report)) => (JobState::Done, Some(report.clone()), None),
            Some(Outcome::Cancelled) => (JobState::Cancelled, None, None),
            Some(Outcome::Failed(error)) => (JobState::Failed, None, Some(error.clone())),
            Some(Outcome::Stopped { stage, retry }) => {
                (JobState::Failed, stage.report(), Some(stage.words(retry)))
            }
        };
        let ended = entry.end.as_ref().map(|&(_, at)| at);
        let elapsed = ended.unwrap_or_else(Instant::now) - entry.since;
        JobView {
            id,
            kind: entry.kind.clone(),
            repository: entry.repository.clone(),
            description: entry.description.clone(),
            state,
            phase: entry.phase.clone(),
            files_done,
            files,
            parsed: entry.progress.parsed(),
            started: entry
                .started
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            report,
            error,
        }
    }

    /// Every job kept, the latest first.
    pub fn list(&self) -> Vec<JobView> {
        let inner = self.lock();
        inner
            .1
            .iter()
            .rev()
            .map(|(&id, entry)| Self::view(id, entry))
            .collect()
    }

    pub fn get(&self, id: u64) -> Option<JobView> {
        let inner = self.lock();
        inner.1.get(&id).map(|entry| Self::view(id, entry))
    }

    /// Cancels job `id` if it runs; whether it ran.
    pub fn cancel(&self, id: u64) -> bool {
        let inner = self.lock();
        match inner.1.get(&id) {
            Some(entry) if entry.end.is_none() => {
                entry.progress.cancel();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(jobs: &Arc<Jobs>) -> Job {
        jobs.start("import", "nrese", "an uploaded document", "send")
    }

    // Spec: docs/ops/http-api.md says an import job ends `done`, `failed` or `cancelled`. A
    // job whose owner panics after a panic poisoned the registry's lock -> it ends `failed`,
    // and is read, listed and not cancelled as such.
    #[test]
    fn a_job_ends_and_is_read_after_a_panic_poisoned_the_registry() {
        let jobs = Arc::new(Jobs::default());
        let job = started(&jobs);
        let id = job.id();
        let poisoner = Arc::clone(&jobs);
        std::thread::spawn(move || {
            let _held = poisoner.inner.lock();
            panic!("a panic under the jobs lock");
        })
        .join()
        .unwrap_err();
        assert!(jobs.inner.is_poisoned());
        std::thread::spawn(move || {
            let _owned = job;
            panic!("its owner panics");
        })
        .join()
        .unwrap_err();

        let view = jobs.get(id).expect("the job is kept");
        assert_eq!(view.state, JobState::Failed, "{view:?}");
        let listed = jobs.list();
        assert_eq!(
            listed.first().map(|job| (job.id, job.state.clone())),
            Some((id, JobState::Failed))
        );
        assert!(!jobs.cancel(id), "an ended job isn't cancelled");
    }

    // Spec: docs/ops/http-api.md says a failed job's error is its load's message. A load that
    // returned an unsettled commit -> `failed` with the engine's own words for it.
    #[test]
    fn a_returned_unsettled_commit_keeps_its_own_words() {
        let jobs = Arc::new(Jobs::default());
        let job = started(&jobs);
        let id = job.id();
        let error =
            crate::StoreError::Engine(nrese_engine::EngineError::CommitUnsettled { revision: 7 });
        job.finish(Err(JobFailure::of_load(error, "send", None)));

        let view = jobs.get(id).unwrap();
        assert_eq!(view.state, JobState::Failed, "{view:?}");
        assert_eq!(
            view.error.as_deref(),
            Some(
                "storage engine error: revision 7 may be committed but isn't served; the engine takes no more writes: reopen it to recover what is on disk"
            )
        );
    }

    // Spec: docs/ops/http-api.md says an import that could not keep its quarantine
    // and published nothing ends failed with retry advice, even if cancelled late.
    #[cfg(feature = "fault-points")]
    #[test]
    fn late_cancel_preserves_import_failure() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.nt");
        std::fs::write(&file, "<http://example.com/a> <http://example.com/p> .\n").unwrap();
        let store = crate::StoreService::new(crate::StoreConfig::on_disk(dir.path().join("store")))
            .unwrap();
        let request = crate::BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: crate::GraphTarget::DefaultGraph,
            invalid: None,
        };
        let jobs = Arc::new(Jobs::default());
        let job = jobs.start("import", "nrese", "an uploaded document", "send");
        let id = job.id();
        store.quarantine().fail_next_commit();
        let error = store.bulk_load_with(&request, job.progress()).unwrap_err();
        assert!(
            matches!(error, crate::StoreError::LoadQuarantineNotRecorded(_)),
            "expected an unpublished quarantine error, got {error}"
        );
        jobs.cancel(id);
        job.finish(Err(JobFailure::of_load(error, "send", None)));

        let view = serde_json::to_value(jobs.get(id).unwrap()).unwrap();
        assert_eq!(view["state"], "failed", "GET job body: {view}");
        assert_eq!(
            view["error"],
            "the load published nothing. The quarantine record couldn't be kept: simulated failure. It's safe to send it again once the cause is resolved."
        );
    }

    // Spec: docs/ops/http-api.md says cancelling an import stops it before its next file;
    // one cancelled before its first file loads nothing and its job ends cancelled.
    #[test]
    fn a_load_cancelled_before_its_first_file_ends_its_job_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("good.nt");
        std::fs::write(
            &file,
            "<http://example.com/a> <http://example.com/p> <http://example.com/o> .\n",
        )
        .unwrap();
        let store = crate::StoreService::new(crate::StoreConfig::on_disk(dir.path().join("store")))
            .unwrap();
        let request = crate::BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: crate::GraphTarget::DefaultGraph,
            invalid: None,
        };
        let jobs = Arc::new(Jobs::default());
        let job = jobs.start("import", "nrese", "an uploaded document", "send");
        let id = job.id();
        assert!(jobs.cancel(id));
        let error = store.bulk_load_with(&request, job.progress()).unwrap_err();
        let failure = JobFailure::of_load(error, "send", None);
        assert_eq!(failure, JobFailure::Cancelled);
        job.finish(Err(failure));

        let view = serde_json::to_value(jobs.get(id).unwrap()).unwrap();
        assert_eq!(view["state"], "cancelled", "GET job body: {view}");
        assert_eq!(store.stats().unwrap().quad_count, 0, "nothing was loaded");
    }

    #[test]
    fn jobs_report_their_progress_and_outcome() {
        let jobs = Arc::new(Jobs::default());
        let job = jobs.start("import", "nrese", "a.ttl", "send");
        let id = job.id();
        job.phase("loading");
        let view = jobs.get(id).unwrap();
        assert_eq!(view.state, JobState::Running);
        assert_eq!(view.phase.as_deref(), Some("loading"));
        job.finish(Ok(serde_json::json!({ "inserted": 3 })));
        let view = jobs.get(id).unwrap();
        assert_eq!(view.state, JobState::Done);
        assert_eq!(view.report.unwrap()["inserted"], 3);
        assert!(!jobs.cancel(id), "a finished job isn't cancelled");

        let job = jobs.start("import", "nrese", "b.ttl", "send");
        assert!(jobs.cancel(job.id()));
        assert!(job.progress().is_cancelled());
        let id = job.id();
        job.finish(Err(JobFailure::Cancelled));
        assert_eq!(jobs.get(id).unwrap().state, JobState::Cancelled);

        let job = jobs.start("import", "nrese", "c.ttl", "send");
        let id = job.id();
        job.finish(Err(JobFailure::Failed("no such file".to_owned())));
        let view = jobs.get(id).unwrap();
        assert_eq!(view.state, JobState::Failed);
        assert_eq!(view.error.as_deref(), Some("no such file"));
        assert_eq!(jobs.list().first().map(|j| j.id), Some(id));

        for _ in 0..KEPT + 5 {
            jobs.start("import", "nrese", "x", "send")
                .finish(Ok(serde_json::Value::Null));
        }
        assert_eq!(jobs.list().len(), KEPT);
    }
}
