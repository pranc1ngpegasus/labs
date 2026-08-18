use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::HostError;

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
// `output_schema` holds a `serde_json::Value`, which is not `Eq` (JSON numbers
// may be floats), so only `PartialEq` can be derived here.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentOptions {
    /// Human-readable label for the invocation.
    pub label: Option<String>,
    /// Phase associated with the invocation.
    pub phase: Option<String>,
    /// Requested capability mode.
    pub capability_mode: Option<String>,
    /// Optional output schema supplied to the agent.
    pub output_schema: Option<Value>,
    /// Requested agent type.
    pub agent_type: Option<String>,
    /// Requested model.
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
pub trait Host {
    /// Returns the maximum capability this host grants to agent requests.
    fn granted_capability(&self) -> Capability {
        Capability::ReadOnly
    }

    /// Runs one agent request.
    ///
    /// Agent-level failures should be returned as [`AgentResult`] values with
    /// `success` set to `false`. The error channel is reserved for host or
    /// infrastructure failures.
    ///
    /// # Errors
    ///
    /// Returns [`HostError`] when the host infrastructure cannot execute the
    /// request.
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
        // The echo host only records a plan and never performs the requested
        // operation. Accept every capability so workflows that describe edits
        // or command execution can still produce a complete execution plan.
        Capability::All
    }

    fn run_agent(
        &self,
        request: &AgentRequest,
    ) -> Result<AgentResult, HostError> {
        let agent_id = format!(
            "echo-{}",
            crate::hash::sha256_hex(request.prompt.as_bytes())
        );
        let tokens_used = u64::try_from(request.prompt.split_whitespace().count())
            .map_err(|error| HostError::new(error.to_string()))?;

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
