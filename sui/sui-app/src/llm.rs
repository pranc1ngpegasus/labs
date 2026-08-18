//! Non-blocking streaming LLM chat for [`crate::Mode::Prompt`].
//!
//! The TUI event loop stays responsive while a chat request runs: the worker
//! thread owns a current-thread Tokio runtime (same pattern as [`crate::bang`])
//! and forwards stream deltas on a channel. The app polls that channel between
//! draw ticks so Markdown can render incrementally above the prompt.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use futures::StreamExt;
use sui_llm::{ChatMessage, ChatResponse, LlmClient};

/// Default deadline for a single streaming chat completion.
pub const DEFAULT_CHAT_TIMEOUT: Duration = Duration::from_mins(2);

/// Default deadline for a full agent turn (multiple samples + tools).
pub const DEFAULT_AGENT_TIMEOUT: Duration = Duration::from_mins(10);

/// Spinner frame interval while an LLM request is in flight.
pub const SPINNER_TICK: Duration = Duration::from_millis(80);

/// Braille spinner glyphs (clockwise).
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Incremental update from the background chat worker.
pub enum LlmStreamMsg {
    /// A text delta from the OpenAI-compatible stream (plain chat path).
    Chunk(String),
    /// A local tool finished; render as ghost scrollback.
    Tool(String),
    /// Stream / agent turn finished successfully.
    Done {
        /// Final assistant text for Markdown scrollback.
        response: ChatResponse,
        /// Full transcript to persist (includes tool calls / results).
        history: Vec<ChatMessage>,
    },
    /// Stream failed or timed out.
    Failed(String),
}

/// Glyph for a spinner that started `elapsed` ago.
#[must_use]
pub fn spinner_glyph(elapsed: Duration) -> char {
    let tick = SPINNER_TICK.as_millis().max(1);
    let idx = (elapsed.as_millis() / tick) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[idx]
}

/// Spawns a streaming chat and returns a receiver for incremental updates.
///
/// The worker uses timeout [`DEFAULT_CHAT_TIMEOUT`]. Dropping the receiver does
/// not cancel the request; sends are best-effort.
#[must_use]
pub fn chat_stream_spawn(
    client: &LlmClient,
    messages: &[ChatMessage],
) -> Receiver<LlmStreamMsg> {
    chat_stream_spawn_with_timeout(client, messages, DEFAULT_CHAT_TIMEOUT)
}

fn chat_stream_spawn_with_timeout(
    client: &LlmClient,
    messages: &[ChatMessage],
    timeout: Duration,
) -> Receiver<LlmStreamMsg> {
    let (tx, rx) = mpsc::channel();
    let client = client.clone();
    let messages = messages.to_vec();
    let default_model = client.default_model().to_owned();
    std::thread::spawn(move || {
        let result = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("failed to create tokio runtime: {error}"))?;
            runtime.block_on(async {
                tokio::time::timeout(timeout, async {
                    let mut stream = client
                        .chat_stream(&messages)
                        .await
                        .map_err(|error| error.to_string())?;
                    let mut content = String::new();
                    while let Some(item) = stream.next().await {
                        let chunk = item.map_err(|error| error.to_string())?;
                        if chunk.delta.is_empty() {
                            continue;
                        }
                        content.push_str(&chunk.delta);
                        if tx.send(LlmStreamMsg::Chunk(chunk.delta)).is_err() {
                            break;
                        }
                    }
                    Ok(ChatResponse::new(content, &default_model))
                })
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "llm request timed out after {}s",
                        timeout.as_secs()
                    ))
                })
            })
        })();
        match result {
            Ok(response) => {
                let mut history = messages;
                history.push(ChatMessage::assistant(response.content.clone()));
                let _ = tx.send(LlmStreamMsg::Done { response, history });
            },
            Err(error) => {
                let _ = tx.send(LlmStreamMsg::Failed(error));
            },
        }
    });
    rx
}

/// Spawns the agent tool loop and returns a receiver for UI updates.
///
/// `messages` is the transcript *before* the new user turn. The worker appends
/// the user message, runs tools, and returns the full history on success.
#[must_use]
pub fn agent_spawn(
    client: &LlmClient,
    tools: sui_tools::ToolRegistry,
    messages: &[ChatMessage],
    user: &str,
) -> Receiver<LlmStreamMsg> {
    agent_spawn_with_timeout(client, tools, messages, user, DEFAULT_AGENT_TIMEOUT)
}

fn agent_spawn_with_timeout(
    client: &LlmClient,
    tools: sui_tools::ToolRegistry,
    messages: &[ChatMessage],
    user: &str,
    timeout: Duration,
) -> Receiver<LlmStreamMsg> {
    let (tx, rx) = mpsc::channel();
    let client = client.clone();
    let mut messages = messages.to_vec();
    let user = user.to_owned();
    let default_model = client.default_model().to_owned();
    std::thread::spawn(move || {
        let result = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("failed to create tokio runtime: {error}"))?;
            runtime.block_on(async {
                tokio::time::timeout(timeout, async {
                    let tx_tools = tx.clone();
                    let text = sui_agent::run_turn(
                        &client,
                        &tools,
                        &mut messages,
                        user,
                        sui_agent::TurnOptions::default(),
                        |event| {
                            if let sui_agent::AgentEvent::ToolEnd { name, result, .. } = event {
                                let line = format_tool_line(&name, &result);
                                let _ = tx_tools.send(LlmStreamMsg::Tool(line));
                            }
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    if !text.is_empty() {
                        let _ = tx.send(LlmStreamMsg::Chunk(text.clone()));
                    }
                    Ok((ChatResponse::new(text, &default_model), messages))
                })
                .await
                .unwrap_or_else(|_| {
                    Err(format!("agent turn timed out after {}s", timeout.as_secs()))
                })
            })
        })();
        match result {
            Ok((response, history)) => {
                let _ = tx.send(LlmStreamMsg::Done { response, history });
            },
            Err(error) => {
                let _ = tx.send(LlmStreamMsg::Failed(error));
            },
        }
    });
    rx
}

fn format_tool_line(
    name: &str,
    result: &str,
) -> String {
    let preview: String = result.chars().take(120).collect();
    if result.chars().count() > 120 {
        format!("tool {name}: {preview}…")
    } else {
        format!("tool {name}: {preview}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_glyph_cycles() {
        assert_eq!(spinner_glyph(Duration::ZERO), '⠋');
        assert_eq!(spinner_glyph(SPINNER_TICK), '⠙');
        let full_cycle = SPINNER_TICK * u32::try_from(SPINNER_FRAMES.len()).unwrap_or(1);
        assert_eq!(spinner_glyph(full_cycle), '⠋');
    }
}
