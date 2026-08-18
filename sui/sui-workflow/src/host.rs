//! Host boundary for agent invocations.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    HostError,
    hash::{ContentHash, hash_json},
};

/// Whether a host infrastructure failure is safe to retry.
///
/// Mirrors celld's peer-dispatch classification: only failures that never
/// reached the effect may be retried automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFailureKind {
    /// The host never started the effect; retrying cannot double-apply.
    Retryable,
    /// The host may have started the effect; auto-retry is forbidden.
    Ambiguous,
}

impl std::fmt::Display for HostFailureKind {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Retryable => formatter.write_str("retryable"),
            Self::Ambiguous => formatter.write_str("ambiguous"),
        }
    }
}

/// A host capability level requested by an agent invocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// The agent may only inspect data.
    #[default]
    ReadOnly,
    /// The agent may inspect and modify data.
    ReadWrite,
    /// The agent may execute external operations.
    Execute,
    /// The agent may perform any host-supported operation.
    All,
}

impl std::fmt::Display for Capability {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        let value = match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
            Self::Execute => "execute",
            Self::All => "all",
        };
        formatter.write_str(value)
    }
}

impl FromStr for Capability {
    type Err = HostError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "read-only" => Ok(Self::ReadOnly),
            "read-write" => Ok(Self::ReadWrite),
            "execute" => Ok(Self::Execute),
            "all" => Ok(Self::All),
            _ => Err(HostError::new(format!(
                "invalid capability_mode `{value}`; expected read-only, read-write, execute, or all"
            ))),
        }
    }
}

/// Options attached to one agent invocation.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentOptions {
    /// Human-readable label for the invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Phase associated with the invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Requested capability mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_mode: Option<String>,
    /// Optional output schema supplied to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Requested agent type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Requested model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A deterministic request passed across the host boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentRequest {
    /// Prompt passed to the agent.
    pub prompt: String,
    /// Invocation options.
    pub options: AgentOptions,
}

impl AgentRequest {
    /// Content checksum used for journal resume identity.
    ///
    /// # Errors
    ///
    /// Returns a JSON error if the request cannot be serialized.
    pub fn content_hash(&self) -> Result<ContentHash, serde_json::Error> {
        hash_json(&serde_json::to_value(self)?)
    }
}

/// The data result of an agent invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResult {
    /// Stable host-defined agent identifier.
    pub agent_id: String,
    /// Whether the agent-level operation succeeded.
    pub success: bool,
    /// Agent output text.
    pub output: String,
    /// Whether the operation was cancelled.
    pub cancelled: bool,
    /// Deterministic token usage reported by the host.
    pub tokens_used: u64,
    /// Deterministic duration reported by the host.
    pub duration_ms: u64,
}

/// The effectful boundary used by the workflow engine.
///
/// Hosts are invoked synchronously on the engine thread. Implementations must
/// not call back into [`crate::Engine::run`] while a run is in progress
/// (`RefCell` borrow), and async work should be completed via blocking inside
/// [`Host::run_agent`].
///
/// # Failure classification
///
/// Return [`HostError::retryable`] only when the effect definitely did not
/// start (never connected, rejected before dispatch). Return
/// [`HostError::ambiguous`] (or [`HostError::new`]) when the effect may have
/// started — for example a timeout after bytes were written. Ambiguous
/// failures are journaled and block auto-retry until
/// [`crate::Journal::retry_failed`] is called deliberately.
pub trait Host {
    /// Returns the maximum capability this host grants to agent requests.
    fn granted_capability(&self) -> Capability {
        Capability::ReadOnly
    }

    /// Runs one agent request.
    ///
    /// # Errors
    ///
    /// Returns [`HostError`] when the host infrastructure cannot execute the
    /// request. Prefer [`HostError::retryable`] / [`HostError::ambiguous`]
    /// over the default constructor when the outcome class is known.
    fn run_agent(
        &self,
        request: &AgentRequest,
    ) -> Result<AgentResult, HostError>;
}

/// A deterministic host that echoes prompts and never accesses external state.
#[derive(Clone, Copy, Debug, Default)]
pub struct EchoHost;

impl Host for EchoHost {
    fn granted_capability(&self) -> Capability {
        Capability::All
    }

    fn run_agent(
        &self,
        request: &AgentRequest,
    ) -> Result<AgentResult, HostError> {
        let request_hash = request
            .content_hash()
            .map_err(|error| HostError::retryable(error.to_string()))?;
        let agent_id = format!("echo-{request_hash}");
        let tokens_used = u64::try_from(request.prompt.split_whitespace().count())
            .map_err(|error| HostError::retryable(error.to_string()))?;

        Ok(AgentResult {
            agent_id,
            success: true,
            output: request.prompt.clone(),
            cancelled: false,
            tokens_used,
            duration_ms: 0,
        })
    }
}
