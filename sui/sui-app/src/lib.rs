//! Application layer for the sui coding agent.
//!
//! Provides [`App`], which owns the prompt state, message history, and the
//! terminal run-loop (event → update → render).
//!
//! The interactive UI uses an inline [`ratatui::Viewport`] so only the prompt
//! (and slash suggestions) occupy the screen; submitted output is inserted
//! above it and scrolls into the terminal scrollback.
//!
//! # Modes
//!
//! Interaction uses explicit [`Mode`] state, not input-prefix inference:
//! - [`Mode::Prompt`] (default) — chat via optional [`sui_llm::LlmClient`];
//!   `/` opens slash commands and `@` opens a workspace file picker (typing
//!   `@` anywhere suggests files; Enter/Tab inserts the relative path). While a
//!   chat request is in flight the prompt border shows a working spinner,
//!   assistant Markdown streams above the prompt, and further chat submits are
//!   deferred until the stream completes (Ctrl-C / Esc still quit). When
//!   [`App::with_tools`] is set, prompt submits run the agent tool loop
//!   (`sui-agent`) instead of a single completion; tool results appear as ghost
//!   lines.
//! - [`Mode::Shell`] — entered with `!` on an empty prompt; Enter runs bash
//!   via [`sui_tools::run_line`], then returns to [`Mode::Prompt`]; Esc cancels
//!   back to Prompt without running; output is flushed as dim ghost text
//!
//! Attach a client with [`App::with_llm`] (typically
//! [`sui_llm::LlmClient::from_config_or_env`] in the binary) and tools with
//! [`App::with_tools`]. Without a client, prompt submits report a configuration
//! error instead of calling the configured API.
//!
//! Future surfaces (subagent, workflow) should add [`Mode`] variants rather
//! than new prefix heuristics.

pub mod app;
pub(crate) mod bang;
pub mod input;
pub(crate) mod llm;
pub(crate) mod markdown;
pub(crate) mod mention;
pub mod mode;
pub mod slash;

pub use app::{App, PROMPT_HEIGHT};
pub use mode::Mode;
pub use slash::SlashCommand;

/// Converts a char-based index into a byte offset within `s`.
///
/// Returns `None` when `char_idx` is past the end of the string.
#[inline]
pub(crate) fn char_index_to_byte(
    s: &str,
    char_idx: usize,
) -> Option<usize> {
    s.char_indices().nth(char_idx).map(|(i, _)| i)
}

#[cfg(test)]
mod tests;
