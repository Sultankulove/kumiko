//! E2 acceptance (spec, section 28): 10 000 parallel requests, cancellation, a 100 MB stream
//! and a dropped connection without panics, plus backpressure and malformed input.

// Test harness: helpers outside `#[test]` functions may panic too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf, duplex, split};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;

use kumiko_ipc::{Frame, Handler, Peer, PeerConfig, Router, RpcError, StreamReader};
use kumiko_protocol::common::{CommandCall, StreamId};
use kumiko_protocol::methods::base::{CancelParams, CancelRequest, Log, LogLevel, LogParams, Ping};
use kumiko_protocol::methods::command::{ExecuteCommand, InvokeCommand};
use kumiko_protocol::{ErrorCode, Message, RequestId, ResponseError};
use kumiko_types::CommandId;

const WAIT: Duration = Duration::from_secs(5);

fn call(args: Vec<Value>) -> CommandCall {
    CommandCall {
        command: CommandId::new("test.echo").unwrap(),
        args,
    }
}

fn echo() -> Router {
    Router::new().on_request::<ExecuteCommand, _, _>(|call| async move { Ok(json!(call.args)) })
}

fn pair<A: Handler, B: Handler>(
    a: A,
    config_b: PeerConfig,
    b: impl FnOnce(Peer) -> B,
) -> (Peer, Peer) {
    let (left, right) = duplex(256 * 1024);
    let (lr, lw) = split(left);
    let (rr, rw) = split(right);
    let a = Peer::spawn(lr, lw, PeerConfig::default(), |_| a);
    let b = Peer::spawn(rr, rw, config_b, b);
    (a, b)
}

/// A peer whose other end is driven by the test frame by frame.
fn with_raw_end(
    capacity: usize,
    config: PeerConfig,
    handler: impl Handler,
) -> (Peer, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) {
    let (left, right) = duplex(capacity);
    let (lr, lw) = split(left);
    let (rr, rw) = split(right);
    (Peer::spawn(lr, lw, config, |_| handler), rr, rw)
}

async fn eventually(what: &str, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Handler that never finishes and records when it is dropped (i.e. aborted).
fn hanging(dropped: Arc<AtomicBool>) -> Router {
    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    Router::new().on_request::<ExecuteCommand, _, _>(move |_| {
        let guard = SetOnDrop(dropped.clone());
        async move {
            let _guard = guard;
            std::future::pending::<Result<Value, ResponseError>>().await
        }
    })
}

#[tokio::test]
async fn typed_request_and_error_codes() {
    let (a, _b) = pair(Router::new(), PeerConfig::default(), |_| echo());
    let result = a
        .request::<ExecuteCommand>(&call(vec![json!(1), json!("x")]))
        .await
        .unwrap();
    assert_eq!(result, json!([1, "x"]));

    a.request::<Ping>(&()).await.unwrap();

    let unknown = a.request_raw("nope/method", Value::Null, None).await;
    assert_eq!(unknown.unwrap_err().code(), Some(ErrorCode::MethodNotFound));

    let invalid = a
        .request_raw("command/execute", json!({"command": "no-dot"}), None)
        .await;
    assert_eq!(invalid.unwrap_err().code(), Some(ErrorCode::InvalidParams));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_thousand_parallel_requests() {
    let (a, b) = pair(Router::new(), PeerConfig::default(), |_| echo());
    let started = Instant::now();
    let mut requests = JoinSet::new();
    for i in 0..10_000 {
        let a = a.clone();
        requests.spawn(async move {
            let result = a.request::<ExecuteCommand>(&call(vec![json!(i)])).await;
            (i, result)
        });
    }
    let mut done = 0;
    while let Some(joined) = requests.join_next().await {
        let (i, result) = joined.unwrap();
        assert_eq!(result.unwrap(), json!([i]));
        done += 1;
    }
    eprintln!("10 000 parallel requests: {:?}", started.elapsed());
    assert_eq!(done, 10_000);
    assert_eq!(a.pending_requests(), 0);
    eventually("handlers to finish", || b.requests_in_flight() == 0).await;
}

#[tokio::test]
async fn timeout_cancels_the_remote_handler() {
    let dropped = Arc::new(AtomicBool::new(false));
    let handler = hanging(dropped.clone());
    let (a, b) = pair(Router::new(), PeerConfig::default(), |_| handler);

    let result = a
        .request_timeout::<ExecuteCommand>(&call(vec![]), Some(Duration::from_millis(50)))
        .await;
    assert!(matches!(result, Err(RpcError::Timeout)), "{result:?}");
    assert_eq!(a.pending_requests(), 0);
    eventually("the handler to be aborted", || {
        dropped.load(Ordering::SeqCst)
    })
    .await;
    eventually("in-flight to drain", || b.requests_in_flight() == 0).await;
}

#[tokio::test]
async fn dropping_a_request_cancels_it() {
    let dropped = Arc::new(AtomicBool::new(false));
    let handler = hanging(dropped.clone());
    let (a, b) = pair(Router::new(), PeerConfig::default(), |_| handler);

    let task = tokio::spawn({
        let a = a.clone();
        async move { a.request::<ExecuteCommand>(&call(vec![])).await }
    });
    eventually("the request to arrive", || b.requests_in_flight() == 1).await;
    task.abort();
    eventually("the handler to be aborted", || {
        dropped.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(a.pending_requests(), 0);
}

#[tokio::test]
async fn cancel_request_answers_request_cancelled() {
    let dropped = Arc::new(AtomicBool::new(false));
    let handler = hanging(dropped.clone());
    let (a, b) = pair(Router::new(), PeerConfig::default(), |_| handler);

    let task = tokio::spawn({
        let a = a.clone();
        async move { a.request::<ExecuteCommand>(&call(vec![])).await }
    });
    eventually("the request to arrive", || b.requests_in_flight() == 1).await;
    // The first request of a fresh peer has id 1.
    a.notify::<CancelRequest>(&CancelParams {
        id: RequestId::Number(1),
    })
    .await
    .unwrap();
    let result = tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    assert_eq!(
        result.unwrap_err().code(),
        Some(ErrorCode::RequestCancelled)
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn requests_beyond_the_in_flight_limit_are_overloaded() {
    let release = Arc::new(Notify::new());
    let handler = Router::new().on_request::<ExecuteCommand, _, _>({
        let release = release.clone();
        move |_| {
            let release = release.clone();
            async move {
                release.notified().await;
                Ok(json!("released"))
            }
        }
    });
    let config = PeerConfig {
        max_in_flight: 1,
        ..PeerConfig::default()
    };
    let (a, b) = pair(Router::new(), config, |_| handler);

    let first = tokio::spawn({
        let a = a.clone();
        async move { a.request::<ExecuteCommand>(&call(vec![])).await }
    });
    eventually("the first request to arrive", || {
        b.requests_in_flight() == 1
    })
    .await;
    let second = a.request::<ExecuteCommand>(&call(vec![])).await;
    assert_eq!(second.unwrap_err().code(), Some(ErrorCode::Overloaded));

    release.notify_one();
    let first = tokio::time::timeout(WAIT, first).await.unwrap().unwrap();
    assert_eq!(first.unwrap(), json!("released"));
}

#[tokio::test]
async fn a_panicking_handler_is_an_internal_error() {
    let handler = Router::new().on_request::<ExecuteCommand, _, _>(|call| async move {
        if call.args.is_empty() {
            panic!("handler bug");
        }
        Ok(json!(call.args))
    });
    let (a, _b) = pair(Router::new(), PeerConfig::default(), |_| handler);
    let result = a.request::<ExecuteCommand>(&call(vec![])).await;
    assert_eq!(result.unwrap_err().code(), Some(ErrorCode::InternalError));
    let result = a.request::<ExecuteCommand>(&call(vec![json!(1)])).await;
    assert_eq!(result.unwrap(), json!([1]));
}

#[tokio::test]
async fn handlers_can_call_the_other_side() {
    let b_handler = |peer: Peer| {
        Router::new().on_request::<InvokeCommand, _, _>(move |invoke| {
            let peer = peer.clone();
            async move {
                // Ask the caller back while handling its request.
                let echoed = peer
                    .request::<ExecuteCommand>(&invoke)
                    .await
                    .map_err(|e| ResponseError::new(ErrorCode::InternalError, e.to_string()))?;
                Ok(json!({"echoed": echoed}))
            }
        })
    };
    let (a, _b) = pair(echo(), PeerConfig::default(), b_handler);
    let result = a
        .request::<InvokeCommand>(&call(vec![json!("ping-pong")]))
        .await
        .unwrap();
    assert_eq!(result, json!({"echoed": ["ping-pong"]}));
}

#[tokio::test]
async fn notifications_arrive_in_order() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = Router::new().on_notification::<Log, _>({
        let seen = seen.clone();
        move |log| seen.lock().unwrap().push(log.message)
    });
    let (a, _b) = pair(Router::new(), PeerConfig::default(), |_| handler);
    for i in 0..1000 {
        a.notify::<Log>(&LogParams {
            level: LogLevel::Info,
            message: i.to_string(),
            target: None,
        })
        .await
        .unwrap();
    }
    // The ping is read after every notification before it.
    a.request::<Ping>(&()).await.unwrap();
    let expected: Vec<String> = (0..1000).map(|i| i.to_string()).collect();
    assert_eq!(*seen.lock().unwrap(), expected);
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn checksum(sum: u64, data: &[u8]) -> u64 {
    data.chunks(8).fold(sum, |acc, chunk| {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        acc.rotate_left(5) ^ u64::from_le_bytes(word)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_of_100_megabytes() {
    const TOTAL: usize = 100 * 1024 * 1024;
    const CHUNK: usize = 256 * 1024;

    let (report_tx, mut report_rx) = mpsc::unbounded_channel();
    let handler = Router::new().on_stream(move |mut reader: StreamReader| {
        let report = report_tx.clone();
        tokio::spawn(async move {
            let (mut len, mut sum) = (0usize, 0u64);
            while let Some(chunk) = reader.recv().await.unwrap() {
                // Checksum over whole words: every chunk the sender writes is a multiple of 8.
                len += chunk.len();
                sum = checksum(sum, &chunk);
            }
            let _ = report.send((reader.id(), len, sum));
        });
    });
    let (a, _b) = pair(Router::new(), PeerConfig::default(), |_| handler);

    let started = Instant::now();
    let mut writer = a.open_stream();
    let id = writer.id();
    let (mut state, mut sum) = (0x9E37_79B9_7F4A_7C15u64, 0u64);
    let mut buffer = vec![0u8; CHUNK];
    for _ in 0..TOTAL / CHUNK {
        for word in buffer.chunks_mut(8) {
            word.copy_from_slice(&xorshift(&mut state).to_le_bytes());
        }
        sum = checksum(sum, &buffer);
        writer.write(&buffer).await.unwrap();
    }
    writer.close().await.unwrap();

    let (got_id, len, got_sum) = tokio::time::timeout(Duration::from_secs(60), report_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let elapsed = started.elapsed();
    eprintln!(
        "100 MB stream: {elapsed:?} ({:.0} MB/s)",
        100.0 / elapsed.as_secs_f64()
    );
    assert_eq!(got_id, id);
    assert_eq!(len, TOTAL);
    assert_eq!(got_sum, sum);
}

#[tokio::test]
async fn dropped_connection_fails_everything_without_panics() {
    let (streams_tx, mut streams_rx) = mpsc::unbounded_channel();
    let handler = Router::new().on_stream(move |reader| {
        let _ = streams_tx.send(reader);
    });
    let (a, mut raw_read, mut raw_write) = with_raw_end(64 * 1024, PeerConfig::default(), handler);

    // An incoming stream with one chunk and no close frame.
    Frame::StreamData {
        stream: StreamId(7),
        data: b"partial".to_vec(),
    }
    .write(&mut raw_write)
    .await
    .unwrap();
    raw_write.flush().await.unwrap();
    let mut incoming = streams_rx.recv().await.unwrap();
    assert_eq!(incoming.recv().await.unwrap().unwrap(), b"partial");

    let mut pending = JoinSet::new();
    for i in 0..3 {
        let a = a.clone();
        pending.spawn(async move { a.request::<ExecuteCommand>(&call(vec![json!(i)])).await });
    }
    for _ in 0..3 {
        let frame = Frame::read(&mut raw_read).await.unwrap().unwrap();
        assert!(matches!(frame, Frame::Message(_)));
    }
    let mut outgoing = a.open_stream();

    drop(raw_read);
    drop(raw_write);

    while let Some(result) = pending.join_next().await {
        assert!(matches!(result.unwrap(), Err(RpcError::ConnectionClosed)));
    }
    tokio::time::timeout(WAIT, a.closed()).await.unwrap();
    assert!(a.is_closed());
    assert!(matches!(
        incoming.recv().await,
        Err(RpcError::ConnectionClosed)
    ));
    assert!(matches!(
        a.request::<Ping>(&()).await,
        Err(RpcError::ConnectionClosed)
    ));
    assert!(matches!(
        outgoing.write(b"late").await,
        Err(RpcError::ConnectionClosed)
    ));
}

#[tokio::test]
async fn malformed_messages_get_errors_and_bad_frames_close() {
    let (a, mut raw_read, mut raw_write) = with_raw_end(64 * 1024, PeerConfig::default(), echo());

    Frame::Message(b"{not json".to_vec())
        .write(&mut raw_write)
        .await
        .unwrap();
    raw_write.flush().await.unwrap();
    let Some(Frame::Message(reply)) = Frame::read(&mut raw_read).await.unwrap() else {
        panic!("expected a message frame");
    };
    let Message::Response(reply) = Message::from_slice(&reply).unwrap() else {
        panic!("expected a response");
    };
    assert_eq!(reply.id, None);
    assert_eq!(
        reply.result.unwrap_err().error_code(),
        Some(ErrorCode::ParseError)
    );

    // Still serving after the bad message.
    let ping = json!({"jsonrpc": "2.0", "id": 5, "method": "$/ping"});
    Frame::Message(serde_json::to_vec(&ping).unwrap())
        .write(&mut raw_write)
        .await
        .unwrap();
    raw_write.flush().await.unwrap();
    let Some(Frame::Message(reply)) = Frame::read(&mut raw_read).await.unwrap() else {
        panic!("expected a message frame");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&reply).unwrap(),
        json!({"jsonrpc": "2.0", "id": 5, "result": null})
    );

    // A frame header over the size limit ends the connection.
    raw_write.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
    raw_write.flush().await.unwrap();
    tokio::time::timeout(WAIT, a.closed()).await.unwrap();
}

#[tokio::test]
async fn try_notify_reports_a_full_queue() {
    let config = PeerConfig {
        queue_capacity: 4,
        ..PeerConfig::default()
    };
    // Nobody reads the other end, so the transport and then the queue fill up.
    let (a, _raw_read, _raw_write) = with_raw_end(64, config, Router::new());
    let log = LogParams {
        level: LogLevel::Info,
        message: "x".repeat(100),
        target: None,
    };
    let mut overloaded = false;
    for _ in 0..100 {
        match a.try_notify::<Log>(&log) {
            Ok(()) => tokio::task::yield_now().await,
            Err(RpcError::Overloaded) => {
                overloaded = true;
                break;
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert!(overloaded, "the queue never filled up");
}
