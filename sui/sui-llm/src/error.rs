use std::fmt;

use thiserror::Error;

/// Opaque details for an HTTP/API failure.
///
/// The status code is available without exposing response bodies or request
/// details. The source chain is intentionally retained for callers that need
/// transport diagnostics; log complete chains only in trusted sinks.
#[non_exhaustive]
pub struct ApiError {
    status: Option<reqwest::StatusCode>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ApiError {
    /// Returns the HTTP status when the server returned one.
    #[must_use]
    pub const fn status(&self) -> Option<reqwest::StatusCode> {
        self.status
    }

    fn from_source<E>(error: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self {
            status: None,
            source: Some(Box::new(error)),
        }
    }

    fn with_status(status: reqwest::StatusCode) -> Self {
        Self {
            status: Some(status),
            source: Some(Box::new(ApiStatus(status))),
        }
    }

    fn with_message(message: &'static str) -> Self {
        Self {
            status: None,
            source: Some(Box::new(ApiMessage(message))),
        }
    }
}

impl fmt::Debug for ApiError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.debug_struct("ApiError")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ApiError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str("LLM API error")
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// Errors from configuration or OpenAI-compatible API calls.
#[derive(Error)]
#[non_exhaustive]
pub enum LlmError {
    /// A required environment variable was not set.
    #[error("missing environment variable `{0}`")]
    MissingEnv(&'static str),
    /// No `[llm]` configuration was present at the requested location.
    #[error("LLM configuration was not found")]
    MissingConfig,
    /// The configuration file could not be read.
    #[error("could not read LLM configuration file")]
    ConfigFile(#[source] std::io::Error),
    /// Configuration values failed validation.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    /// A call-site argument failed validation (model override, messages, …).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The OpenAI-compatible HTTP/API layer failed.
    ///
    /// [`Display`](std::fmt::Display) and [`Debug`](std::fmt::Debug) are
    /// intentionally opaque. Walking the source chain may expose transport
    /// details, so treat full error-chain logs as trusted output.
    #[error("LLM API error")]
    Api(#[source] ApiError),
    /// The API returned a syntactically valid response that the client cannot
    /// safely map to its public response types.
    #[error("invalid LLM API response: {0}")]
    InvalidResponse(&'static str),
    /// The Responses API did not complete the requested generation.
    #[error("LLM response was incomplete")]
    IncompleteResponse,
    /// The API returned no usable assistant text or tool calls.
    #[error("empty chat completion response")]
    EmptyResponse,
    /// The model refused to produce assistant content.
    #[error("model refused: {0}")]
    Refused(String),
}

impl LlmError {
    /// Returns the HTTP status associated with an API error, if one exists.
    #[must_use]
    pub const fn api_status(&self) -> Option<reqwest::StatusCode> {
        match self {
            Self::Api(error) => error.status(),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for LlmError {
    fn from(value: reqwest::Error) -> Self {
        Self::Api(ApiError::from_source(value))
    }
}

impl From<serde_json::Error> for LlmError {
    fn from(value: serde_json::Error) -> Self {
        Self::Api(ApiError::from_source(value))
    }
}

#[allow(clippy::redundant_pub_crate)]
pub(super) fn api_error<E>(error: E) -> LlmError
where
    E: std::error::Error + Send + Sync + 'static,
{
    LlmError::Api(ApiError::from_source(error))
}

#[allow(clippy::redundant_pub_crate)]
pub(super) fn api_status(status: reqwest::StatusCode) -> LlmError {
    LlmError::Api(ApiError::with_status(status))
}

#[allow(clippy::redundant_pub_crate)]
pub(super) fn api_message(message: &'static str) -> LlmError {
    LlmError::Api(ApiError::with_message(message))
}

#[derive(Debug)]
struct ApiStatus(reqwest::StatusCode);

impl fmt::Display for ApiStatus {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "HTTP status {}", self.0)
    }
}

impl std::error::Error for ApiStatus {}

#[derive(Debug)]
struct ApiMessage(&'static str);

impl fmt::Display for ApiMessage {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ApiMessage {}

impl fmt::Debug for LlmError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::MissingEnv(name) => f.debug_tuple("MissingEnv").field(name).finish(),
            Self::MissingConfig => f.write_str("MissingConfig"),
            Self::ConfigFile(_) => f.write_str("ConfigFile(/* redacted */)"),
            Self::InvalidConfig(msg) => f.debug_tuple("InvalidConfig").field(msg).finish(),
            Self::InvalidArgument(msg) => f.debug_tuple("InvalidArgument").field(msg).finish(),
            Self::Api(error) => f.debug_tuple("Api").field(&error.status()).finish(),
            Self::InvalidResponse(msg) => f.debug_tuple("InvalidResponse").field(msg).finish(),
            Self::IncompleteResponse => f.write_str("IncompleteResponse"),
            Self::EmptyResponse => f.write_str("EmptyResponse"),
            Self::Refused(msg) => f.debug_tuple("Refused").field(msg).finish(),
        }
    }
}
