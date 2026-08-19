//! Hermetic end-to-end tests for the proxy router, using wiremock to stand in
//! for both the `ChatGPT` backend and the OAuth token endpoint.
#![allow(clippy::expect_used, clippy::unwrap_used)] // conventional in test crates

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use codex_proxy::auth::Auth;
use codex_proxy::proxy;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EXPIRED_ACCESS: &str = "eyJhbGciOiJub25lIn0.eyJleHAiOjF9.sig";
const FRESH_ACCESS: &str = "eyJhbGciOiJub25lIn0.eyJleHAiOjk5OTk5OTk5OTl9.sig";
/// The client API key the test proxy requires.
const TEST_CLIENT_KEY: &str = "test-client-key";

/// Writes an auth.json with an expired token into `dir` and returns its path.
fn write_expired_auth(dir: &std::path::Path) -> std::path::PathBuf {
    let json = format!(
        r#"{{
            "auth_mode": "chatgpt",
            "last_refresh": "2025-01-01T00:00:00.000Z",
            "tokens": {{
                "access_token": "{EXPIRED_ACCESS}",
                "refresh_token": "rt_test",
                "account_id": "acct_1"
            }}
        }}"#
    );
    let path = dir.join("auth.json");
    std::fs::write(&path, json).expect("write auth.json");
    path
}

/// A ready-to-use proxy router pointing at `backend_server` and refreshing
/// tokens via `token_server`.
async fn build_router(
    backend_server: &MockServer,
    token_server: &MockServer,
) -> Result<(axum::Router, tempfile::TempDir), String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let auth_file = write_expired_auth(dir.path());
    let auth = Auth::load(&auth_file, &format!("{}/oauth/token", token_server.uri()))
        .await
        .map_err(|e| e.to_string())?;
    let app = proxy::router(auth, &backend_server.uri(), TEST_CLIENT_KEY);
    Ok((app, dir))
}

/// Mocks the token endpoint to return a fresh access token on refresh.
async fn mock_token_endpoint(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": FRESH_ACCESS,
            "refresh_token": "rt_new",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .expect(1..)
        .mount(server)
        .await;
}

#[tokio::test]
async fn forwards_responses_with_refreshed_token() {
    let backend = MockServer::start().await;
    let token = MockServer::start().await;
    mock_token_endpoint(&token).await;

    let seen_authorization = Arc::new(std::sync::Mutex::new(None::<String>));
    let seen_seen = Arc::clone(&seen_authorization);
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            let auth = req
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            *seen_seen.lock().expect("lock") = auth;
            assert_eq!(
                req.headers
                    .get("chatgpt-account-id")
                    .and_then(|v| v.to_str().ok()),
                Some("acct_1")
            );
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "object": "response",
                "status": "completed",
                "output": []
            }))
        })
        .mount(&backend)
        .await;

    let (app, _dir) = build_router(&backend, &token).await.expect("router");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.1","input":[]}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    // The token was refreshed from the (expired) initial one.
    let expected = format!("Bearer {FRESH_ACCESS}");
    let seen = seen_authorization.lock().expect("lock").clone();
    assert_eq!(seen.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn forwards_models_with_schema_rewrite() {
    let backend = MockServer::start().await;
    let token = MockServer::start().await;
    mock_token_endpoint(&token).await;

    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"slug": "gpt-5.1-codex-mini"}, {"slug": "gpt-5.1"}]
        })))
        .mount(&backend)
        .await;

    let (app, _dir) = build_router(&backend, &token).await.expect("router");
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let v: Value = serde_json::from_slice(&body_bytes).expect("json");
    assert_eq!(v["object"], "list");
    let ids: Vec<&str> = v["data"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(ids, ["gpt-5.1-codex-mini", "gpt-5.1"]);
}

#[tokio::test]
async fn models_relays_upstream_error_body() {
    let backend = MockServer::start().await;
    let token = MockServer::start().await;
    mock_token_endpoint(&token).await;

    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "bad request", "type": "invalid_request_error" }
        })))
        .mount(&backend)
        .await;

    let (app, _dir) = build_router(&backend, &token).await.expect("router");
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let v: Value = serde_json::from_slice(&body_bytes).expect("json");
    assert_eq!(v["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn retries_once_after_401_and_force_refresh() {
    let backend = MockServer::start().await;
    let token = MockServer::start().await;
    mock_token_endpoint(&token).await;

    // First call: 401 (token rejected). Second call after refresh: 200.
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let front_responses_calls = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |_req: &wiremock::Request| {
            let n = front_responses_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "expired"}}))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"id": "resp_2", "object": "response", "status": "completed", "output": []}))
            }
        })
        .expect(2)
        .mount(&backend)
        .await;

    let (app, _dir) = build_router(&backend, &token).await.expect("router");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.1","input":[]}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrent_expiry_misses_refresh_only_once() {
    let backend = MockServer::start().await;
    let token = MockServer::start().await;

    // Count token-endpoint refresh calls.
    let refresh_calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let front_refresh = Arc::clone(&refresh_calls);
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(move |_req: &wiremock::Request| {
            front_refresh.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({
                "access_token": FRESH_ACCESS,
                "token_type": "Bearer",
                "expires_in": 3600,
            }))
        })
        .expect(1)
        .mount(&token)
        .await;

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_n",
            "object": "response",
            "status": "completed",
            "output": []
        })))
        .expect(8)
        .mount(&backend)
        .await;

    let (app, _dir) = build_router(&backend, &token).await.expect("router");

    // Fire 8 concurrent requests while the initial token is expired; the
    // single-flight refresh must hit the token endpoint exactly once.
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"gpt-5.1","input":[]}"#))
                    .expect("request"),
            )
            .await
            .expect("response")
            .status()
        }));
    }
    for task in tasks {
        let status = task.await.expect("task");
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(refresh_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn responses_moves_system_message_into_instructions_upstream() {
    let backend = MockServer::start().await;
    let token = MockServer::start().await;
    mock_token_endpoint(&token).await;

    let seen_body = Arc::new(std::sync::Mutex::new(None::<Value>));
    let capture = Arc::clone(&seen_body);
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            *capture.lock().expect("lock") = serde_json::from_slice(&req.body).ok();
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "object": "response",
                "status": "completed",
                "output": []
            }))
        })
        .mount(&backend)
        .await;

    let (app, _dir) = build_router(&backend, &token).await.expect("router");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.1","input":[{"role":"system","content":"rules"},{"role":"user","content":"hi"}]}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = seen_body
        .lock()
        .expect("lock")
        .clone()
        .expect("captured body");
    assert_eq!(body["instructions"], "rules");
    let roles: Vec<&str> = body["input"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|m| m["role"].as_str())
        .collect();
    assert_eq!(roles, ["user"]);
}

#[tokio::test]
async fn responses_injects_event_stream_content_type_when_upstream_omits_it() {
    let token = MockServer::start().await;
    mock_token_endpoint(&token).await;

    // The ChatGPT/WHAM backend returns a 200 SSE body with no Content-Type and
    // delimits it by closing the connection. Reproduce that exactly with a raw
    // TCP server so we can assert the proxy labels the stream for the client.
    let sse_body = "event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n";
    let backend_uri = spawn_raw_sse_backend(sse_body).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let auth_file = write_expired_auth(dir.path());
    let auth = Auth::load(&auth_file, &format!("{}/oauth/token", token.uri()))
        .await
        .expect("auth");
    let app = proxy::router(auth, &backend_uri, TEST_CLIENT_KEY);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.1","input":"hi"}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("read body");
    assert_eq!(bytes.as_ref(), sse_body.as_bytes());
}

/// Spawns a one-shot raw TCP HTTP/1.1 server that replies with `200 OK`, no
/// `Content-Type`, and `body`, delimiting it by closing the connection — the
/// shape the WHAM backend uses. Returns its base URL.
async fn spawn_raw_sse_backend(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf).await;
            let response = format!("HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n{body}");
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// Like [`spawn_raw_sse_backend`] but sends `body` as a single HTTP chunk and
/// then holds the keep-alive connection open without the terminating chunk —
/// mimicking WHAM, which does not close after `response.completed`. The proxy
/// must still end the client stream once it relays the terminal event.
async fn spawn_raw_chunked_never_closing_backend(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf).await;
            let header =
                "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n";
            let _ = socket.write_all(header.as_bytes()).await;
            let chunk = format!("{:x}\r\n{body}\r\n", body.len());
            let _ = socket.write_all(chunk.as_bytes()).await;
            // Deliberately never send the terminating "0\r\n\r\n" chunk, and
            // keep the socket open so the client cannot rely on EOF.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            drop(socket);
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn responses_fills_empty_completed_output_and_closes_despite_open_upstream() {
    let token = MockServer::start().await;
    mock_token_endpoint(&token).await;

    // WHAM streams the output only as incremental items, returns
    // response.completed with an empty output array, and then keeps the
    // keep-alive connection open. The proxy must rebuild output and end the
    // client body promptly.
    let sse_body = concat!(
        "event: response.output_item.done\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello!\"}]}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n",
    );
    let backend_uri = spawn_raw_chunked_never_closing_backend(sse_body).await;

    let dir = tempfile::tempdir().expect("tempdir");
    let auth_file = write_expired_auth(dir.path());
    let auth = Auth::load(&auth_file, &format!("{}/oauth/token", token.uri()))
        .await
        .expect("auth");
    let app = proxy::router(auth, &backend_uri, TEST_CLIENT_KEY);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {TEST_CLIENT_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.1","input":"hi"}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    // The body must complete promptly even though the upstream never closes.
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        axum::body::to_bytes(response.into_body(), 1 << 20),
    )
    .await
    .expect("body did not complete after terminal event")
    .expect("read body");

    // The relayed completed event must carry the reconstructed output.
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");
    let completed = text
        .split("\n\n")
        .find(|block| block.contains("response.completed"))
        .expect("completed event");
    let data = completed
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("data line");
    let value: Value = serde_json::from_str(data).expect("json");
    let output = value["response"]["output"]
        .as_array()
        .expect("output array");
    assert_eq!(output.len(), 1);
    assert_eq!(output[0]["content"][0]["text"], "Hello!");
}

#[tokio::test]
async fn missing_or_wrong_client_key_is_rejected() {
    let backend = MockServer::start().await;
    let token = MockServer::start().await;

    // Because these requests are rejected before any token is needed, the
    // token endpoint must never be contacted.
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&token)
        .await;

    // The backend must never be reached when the client key is wrong.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&backend)
        .await;

    let (app, _dir) = build_router(&backend, &token).await.expect("router");

    // No Authorization header at all.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.1","input":[]}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A wrong Authorization header.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", "Bearer wrong-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.1","input":[]}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
