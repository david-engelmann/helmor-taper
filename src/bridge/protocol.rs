//! Wire types for the MCP bridge protocol. Pure data + (de)serialization,
//! no I/O — kept here so the (de)serializer shape can be unit-tested
//! without spinning up a WebSocket.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// One request frame written to the bridge. `id` correlates with the
/// matching [`BridgeResponse`]; the client generates a fresh UUID per
/// call so concurrent in-flight requests don't collide.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeRequest {
    pub id: String,
    pub command: String,
    pub args: Value,
}

impl BridgeRequest {
    /// Build a request with a freshly-minted UUID v4 as the id.
    pub fn new(command: impl Into<String>, args: Value) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            command: command.into(),
            args,
        }
    }
}

/// One response frame from the bridge. Either `success: true` with a
/// `data` payload, or `success: false` with an `error` string — never
/// both. Mapped to a Result-shaped accessor via [`BridgeResponse::into_result`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeResponse {
    pub id: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BridgeResponse {
    /// Build the success variant.
    pub fn ok(id: impl Into<String>, data: Value) -> Self {
        Self {
            id: id.into(),
            success: true,
            data: Some(data),
            error: None,
        }
    }

    /// Build the error variant.
    pub fn err(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            success: false,
            data: None,
            error: Some(error.into()),
        }
    }

    /// Convert into a `Result` whose `Ok` carries the data payload and
    /// whose `Err` carries the error message string.
    pub fn into_result(self) -> Result<Value, BridgeError> {
        if self.success {
            // success: true with no data is unusual but legal — return
            // Null so callers don't have to handle Option themselves.
            Ok(self.data.unwrap_or(Value::Null))
        } else {
            Err(BridgeError::Command {
                message: self
                    .error
                    .unwrap_or_else(|| "<unknown bridge error>".into()),
            })
        }
    }
}

/// Errors returned by the bridge client. Separates wire-shape problems
/// (malformed frames, no response received) from business-level errors
/// reported by the bridge itself.
#[derive(Debug, Error)]
pub enum BridgeError {
    /// The bridge replied `{success: false, error: "..."}`.
    #[error("bridge command failed: {message}")]
    Command { message: String },

    /// We didn't get a response within the deadline.
    #[error("timed out waiting for response to id={id} after {timeout_ms}ms")]
    Timeout { id: String, timeout_ms: u64 },

    /// Bridge connection dropped while a request was in flight.
    #[error("bridge connection closed while {pending} requests were in flight")]
    ConnectionClosed { pending: usize },

    /// The bridge replied but the JSON didn't parse.
    #[error("failed to parse bridge response: {0}")]
    MalformedResponse(#[from] serde_json::Error),

    /// Generic I/O failure on the WebSocket.
    #[error("bridge i/o error: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_assigns_unique_ids() {
        let a = BridgeRequest::new("list_windows", json!({}));
        let b = BridgeRequest::new("list_windows", json!({}));
        assert_ne!(a.id, b.id, "each new BridgeRequest must get a fresh uuid");
    }

    #[test]
    fn request_serializes_to_expected_wire_shape() {
        let req = BridgeRequest {
            id: "abc-123".into(),
            command: "execute_js".into(),
            args: json!({"windowLabel": "main", "script": "1+1"}),
        };
        let wire = serde_json::to_value(&req).unwrap();
        assert_eq!(
            wire,
            json!({
                "id": "abc-123",
                "command": "execute_js",
                "args": {"windowLabel": "main", "script": "1+1"},
            })
        );
    }

    #[test]
    fn response_ok_round_trips() {
        let r = BridgeResponse::ok("id-1", json!({"hello": "world"}));
        let wire = serde_json::to_string(&r).unwrap();
        let back: BridgeResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(r, back);
        assert!(back.success);
        assert!(back.error.is_none());
    }

    #[test]
    fn response_err_round_trips() {
        let r = BridgeResponse::err("id-2", "window not found");
        let wire = serde_json::to_string(&r).unwrap();
        let back: BridgeResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(r, back);
        assert!(!back.success);
        assert!(back.data.is_none());
        assert_eq!(back.error.as_deref(), Some("window not found"));
    }

    #[test]
    fn into_result_returns_data_on_success() {
        let r = BridgeResponse::ok("id-3", json!([1, 2, 3]));
        let res = r.into_result().expect("success variant should resolve");
        assert_eq!(res, json!([1, 2, 3]));
    }

    #[test]
    fn into_result_returns_null_when_success_has_no_data() {
        // `{success: true}` with no data — null is the sentinel, not an error.
        let r = BridgeResponse {
            id: "id-4".into(),
            success: true,
            data: None,
            error: None,
        };
        let res = r.into_result().expect("success without data → Null");
        assert_eq!(res, Value::Null);
    }

    #[test]
    fn into_result_returns_command_error_on_failure() {
        let r = BridgeResponse::err("id-5", "no such command");
        let err = r.into_result().expect_err("failure variant should error");
        assert!(
            matches!(err, BridgeError::Command { ref message } if message == "no such command")
        );
    }

    #[test]
    fn into_result_falls_back_when_error_is_missing() {
        // `{success: false}` with no error string — pathological, but
        // we don't want to panic. Fall back to a placeholder.
        let r = BridgeResponse {
            id: "id-6".into(),
            success: false,
            data: None,
            error: None,
        };
        let err = r.into_result().expect_err("failure variant should error");
        match err {
            BridgeError::Command { message } => {
                assert!(message.contains("unknown"), "got: {message}")
            }
            other => panic!("expected Command error, got {other:?}"),
        }
    }

    #[test]
    fn response_skip_serializing_omits_none_fields() {
        // The TypeScript client expects `{success: true, data: ...}` —
        // no `error` key when data is present. Confirm None fields are
        // serialized as absent, not as null.
        let r = BridgeResponse::ok("id-7", json!("hi"));
        let wire = serde_json::to_string(&r).unwrap();
        assert!(
            !wire.contains("error"),
            "ok response must not emit error field: {wire}"
        );

        let r = BridgeResponse::err("id-8", "boom");
        let wire = serde_json::to_string(&r).unwrap();
        assert!(
            !wire.contains("\"data\""),
            "err response must not emit data field: {wire}"
        );
    }
}
