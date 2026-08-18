//! Direct OpenAI-compatible HTTP client for Chat Completions and Responses.
//!
//! Configure an endpoint with [`LlmConfig`] and use [`LlmClient`] for text or
//! tool-calling turns. Chat Completions is the default wire format; select the
//! Responses API with [`ApiMode::Responses`]. Both modes support streaming.
//!
//! # Configuration
//!
//! The binary loads switchable `[[model."name"]]` entries from `$SUI_CONFIG`,
//! `$XDG_CONFIG_HOME/sui/config.toml`, or `~/.config/sui/config.toml`. Each
//! entry accepts `base_url`, `model`, optional `api_key`, optional `env_key`
//! (environment variable holding the API key), and optional `api_mode`.
//!
//! When no named models are configured, the binary falls back to a legacy
//! `[llm]` section at the same config path. The same values can also be supplied
//! through `SUI_LLM_BASE_URL`, optional `SUI_LLM_API_KEY`, `SUI_LLM_MODEL`, and
//! optional `SUI_LLM_API_MODE`.
//!
//! `base_url` is trusted operator config (see [`LlmConfig`]). Do not pass
//! untrusted URLs.
//!
//! Request timeouts are not configured by this crate; wrap calls with your
//! runtime's timeout helper (for example Tokio's `timeout`) at the call site if
//! you need one.
//!
//! # Example
//!
//! ```no_run
//! # async fn demo() -> Result<(), sui_llm::LlmError> {
//! use sui_llm::{ChatMessage, LlmClient, LlmConfig};
//!
//! let config = LlmConfig::new("https://api.openai.com", "sk-...", "gpt-4o")?;
//! let client = LlmClient::new(&config);
//! let reply = client.chat(&[ChatMessage::user("hello")]).await?;
//! println!("{}", reply.content);
//! # let _ = reply;
//! # Ok(())
//! # }
//! ```

mod client;
mod config;
mod error;
mod message;

pub use client::{ChatChunk, ChatResponse, ChatStream, LlmClient};
pub use config::{ApiMode, LlmConfig, LlmModel, default_config_path};
pub use error::{ApiError, LlmError};
pub use message::{ChatMessage, Role, ToolCall, ToolSpec};
