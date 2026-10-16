//! JSON-RPC over the LSP transport: requests with ids, notifications, and requests the server
//! sends back (`workspace/applyEdit`, `workspace/configuration`, …). A request whose future is
//! dropped is cancelled with `$/cancelRequest`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

use kumiko_service_api::BoxFuture;

use crate::transport::{read_message, write_message};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LspError {
    #[error("the language server is not running")]
    Closed,
    #[error("language server error {code}: {message}")]
    Server { code: i64, message: String },
    #[error("unexpected answer from the language server: {0}")]
    Decode(String),
}

/// An error answer to a server request.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
}

impl ResponseError {
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const REQUEST_CANCELLED: i64 = -32800;

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: format!("unhandled method {method}"),
        }
    }
}

/// What the client does with messages the server starts.
pub trait ServerHandler: Send + Sync + 'static {
    fn notification(&self, method: &str, params: Value);
    fn request(
        &self,
        method: &str,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, ResponseError>>;
}

type Pending = HashMap<i64, oneshot::Sender<Result<Value, LspError>>>;

struct Shared {
    outgoing: mpsc::UnboundedSender<Value>,
    pending: Mutex<Pending>,
    next_id: AtomicI64,
}

fn lock(pending: &Mutex<Pending>) -> MutexGuard<'_, Pending> {
    pending.lock().unwrap_or_else(|e| e.into_inner())
}

/// A connection to one server.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
    closed: watch::Receiver<bool>,
}

impl Client {
    /// Starts reading and writing on the runtime. The connection closes when the server's
    /// output ends; pending requests then fail with [`LspError::Closed`].
    pub fn start(
        input: impl AsyncWrite + Send + Unpin + 'static,
        output: impl AsyncRead + Send + Unpin + 'static,
        handler: Arc<dyn ServerHandler>,
    ) -> Self {
        let (outgoing, mut queue) = mpsc::unbounded_channel::<Value>();
        let shared = Arc::new(Shared {
            outgoing,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
        });
        let (closed_tx, closed) = watch::channel(false);

        let mut input = input;
        tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                if let Err(error) = write_message(&mut input, &message).await {
                    tracing::debug!("language server input closed: {error}");
                    break;
                }
            }
        });

        let reader_shared = shared.clone();
        tokio::spawn(async move {
            let mut output = BufReader::new(output);
            loop {
                match read_message(&mut output).await {
                    Ok(Some(message)) => dispatch(&reader_shared, &handler, message),
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!("language server output: {error}");
                        break;
                    }
                }
            }
            for (_, waiter) in lock(&reader_shared.pending).drain() {
                let _ = waiter.send(Err(LspError::Closed));
            }
            let _ = closed_tx.send(true);
        });
        Self { shared, closed }
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// Resolves once the server's output has ended.
    pub async fn wait_closed(&self) {
        let mut closed = self.closed.clone();
        let _ = closed.wait_for(|c| *c).await;
    }

    /// Sends a request and waits for its result.
    pub async fn request<R: lsp_types::request::Request>(
        &self,
        params: R::Params,
    ) -> Result<R::Result, LspError> {
        let value = self.request_value(R::METHOD, &params).await?;
        decode(value)
    }

    /// An untyped request, e.g. one a plugin forwards with `lsp/sendRequest`.
    pub async fn request_value(
        &self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<Value, LspError> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.shared.pending).insert(id, tx);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.shared.outgoing.send(message).is_err() || self.is_closed() {
            lock(&self.shared.pending).remove(&id);
            return Err(LspError::Closed);
        }
        let mut guard = CancelOnDrop {
            shared: &self.shared,
            id: Some(id),
        };
        let result = rx.await.unwrap_or(Err(LspError::Closed));
        guard.id = None;
        result
    }

    pub fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        self.notify_value(N::METHOD, &params);
    }

    pub fn notify_value(&self, method: &str, params: &impl Serialize) {
        let _ = self
            .shared
            .outgoing
            .send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, LspError> {
    serde_json::from_value(value).map_err(|e| LspError::Decode(e.to_string()))
}

/// Tells the server a request is no longer wanted when its future is dropped early.
struct CancelOnDrop<'a> {
    shared: &'a Shared,
    id: Option<i64>,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id
            && lock(&self.shared.pending).remove(&id).is_some()
        {
            let _ = self
                .shared
                .outgoing
                .send(json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": id}}));
        }
    }
}

fn dispatch(shared: &Arc<Shared>, handler: &Arc<dyn ServerHandler>, message: Value) {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let id = message.get("id").cloned().filter(|id| !id.is_null());
    match (method, id) {
        // A response to one of ours.
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else {
                return;
            };
            let Some(waiter) = lock(&shared.pending).remove(&id) else {
                return; // cancelled meanwhile
            };
            let result = match message.get("error") {
                Some(error) => Err(LspError::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = waiter.send(result);
        }
        // A request from the server: answered asynchronously, in any order.
        (Some(method), Some(id)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let answer = handler.request(&method, params);
            let outgoing = shared.outgoing.clone();
            tokio::spawn(async move {
                let response = match answer.await {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(error) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": error.code, "message": error.message},
                    }),
                };
                let _ = outgoing.send(response);
            });
        }
        (Some(method), None) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            handler.notification(&method, params);
        }
        (None, None) => tracing::debug!("language server sent a message without method or id"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lsp_types::request::{HoverRequest, Request};
    use tokio::io::{DuplexStream, duplex};

    use super::*;

    struct Recorder(Mutex<Vec<String>>);

    impl ServerHandler for Recorder {
        fn notification(&self, method: &str, _params: Value) {
            self.0.lock().unwrap().push(method.to_owned());
        }
        fn request(
            &self,
            method: &str,
            params: Value,
        ) -> BoxFuture<'static, Result<Value, ResponseError>> {
            let method = method.to_owned();
            Box::pin(async move {
                if method == "echo" {
                    Ok(params)
                } else {
                    Err(ResponseError::method_not_found(&method))
                }
            })
        }
    }

    /// A client wired to a scripted peer: the test reads what the client wrote and writes
    /// what the "server" says.
    fn connect() -> (Client, Arc<Recorder>, BufReader<DuplexStream>, DuplexStream) {
        let (client_in, server_reads) = duplex(1 << 16);
        let (server_writes, client_out) = duplex(1 << 16);
        let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
        let client = Client::start(client_in, client_out, recorder.clone());
        (
            client,
            recorder,
            BufReader::new(server_reads),
            server_writes,
        )
    }

    #[tokio::test]
    async fn requests_get_their_answers_and_errors() {
        let (client, _, mut server_in, mut server_out) = connect();
        let call = tokio::spawn({
            let client = client.clone();
            async move { client.request_value("a/b", &json!({"x": 1})).await }
        });
        let request = read_message(&mut server_in).await.unwrap().unwrap();
        assert_eq!(request["method"], "a/b");
        let id = request["id"].clone();
        write_message(
            &mut server_out,
            &json!({"jsonrpc": "2.0", "id": id, "result": [1, 2]}),
        )
        .await
        .unwrap();
        assert_eq!(call.await.unwrap(), Ok(json!([1, 2])));

        let call = tokio::spawn({
            let client = client.clone();
            async move { client.request_value("a/c", &Value::Null).await }
        });
        let request = read_message(&mut server_in).await.unwrap().unwrap();
        write_message(
            &mut server_out,
            &json!({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32603, "message": "boom"}}),
        )
        .await
        .unwrap();
        assert_eq!(
            call.await.unwrap(),
            Err(LspError::Server {
                code: -32603,
                message: "boom".into()
            })
        );
    }

    #[tokio::test]
    async fn server_messages_reach_the_handler() {
        let (_client, recorder, mut server_in, mut server_out) = connect();
        write_message(
            &mut server_out,
            &json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {}}),
        )
        .await
        .unwrap();
        write_message(
            &mut server_out,
            &json!({"jsonrpc": "2.0", "id": "s1", "method": "echo", "params": {"v": 7}}),
        )
        .await
        .unwrap();
        write_message(
            &mut server_out,
            &json!({"jsonrpc": "2.0", "id": 9, "method": "nope"}),
        )
        .await
        .unwrap();
        let answers = [
            read_message(&mut server_in).await.unwrap().unwrap(),
            read_message(&mut server_in).await.unwrap().unwrap(),
        ];
        let answer = |id: Value| answers.iter().find(|a| a["id"] == id).unwrap();
        assert_eq!(
            answer(json!(9))["error"]["code"],
            ResponseError::METHOD_NOT_FOUND
        );
        assert_eq!(
            *answer(json!("s1")),
            json!({"jsonrpc": "2.0", "id": "s1", "result": {"v": 7}})
        );
        assert_eq!(*recorder.0.lock().unwrap(), ["window/logMessage"]);
    }

    #[tokio::test]
    async fn dropped_requests_are_cancelled_and_closing_fails_the_rest() {
        let (client, _, mut server_in, server_out) = connect();
        let hover = client.request::<HoverRequest>(lsp_types::HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: "file:///a".parse().unwrap(),
                },
                position: lsp_types::Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
        });
        let _ = tokio::time::timeout(Duration::from_millis(20), hover).await;
        let request = read_message(&mut server_in).await.unwrap().unwrap();
        assert_eq!(request["method"], HoverRequest::METHOD);
        let cancel = read_message(&mut server_in).await.unwrap().unwrap();
        assert_eq!(cancel["method"], "$/cancelRequest");
        assert_eq!(cancel["params"]["id"], request["id"]);

        let pending = tokio::spawn({
            let client = client.clone();
            async move { client.request_value("slow", &Value::Null).await }
        });
        let _ = read_message(&mut server_in).await.unwrap();
        drop(server_out);
        assert_eq!(pending.await.unwrap(), Err(LspError::Closed));
        client.wait_closed().await;
        assert!(client.is_closed());
        assert_eq!(
            client.request_value("late", &Value::Null).await,
            Err(LspError::Closed)
        );
    }
}
