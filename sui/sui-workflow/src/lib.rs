//! Deterministic Rhai workflow engine with content-hash resume.
//!
//! Resume identity is checksum-based:
//! - `workflow_hash`: SHA-256 of the workflow source
//! - `input_hash`: SHA-256 of canonical JSON `args`
//! - per-entry hashes for agent prompts/options, results, parallel panels,
//!   scratch I/O, user gates, and durable wakes
//!
//! Changing any of those checksums rejects journal replay instead of silently
//! continuing with mismatched state.
//!
//! Durability follows the celld output-gate contract: a checkpointed commit is
//! not acknowledged until the journal is `fsync`ed, and host failures are
//! classified as retryable or ambiguous so auto-retry cannot double-apply.

mod engine;
mod error;
mod hash;
mod host;
mod journal;
mod meta;
mod schema;
mod value;

pub use engine::{CompiledWorkflow, Engine, PauseInfo, RunOptions, RunResult};
pub use error::{HostError, WorkflowError};
pub use hash::{ContentHash, hash_bytes, hash_json, hash_str};
pub use host::{
    AgentOptions, AgentRequest, AgentResult, Capability, EchoHost, Host, HostFailureKind,
};
pub use journal::{Journal, JournalEntry, ParallelSlot};
pub use meta::{MetaPhase, WorkflowMeta};
pub use schema::{tool_descriptor, validate_args};

/// Runs a workflow script against any [`Host`].
///
/// # Errors
///
/// Returns any compilation or execution error surfaced by the engine.
pub fn run_with_host<H>(
    host: H,
    script: &str,
    options: RunOptions,
) -> Result<RunResult, WorkflowError>
where
    H: Host + 'static,
{
    Engine::new(host).run_script(script, options)
}

#[cfg(test)]
mod tests;
