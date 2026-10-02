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

    /// Ends the job: done with `report`, or failed with an error (cancelled if it was
    /// cancelled).
    pub fn finish(self, outcome: Result<serde_json::Value, String>) {
        let cancelled = self.progress.is_cancelled();
        self.jobs.update(self.id, |entry| {
            entry.ended = Some(Instant::now());
            entry.phase = None;
            match outcome {
                Ok(report) => {
                    entry.state = JobState::Done;
                    entry.report = Some(report);
                }
                Err(_) if cancelled => entry.state = JobState::Cancelled,
                Err(error) => {
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
        job.finish(Err("the load was cancelled".to_owned()));
        assert_eq!(jobs.get(id).unwrap().state, JobState::Cancelled);

        let job = jobs.start("import", "nrese", "c.ttl");
        let id = job.id();
        job.finish(Err("no such file".to_owned()));
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
