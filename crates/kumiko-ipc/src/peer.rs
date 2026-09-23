//! Symmetric JSON-RPC peer over a frame transport.
//!
//! Each side can send requests and notifications and handle the other side's. Two tasks own
//! the transport: a reader that dispatches frames and a writer that drains two queues:
//!
//! - the **control** queue (unbounded): responses and `$/cancelRequest`. It never blocks, so a
//!   handler can always answer and a dropped request can always cancel itself;
//! - the **outgoing** queue (bounded, `queue_capacity`): requests, notifications and stream
//!   frames in send order. A full queue makes senders wait, `try_notify` fails with
//!   `Overloaded` instead.
//!
//! The writer prefers the control queue, so a response may overtake notifications that its
//! handler sent earlier.
//!
//! Incoming requests run as separate tasks, at most `max_in_flight` at a time; beyond that
//! the peer answers `Overloaded` right away. `$/cancelRequest` aborts the handler task and
//! answers `RequestCancelled`. A caller that drops a pending request (or hits its timeout)
//! sends `$/cancelRequest` automatically. `$/ping` is answered by the peer itself.
//!
//! When the connection ends (EOF, I/O error, malformed frame or [`Peer::close`]) all pending
//! requests fail with [`RpcError::ConnectionClosed`], running handlers are aborted and
//! incoming streams end with an error. Nothing panics.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufWriter};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::AbortHandle;

use kumiko_protocol::common::StreamId;
use kumiko_protocol::methods::base::{CancelParams, CancelRequest, Ping};
use kumiko_protocol::{
    ErrorCode, Message, MessageError, Notification, NotificationMessage, Request, RequestId,
    RequestMessage, ResponseError, ResponseMessage,
};

use crate::frame::{Frame, MAX_MESSAGE_LEN};
use crate::stream::{StreamReader, StreamWriter};

/// Result of an incoming request handler.
pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, ResponseError>> + Send>>;

/// Handles messages from the other side.
pub trait Handler: Send + Sync + 'static {
    /// Handle a request; runs as its own task and may be aborted by `$/cancelRequest`.
    fn request(&self, method: String, params: Value) -> HandlerFuture;

    /// Handle a notification. Called on the reader task in arrival order, so it must not block.
    fn notification(&self, method: String, params: Value) {
        let _ = (method, params);
    }

    /// A new incoming stream. Dropping the reader discards the stream's data.
    fn stream(&self, reader: StreamReader) {
        drop(reader);
    }
}

#[derive(Debug, Clone)]
pub struct PeerConfig {
    /// Capacity of the outgoing queue, in frames.
    pub queue_capacity: usize,
    /// Incoming requests handled concurrently; more are answered with `Overloaded`.
    pub max_in_flight: usize,
    /// Timeout of outgoing requests that do not specify one; `None` waits forever.
    pub default_timeout: Option<Duration>,
    /// Chunks buffered per incoming stream before the reader waits for the consumer.
    pub stream_buffer: usize,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 1024,
            max_in_flight: 1024,
            default_timeout: None,
            stream_buffer: 16,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The other side answered with an error.
    #[error("{0}")]
    Remote(ResponseError),
    #[error("request timed out")]
    Timeout,
    #[error("connection closed")]
    ConnectionClosed,
    /// The outgoing queue is full (only from non-waiting sends).
    #[error("outgoing queue is full")]
    Overloaded,
    #[error("message of {0} bytes does not fit into a frame")]
    TooLarge(usize),
    #[error(transparent)]
    Message(#[from] MessageError),
}

impl RpcError {
    /// Error code of a remote error.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Remote(error) => error.error_code(),
            _ => None,
        }
    }
}

type PendingResult = Result<Value, ResponseError>;

struct Shared {
    config: PeerConfig,
    out: mpsc::Sender<Frame>,
    control: mpsc::UnboundedSender<Frame>,
    pending: Mutex<HashMap<RequestId, oneshot::Sender<PendingResult>>>,
    incoming: Mutex<HashMap<RequestId, AbortHandle>>,
    streams: Mutex<HashMap<StreamId, IncomingStream>>,
    in_flight: Arc<Semaphore>,
    next_request: AtomicI64,
    next_stream: AtomicU32,
    closed: watch::Sender<bool>,
}

struct IncomingStream {
    data: mpsc::Sender<Vec<u8>>,
    closed_by_sender: Arc<AtomicBool>,
}

/// Recovers the data of a poisoned lock: every critical section leaves the maps consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    fn send_control(&self, message: Message) {
        let bytes = message.to_vec();
        let _ = self.control.send(Frame::Message(bytes));
    }

    fn respond(&self, id: Option<RequestId>, result: PendingResult) {
        let mut bytes = Message::Response(ResponseMessage {
            id: id.clone(),
            result,
        })
        .to_vec();
        if bytes.len() > MAX_MESSAGE_LEN {
            bytes = Message::Response(ResponseMessage::error(
                id,
                ResponseError::new(ErrorCode::InternalError, "result does not fit into a frame"),
            ))
            .to_vec();
        }
        let _ = self.control.send(Frame::Message(bytes));
    }

    /// Ends the connection; idempotent.
    fn shutdown(&self) {
        if self.closed.send_replace(true) {
            return;
        }
        lock(&self.pending).clear();
        for (_, handle) in lock(&self.incoming).drain() {
            handle.abort();
        }
        lock(&self.streams).clear();
    }
}

/// Handle to a connection; cheap to clone.
#[derive(Clone)]
pub struct Peer {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("closed", &self.shared.is_closed())
            .finish_non_exhaustive()
    }
}

impl Peer {
    /// Starts the reader and writer tasks on the current Tokio runtime. `make_handler` gets
    /// a handle to the new peer, so the handler can call the other side.
    pub fn spawn<R, W, H>(
        reader: R,
        writer: W,
        config: PeerConfig,
        make_handler: impl FnOnce(Peer) -> H,
    ) -> Peer
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
        H: Handler,
    {
        let (out_tx, out_rx) = mpsc::channel(config.queue_capacity.max(1));
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (closed, _) = watch::channel(false);
        let shared = Arc::new(Shared {
            in_flight: Arc::new(Semaphore::new(config.max_in_flight)),
            config,
            out: out_tx,
            control: control_tx,
            pending: Mutex::default(),
            incoming: Mutex::default(),
            streams: Mutex::default(),
            next_request: AtomicI64::new(1),
            next_stream: AtomicU32::new(1),
            closed,
        });
        let peer = Peer {
            shared: shared.clone(),
        };
        let handler: Arc<dyn Handler> = Arc::new(make_handler(peer.clone()));
        tokio::spawn(write_loop(shared.clone(), writer, out_rx, control_rx));
        tokio::spawn(read_loop(shared, reader, handler));
        peer
    }

    /// Sends a typed request and waits for its result (with the default timeout).
    pub async fn request<R: Request>(&self, params: &R::Params) -> Result<R::Result, RpcError> {
        self.request_timeout::<R>(params, self.shared.config.default_timeout)
            .await
    }

    /// Sends a typed request with an explicit timeout (`None` waits forever).
    pub async fn request_timeout<R: Request>(
        &self,
        params: &R::Params,
        timeout: Option<Duration>,
    ) -> Result<R::Result, RpcError> {
        let params = serde_json::to_value(params).map_err(|source| MessageError::Payload {
            method: R::METHOD,
            source,
        })?;
        let result = self.call(R::METHOD, params, timeout).await?;
        serde_json::from_value(result).map_err(|source| {
            RpcError::Message(MessageError::Payload {
                method: R::METHOD,
                source,
            })
        })
    }

    /// Sends a request by method name (e.g. when bridging WASM `call`).
    pub async fn request_raw(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RpcError> {
        self.call(method, params, timeout).await
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RpcError> {
        if self.shared.is_closed() {
            return Err(RpcError::ConnectionClosed);
        }
        let id = RequestId::Number(self.shared.next_request.fetch_add(1, Ordering::Relaxed));
        let bytes = Message::Request(RequestMessage {
            id: id.clone(),
            method: method.to_owned(),
            params,
        })
        .to_vec();
        if bytes.len() > MAX_MESSAGE_LEN {
            return Err(RpcError::TooLarge(bytes.len()));
        }

        let (tx, rx) = oneshot::channel();
        lock(&self.shared.pending).insert(id.clone(), tx);
        let mut guard = PendingGuard {
            shared: &self.shared,
            id,
            sent: false,
            done: false,
        };

        let exchange = async {
            self.shared
                .out
                .send(Frame::Message(bytes))
                .await
                .map_err(|_| RpcError::ConnectionClosed)?;
            guard.sent = true;
            rx.await.map_err(|_| RpcError::ConnectionClosed)
        };
        let result = match timeout {
            Some(limit) => tokio::time::timeout(limit, exchange)
                .await
                .map_err(|_| RpcError::Timeout)?,
            None => exchange.await,
        };
        guard.done = true;
        result?.map_err(RpcError::Remote)
    }

    /// Sends a typed notification, waiting while the outgoing queue is full.
    pub async fn notify<N: Notification>(&self, params: &N::Params) -> Result<(), RpcError> {
        let frame = notification_frame(NotificationMessage::new::<N>(params)?)?;
        self.shared
            .out
            .send(frame)
            .await
            .map_err(|_| RpcError::ConnectionClosed)
    }

    /// Sends a typed notification or fails with `Overloaded` if the queue is full.
    pub fn try_notify<N: Notification>(&self, params: &N::Params) -> Result<(), RpcError> {
        let frame = notification_frame(NotificationMessage::new::<N>(params)?)?;
        self.shared
            .out
            .try_send(frame)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RpcError::Overloaded,
                mpsc::error::TrySendError::Closed(_) => RpcError::ConnectionClosed,
            })
    }

    /// Sends a notification by method name.
    pub async fn notify_raw(&self, method: &str, params: Value) -> Result<(), RpcError> {
        let frame = notification_frame(NotificationMessage {
            method: method.to_owned(),
            params,
        })?;
        self.shared
            .out
            .send(frame)
            .await
            .map_err(|_| RpcError::ConnectionClosed)
    }

    /// Opens an outgoing stream; announce its id to the other side in some message.
    pub fn open_stream(&self) -> StreamWriter {
        let id = StreamId(self.shared.next_stream.fetch_add(1, Ordering::Relaxed));
        StreamWriter::new(id, self.shared.out.clone())
    }

    /// Ends the connection: pending requests fail, handlers are aborted.
    pub fn close(&self) {
        self.shared.shutdown();
    }

    pub fn is_closed(&self) -> bool {
        self.shared.is_closed()
    }

    /// Resolves when the connection has ended for any reason.
    pub async fn closed(&self) {
        let mut closed = self.shared.closed.subscribe();
        let _ = closed.wait_for(|closed| *closed).await;
    }

    /// Number of outgoing requests waiting for a response.
    pub fn pending_requests(&self) -> usize {
        lock(&self.shared.pending).len()
    }

    /// Number of incoming requests being handled.
    pub fn requests_in_flight(&self) -> usize {
        lock(&self.shared.incoming).len()
    }
}

fn notification_frame(notification: NotificationMessage) -> Result<Frame, RpcError> {
    let bytes = Message::Notification(notification).to_vec();
    if bytes.len() > MAX_MESSAGE_LEN {
        return Err(RpcError::TooLarge(bytes.len()));
    }
    Ok(Frame::Message(bytes))
}

/// Removes a pending request when its caller stops waiting and cancels it on the other side
/// if it was already sent.
struct PendingGuard<'a> {
    shared: &'a Shared,
    id: RequestId,
    sent: bool,
    done: bool,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        lock(&self.shared.pending).remove(&self.id);
        if !self.sent || self.done || self.shared.is_closed() {
            return;
        }
        let params = CancelParams {
            id: self.id.clone(),
        };
        if let Ok(cancel) = NotificationMessage::new::<CancelRequest>(&params) {
            self.shared.send_control(Message::Notification(cancel));
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    shared: Arc<Shared>,
    writer: W,
    mut out: mpsc::Receiver<Frame>,
    mut control: mpsc::UnboundedReceiver<Frame>,
) {
    let mut writer = BufWriter::new(writer);
    let mut closed = shared.closed.subscribe();
    loop {
        let frame = tokio::select! {
            biased;
            _ = closed.wait_for(|closed| *closed) => break,
            Some(frame) = control.recv() => frame,
            Some(frame) = out.recv() => frame,
            else => break,
        };
        if let Err(error) = frame.write(&mut writer).await {
            tracing::warn!(%error, "writing a frame failed; closing the connection");
            break;
        }
        if control.is_empty() && out.is_empty() && writer.flush().await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
    shared.shutdown();
}

async fn read_loop<R: AsyncRead + Unpin>(
    shared: Arc<Shared>,
    mut reader: R,
    handler: Arc<dyn Handler>,
) {
    let mut closed = shared.closed.subscribe();
    loop {
        let frame = tokio::select! {
            biased;
            _ = closed.wait_for(|closed| *closed) => break,
            frame = Frame::read(&mut reader) => frame,
        };
        match frame {
            Ok(Some(frame)) => dispatch(&shared, &handler, frame).await,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "reading a frame failed; closing the connection");
                break;
            }
        }
    }
    shared.shutdown();
}

async fn dispatch(shared: &Arc<Shared>, handler: &Arc<dyn Handler>, frame: Frame) {
    match frame {
        Frame::Message(bytes) => match Message::from_slice(&bytes) {
            Ok(Message::Request(request)) => handle_request(shared, handler, request),
            Ok(Message::Notification(notification)) => {
                handle_notification(shared, handler, notification)
            }
            Ok(Message::Response(response)) => handle_response(shared, response),
            Err(error) => {
                tracing::warn!(%error, "malformed message");
                shared.respond(None, Err(error.to_response_error()));
            }
        },
        Frame::StreamData { stream, data } => {
            let sink = {
                let mut streams = lock(&shared.streams);
                match streams.get(&stream) {
                    Some(incoming) => incoming.data.clone(),
                    None => {
                        let (tx, rx) = mpsc::channel(shared.config.stream_buffer.max(1));
                        let closed_by_sender = Arc::new(AtomicBool::new(false));
                        streams.insert(
                            stream,
                            IncomingStream {
                                data: tx.clone(),
                                closed_by_sender: closed_by_sender.clone(),
                            },
                        );
                        drop(streams);
                        handler.stream(StreamReader::new(stream, rx, closed_by_sender));
                        tx
                    }
                }
            };
            // Waiting here applies backpressure to the whole connection, until it closes.
            let mut closed = shared.closed.subscribe();
            tokio::select! {
                sent = sink.send(data) => if sent.is_err() {
                    // The consumer is gone; later chunks of this stream are dropped quietly.
                    tracing::debug!(stream = stream.0, "dropping data of an abandoned stream");
                },
                _ = closed.wait_for(|closed| *closed) => {}
            }
        }
        Frame::StreamClose { stream } => {
            let incoming = lock(&shared.streams).remove(&stream);
            match incoming {
                Some(incoming) => incoming.closed_by_sender.store(true, Ordering::Release),
                None => {
                    // A stream closed before any data: deliver it as empty.
                    let (_tx, rx) = mpsc::channel(1);
                    handler.stream(StreamReader::new(
                        stream,
                        rx,
                        Arc::new(AtomicBool::new(true)),
                    ));
                }
            }
        }
    }
}

fn handle_request(shared: &Arc<Shared>, handler: &Arc<dyn Handler>, request: RequestMessage) {
    let RequestMessage { id, method, params } = request;
    if method == Ping::METHOD {
        shared.respond(Some(id), Ok(Value::Null));
        return;
    }
    let Ok(permit) = shared.in_flight.clone().try_acquire_owned() else {
        shared.respond(
            Some(id),
            Err(ResponseError::new(
                ErrorCode::Overloaded,
                "too many requests in flight",
            )),
        );
        return;
    };

    let mut incoming = lock(&shared.incoming);
    if incoming.contains_key(&id) {
        drop(incoming);
        shared.respond(
            Some(id),
            Err(ResponseError::new(
                ErrorCode::InvalidRequest,
                "request id is already in use",
            )),
        );
        return;
    }
    let task = tokio::spawn(handler.request(method, params));
    incoming.insert(id.clone(), task.abort_handle());
    drop(incoming);

    let shared = shared.clone();
    tokio::spawn(async move {
        let outcome = task.await;
        drop(permit);
        lock(&shared.incoming).remove(&id);
        let result = match outcome {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Err(ResponseError::new(
                ErrorCode::RequestCancelled,
                "request cancelled",
            )),
            Err(_) => Err(ResponseError::new(
                ErrorCode::InternalError,
                "request handler panicked",
            )),
        };
        shared.respond(Some(id), result);
    });
}

fn handle_notification(
    shared: &Arc<Shared>,
    handler: &Arc<dyn Handler>,
    notification: NotificationMessage,
) {
    if let Some(cancel) = notification.params::<CancelRequest>() {
        match cancel {
            Ok(CancelParams { id }) => {
                if let Some(handle) = lock(&shared.incoming).get(&id) {
                    handle.abort();
                }
            }
            Err(error) => tracing::warn!(%error, "malformed $/cancelRequest"),
        }
        return;
    }
    handler.notification(notification.method, notification.params);
}

fn handle_response(shared: &Arc<Shared>, response: ResponseMessage) {
    let Some(id) = response.id else {
        if let Err(error) = response.result {
            tracing::warn!(%error, "the other side could not read a message");
        }
        return;
    };
    match lock(&shared.pending).remove(&id) {
        Some(waiter) => {
            let _ = waiter.send(response.result);
        }
        None => tracing::debug!(%id, "response to an unknown or abandoned request"),
    }
}
