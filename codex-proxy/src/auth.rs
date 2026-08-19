//! Codex OAuth token management.
//!
//! Loads the access/refresh tokens from Codex's `auth.json` (`ChatGPT` mode
//! only), derives the access-token expiry from its JWT `exp` claim (the file
//! does not store an explicit expiry), and refreshes the access token in
//! memory when it is close to expiring. The on-disk `auth.json` is left
//! untouched: Codex owns that file and writes to it concurrently.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::AuthError;

/// The public OAuth client ID used by the Codex CLI.
///
/// This is the client that `codex login` (`ChatGPT` mode) authenticates as and
/// that the token endpoint expects on refresh. There is no standard allocation
/// mechanism for third-party clients, so we reuse it verbatim.
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default OAuth token endpoint.
pub const DEFAULT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// Refresh the token this long before its nominal expiry.
const SAFETY_MARGIN_SECS: i64 = 60;

/// Ceiling on a token's `expires_in` so a misreporting server cannot pin the
/// token "valid" far beyond reality.
const MAX_EXPIRES_IN_SECS: i64 = 86_400; // one day
/// Fallback lifetime when the token endpoint omits `expires_in`.
const DEFAULT_EXPIRES_IN_SECS: i64 = 3600;

/// Shape of the `tokens` block in Codex's `auth.json`.
#[derive(Debug, Deserialize)]
struct Tokens {
    #[serde(rename = "access_token")]
    access_token: String,
    #[serde(rename = "refresh_token")]
    refresh_token: String,
    #[serde(rename = "account_id")]
    account_id: Option<String>,
}

/// Shape of Codex's `auth.json`, restricted to the fields we consume.
#[derive(Debug, Deserialize)]
struct AuthFile {
    #[serde(rename = "auth_mode")]
    auth_mode: Option<String>,
    tokens: Tokens,
}

/// The current, in-memory token state shared across requests.
#[derive(Debug, Clone)]
struct TokenState {
    access_token: String,
    refresh_token: String,
    account_id: Option<String>,
    /// Nominal expiry of `access_token`, as unix epoch seconds.
    expires_at_secs: i64,
}

/// Shared, refresh-capable holder for the OAuth token set. Cheap to clone.
#[derive(Clone)]
pub struct Auth {
    http: Client,
    token_url: String,
    state: Arc<RwLock<TokenState>>,
    /// Serializes refresh so concurrent expiry misses don't stampede the token
    /// endpoint with simultaneous requests.
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    /// Injectable clock: returns unix epoch seconds. Defaults to wall clock.
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for Auth {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("token_url", &self.token_url)
            .finish_non_exhaustive()
    }
}

impl Auth {
    /// Loads tokens from `path` and returns a handle that refreshes them.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] when the file is unreadable, malformed, or uses
    /// an unsupported auth mode.
    pub async fn load(
        path: &Path,
        token_url: &str,
    ) -> Result<Self, AuthError> {
        let contents = tokio::fs::read_to_string(path)
            .await
            .map_err(|source| AuthError::Read {
                path: path.display().to_string(),
                source,
            })?;
        Self::from_json(&contents, token_url)
    }

    /// Loads tokens from raw JSON. Used by [`Self::load`] and by tests.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] for malformed JSON or unsupported modes.
    pub fn from_json(
        contents: &str,
        token_url: &str,
    ) -> Result<Self, AuthError> {
        let file: AuthFile = serde_json::from_str(contents).map_err(|source| AuthError::Parse {
            path: "(json)".to_owned(),
            source,
        })?;

        let mode = file.auth_mode.unwrap_or_else(|| "chatgpt".to_owned());
        if mode != "chatgpt" {
            return Err(AuthError::UnsupportedMode(mode));
        }

        let expires_at_secs = jwt_exp(&file.tokens.access_token)?;
        let state = TokenState {
            access_token: file.tokens.access_token,
            refresh_token: file.tokens.refresh_token,
            account_id: file.tokens.account_id,
            expires_at_secs,
        };

        Ok(Self {
            http: Client::new(),
            token_url: token_url.to_owned(),
            state: Arc::new(RwLock::new(state)),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            now: Arc::new(system_now),
        })
    }

    /// Overrides the clock (unix seconds). Test-only.
    #[cfg(test)]
    fn set_now(
        &mut self,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) {
        self.now = now;
    }

    /// The current account id, if any.
    pub(crate) async fn account_id(&self) -> Option<String> {
        self.state.read().await.account_id.clone()
    }

    /// Returns a valid access token, refreshing first if it is expiring.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] when a refresh is needed but fails.
    pub(crate) async fn access_token(&self) -> Result<String, AuthError> {
        self.ensure_valid().await?;
        let state = self.state.read().await;
        Ok(state.access_token.clone())
    }

    /// Refreshes the access token if it is within the safety margin of expiry.
    ///
    /// Concurrent callers that observe an expiring token serialize on a single
    /// refresh: each re-checks after acquiring the lock, so only the first
    /// waiter actually hits the token endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Refresh`] if the token endpoint rejects us, or
    /// [`AuthError::MissingRefresh`] if no refresh token is available.
    pub(crate) async fn ensure_valid(&self) -> Result<(), AuthError> {
        if !self.expiring_soon().await {
            return Ok(());
        }
        let _guard = self.refresh_lock.lock().await;
        // Another caller may have refreshed while we waited for the lock.
        if self.expiring_soon().await {
            self.do_refresh().await?;
        }
        Ok(())
    }

    /// Refreshes unconditionally. Used when the backend rejects the current
    /// token (401); this also flows through the single-flight lock so a burst
    /// of 401s cannot stampede the token endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Refresh`] on transport or non-2xx from the endpoint,
    /// or [`AuthError::MissingRefresh`] when there is no refresh token.
    pub(crate) async fn force_refresh(&self) -> Result<(), AuthError> {
        let _guard = self.refresh_lock.lock().await;
        self.do_refresh().await
    }

    /// Returns true when the current token is absent or within the safety
    /// margin of its expiry.
    async fn expiring_soon(&self) -> bool {
        let state = self.state.read().await;
        state.access_token.is_empty() || state.expires_at_secs <= (self.now)() + SAFETY_MARGIN_SECS
    }

    /// Swaps the in-memory token state after a successful refresh. Callers must
    /// hold [`Self::refresh_lock`].
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Refresh`] on transport or non-2xx from the endpoint,
    /// or [`AuthError::MissingRefresh`] when there is no refresh token.
    #[allow(clippy::significant_drop_tightening)] // write guard held across the mutation block
    async fn do_refresh(&self) -> Result<(), AuthError> {
        let refresh_token = {
            let state = self.state.read().await;
            state.refresh_token.clone()
        };
        if refresh_token.is_empty() {
            return Err(AuthError::MissingRefresh);
        }

        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", CODEX_CLIENT_ID),
        ];
        let response = self
            .http
            .post(&self.token_url)
            .form(&form)
            .send()
            .await
            .map_err(|source| AuthError::Refresh(source.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(AuthError::Refresh(format!(
                "token endpoint returned {status}"
            )));
        }

        let body: Value = response
            .json()
            .await
            .map_err(|source| AuthError::Refresh(source.to_string()))?;

        let new_access = required_string(&body, "access_token")?;
        let new_refresh = string_or(&body, "refresh_token", refresh_token);
        let expires_in_secs = body
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_EXPIRES_IN_SECS)
            .clamp(1, MAX_EXPIRES_IN_SECS);
        let expires_at_secs = (self.now)() + expires_in_secs;

        let mut state = self.state.write().await;
        state.access_token = new_access;
        state.refresh_token = new_refresh;
        // `account_id` is stable per account and not refreshed; leave it as is.
        state.expires_at_secs = expires_at_secs;
        Ok(())
    }
}

fn required_string(
    body: &Value,
    key: &str,
) -> Result<String, AuthError> {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AuthError::Refresh(format!("token endpoint response missing `{key}`")))
}

fn string_or(
    body: &Value,
    key: &str,
    fallback: String,
) -> String {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map_or(fallback, str::to_owned)
}

/// Reads the JWT `exp` claim (unix seconds) from an access token.
///
/// Only the unverified payload is decoded; signatures are not checked here.
///
/// # Errors
///
/// Returns [`AuthError::Jwt`] for malformed tokens or a missing/invalid `exp`.
fn jwt_exp(token: &str) -> Result<i64, AuthError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| AuthError::Jwt("token is not a JWT".to_owned()))?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| AuthError::Jwt("payload is not valid base64url".to_owned()))?;
    let value: Value =
        serde_json::from_slice(&decoded).map_err(|e| AuthError::Jwt(e.to_string()))?;
    value
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or_else(|| AuthError::Jwt("payload has no numeric `exp` claim".to_owned()))
}

fn system_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_with_exp(exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.signature")
    }

    fn auth_with_json(
        auth_mode: &str,
        access: &str,
        refresh: &str,
    ) -> Result<Auth, AuthError> {
        let json = format!(
            r#"{{
                "auth_mode": "{auth_mode}",
                "tokens": {{
                    "access_token": "{access}",
                    "refresh_token": "{refresh}",
                    "account_id": "acct_1"
                }}
            }}"#
        );
        Auth::from_json(&json, "https://example.invalid/token")
    }

    fn auth_with(exp: i64) -> Auth {
        auth_with_json("chatgpt", &token_with_exp(exp), "rt_test").expect("valid json")
    }

    #[test]
    #[allow(clippy::significant_drop_tightening)] // read guard read across two assertions
    fn loads_chatgpt_mode_and_expiry() {
        let now = 1_000_000_000_i64;
        let mut auth = auth_with(now + 10_000);
        auth.set_now(Arc::new(move || now));
        let state = auth.state.blocking_read();
        assert_eq!(state.expires_at_secs, now + 10_000);
        assert_eq!(state.account_id.as_deref(), Some("acct_1"));
    }

    #[test]
    fn rejects_api_key_mode() {
        let err = auth_with_json("api_key", "a.b.c", "r").expect_err("unsupported mode");
        assert!(matches!(err, AuthError::UnsupportedMode(ref m) if m == "api_key"));
    }

    #[test]
    fn rejects_invalid_jwt() {
        let err = auth_with_json("chatgpt", "not-a-jwt", "r").expect_err("bad jwt");
        assert!(matches!(err, AuthError::Jwt(_)));
    }

    #[test]
    fn expires_in_reads_exp_claim() {
        assert_eq!(
            jwt_exp(&token_with_exp(123_456_789)).expect("exp"),
            123_456_789
        );
    }

    #[test]
    fn missing_refresh_token_is_reported() {
        let now = 1_000_000_000_i64;
        let mut auth = auth_with(now + 5);
        auth.set_now(Arc::new(move || now));
        {
            let mut state = auth.state.blocking_write();
            state.refresh_token.clear();
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let err = auth.ensure_valid().await.expect_err("no refresh token");
            assert!(matches!(err, AuthError::MissingRefresh));
        });
    }

    #[test]
    fn valid_token_does_not_refresh() {
        let now = 1_000_000_000_i64;
        let mut auth = auth_with(now + 3600);
        auth.set_now(Arc::new(move || now));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let token = auth.access_token().await.expect("valid");
            assert!(token.ends_with(".signature"));
        });
    }
}
