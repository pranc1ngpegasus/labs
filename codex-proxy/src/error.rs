//! Error types for `codex-proxy`.

use std::io;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Errors produced while loading or refreshing Codex OAuth tokens.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The auth file could not be read.
    #[error("failed to read auth file {path}: {source}")]
    Read {
        /// Path of the offending file.
        path: String,
        /// Underlying IO error.
        source: io::Error,
    },
    /// The auth file is not valid JSON.
    #[error("failed to parse auth file {path}: {source}")]
    Parse {
        /// Path of the offending file.
        path: String,
        /// Underlying JSON error.
        source: serde_json::Error,
    },
    /// The authenticated mode is not supported by this proxy.
    #[error("unsupported auth mode `{0}`: only ChatGPT (OAuth) login is supported")]
    UnsupportedMode(String),
    /// The access token is not a decodable JWT, so its expiry could not be read.
    #[error("access token is not a decodable JWT: {0}")]
    Jwt(String),
    /// The token endpoint rejected the refresh attempt.
    #[error("token refresh failed: {0}")]
    Refresh(String),
    /// A refresh was attempted but no refresh token is available.
    #[error("no refresh token available; run `codex login`")]
    MissingRefresh,
}

impl AuthError {
    /// Maps an error to an HTTP status and a JSON error body for the client.
    fn http_response(&self) -> (StatusCode, Json<serde_json::Value>) {
        let (status, message) = match self {
            // Configuration problems are the operator's fault; surface them as
            // 500 so the client knows the proxy itself (not the upstream) failed.
            Self::UnsupportedMode(mode) => (
                StatusCode::BAD_REQUEST,
                serde_json::json!({ "error": { "message": format!("unsupported auth mode: {mode}"), "type": "invalid_request_error" } }),
            ),
            Self::MissingRefresh => (
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": { "message": self.to_string(), "type": "server_error" } }),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": { "message": self.to_string(), "type": "server_error" } }),
            ),
        };
        (status, Json(message))
    }
}

/// Errors produced while forwarding a request to the backend.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// Transport-level failure talking to the backend.
    #[error("backend request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// A refresh was needed but failed while handling this request.
    #[error("authentication failure: {0}")]
    Auth(#[from] AuthError),
    /// The backend returned a body we could not parse/translate.
    #[error("backend returned an unparseable response")]
    Upstream(StatusCode),
    /// The client request body exceeded the configured limit.
    #[error("request body exceeds the maximum size")]
    PayloadTooLarge,
    /// The request body could not be read.
    #[error("failed to read request body: {0}")]
    Body(std::io::Error),
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(
                    serde_json::json!({ "error": { "message": self.to_string(), "type": "invalid_request_error" } }),
                ),
            ),
            Self::Auth(auth) => {
                let (status, body) = auth.http_response();
                (status, body)
            },
            Self::Upstream(status) => (
                status,
                Json(
                    serde_json::json!({ "error": { "message": "upstream error", "type": "upstream_error" } }),
                ),
            ),
            _ => (
                StatusCode::BAD_GATEWAY,
                Json(
                    serde_json::json!({ "error": { "message": self.to_string(), "type": "proxy_error" } }),
                ),
            ),
        };
        (status, message).into_response()
    }
}
