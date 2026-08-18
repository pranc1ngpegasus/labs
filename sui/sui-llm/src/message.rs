/// Author of a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// System / developer instruction.
    System,
    /// End-user turn.
    User,
    /// Prior assistant turn.
    Assistant,
    /// Tool result answering a previous [`ToolCall`].
    Tool,
}

/// A model-emitted function call (`OpenAI` `tool_calls` item).
///
/// `arguments` is the raw JSON object string from the model so it can be
/// echoed back on the next request without re-serialization drift.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolCall {
    /// Provider-assigned call id (`call_…`); required on the tool-result turn.
    pub id: String,
    /// Registered tool name.
    pub name: String,
    /// JSON object payload as emitted by the model.
    pub arguments: String,
}

impl ToolCall {
    /// Builds a tool call from id, name, and raw JSON arguments.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
}

/// JSON-Schema tool advertised to the model on a chat request.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolSpec {
    /// Stable tool name the model must use in [`ToolCall::name`].
    pub name: String,
    /// When and how to use the tool (shown to the model).
    pub description: String,
    /// JSON Schema object for the tool's arguments (`type: object`).
    pub parameters: serde_json::Value,
}

impl ToolSpec {
    /// Builds a tool spec from name, description, and a JSON Schema object.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A single chat message.
///
/// Prefer the constructors ([`ChatMessage::system`], [`ChatMessage::user`],
/// [`ChatMessage::assistant`], [`ChatMessage::assistant_tools`],
/// [`ChatMessage::tool`]) over struct literals so new fields remain
/// non-breaking (`#[non_exhaustive]` rejects crate-external literals).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatMessage {
    /// Who authored the message.
    pub role: Role,
    /// Plain-text content (empty on assistant turns that only call tools).
    pub content: String,
    /// Assistant-emitted tool calls (empty unless [`Role::Assistant`]).
    pub tool_calls: Vec<ToolCall>,
    /// Id of the tool call this result answers ([`Role::Tool`] only).
    pub tool_call_id: Option<String>,
    pub(crate) responses_output: Option<Vec<serde_json::Value>>,
}

impl ChatMessage {
    /// Builds a system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            responses_output: None,
        }
    }

    /// Builds a user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            responses_output: None,
        }
    }

    /// Builds an assistant text message (no tool calls).
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            responses_output: None,
        }
    }

    /// Builds an assistant turn that requested tools.
    #[must_use]
    pub fn assistant_tools(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            responses_output: None,
        }
    }

    /// Builds a tool-result message for `tool_call_id`.
    #[must_use]
    pub fn tool(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            responses_output: None,
        }
    }

    pub(crate) fn assistant_with_responses_output(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
        responses_output: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            responses_output: Some(responses_output),
        }
    }
}
