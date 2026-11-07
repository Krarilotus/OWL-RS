//! A replica's rounds with a primary that misbehaves: one that sends a log answer without
//! its progress, or one that accepts the connection and then stops sending. Each round
//! ends, as an error naming the limit it passed, and no successful contact is recorded for
//! it; a primary that is slow but keeps sending isn't cut off.
//!
//! The stall tests run on a paused clock that only they move (`ClockHeld`), after giving the
//! replica a moment of real time to read what the primary sent.

use std::time::Duration;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_server::ai::AiSuggestionService;
use nrese_server::policy::PolicyConfig;
use nrese_server::replication::{self, ReplicationConfig, ReplicationMode};
use nrese_server::{AppState, DeploymentPosture};
use nrese_store::{StoreConfig, StoreService};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// How long, in real time, a test waits for anything before it fails instead of hanging.
const HANG: Duration = Duration::from_secs(20);
/// The real time a replica gets to read what the primary sent before the clock moves.
const SETTLE: Duration = Duration::from_millis(300);
/// A little past the 30 s a log answer has to begin and a body may pause.
const PAST_IDLE: Duration = Duration::from_secs(31);

fn replica_with(config: ReplicationConfig) -> AppState {
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    let state = AppState::new(
        store,
        ReasonerService::new(ReasonerConfig::default()),
        PolicyConfig::default(),
        AiSuggestionService::disabled(),
        DeploymentPosture::OpenWorkbench,
    )
    .with_replication(ReplicationConfig {
        mode: ReplicationMode::Replica,
        ..config
    });
    state.mark_ready();
    state
}

fn replica() -> AppState {
    replica_with(ReplicationConfig::default())
}

/// A primary that answers every request with `answer` (a whole HTTP response) and closes;
/// its URL.
async fn answering(answer: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let _ = read_request(&mut stream).await;
            let _ = stream.write_all(answer.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    url
}

/// Reads a request's head from `stream`.
async fn read_request(stream: &mut TcpStream) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.push(byte[0]);
    }
    Ok(())
}

/// A log answer: status 200, `headers` (`name: value` lines), `body`.
fn log_answer(headers: &[(&str, &str)], body: &str) -> String {
    let mut answer = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/vnd.nrese.log\r\ncontent-length: {}\r\n\
         connection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        answer.push_str(&format!("{name}: {value}\r\n"));
    }
    answer.push_str("\r\n");
    answer.push_str(body);
    answer
}

/// A log answer that doesn't say what it carries, or says something its body or the log
/// contradicts, is an error, and the status records no contact. Every row is checked; the
/// failures are reported together.
#[tokio::test]
async fn a_log_answer_without_its_progress_is_an_error() {
    const RECORDS: &str = "x-nrese-records";
    const LAST: &str = "x-nrese-last-revision";
    const LATEST: &str = "x-nrese-latest-revision";
    /// A row: its name, the answer's progress headers, its body.
    type Row<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);
    let rows: [Row; 5] = [
        ("no progress headers, a body", vec![], "frames"),
        (
            "records not a number",
            vec![(RECORDS, "some"), (LAST, "0"), (LATEST, "0")],
            "frames",
        ),
        (
            "no records, a body",
            vec![(RECORDS, "0"), (LAST, "0"), (LATEST, "0")],
            "frames",
        ),
        (
            "a record, no body",
            vec![(RECORDS, "1"), (LAST, "1"), (LATEST, "1")],
            "",
        ),
        ("no latest revision", vec![(RECORDS, "0"), (LAST, "0")], ""),
    ];
    let client = reqwest::Client::new();
    let mut wrong = Vec::new();
    for (row, headers, body) in rows {
        let primary = answering(log_answer(&headers, body)).await;
        let replica = replica();
        let round = replication::pull_once(&client, &primary, &replica).await;
        let status = replica.replication().status(0);
        if let Ok(records) = round {
            wrong.push(format!("{row}: accepted, {records} records"));
        }
        if let Some(contact) = status.last_contact_unix {
            wrong.push(format!("{row}: a contact recorded at {contact}"));
        }
    }
    // Guard (passes before the check too): an empty answer that says so is a contact.
    let primary = answering(log_answer(
        &[(RECORDS, "0"), (LAST, "0"), (LATEST, "0")],
        "",
    ))
    .await;
    let replica = replica();
    let round = replication::pull_once(&client, &primary, &replica).await;
    if round.as_ref().ok() != Some(&0)
        || replica.replication().status(0).last_contact_unix.is_none()
    {
        wrong.push(format!("an empty answer: {round:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// While this lives a blocking task is in flight, and tokio doesn't move the paused clock
/// by itself: only `tokio::time::advance` does. Dropped, the task ends.
struct ClockHeld {
    _release: std::sync::mpsc::Sender<()>,
}

fn hold_clock() -> ClockHeld {
    let (release, held) = std::sync::mpsc::channel::<()>();
    tokio::task::spawn_blocking(move || held.recv());
    ClockHeld { _release: release }
}

/// A primary that accepts one connection and sends only what the test tells it to.
struct HeldPrimary {
    listener: TcpListener,
    url: String,
}

impl HeldPrimary {
    async fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        Self { listener, url }
    }

    /// The connection of `running`'s request, the request read.
    async fn request<T: std::fmt::Debug>(
        &self,
        running: &mut JoinHandle<T>,
    ) -> Result<TcpStream, String> {
        let mut stream = tokio::select! {
            accepted = self.listener.accept() => accepted.map_err(|e| e.to_string())?.0,
            ended = &mut *running => return Err(format!("ended before its request: {ended:?}")),
            _ = real_time(HANG) => return Err("no request came".to_owned()),
        };
        read_request(&mut stream).await.map_err(|e| e.to_string())?;
        Ok(stream)
    }
}

/// `bytes` sent, and a moment of real time (the clock held) for the replica to read them.
async fn send(stream: &mut TcpStream, bytes: &[u8]) {
    stream.write_all(bytes).await.expect("sent");
    stream.flush().await.expect("flushed");
    real_time(SETTLE).await;
}

/// `running`'s result once the clock has moved on by `by`; an error if it hasn't ended
/// `HANG` of real time later.
async fn after<T: std::fmt::Debug>(by: Duration, running: &mut JoinHandle<T>) -> Result<T, String> {
    tokio::time::advance(by).await;
    tokio::select! {
        ended = running => ended.map_err(|e| e.to_string()),
        _ = real_time(HANG) => Err(format!("still running {by:?} later")),
    }
}

/// Whether `running` still runs once the clock has moved on by `by` (and a moment of real
/// time has passed).
async fn still_running_after<T>(by: Duration, running: &JoinHandle<T>) -> bool {
    tokio::time::advance(by).await;
    real_time(SETTLE).await;
    !running.is_finished()
}

/// Fires after `limit` of real time, not the test's paused clock.
async fn real_time(limit: Duration) {
    let (fired, watchdog) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        let _ = fired.send(());
    });
    let _ = watchdog.await;
}

/// An error whose text contains `expected`.
fn failed_with(ended: &Result<Result<(), String>, String>, expected: &str) -> bool {
    matches!(ended, Ok(Err(error)) if error.contains(expected))
}

fn pull(primary: &HeldPrimary, replica: AppState) -> JoinHandle<Result<(), String>> {
    let url = primary.url.clone();
    tokio::spawn(async move {
        replication::pull_once(&reqwest::Client::new(), &url, &replica)
            .await
            .map(drop)
            .map_err(|error| format!("{error:#}"))
    })
}

fn start(primary: &HeldPrimary, data: &tempfile::TempDir) -> JoinHandle<Result<(), String>> {
    let config = ReplicationConfig {
        mode: ReplicationMode::Replica,
        primary: Some(primary.url.clone()),
        ..ReplicationConfig::default()
    };
    let dir = data.path().to_path_buf();
    tokio::spawn(async move {
        replication::bootstrap(&config, &dir)
            .await
            .map(drop)
            .map_err(|error| format!("{error:#}"))
    })
}

/// An image answer's head: its manifest, a 1000-byte body to come.
fn image_head() -> String {
    let manifest = serde_json::to_string(&nrese_store::ImageManifest {
        format: "nrese-checkpoint".to_owned(),
        file: "checkpoint-1.nck".to_owned(),
        revision: 1,
        quads: 0,
        inferred: 0,
        bytes: 1000,
        sha256: "0".repeat(64),
        created_unix: 0,
        nrese_version: "test".to_owned(),
    })
    .expect("manifest");
    format!("HTTP/1.1 200 OK\r\ncontent-length: 1000\r\nx-nrese-image-manifest: {manifest}\r\n\r\n")
}

const LOG_HEAD: &str = "HTTP/1.1 200 OK\r\ncontent-length: 1000\r\nx-nrese-records: 1\r\n\
                        x-nrese-last-revision: 1\r\nx-nrese-latest-revision: 1\r\n\r\n";

/// A log round whose primary never begins its answer ends once the answer's 30 s have
/// passed.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_log_round_without_an_answer_ends() {
    let _clock = hold_clock();
    let primary = HeldPrimary::bind().await;
    let mut running = pull(&primary, replica());
    let _stream = primary.request(&mut running).await.expect("request");
    let ended = after(PAST_IDLE, &mut running).await;
    assert!(failed_with(&ended, "didn't answer"), "{ended:?}");
}

/// A log round whose primary stops in the middle of its body ends once it has paused
/// 30 s.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_log_round_whose_body_stops_ends() {
    let _clock = hold_clock();
    let primary = HeldPrimary::bind().await;
    let mut running = pull(&primary, replica());
    let mut stream = primary.request(&mut running).await.expect("request");
    send(&mut stream, format!("{LOG_HEAD}part").as_bytes()).await;
    let ended = after(PAST_IDLE, &mut running).await;
    assert!(failed_with(&ended, "stopped sending"), "{ended:?}");
}

/// The primary prepares an image before its answer begins: a replica waits past the 30 s
/// a log answer has, and gives up once `replication.image_timeout` (an hour) has passed.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn an_image_request_without_an_answer_ends() {
    let _clock = hold_clock();
    let primary = HeldPrimary::bind().await;
    let data = tempfile::tempdir().expect("data directory");
    let mut running = start(&primary, &data);
    let _stream = primary.request(&mut running).await.expect("request");
    assert!(still_running_after(PAST_IDLE, &running).await);
    let ended = after(Duration::from_secs(3600), &mut running).await;
    assert!(failed_with(&ended, "didn't answer"), "{ended:?}");
}

/// An image whose primary stops sending in the middle of it ends once it has paused 30 s.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn an_image_whose_body_stops_ends() {
    let _clock = hold_clock();
    let primary = HeldPrimary::bind().await;
    let data = tempfile::tempdir().expect("data directory");
    let mut running = start(&primary, &data);
    let mut stream = primary.request(&mut running).await.expect("request");
    send(&mut stream, format!("{}part", image_head()).as_bytes()).await;
    let ended = after(PAST_IDLE, &mut running).await;
    assert!(failed_with(&ended, "stopped sending"), "{ended:?}");
}

/// Guard (passes before the limits too): an image prepared for a minute, then sent a piece
/// every 20 s for more than five minutes, still comes.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_slow_image_that_keeps_coming_is_not_cut_off() {
    let _clock = hold_clock();
    let primary = HeldPrimary::bind().await;
    let data = tempfile::tempdir().expect("data directory");
    let mut running = start(&primary, &data);
    let mut stream = primary.request(&mut running).await.expect("request");
    assert!(still_running_after(Duration::from_secs(60), &running).await);
    send(&mut stream, image_head().as_bytes()).await;
    for piece in 0..16 {
        send(&mut stream, b"piece").await;
        if !still_running_after(Duration::from_secs(20), &running).await {
            panic!("cut off after piece {piece}: {:?}", running.await);
        }
    }
}

/// A stopped follower lets go of its round: the connection of the request it was waiting
/// on closes (written with the fix: `Follower` is its API).
#[tokio::test]
async fn a_stopped_follower_lets_go_of_its_round() {
    let primary = HeldPrimary::bind().await;
    let state = replica_with(ReplicationConfig {
        primary: Some(primary.url.clone()),
        ..ReplicationConfig::default()
    });
    let follower = replication::follow(state).expect("follower");
    let mut stream = tokio::time::timeout(HANG, primary.listener.accept())
        .await
        .expect("the follower's request")
        .expect("accepted")
        .0;
    read_request(&mut stream).await.expect("its request");
    tokio::time::timeout(HANG, follower.stop())
        .await
        .expect("stopped");
    let mut rest = Vec::new();
    let closed = tokio::time::timeout(HANG, stream.read_to_end(&mut rest)).await;
    assert!(matches!(closed, Ok(Ok(_))), "{closed:?}");
}

/// A log round whose primary sends a piece every 20 s (never pausing 30 s) still ends once
/// `replication.round_timeout` has passed (written with the fix: the setting is new).
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_log_round_that_trickles_ends_at_its_round_timeout() {
    let _clock = hold_clock();
    let primary = HeldPrimary::bind().await;
    let mut running = pull(
        &primary,
        replica_with(ReplicationConfig {
            round_timeout: Some(Duration::from_secs(60)),
            ..ReplicationConfig::default()
        }),
    );
    let mut stream = primary.request(&mut running).await.expect("request");
    send(&mut stream, LOG_HEAD.as_bytes()).await;
    for _ in 0..2 {
        send(&mut stream, b"x").await;
        assert!(still_running_after(Duration::from_secs(20), &running).await);
    }
    send(&mut stream, b"x").await;
    let ended = after(Duration::from_secs(25), &mut running).await;
    assert!(failed_with(&ended, "round_timeout"), "{ended:?}");
}
