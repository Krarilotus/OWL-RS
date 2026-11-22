//! Long operations run as jobs: an import of large files answers at once with a job, which
//! reports how far it is, ends with a report or an error, and can be cancelled. The last
//! [`KEPT`] finished jobs stay listed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
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

struct Entry {
    kind: String,
    repository: String,
    description: String,
    state: JobState,
    phase: Option<String>,
    progress: Arc<LoadProgress>,
    started: SystemTime,
    since: Instant,
    ended: Option<Instant>,
    report: Option<serde_json::Value>,
    error: Option<String>,
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

/// A running job, for the code that runs it.
pub struct Job {
    jobs: Arc<Jobs>,
    id: u64,
    progress: Arc<LoadProgress>,
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

    /// Ends the job with the operation's outcome, preserving a genuine failure
    /// even when a cancel request arrived before this call.
    pub fn finish(self, outcome: Result<serde_json::Value, JobFailure>) {
        self.jobs.update(self.id, |entry| {
            entry.ended = Some(Instant::now());
            entry.phase = None;
            match outcome {
                Ok(report) => {
                    entry.state = JobState::Done;
                    entry.report = Some(report);
                }
                Err(JobFailure::Cancelled) => entry.state = JobState::Cancelled,
                Err(JobFailure::Failed(error)) => {
                    entry.state = JobState::Failed;
                    entry.error = Some(error);
                }
            }
        });
        self.jobs.forget_old();
    }
}

impl Jobs {
    /// Starts a job.
    pub fn start(self: &Arc<Self>, kind: &str, repository: &str, description: &str) -> Job {
        let progress = Arc::new(LoadProgress::default());
        let mut inner = self.inner.lock().expect("jobs");
        inner.0 += 1;
        let id = inner.0;
        inner.1.insert(
            id,
            Entry {
                kind: kind.to_owned(),
                repository: repository.to_owned(),
                description: description.to_owned(),
                state: JobState::Running,
                phase: None,
                progress: Arc::clone(&progress),
                started: SystemTime::now(),
                since: Instant::now(),
                ended: None,
                report: None,
                error: None,
            },
        );
        Job {
            jobs: Arc::clone(self),
            id,
            progress,
        }
    }

    fn update(&self, id: u64, change: impl FnOnce(&mut Entry)) {
        if let Some(entry) = self.inner.lock().expect("jobs").1.get_mut(&id) {
            change(entry);
        }
    }

    fn forget_old(&self) {
        let mut inner = self.inner.lock().expect("jobs");
        let finished: Vec<u64> = inner
            .1
            .iter()
            .filter(|(_, entry)| entry.state != JobState::Running)
            .map(|(&id, _)| id)
            .collect();
        for id in finished.iter().take(finished.len().saturating_sub(KEPT)) {
            inner.1.remove(id);
        }
    }

    fn view(id: u64, entry: &Entry) -> JobView {
        let (files_done, files) = entry.progress.files();
        let elapsed = entry.ended.unwrap_or_else(Instant::now) - entry.since;
        JobView {
            id,
            kind: entry.kind.clone(),
            repository: entry.repository.clone(),
            description: entry.description.clone(),
            state: entry.state.clone(),
            phase: entry.phase.clone(),
            files_done,
            files,
            parsed: entry.progress.parsed(),
            started: entry
                .started
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            report: entry.report.clone(),
            error: entry.error.clone(),
        }
    }

    /// Every job kept, the latest first.
    pub fn list(&self) -> Vec<JobView> {
        let inner = self.inner.lock().expect("jobs");
        inner
            .1
            .iter()
            .rev()
            .map(|(&id, entry)| Self::view(id, entry))
            .collect()
    }

    pub fn get(&self, id: u64) -> Option<JobView> {
        let inner = self.inner.lock().expect("jobs");
        inner.1.get(&id).map(|entry| Self::view(id, entry))
    }

    /// Cancels job `id` if it runs; whether it ran.
    pub fn cancel(&self, id: u64) -> bool {
        let inner = self.inner.lock().expect("jobs");
        match inner.1.get(&id) {
            Some(entry) if entry.state == JobState::Running => {
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
        let job = jobs.start("import", "nrese", "an uploaded document");
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
        let job = jobs.start("import", "nrese", "an uploaded document");
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
        let job = jobs.start("import", "nrese", "a.ttl");
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

        let job = jobs.start("import", "nrese", "b.ttl");
        assert!(jobs.cancel(job.id()));
        assert!(job.progress().is_cancelled());
        let id = job.id();
        job.finish(Err(JobFailure::Cancelled));
        assert_eq!(jobs.get(id).unwrap().state, JobState::Cancelled);

        let job = jobs.start("import", "nrese", "c.ttl");
        let id = job.id();
        job.finish(Err(JobFailure::Failed("no such file".to_owned())));
        let view = jobs.get(id).unwrap();
        assert_eq!(view.state, JobState::Failed);
        assert_eq!(view.error.as_deref(), Some("no such file"));
        assert_eq!(jobs.list().first().map(|j| j.id), Some(id));

        for _ in 0..KEPT + 5 {
            jobs.start("import", "nrese", "x")
                .finish(Ok(serde_json::Value::Null));
        }
        assert_eq!(jobs.list().len(), KEPT);
    }
}
