//! The HTTP proxy layer.
//!
//! Exposes OpenAI-compatible routes over the `ChatGPT` backend, attaching a
//! freshly-refreshed OAuth token to each upstream call.
//!
//! - `POST /v1/responses` is forwarded verbatim (headers, body, and the
//!   streaming SSE response) to the backend.
//! - `GET /v1/models` is forwarded, but the backend's non-standard
//!   `{ "models": [{ "slug": ... }] }` shape is rewritten to the standard
//!   `{ "data": [{ "id": ... }] }` `OpenAI` shape.
//!
//! On a 401 from the backend the token is force-refreshed once and the request
//! is retried a single time.

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::auth::Auth;
use crate::error::ProxyError;

/// Headers that describe the `ChatGPT` account and must be attached upstream.
const ACCOUNT_HEADER: &str = "ChatGPT-Account-Id";

/// State shared by all handlers.
#[derive(Clone)]
struct AppState {
    auth: Auth,
    http: reqwest::Client,
    backend: String,
    /// SHA-256 of the client API key clients must present on every call.
    client_key_hash: [u8; 32],
}

/// Builds the application router. `client_api_key` is the key that calling
/// clients must present as `Authorization: Bearer <key>`; requests without a
/// matching key are rejected with 401.
pub fn router(
    auth: Auth,
    backend: &str,
    client_api_key: &str,
) -> Router {
    let state = AppState {
        auth,
        http: reqwest::Client::new(),
        backend: backend.trim_end_matches('/').to_owned(),
        client_key_hash: hash_key(client_api_key),
    };
    Router::new()
        .route("/v1/responses", post(responses))
        .route("/v1/models", get(models))
        .with_state(state)
}

/// Forwards `POST /v1/responses` to the backend.
async fn responses(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, ProxyError> {
    if !client_authorized(request.headers(), &state.client_key_hash) {
        return Ok(unauthorized());
    }
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|e| ProxyError::Body(std::io::Error::other(e)))?;
    let upstream_url = format!("{}/responses", state.backend);
    forward(&state, &upstream_url, content_type, body).await
}

/// Forwards `GET /v1/models`, rewriting the response to the standard shape.
async fn models(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, ProxyError> {
    if !client_authorized(request.headers(), &state.client_key_hash) {
        return Ok(unauthorized());
    }
    let upstream_url = format!("{}/models", state.backend);
    let access = state.auth.access_token().await?;
    let account_header = account_header_value(&state).await;
    let mut builder = state
        .http
        .get(&upstream_url)
        .header(reqwest::header::AUTHORIZATION, bearer(&access));
    if let Some(value) = account_header {
        builder = builder.header(ACCOUNT_HEADER, value);
    }
    // WHAM's /models requires the client_version query param; pass through any
    // that the caller sent on the incoming request.
    if let Some(query) = request.uri().query() {
        builder = builder.query(&query);
    }

    let response = builder.send().await?;
    let status = response.status();

    // Relay upstream failures (status + body) so OpenAI clients receive the
    // backend's structured error rather than a generic placeholder.
    if !status.is_success() {
        let mut builder = Response::builder().status(status);
        for (name, value) in response.headers() {
            if is_forwardable_header(name) {
                builder = builder.header(name, value);
            }
        }
        let stream = response.bytes_stream();
        return builder
            .body(Body::from_stream(stream))
            .map_err(|e| ProxyError::Body(std::io::Error::other(e)));
    }

    let body_bytes = response.bytes().await?;
    let rewritten = rewrite_models(&body_bytes)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(rewritten))
        .map_err(|e| ProxyError::Body(std::io::Error::other(e)))
}

/// Forwards a request/body to `upstream_url`, streaming the backend response
/// back to the client and retrying once on 401 after a forced refresh.
async fn forward(
    state: &AppState,
    upstream_url: &str,
    content_type: Option<String>,
    body: bytes::Bytes,
) -> Result<Response, ProxyError> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let access = state.auth.access_token().await?;
        let account_header = account_header_value(state).await;
        let mut builder = state
            .http
            .post(upstream_url)
            .header(reqwest::header::AUTHORIZATION, bearer(&access))
            .body(body.clone());
        if let Some(value) = account_header {
            builder = builder.header(ACCOUNT_HEADER, value);
        }
        if let Some(ct) = &content_type {
            builder = builder.header(reqwest::header::CONTENT_TYPE, ct);
        }

        let response = builder.send().await?;
        let status = response.status();

        if status == StatusCode::UNAUTHORIZED && attempt < MAX_ATTEMPTS {
            // The token may have been revoked; force a refresh and retry once.
            state.auth.force_refresh().await?;
            continue;
        }

        let mut builder = Response::builder().status(status);
        for (name, value) in response.headers() {
            if is_forwardable_header(name) {
                builder = builder.header(name, value);
            }
        }
        // Stream the upstream body through so long-lived SSE conversations are
        // relayed incrementally rather than buffered whole.
        let stream = response.bytes_stream();
        return builder
            .body(Body::from_stream(stream))
            .map_err(|e| ProxyError::Body(std::io::Error::other(e)));
    }
}

/// Computes the `ChatGPT-Account-Id` header value, if an account is known.
async fn account_header_value(state: &AppState) -> Option<HeaderValue> {
    state
        .auth
        .account_id()
        .await
        .and_then(|id| HeaderValue::from_str(&id).ok())
}

fn bearer(access: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("Bearer {access}")).unwrap_or(HeaderValue::from_static(""))
}

/// Returns true when the request presents the expected client API key.
///
/// The key is extracted from the `Authorization: Bearer <key>` header and the
/// SHA-256 digest is compared in constant time, so an attacker cannot learn
/// the key (or its length) by timing the rejection.
fn client_authorized(
    headers: &HeaderMap,
    expected_hash: &[u8; 32],
) -> bool {
    let Some(authorization) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(header) = authorization.to_str() else {
        return false;
    };
    let Some(key) = header.strip_prefix("Bearer ") else {
        return false;
    };
    hash_key(key).ct_eq(expected_hash).into()
}

/// A constant-time-friendly digest of a client key.
fn hash_key(key: &str) -> [u8; 32] {
    let digest = Sha256::digest(key.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// An OpenAI-style 401 response for a missing or wrong client key.
fn unauthorized() -> Response {
    let body = serde_json::json!({
        "error": {
            "message": "invalid API key",
            "type": "invalid_request_error",
            "code": "invalid_api_key",
        }
    });
    (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
}

/// Whether a backend response header is safe to relay to the client.
///
/// Hop-by-hop headers and the length are recomputed by axum and must not be
/// copied verbatim.
fn is_forwardable_header(name: &HeaderName) -> bool {
    !matches!(
        name.as_str(),
        "connection" | "keep-alive" | "transfer-encoding" | "content-length"
    )
}

/// Rewrites the backend's `/models` payload to the OpenAI-standard shape.
///
/// The `ChatGPT` backend returns `{ "models": [{ "slug": "..." }, ...] }`; the
/// standard shape is `{ "object": "list", "data": [{ "id": "..." }, ...] }`.
///
/// # Errors
///
/// Returns [`ProxyError::Upstream`] when the payload cannot be parsed or is
/// not in the expected shape.
fn rewrite_models(body: &[u8]) -> Result<Vec<u8>, ProxyError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| ProxyError::Upstream(StatusCode::BAD_GATEWAY))?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| ProxyError::Upstream(StatusCode::BAD_GATEWAY))?;
    let data: Vec<Value> = models
        .iter()
        .filter_map(|m| {
            let id = m.get("slug").and_then(Value::as_str)?;
            Some(serde_json::json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": "openai",
            }))
        })
        .collect();
    let output = serde_json::json!({ "object": "list", "data": data });
    serde_json::to_vec(&output).map_err(|_| ProxyError::Upstream(StatusCode::BAD_GATEWAY))
}

const MAX_ATTEMPTS: u32 = 2;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_wham_models_to_standard() {
        let body = br#"{"models":[{"slug":"gpt-5.1-codex-mini"},{"slug":"gpt-5.1"}]}"#;
        let out = rewrite_models(body).expect("rewrite");
        let v: Value = serde_json::from_slice(&out).expect("json");
        let ids: Vec<&str> = v["data"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert_eq!(ids, ["gpt-5.1-codex-mini", "gpt-5.1"]);
    }

    #[test]
    fn rejects_malformed_models_payload() {
        let err = rewrite_models(b"not json").expect_err("error");
        assert!(matches!(err, ProxyError::Upstream(_)));
    }
}
