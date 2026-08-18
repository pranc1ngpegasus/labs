//! TUI widgets for the sui coding agent.
//!
//! This crate provides [`PromptWidget`], a text-input widget with a configurable
//! prefix, wrapped input, and cursor tracking built on [ratatui].

mod prompt;
mod wrap;

pub use prompt::{PROMPT_MIN_HEIGHT, PromptWidget};
pub use wrap::{segment_ranges, wrap_prefixed, wrap_text};
