//! `codex-proxy` — 常駐 OAuth プロキシの CLI エントリポイント。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::Parser;
use tokio::net::TcpListener;

use codex_proxy::auth::{Auth, DEFAULT_TOKEN_URL};
use codex_proxy::error::AuthError;
use codex_proxy::proxy;

/// 既定の ChatGPT/WHAM バックエンド基底。
const DEFAULT_BACKEND: &str = "https://chatgpt.com/backend-api/wham";
/// auto で解決する既定 auth ファイルのサブパス。
const DEFAULT_AUTH_REL: &str = ".codex/auth.json";

#[derive(Debug, Parser)]
#[command(
    name = "codex-proxy",
    version,
    about = "常駐 OAuth プロキシ: Codex の ChatGPT トークンを自動更新し、OpenAI-compatible な /v1 をローカルに expose する"
)]
struct Cli {
    /// バインドするホスト。
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// バインドするポート。
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Codex の auth.json のパス。未指定なら $HOME/.codex/auth.json。
    #[arg(long)]
    auth_file: Option<PathBuf>,

    /// OAuth トークンエンドポイント。
    #[arg(long, default_value = DEFAULT_TOKEN_URL)]
    token_url: String,

    /// `ChatGPT` バックエンドの基底 URL。
    #[arg(long, default_value = DEFAULT_BACKEND)]
    backend: String,

    /// クライアント API キー。未指定なら起動時に生成して表示する。
    #[arg(long)]
    api_key: Option<String>,
}

fn resolve_auth_file(cli: &Cli) -> PathBuf {
    cli.auth_file.clone().unwrap_or_else(|| {
        home_dir()
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join(DEFAULT_AUTH_REL)
    })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(message) => {
            eprintln!("codex-proxy: {message}");
            ExitCode::from(2)
        },
    }
}

async fn run() -> Result<ExitCode, String> {
    let cli = Cli::parse();
    let auth_file = resolve_auth_file(&cli);
    let backend = cli.backend.trim_end_matches('/').to_owned();

    let auth = Auth::load(&auth_file, &cli.token_url)
        .await
        .map_err(|e| auth_load_message(e, &auth_file))?;

    let client_api_key = match cli.api_key {
        Some(key) => key,
        None => generate_client_key()?,
    };

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("no local addr: {e}"))?;

    let app = proxy::router(auth, &backend, &client_api_key);
    println!("codex-proxy listening on http://{local}/v1");
    println!("  client API key : {client_api_key}");
    println!("  POST /v1/responses -> {backend}/responses");
    println!("  GET  /v1/models    -> {backend}/models");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("server error: {e}"))?;
    Ok(ExitCode::SUCCESS)
}

/// Generates a fresh client API key from 32 bytes of OS randomness, rendered
/// as unpadded base64url. The caller prints it for the operator to configure
/// in the client (e.g. `sui`'s `[llm] api_key`).
///
/// # Errors
///
/// Returns an error if the operating system cannot supply randomness.
fn generate_client_key() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("failed to generate API key: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Completes when SIGINT (Ctrl-C) or SIGTERM is received, allowing in-flight
/// requests (e.g. long-running SSE) to drain before the server stops.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            () = sigterm() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Resolves when a SIGTERM is delivered. If the handler cannot be registered
/// the future simply never resolves (Ctrl-C is still handled by the caller).
#[cfg(unix)]
async fn sigterm() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut signal) => {
            signal.recv().await;
        },
        Err(_) => std::future::pending::<()>().await,
    }
}

fn auth_load_message(
    error: AuthError,
    path: &Path,
) -> String {
    let path = path.display();
    match error {
        AuthError::UnsupportedMode(mode) => format!(
            "{path}: unsupported auth mode `{mode}`; codex-proxy needs ChatGPT (OAuth) login (`codex login`)"
        ),
        other => format!("{path}: {other}"),
    }
}
