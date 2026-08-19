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
use futures::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::auth::Auth;
use crate::error::ProxyError;

/// Headers that describe the `ChatGPT` account and must be attached upstream.
const ACCOUNT_HEADER: &str = "ChatGPT-Account-Id";

/// Environment variable that, when set to a truthy value, streams the upstream
/// request/response wire to stderr for debugging (see [`debug_enabled`]).
const DEBUG_ENV: &str = "CODEX_PROXY_DEBUG";

/// State shared by all handlers.
#[derive(Clone)]
struct AppState {
    auth: Auth,
    http: reqwest::Client,
    backend: String,
    /// SHA-256 of the client API key clients must present on every call.
    client_key_hash: [u8; 32],
    /// When true, the upstream request/response wire is logged to stderr.
    debug: bool,
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
        debug: debug_enabled(),
    };
    Router::new()
        .route("/v1/responses", post(responses))
        .route("/v1/models", get(models))
        .with_state(state)
}

/// Whether upstream wire logging is enabled via [`DEBUG_ENV`].
///
/// Any value other than unset, empty, `0`, or `false` (case-insensitive)
/// enables it, so `CODEX_PROXY_DEBUG=1` is the ergonomic switch.
fn debug_enabled() -> bool {
    std::env::var(DEBUG_ENV).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty()
            && !value.eq_ignore_ascii_case("0")
            && !value.eq_ignore_ascii_case("false")
    })
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
    let body = normalize_responses_body(body);
    let upstream_url = format!("{}/responses", state.backend);
    forward(&state, &upstream_url, content_type, body).await
}

/// Moves `system` messages out of a Responses request's `input` array and into
/// the top-level `instructions` field.
///
/// The `ChatGPT`/codex backend rejects `role: "system"` items inside `input`
/// with `400 System messages are not allowed`, but standard `OpenAI` Responses
/// clients routinely put the system prompt there. Rewriting the body keeps the
/// proxy usable by unmodified clients without losing the prompt.
///
/// The original bytes are returned unchanged when the body is not JSON, has no
/// array `input`, or carries no system messages, so non-Responses payloads and
/// already-valid requests pass through untouched.
fn normalize_responses_body(body: bytes::Bytes) -> bytes::Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(input) = value.get_mut("input").and_then(Value::as_array_mut) else {
        return body;
    };
    let mut system_texts = Vec::new();
    input.retain(|item| {
        if item.get("role").and_then(Value::as_str) == Some("system") {
            if let Some(text) = message_content_text(item.get("content")) {
                system_texts.push(text);
            }
            false
        } else {
            true
        }
    });
    if system_texts.is_empty() {
        return body;
    }
    let mut parts = Vec::with_capacity(system_texts.len() + 1);
    if let Some(existing) = value.get("instructions").and_then(Value::as_str)
        && !existing.is_empty()
    {
        parts.push(existing.to_owned());
    }
    parts.extend(system_texts);
    value["instructions"] = Value::String(parts.join("\n\n"));
    serde_json::to_vec(&value).map_or(body, bytes::Bytes::from)
}

/// Extracts the text of a Responses message `content`, which may be a bare
/// string or an array of typed parts (`{ "type": "input_text", "text": ... }`).
fn message_content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        },
        _ => None,
    }
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
    if state.debug {
        eprintln!("[codex-proxy] > POST {upstream_url}");
        eprintln!("[codex-proxy] > body: {}", String::from_utf8_lossy(&body));
    }
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

        if state.debug {
            eprintln!("[codex-proxy] < status: {status}");
            eprintln!("[codex-proxy] < upstream headers:");
            for (name, value) in response.headers() {
                eprintln!(
                    "[codex-proxy]     {name}: {}",
                    value.to_str().unwrap_or("<non-ascii>")
                );
            }
        }

        let mut builder = Response::builder().status(status);
        for (name, value) in response.headers() {
            if is_forwardable_header(name) {
                builder = builder.header(name, value);
            }
        }
        // WHAM streams `/responses` as SSE but omits `Content-Type`. Streaming
        // clients need `text/event-stream` to recognize the body, so label it
        // when the successful upstream response left it unset.
        if let Some(content_type) = injected_content_type(status, response.headers()) {
            builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        if state.debug {
            eprintln!("[codex-proxy] < downstream headers sent to client:");
            if let Some(headers) = builder.headers_ref() {
                for (name, value) in headers {
                    eprintln!(
                        "[codex-proxy]     {name}: {}",
                        value.to_str().unwrap_or("<non-ascii>")
                    );
                }
            }
        }
        // Relay the upstream SSE, but end the downstream body as soon as the
        // response is logically complete. The WHAM backend keeps the
        // connection open (keep-alive) after `response.completed` instead of
        // closing it, so clients that wait for body EOF (rather than stopping
        // at the terminal event) would otherwise stall until a read timeout
        // and retry. Closing here gives them EOF immediately.
        let debug = state.debug;
        let body_stream = futures::stream::unfold(
            (Box::pin(response.bytes_stream()), Vec::<u8>::new(), false),
            move |(mut upstream, tail, finished)| async move {
                if finished {
                    return None;
                }
                match upstream.next().await {
                    Some(Ok(chunk)) => {
                        if debug {
                            eprint!("{}", String::from_utf8_lossy(&chunk));
                        }
                        let terminal = sse_stream_terminates(&tail, &chunk);
                        let next_tail = terminal_scan_tail(&chunk);
                        Some((Ok(chunk), (upstream, next_tail, terminal)))
                    },
                    Some(Err(error)) => {
                        if debug {
                            eprintln!("\n[codex-proxy] < stream error: {error}");
                        }
                        Some((Err(error), (upstream, tail, true)))
                    },
                    None => None,
                }
            },
        );
        return builder
            .body(Body::from_stream(body_stream))
            .map_err(|e| ProxyError::Body(std::io::Error::other(e)));
    }
}

/// SSE `data` payloads that mark the logical end of a Responses stream.
const TERMINAL_MARKERS: [&[u8]; 3] = [
    b"\"type\":\"response.completed\"",
    b"\"type\":\"response.failed\"",
    b"\"type\":\"response.incomplete\"",
];

/// How many trailing bytes of the previous chunk to keep so a terminal marker
/// split across a chunk boundary is still detected. Must exceed the longest
/// marker in [`TERMINAL_MARKERS`].
const TERMINAL_SCAN_TAIL: usize = 32;

/// Whether `chunk` (considered together with the previous chunk's `tail`)
/// contains a terminal Responses event, signaling the stream can be closed.
fn sse_stream_terminates(
    tail: &[u8],
    chunk: &[u8],
) -> bool {
    if TERMINAL_MARKERS
        .iter()
        .any(|marker| contains_subslice(chunk, marker))
    {
        return true;
    }
    // Only the boundary (previous tail + this chunk's head) can hide a marker
    // the within-chunk scan missed, so check just that seam.
    let mut boundary = tail.to_vec();
    boundary.extend_from_slice(&chunk[..chunk.len().min(TERMINAL_SCAN_TAIL)]);
    TERMINAL_MARKERS
        .iter()
        .any(|marker| contains_subslice(&boundary, marker))
}

/// The trailing bytes of `chunk` to carry into the next boundary check.
fn terminal_scan_tail(chunk: &[u8]) -> Vec<u8> {
    let start = chunk.len().saturating_sub(TERMINAL_SCAN_TAIL);
    chunk[start..].to_vec()
}

/// Whether `haystack` contains `needle` as a contiguous subslice.
fn contains_subslice(
    haystack: &[u8],
    needle: &[u8],
) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The `Content-Type` to inject into a forwarded upstream response, if any.
///
/// The `ChatGPT`/WHAM backend streams `/responses` as Server-Sent Events but
/// returns no `Content-Type`, which makes streaming clients fail to recognize
/// the body and retry. Returns `text/event-stream` when a successful upstream
/// response omitted the header, and `None` when it already set one or the
/// response is an error (whose body should be relayed as-is).
fn injected_content_type(
    status: StatusCode,
    headers: &HeaderMap,
) -> Option<HeaderValue> {
    (status.is_success() && !headers.contains_key(reqwest::header::CONTENT_TYPE))
        .then(|| HeaderValue::from_static("text/event-stream"))
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

    #[test]
    fn moves_system_message_into_instructions() {
        let body = br#"{"model":"m","input":[{"role":"system","content":"rules"},{"role":"user","content":"hi"}]}"#;
        let out = normalize_responses_body(bytes::Bytes::from_static(body));
        let v: Value = serde_json::from_slice(&out).expect("json");
        assert_eq!(v["instructions"], "rules");
        let roles: Vec<&str> = v["input"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|m| m["role"].as_str())
            .collect();
        assert_eq!(roles, ["user"]);
    }

    #[test]
    fn joins_multiple_system_messages_and_existing_instructions() {
        let body = br#"{"model":"m","instructions":"base","input":[{"role":"system","content":[{"type":"input_text","text":"a"}]},{"role":"user","content":"hi"},{"role":"system","content":"b"}]}"#;
        let out = normalize_responses_body(bytes::Bytes::from_static(body));
        let v: Value = serde_json::from_slice(&out).expect("json");
        assert_eq!(v["instructions"], "base\n\na\n\nb");
        assert_eq!(v["input"].as_array().expect("array").len(), 1);
    }

    #[test]
    fn passes_through_when_no_system_message() {
        let body = br#"{"model":"m","input":[{"role":"user","content":"hi"}]}"#;
        let out = normalize_responses_body(bytes::Bytes::from_static(body));
        assert_eq!(out.as_ref(), body);
    }

    #[test]
    fn passes_through_non_json_body() {
        let body = b"not json at all";
        let out = normalize_responses_body(bytes::Bytes::from_static(body));
        assert_eq!(out.as_ref(), body);
    }

    #[test]
    fn injects_event_stream_when_upstream_omits_content_type() {
        let headers = HeaderMap::new();
        assert_eq!(
            injected_content_type(StatusCode::OK, &headers),
            Some(HeaderValue::from_static("text/event-stream"))
        );
    }

    #[test]
    fn keeps_upstream_content_type_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        assert_eq!(injected_content_type(StatusCode::OK, &headers), None);
    }

    #[test]
    fn does_not_inject_content_type_on_error_status() {
        let headers = HeaderMap::new();
        assert_eq!(
            injected_content_type(StatusCode::BAD_REQUEST, &headers),
            None
        );
    }

    #[test]
    fn detects_terminal_event_within_a_chunk() {
        let chunk = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n";
        assert!(sse_stream_terminates(&[], chunk));
    }

    #[test]
    fn detects_terminal_event_split_across_chunk_boundary() {
        let full = b"data: {\"type\":\"response.completed\"}";
        let split = 20;
        let tail = terminal_scan_tail(&full[..split]);
        assert!(!sse_stream_terminates(&[], &full[..split]));
        assert!(sse_stream_terminates(&tail, &full[split..]));
    }

    #[test]
    fn does_not_terminate_on_ordinary_delta_chunk() {
        let chunk = b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n";
        assert!(!sse_stream_terminates(&[], chunk));
    }

    #[test]
    fn contains_subslice_matches_and_rejects() {
        assert!(contains_subslice(b"abcdef", b"cde"));
        assert!(!contains_subslice(b"abcdef", b"xyz"));
        assert!(!contains_subslice(b"ab", b"abc"));
    }
}
