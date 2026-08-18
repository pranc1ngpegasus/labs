use thiserror::Error;

use sui_llm::LlmError;

/// Errors from the agent tool loop (not from individual tools).
#[derive(Error)]
#[non_exhaustive]
pub enum AgentError {
    /// The model kept requesting tools past the configured sample cap.
    #[error("agent turn limit reached ({0} samples)")]
    TurnLimit(usize),
    /// The API kept returning empty completions across bounded retries.
    #[error("model returned an empty response after {0} attempts")]
    EmptyResponse(usize),
    /// A call-site option failed validation.
    #[error("invalid agent option: {0}")]
    Invalid(String),
    /// The underlying chat completion failed.
    #[error(transparent)]
    Llm(#[from] LlmError),
}

impl std::fmt::Debug for AgentError {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::TurnLimit(n) => f.debug_tuple("TurnLimit").field(n).finish(),
            Self::EmptyResponse(n) => f.debug_tuple("EmptyResponse").field(n).finish(),
            Self::Invalid(msg) => f.debug_tuple("Invalid").field(msg).finish(),
            Self::Llm(err) => f.debug_tuple("Llm").field(err).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_turn_limit() {
        let err = AgentError::TurnLimit(32);
        assert_eq!(err.to_string(), "agent turn limit reached (32 samples)");
    }

    #[test]
    fn display_empty_response() {
        let err = AgentError::EmptyResponse(3);
        assert_eq!(
            err.to_string(),
            "model returned an empty response after 3 attempts"
        );
    }
}
