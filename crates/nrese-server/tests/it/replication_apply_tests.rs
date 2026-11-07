//! A replica refuses a log answer whose records don't match what it says it carries, and
//! keeps nothing of it: not its revision, not its statements, not after a reopen (review
//! p2-b7-syn69 r1, R1-F1). The records are real: a primary's log after ordinary commits.

use std::path::Path;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::replication::{self, ReplicationConfig, ReplicationMode};
use nrese_server::{AppState, DeploymentPosture, build_app};
use nrese_store::{StoreConfig, StoreService};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::support::query_text;

const Q: &str = "<http://e/q> <http://e/p> <http://e/o>";
const P: &str = "<http://e/p> <http://e/p> <http://e/o>";

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// `text` written to `store` through the mutation pipeline, as a store without a
/// reasoner is written.
fn update(store: &StoreService, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    nrese_store::MutationPipeline::new(
        std::sync::Arc::new(store.clone()),
        std::sync::Arc::new(ReasonerService::new(ReasonerConfig::default())),
    )
    .execute_update_str(text)?;
    Ok(())
}

/// A primary's log: revision 1 inserts Q; the replica's directory is a copy of the primary
/// at revision 1; then revision 2 inserts P and 3 deletes Q. Returns the replica's
/// directory (holding its parent), the frames of revision 2 alone and of 2 and 3.
/// The replica's directory, the frames of revision 2, the frames of revisions 2 and 3.
type Log = (tempfile::TempDir, Vec<u8>, Vec<u8>);

fn log() -> Result<Log, Box<dyn std::error::Error>> {
    let primary = tempfile::tempdir()?;
    let replica = tempfile::tempdir()?;
    {
        let store = StoreService::new(StoreConfig::on_disk(primary.path()))?;
        update(&store, &format!("INSERT DATA {{ {Q} }}"))?;
        assert_eq!(store.current_revision(), 1);
    }
    copy_dir(primary.path(), &replica.path().join("data"))?;
    let store = StoreService::new(StoreConfig::on_disk(primary.path()))?;
    update(&store, &format!("INSERT DATA {{ {P} }}"))?;
    update(&store, &format!("DELETE DATA {{ {Q} }}"))?;
    assert_eq!(store.current_revision(), 3);
    let both = store.replication_log(1, 1 << 20)?;
    assert_eq!((both.records, both.last), (2, 3));
    let first = store.replication_log(1, 1)?;
    assert_eq!((first.records, first.last), (1, 2));
    Ok((replica, first.frames, both.frames))
}

/// A primary that answers every log request with `headers` and `body`; its URL.
async fn answering(headers: [(&'static str, u64); 3], body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await.unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let mut answer = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/vnd.nrese.log\r\n\
                 content-length: {}\r\nconnection: close\r\n",
                body.len()
            )
            .into_bytes();
            for (name, value) in headers {
                answer.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
            }
            answer.extend_from_slice(b"\r\n");
            answer.extend_from_slice(&body);
            let _ = stream.write_all(&answer).await;
            let _ = stream.shutdown().await;
        }
    });
    url
}

fn replica(dir: &Path) -> Result<AppState, Box<dyn std::error::Error>> {
    replica_of(dir, None)
}

fn replica_of(dir: &Path, primary: Option<String>) -> Result<AppState, Box<dyn std::error::Error>> {
    let state = AppState::new(
        StoreService::new(StoreConfig::on_disk(dir))?,
        ReasonerService::new(ReasonerConfig::default()),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary,
        ..ReplicationConfig::default()
    });
    state.mark_ready();
    Ok(state)
}

/// How many records the stop test's batch has: enough that applying them takes far longer
/// than stopping does.
const MANY: u64 = 3000;

/// A follower stopped while it applies a batch: once `stop` returns, nothing more is
/// published, the store's directory opens again (nothing holds its lock), at the revision
/// reached, a whole-record prefix of the batch (review p2-b7-syn69 r1, R1-F2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_follower_publishes_nothing_more_and_lets_go_of_the_store()
-> Result<(), Box<dyn std::error::Error>> {
    let primary_dir = tempfile::tempdir()?;
    let replica_root = tempfile::tempdir()?;
    let data = replica_root.path().join("data");
    let frames = {
        let store = StoreService::new(StoreConfig::on_disk(primary_dir.path()))?;
        drop(store);
        copy_dir(primary_dir.path(), &data)?;
        let store = StoreService::new(StoreConfig::on_disk(primary_dir.path()))?;
        for n in 0..MANY {
            update(
                &store,
                &format!("INSERT DATA {{ <http://e/s{n}> <http://e/p> <http://e/o> }}"),
            )?;
        }
        let batch = store.replication_log(0, 1 << 30)?;
        assert_eq!((batch.records as u64, batch.last), (MANY, MANY));
        batch.frames
    };
    let primary = answering(
        [
            ("x-nrese-records", MANY),
            ("x-nrese-last-revision", MANY),
            ("x-nrese-latest-revision", MANY),
        ],
        frames,
    )
    .await;
    let state = replica_of(&data, Some(primary))?;
    let store = state.store().clone();
    let follower = replication::follow(state)?;
    // Applying: the first records are published.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while store.current_revision() == 0 {
        assert!(std::time::Instant::now() < until, "nothing applied in 60 s");
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    follower.stop().await;
    // Right after stop: this test's handle is the store's last; the directory opens again
    // at once, at the revision stop left.
    let later = store.current_revision();
    drop(store);
    let reopened = StoreService::new(StoreConfig::on_disk(&data));
    let reopened_at = reopened.as_ref().map(StoreService::current_revision);
    let mut wrong = Vec::new();
    if later >= MANY {
        wrong.push(format!("not stopped within the batch: revision {later}"));
    }
    match &reopened_at {
        Ok(at) if *at == later => {}
        other => wrong.push(format!(
            "reopened right after stop: {other:?}, stopped at {later}"
        )),
    }
    if let Ok(store) = reopened {
        drop(store);
        let state = replica(&data)?;
        let ask = |n: u64| format!("ASK {{ <http://e/s{n}> <http://e/p> <http://e/o> }}");
        let app = build_app(state.clone());
        let last_in = query_text(app.clone(), &ask(later - 1))
            .await?
            .contains("true");
        let next_out = !query_text(app, &ask(later)).await?.contains("true");
        if !(last_in && next_out) {
            wrong.push(format!(
                "contents at {later}: s{} held {last_in}, s{later} absent {next_out}",
                later - 1
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}

/// The replica's revision and whether it holds Q and P.
async fn holds(state: &AppState) -> Result<(u64, bool, bool), Box<dyn std::error::Error>> {
    let ask = |triple: &str| format!("ASK {{ {triple} }}");
    let app = build_app(state.clone());
    let q = query_text(app.clone(), &ask(Q)).await?.contains("true");
    let p = query_text(app, &ask(P)).await?.contains("true");
    Ok((state.store().current_revision(), q, p))
}

/// Answers whose body disagrees with their headers are refused, with the replica as it was
/// (revision 1, Q, no P), also after a reopen. Guard (passes before the fix too): a batch
/// that matches its headers is applied. Every row is checked; the failures are reported
/// together.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_log_answer_leaves_nothing_behind() -> Result<(), Box<dyn std::error::Error>> {
    const RECORDS: &str = "x-nrese-records";
    const LAST: &str = "x-nrese-last-revision";
    const LATEST: &str = "x-nrese-latest-revision";
    let client = reqwest::Client::new();
    let mut wrong = Vec::new();
    for row in [
        "more records than it says",
        "fewer records than it says",
        "an invalid record after a valid one",
    ] {
        let (dir, first, both) = log()?;
        let data = dir.path().join("data");
        let (headers, body) = match row {
            "more records than it says" => ([(RECORDS, 1), (LAST, 2), (LATEST, 3)], both),
            "fewer records than it says" => ([(RECORDS, 2), (LAST, 3), (LATEST, 3)], first),
            _ => {
                let mut body = first;
                body.extend_from_slice(b"not a record at all");
                ([(RECORDS, 2), (LAST, 3), (LATEST, 3)], body)
            }
        };
        let primary = answering(headers, body).await;
        let state = replica(&data)?;
        let round = replication::pull_once(&client, &primary, &state).await;
        if round.is_ok() {
            wrong.push(format!("{row}: accepted: {round:?}"));
        }
        let now = holds(&state).await?;
        if now != (1, true, false) {
            wrong.push(format!(
                "{row}: (revision, Q, P) = {now:?}, not (1, true, false)"
            ));
        }
        drop(state);
        let reopened = holds(&replica(&data)?).await?;
        if reopened != (1, true, false) {
            wrong.push(format!(
                "{row}: after reopen (revision, Q, P) = {reopened:?}"
            ));
        }
    }
    let (dir, _, both) = log()?;
    let data = dir.path().join("data");
    let primary = answering([(RECORDS, 2), (LAST, 3), (LATEST, 3)], both).await;
    let state = replica(&data)?;
    let round = replication::pull_once(&client, &primary, &state).await;
    let now = holds(&state).await?;
    if round.as_ref().ok() != Some(&2) || now != (3, false, true) {
        wrong.push(format!(
            "a matching batch: {round:?}, (revision, Q, P) = {now:?}"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    Ok(())
}
