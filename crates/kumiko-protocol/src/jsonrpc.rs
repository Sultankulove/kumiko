//! JSON-RPC 2.0 envelope and KPP error codes.

use std::fmt;

use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::{Notification, Request};

pub const JSONRPC_VERSION: &str = "2.0";

/// Request id: a number or a string, as in JSON-RPC 2.0.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::String(s) => write!(f, "{s:?}"),
        }
    }
}

/// Standard JSON-RPC codes and the fixed KPP extension set (spec, section 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,
    /// The request was cancelled with `$/cancelRequest`.
    RequestCancelled = -32800,
    /// The document changed and the request or edit is no longer valid for its version.
    ContentModified = -32801,
    /// The receiver's queue is full.
    Overloaded = -32802,
    /// The plugin lacks the permission the method requires.
    PermissionDenied = -32803,
    /// The target (document, command, interface, provider, ...) does not exist.
    NotFound = -32804,
    /// The target plugin is not running and could not be started.
    PluginNotRunning = -32805,
    /// The peer speaks an incompatible major protocol version.
    ApiVersionMismatch = -32806,
    /// No provider of the interface matches the requested version range.
    InterfaceVersionMismatch = -32807,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 13] = [
        Self::ParseError,
        Self::InvalidRequest,
        Self::MethodNotFound,
        Self::InvalidParams,
        Self::InternalError,
        Self::RequestCancelled,
        Self::ContentModified,
        Self::Overloaded,
        Self::PermissionDenied,
        Self::NotFound,
        Self::PluginNotRunning,
        Self::ApiVersionMismatch,
        Self::InterfaceVersionMismatch,
    ];

    pub const fn code(self) -> i32 {
        self as i32
    }

    pub fn from_code(code: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.code() == code)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::ParseError => "ParseError",
            Self::InvalidRequest => "InvalidRequest",
            Self::MethodNotFound => "MethodNotFound",
            Self::InvalidParams => "InvalidParams",
            Self::InternalError => "InternalError",
            Self::RequestCancelled => "RequestCancelled",
            Self::ContentModified => "ContentModified",
            Self::Overloaded => "Overloaded",
            Self::PermissionDenied => "PermissionDenied",
            Self::NotFound => "NotFound",
            Self::PluginNotRunning => "PluginNotRunning",
            Self::ApiVersionMismatch => "ApiVersionMismatch",
            Self::InterfaceVersionMismatch => "InterfaceVersionMismatch",
        }
    }
}

/// Error object of a failed response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResponseError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ResponseError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code(),
            message: message.into(),
            data: None,
        }
    }

    /// The known code, if the peer used one of the catalog codes.
    pub fn error_code(&self) -> Option<ErrorCode> {
        ErrorCode::from_code(self.code)
    }
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.error_code() {
            Some(code) => write!(f, "{} ({}): {}", code.name(), self.code, self.message),
            None => write!(f, "error {}: {}", self.code, self.message),
        }
    }
}

impl std::error::Error for ResponseError {}

/// Envelope-level failure: malformed JSON-RPC or params that do not match the method.
#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    #[error("invalid JSON-RPC message: {0}")]
    Invalid(&'static str),
    #[error("params or result do not match `{method}`: {source}")]
    Payload {
        method: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl MessageError {
    /// The response error a receiver sends back for this failure.
    pub fn to_response_error(&self) -> ResponseError {
        let code = match self {
            Self::Invalid(_) => ErrorCode::InvalidRequest,
            Self::Payload { .. } => ErrorCode::InvalidParams,
            Self::Json(_) => ErrorCode::ParseError,
        };
        ResponseError::new(code, self.to_string())
    }
}

/// A request. `params` is `Value::Null` when the method takes no parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestMessage {
    pub id: RequestId,
    pub method: String,
    pub params: Value,
}

impl RequestMessage {
    pub fn new<R: Request>(id: RequestId, params: &R::Params) -> Result<Self, MessageError> {
        Ok(Self {
            id,
            method: R::METHOD.to_owned(),
            params: to_payload(R::METHOD, params)?,
        })
    }

    /// Typed params, if this request is `R`.
    pub fn params<R: Request>(&self) -> Option<Result<R::Params, MessageError>> {
        (self.method == R::METHOD).then(|| from_payload(R::METHOD, &self.params))
    }
}

/// A notification. `params` is `Value::Null` when the method takes no parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct NotificationMessage {
    pub method: String,
    pub params: Value,
}

impl NotificationMessage {
    pub fn new<N: Notification>(params: &N::Params) -> Result<Self, MessageError> {
        Ok(Self {
            method: N::METHOD.to_owned(),
            params: to_payload(N::METHOD, params)?,
        })
    }

    /// Typed params, if this notification is `N`.
    pub fn params<N: Notification>(&self) -> Option<Result<N::Params, MessageError>> {
        (self.method == N::METHOD).then(|| from_payload(N::METHOD, &self.params))
    }
}

/// A response. `id` is `null` only for errors about a request whose id could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseMessage {
    pub id: Option<RequestId>,
    pub result: Result<Value, ResponseError>,
}

impl ResponseMessage {
    pub fn ok<R: Request>(id: RequestId, result: &R::Result) -> Result<Self, MessageError> {
        Ok(Self {
            id: Some(id),
            result: Ok(to_payload(R::METHOD, result)?),
        })
    }

    pub fn error(id: Option<RequestId>, error: ResponseError) -> Self {
        Self {
            id,
            result: Err(error),
        }
    }

    /// Typed result of request `R` (the caller knows which request the id belongs to).
    pub fn result<R: Request>(&self) -> Result<Result<R::Result, ResponseError>, MessageError> {
        match &self.result {
            Ok(value) => Ok(Ok(from_payload(R::METHOD, value)?)),
            Err(error) => Ok(Err(error.clone())),
        }
    }
}

/// Any JSON-RPC 2.0 message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(RequestMessage),
    Notification(NotificationMessage),
    Response(ResponseMessage),
}

impl Message {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, MessageError> {
        let value: Value = serde_json::from_slice(bytes)?;
        Self::from_value(value)
    }

    pub fn to_vec(&self) -> Vec<u8> {
        // Serializing a `Value` tree cannot fail: all map keys are strings.
        serde_json::to_vec(&self.to_value()).unwrap_or_default()
    }

    pub fn from_value(value: Value) -> Result<Self, MessageError> {
        let Value::Object(mut map) = value else {
            return Err(MessageError::Invalid("message is not an object"));
        };
        if map.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
            return Err(MessageError::Invalid("`jsonrpc` must be \"2.0\""));
        }
        let id = map.remove("id");
        let params = map.remove("params").unwrap_or(Value::Null);
        if let Some(method) = map.remove("method") {
            let Value::String(method) = method else {
                return Err(MessageError::Invalid("`method` must be a string"));
            };
            if !matches!(params, Value::Null | Value::Object(_) | Value::Array(_)) {
                return Err(MessageError::Invalid(
                    "`params` must be an object or an array",
                ));
            }
            return Ok(match id {
                None => Self::Notification(NotificationMessage { method, params }),
                Some(id) => Self::Request(RequestMessage {
                    id: serde_json::from_value(id)
                        .map_err(|_| MessageError::Invalid("`id` must be a number or a string"))?,
                    method,
                    params,
                }),
            });
        }
        let id = match id {
            None => {
                return Err(MessageError::Invalid(
                    "message has neither `method` nor `id`",
                ));
            }
            Some(Value::Null) => None,
            Some(id) => Some(
                serde_json::from_value(id)
                    .map_err(|_| MessageError::Invalid("`id` must be a number or a string"))?,
            ),
        };
        let result = match (map.remove("result"), map.remove("error")) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(serde_json::from_value(error)
                .map_err(|_| MessageError::Invalid("malformed `error` object"))?),
            _ => {
                return Err(MessageError::Invalid(
                    "response needs exactly one of `result` and `error`",
                ));
            }
        };
        Ok(Self::Response(ResponseMessage { id, result }))
    }

    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("jsonrpc".into(), JSONRPC_VERSION.into());
        match self {
            Self::Request(request) => {
                map.insert("id".into(), id_value(&request.id));
                map.insert("method".into(), request.method.clone().into());
                if !request.params.is_null() {
                    map.insert("params".into(), request.params.clone());
                }
            }
            Self::Notification(notification) => {
                map.insert("method".into(), notification.method.clone().into());
                if !notification.params.is_null() {
                    map.insert("params".into(), notification.params.clone());
                }
            }
            Self::Response(response) => {
                map.insert(
                    "id".into(),
                    response.id.as_ref().map_or(Value::Null, id_value),
                );
                match &response.result {
                    Ok(result) => map.insert("result".into(), result.clone()),
                    Err(error) => map.insert(
                        "error".into(),
                        serde_json::to_value(error).unwrap_or(Value::Null),
                    ),
                };
            }
        }
        Value::Object(map)
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

fn id_value(id: &RequestId) -> Value {
    match id {
        RequestId::Number(n) => (*n).into(),
        RequestId::String(s) => s.clone().into(),
    }
}

fn to_payload<T: Serialize>(method: &'static str, value: &T) -> Result<Value, MessageError> {
    serde_json::to_value(value).map_err(|source| MessageError::Payload { method, source })
}

fn from_payload<T: serde::de::DeserializeOwned>(
    method: &'static str,
    value: &Value,
) -> Result<T, MessageError> {
    T::deserialize(value).map_err(|source| MessageError::Payload { method, source })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::methods::base::Ping;
    use crate::methods::command::ExecuteCommand;

    fn parse(value: Value) -> Result<Message, MessageError> {
        Message::from_value(value)
    }

    #[test]
    fn request_notification_response_are_told_apart() {
        let request = parse(json!({"jsonrpc": "2.0", "id": 1, "method": "$/ping"})).unwrap();
        assert!(matches!(request, Message::Request(ref r) if r.params.is_null()));

        let notification =
            parse(json!({"jsonrpc": "2.0", "method": "$/log", "params": {"x": 1}})).unwrap();
        assert!(matches!(notification, Message::Notification(_)));

        let ok = parse(json!({"jsonrpc": "2.0", "id": "a", "result": null})).unwrap();
        assert_eq!(
            ok,
            Message::Response(ResponseMessage {
                id: Some(RequestId::String("a".into())),
                result: Ok(Value::Null),
            })
        );

        let error = parse(json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "bad json"}
        }))
        .unwrap();
        let Message::Response(ResponseMessage {
            id: None,
            result: Err(e),
        }) = error
        else {
            panic!("expected an error response without id");
        };
        assert_eq!(e.error_code(), Some(ErrorCode::ParseError));
    }

    #[test]
    fn malformed_messages_are_rejected() {
        for bad in [
            json!([]),
            json!({"id": 1, "method": "x"}),
            json!({"jsonrpc": "1.0", "id": 1, "method": "x"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": 5}),
            json!({"jsonrpc": "2.0", "id": 1.5, "method": "x"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "x", "params": 3}),
            json!({"jsonrpc": "2.0"}),
            json!({"jsonrpc": "2.0", "id": 1}),
            json!({"jsonrpc": "2.0", "id": 1, "result": 1, "error": {"code": 1, "message": ""}}),
        ] {
            let err = parse(bad.clone()).unwrap_err();
            assert!(matches!(err, MessageError::Invalid(_)), "{bad}: {err}");
            assert_eq!(
                err.to_response_error().code,
                ErrorCode::InvalidRequest.code()
            );
        }
        assert!(matches!(
            Message::from_slice(b"{").unwrap_err(),
            MessageError::Json(_)
        ));
    }

    #[test]
    fn wire_format_is_stable() {
        let request = RequestMessage::new::<Ping>(RequestId::Number(7), &()).unwrap();
        assert_eq!(
            Message::Request(request).to_value(),
            json!({"jsonrpc": "2.0", "id": 7, "method": "$/ping"})
        );
        let response = ResponseMessage::error(
            Some(RequestId::Number(7)),
            ResponseError::new(ErrorCode::ContentModified, "stale"),
        );
        assert_eq!(
            Message::Response(response).to_value(),
            json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32801, "message": "stale"}})
        );
    }

    #[test]
    fn typed_access_checks_method_and_payload() {
        let message = parse(json!({
            "jsonrpc": "2.0", "id": 1, "method": "command/execute",
            "params": {"command": "file.save", "args": []}
        }))
        .unwrap();
        let Message::Request(request) = message else {
            panic!("expected a request");
        };
        assert!(request.params::<Ping>().is_none());
        let params = request.params::<ExecuteCommand>().unwrap().unwrap();
        assert_eq!(params.command.as_str(), "file.save");

        let bad = RequestMessage {
            id: RequestId::Number(2),
            method: "command/execute".into(),
            params: json!({"command": "no-dot"}),
        };
        let err = bad.params::<ExecuteCommand>().unwrap().unwrap_err();
        assert_eq!(
            err.to_response_error().code,
            ErrorCode::InvalidParams.code()
        );
    }

    #[test]
    fn error_codes_are_unique_and_named() {
        let mut codes: Vec<i32> = ErrorCode::ALL.iter().map(|c| c.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ErrorCode::ALL.len());
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_code(code.code()), Some(code));
        }
        assert_eq!(ErrorCode::from_code(1), None);
    }
}
