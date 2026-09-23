//! Typed dispatch of incoming messages by method name.

use std::collections::HashMap;
use std::future::Future;

use serde_json::Value;

use kumiko_protocol::{ErrorCode, Notification, Request, ResponseError};

use crate::peer::{Handler, HandlerFuture};
use crate::stream::StreamReader;

type RequestFn = Box<dyn Fn(Value) -> HandlerFuture + Send + Sync>;
type NotificationFn = Box<dyn Fn(Value) + Send + Sync>;
type StreamFn = Box<dyn Fn(StreamReader) + Send + Sync>;

/// A [`Handler`] built from typed closures. Unknown requests get `MethodNotFound`,
/// requests with malformed params get `InvalidParams`, unknown notifications are ignored.
#[derive(Default)]
pub struct Router {
    requests: HashMap<&'static str, RequestFn>,
    notifications: HashMap<&'static str, NotificationFn>,
    streams: Option<StreamFn>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_request<R, F, Fut>(mut self, handler: F) -> Self
    where
        R: Request,
        F: Fn(R::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R::Result, ResponseError>> + Send + 'static,
    {
        let boxed: RequestFn =
            Box::new(
                move |params| match serde_json::from_value::<R::Params>(params) {
                    Ok(params) => {
                        let result = handler(params);
                        Box::pin(async move {
                            let result = result.await?;
                            serde_json::to_value(result).map_err(|error| {
                                ResponseError::new(ErrorCode::InternalError, error.to_string())
                            })
                        })
                    }
                    Err(error) => {
                        let error = ResponseError::new(
                            ErrorCode::InvalidParams,
                            format!("invalid params for `{}`: {error}", R::METHOD),
                        );
                        Box::pin(std::future::ready(Err(error)))
                    }
                },
            );
        self.requests.insert(R::METHOD, boxed);
        self
    }

    pub fn on_notification<N, F>(mut self, handler: F) -> Self
    where
        N: Notification,
        F: Fn(N::Params) + Send + Sync + 'static,
    {
        let boxed: NotificationFn =
            Box::new(
                move |params| match serde_json::from_value::<N::Params>(params) {
                    Ok(params) => handler(params),
                    Err(error) => {
                        tracing::warn!(%error, method = N::METHOD, "invalid notification")
                    }
                },
            );
        self.notifications.insert(N::METHOD, boxed);
        self
    }

    pub fn on_stream(mut self, handler: impl Fn(StreamReader) + Send + Sync + 'static) -> Self {
        self.streams = Some(Box::new(handler));
        self
    }
}

impl Handler for Router {
    fn request(&self, method: String, params: Value) -> HandlerFuture {
        match self.requests.get(method.as_str()) {
            Some(handler) => handler(params),
            None => Box::pin(std::future::ready(Err(ResponseError::new(
                ErrorCode::MethodNotFound,
                format!("unknown method `{method}`"),
            )))),
        }
    }

    fn notification(&self, method: String, params: Value) {
        match self.notifications.get(method.as_str()) {
            Some(handler) => handler(params),
            None => tracing::debug!(%method, "ignoring an unknown notification"),
        }
    }

    fn stream(&self, reader: StreamReader) {
        if let Some(handler) = &self.streams {
            handler(reader);
        }
    }
}
