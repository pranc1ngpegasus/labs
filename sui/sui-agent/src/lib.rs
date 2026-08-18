//! Agent loop: the model proposes tool calls; this crate executes them locally.
//!
//! There is no custom trigger protocol. The client advertises tools as `OpenAI`
//! function schemas on each sample. The model returns `tool_calls`. The host
//! runs [`sui_tools::ToolRegistry::call`] against the local filesystem / shell
//! and appends `role: tool` results. Repeat until the model answers with text
//! (or the turn limit is hit).
//!
//! This is the same loop as Grok Build, `OpenCode`, and pi-agent-core. What this
//! crate deliberately does **not** copy:
//!
//! - Grok Build's ACP / leader / Computer Hub (multi-client platform)
//! - `OpenCode`'s 9-layer fuzzy edit matching (sui `edit` requires a Git diff)
//! - pi's three-layer `ToolDefinition` wrapping (sui already has [`sui_tools::Tool`])
//!
//! Dedicated `grep` / `read` / `write` tools are deferred: `code_search`,
//! `edit`, and one-shot `bash` (`action=run`) are enough to close the loop.

mod error;

use std::path::Path;

use serde_json::{Value, json};
use sui_llm::{ChatMessage, LlmClient, LlmError, ToolCall, ToolSpec};
use sui_tools::ToolRegistry;

pub use error::AgentError;

/// Default cap on model samples per user turn.
pub const DEFAULT_MAX_TURNS: usize = 32;

/// Soft cap on a single tool result string fed back to the model.
pub const MAX_TOOL_RESULT_CHARS: usize = 32_768;

const EMPTY_RESPONSE_MAX_ATTEMPTS: usize = 3;

/// Short standing orders. Tool *how* lives in JSON schemas, not here.
#[must_use]
pub fn system_prompt(cwd: &Path) -> String {
    format!(
        "You are sui, a coding agent working in {}. \
         Use tools to inspect and change the workspace. \
         Prefer code_search over listing files. \
         Prefer the edit tool for file changes. Provide one Git unified diff with \
         ---/+++ file headers and @@ hunk headers. Example existing-file edit: \
         `diff --git a/path b/path\\n--- a/path\\n+++ b/path\\n@@ -1,1 +1,1 @@\\n-old\\n+new`. \
         For a new file use `--- /dev/null` and `+++ b/path`; for deletion use \
         `--- a/path` and `+++ /dev/null`. Hunk line counts are recalculated by \
         the agent, but the diff must contain valid +/-/context lines. \
         For shell commands, call bash with a single-line `command` (action defaults to run). \
         Do not ask the user to run commands you can run yourself. \
         Talk to the user in assistant text, not via echo.",
        cwd.display()
    )
}

/// Progress from one [`run_turn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A tool is about to run.
    ToolStart {
        /// Tool call id from the model.
        id: String,
        /// Registered tool name.
        name: String,
    },
    /// A tool finished (success or error encoded in `result`).
    ToolEnd {
        /// Tool call id from the model.
        id: String,
        /// Registered tool name.
        name: String,
        /// JSON (or error object) returned to the model.
        result: String,
    },
}

/// Options for [`run_turn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnOptions {
    /// Maximum successful agent samples (each tool round counts as one).
    /// Empty-response retry attempts are bounded separately.
    pub max_turns: usize,
}

impl Default for TurnOptions {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
        }
    }
}

/// Maps [`ToolRegistry::descriptors`] into model-facing [`ToolSpec`]s.
#[must_use]
pub fn specs_from_registry(registry: &ToolRegistry) -> Vec<ToolSpec> {
    registry
        .descriptors()
        .into_iter()
        .filter_map(|descriptor| {
            let name = descriptor.get("name")?.as_str()?.to_owned();
            let description = descriptor
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let parameters = descriptor
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            Some(ToolSpec::new(name, description, parameters))
        })
        .collect()
}

/// Runs the tool loop for one user message.
///
/// Appends `user` to `messages`, then samples until the model returns no tool
/// calls or [`TurnOptions::max_turns`] is exhausted. Tool execution errors are
/// returned to the model as `{"error": …}` results, not as [`AgentError`].
///
/// `on_event` is invoked around each tool call so a TUI can render progress.
///
/// # Errors
///
/// Returns [`AgentError::Llm`] on transport/API failures,
/// [`AgentError::EmptyResponse`] when bounded retries keep returning empty
/// completions, [`AgentError::TurnLimit`] when the model never stops calling
/// tools, or [`AgentError::Invalid`] when `max_turns` is zero.
pub async fn run_turn<F>(
    client: &LlmClient,
    registry: &ToolRegistry,
    messages: &mut Vec<ChatMessage>,
    user: impl Into<String>,
    options: TurnOptions,
    on_event: F,
) -> Result<String, AgentError>
where
    F: FnMut(AgentEvent),
{
    if options.max_turns == 0 {
        return Err(AgentError::Invalid("max_turns must be at least 1".into()));
    }
    messages.push(ChatMessage::user(user.into()));
    drive_turn(client, registry, messages, options, on_event).await
}

async fn drive_turn<F>(
    client: &LlmClient,
    registry: &ToolRegistry,
    messages: &mut Vec<ChatMessage>,
    options: TurnOptions,
    mut on_event: F,
) -> Result<String, AgentError>
where
    F: FnMut(AgentEvent),
{
    if options.max_turns == 0 {
        return Err(AgentError::Invalid("max_turns must be at least 1".into()));
    }
    let specs = specs_from_registry(registry);

    for _ in 0..options.max_turns {
        let response = sample_agent_completion(client, messages, &specs).await?;
        if response.tool_calls.is_empty() {
            messages.push(response.assistant_message());
            return Ok(response.content);
        }
        let tool_calls = response.tool_calls.clone();
        messages.push(response.assistant_message());
        for call in tool_calls {
            on_event(AgentEvent::ToolStart {
                id: call.id.clone(),
                name: call.name.clone(),
            });
            let result = execute_tool(registry, &call).await;
            on_event(AgentEvent::ToolEnd {
                id: call.id.clone(),
                name: call.name.clone(),
                result: result.clone(),
            });
            messages.push(ChatMessage::tool(call.id, result));
        }
    }
    Err(AgentError::TurnLimit(options.max_turns))
}

async fn sample_agent_completion(
    client: &LlmClient,
    messages: &[ChatMessage],
    specs: &[ToolSpec],
) -> Result<sui_llm::ChatResponse, AgentError> {
    let mut empty_responses = 0;
    loop {
        match client.chat_with_tools(messages, specs).await {
            Ok(response) => return Ok(response),
            Err(LlmError::EmptyResponse) => {
                empty_responses += 1;
                if empty_responses == EMPTY_RESPONSE_MAX_ATTEMPTS {
                    return Err(AgentError::EmptyResponse(EMPTY_RESPONSE_MAX_ATTEMPTS));
                }
            },
            Err(error) => return Err(AgentError::from(error)),
        }
    }
}

async fn execute_tool(
    registry: &ToolRegistry,
    call: &ToolCall,
) -> String {
    let args = match serde_json::from_str::<Value>(&call.arguments) {
        Ok(Value::Object(map)) => Value::Object(map),
        Ok(_) => {
            return error_json("tool arguments must be a JSON object");
        },
        Err(error) => {
            return error_json(&format!("invalid tool arguments JSON: {error}"));
        },
    };
    match registry.call(&call.name, args).await {
        Ok(value) => truncate_result(&value.to_string()),
        Err(error) => error_json(&error.to_string()),
    }
}

fn error_json(message: &str) -> String {
    json!({ "error": message }).to_string()
}

fn truncate_result(text: &str) -> String {
    if text.chars().count() <= MAX_TOOL_RESULT_CHARS {
        return text.to_owned();
    }
    let suffix = "\n…(truncated)";
    let limit = MAX_TOOL_RESULT_CHARS.saturating_sub(suffix.chars().count());
    let truncated: String = text.chars().take(limit).collect();
    format!("{truncated}{suffix}")
}

/// Convenience: [`run_turn`] with no event callback.
///
/// # Errors
///
/// Same as [`run_turn`].
pub async fn run_turn_quiet(
    client: &LlmClient,
    registry: &ToolRegistry,
    messages: &mut Vec<ChatMessage>,
    user: impl Into<String>,
    options: TurnOptions,
) -> Result<String, AgentError> {
    run_turn(client, registry, messages, user, options, |_| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sui_llm::{LlmConfig, Role};
    use sui_tools::{Tool, ToolFuture, ToolsError};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, method, path},
    };

    struct EchoTool;

    impl Tool for EchoTool {
        #[allow(clippy::unnecessary_literal_bound)]
        fn name(&self) -> &'static str {
            "echo"
        }

        #[allow(clippy::unnecessary_literal_bound)]
        fn description(&self) -> &'static str {
            "Echo text"
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"],
                "additionalProperties": false
            })
        }

        fn call(
            &self,
            args: Value,
        ) -> ToolFuture<'_> {
            Box::pin(async move {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolsError::InvalidArgs("text required".into()))?;
                Ok(json!({ "text": text }))
            })
        }
    }

    fn client_for(
        server: &MockServer,
        model: &str,
    ) -> LlmClient {
        let config = LlmConfig::new(server.uri(), "test-key", model).expect("config");
        LlmClient::new(&config)
    }

    fn echo_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTool);
        registry
    }

    #[test]
    fn system_prompt_describes_unified_diff_edit_contract() {
        let prompt = system_prompt(Path::new("/tmp/worktree"));
        assert!(prompt.contains("unified diff"));
        assert!(prompt.contains("diff --git a/path b/path"));
        assert!(prompt.contains("/dev/null"));
    }

    fn empty_completion() -> serde_json::Value {
        json!({
            "id": "empty",
            "object": "chat.completion",
            "created": 1,
            "model": "proxy-model",
            "choices": []
        })
    }

    fn text_completion(text: &str) -> serde_json::Value {
        json!({
            "id": "text",
            "object": "chat.completion",
            "created": 1,
            "model": "proxy-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": text },
                "finish_reason": "stop"
            }]
        })
    }

    #[test]
    fn specs_from_registry_maps_descriptors() {
        let specs = specs_from_registry(&echo_registry());
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "echo");
        assert_eq!(specs[0].description, "Echo text");
        assert_eq!(specs[0].parameters["required"], json!(["text"]));
    }

    #[test]
    fn truncate_result_leaves_short_text() {
        assert_eq!(truncate_result("ok"), "ok");
    }

    #[test]
    fn truncate_result_caps_long_text() {
        let long = "x".repeat(MAX_TOOL_RESULT_CHARS + 8);
        let out = truncate_result(&long);
        assert!(out.ends_with("…(truncated)"));
        assert!(out.chars().count() <= MAX_TOOL_RESULT_CHARS);
        assert!(out.chars().count() < long.chars().count());
    }

    #[tokio::test]
    async fn run_turn_executes_tool_then_returns_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [{ "role": "user", "content": "say hi" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "echo",
                                "arguments": "{\"text\":\"hi\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .expect(1)
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [
                    { "role": "user", "content": "say hi" },
                    {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call_1",
                            "function": { "name": "echo", "arguments": "{\"text\":\"hi\"}" }
                        }]
                    },
                    { "role": "tool", "tool_call_id": "call_1" }
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c2",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "hi"
                    },
                    "finish_reason": "stop"
                }]
            })))
            .expect(1)
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let registry = echo_registry();
        let mut messages = Vec::new();
        let mut events = Vec::new();
        let text = run_turn(
            &client,
            &registry,
            &mut messages,
            "say hi",
            TurnOptions::default(),
            |event| events.push(event),
        )
        .await
        .expect("turn");
        assert_eq!(text, "hi");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "say hi");
        assert_eq!(messages[1].tool_calls[0].name, "echo");
        assert!(messages[2].content.contains("hi"));
        assert_eq!(messages[3].content, "hi");
        assert!(matches!(
            &events[..],
            [
                AgentEvent::ToolStart { name, .. },
                AgentEvent::ToolEnd { name: end, .. }
            ] if name == "echo" && end == "echo"
        ));
    }

    #[tokio::test]
    async fn run_turn_unknown_tool_is_fed_back_not_fatal() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [{ "role": "user", "content": "x" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_x",
                            "type": "function",
                            "function": {
                                "name": "nope",
                                "arguments": "{}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [
                    { "role": "user" },
                    { "role": "assistant" },
                    { "role": "tool", "tool_call_id": "call_x" }
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c2",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "gave up" },
                    "finish_reason": "stop"
                }]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let text = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "x",
            TurnOptions::default(),
        )
        .await
        .expect("turn");
        assert_eq!(text, "gave up");
        assert!(messages[2].content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn run_turn_invalid_arguments_json_is_fed_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [{ "role": "user", "content": "x" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_bad",
                            "type": "function",
                            "function": {
                                "name": "echo",
                                "arguments": "[1,2]"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c2",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "fixed" },
                    "finish_reason": "stop"
                }]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let text = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "x",
            TurnOptions::default(),
        )
        .await
        .expect("turn");
        assert_eq!(text, "fixed");
        assert!(
            messages[2].content.contains("JSON object"),
            "result={}",
            messages[2].content
        );
    }

    #[tokio::test]
    async fn run_turn_hits_limit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "echo",
                                "arguments": "{\"text\":\"loop\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let err = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "loop",
            TurnOptions { max_turns: 1 },
        )
        .await
        .expect_err("limit");
        assert!(matches!(err, AgentError::TurnLimit(1)));
    }

    #[tokio::test]
    async fn run_turn_retries_transient_empty_then_succeeds_on_last_attempt() {
        let server = MockServer::start().await;
        assert_eq!(EMPTY_RESPONSE_MAX_ATTEMPTS, 3);
        let empty_attempts = u64::try_from(EMPTY_RESPONSE_MAX_ATTEMPTS - 1).expect("fits");
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [{ "function": { "name": "echo" } }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_completion()))
            .up_to_n_times(empty_attempts)
            .expect(empty_attempts)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [{ "function": { "name": "echo" } }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(text_completion("recovered")))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let text = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "hi",
            TurnOptions::default(),
        )
        .await
        .expect("turn");
        assert_eq!(text, "recovered");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "recovered");
    }

    #[tokio::test]
    async fn run_turn_retries_empty_between_tool_rounds_and_preserves_context() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [{ "role": "user", "content": "go" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "c1",
                "object": "chat.completion",
                "created": 1,
                "model": "proxy-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "echo",
                                "arguments": "{\"text\":\"hi\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [
                    { "role": "user", "content": "go" },
                    {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call_1",
                            "function": { "name": "echo", "arguments": "{\"text\":\"hi\"}" }
                        }]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "call_1",
                        "content": "{\"text\":\"hi\"}"
                    }
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_completion()))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [
                    { "role": "user", "content": "go" },
                    {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call_1",
                            "function": { "name": "echo", "arguments": "{\"text\":\"hi\"}" }
                        }]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "call_1",
                        "content": "{\"text\":\"hi\"}"
                    }
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(text_completion("done")))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let text = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "go",
            TurnOptions::default(),
        )
        .await
        .expect("turn");
        assert_eq!(text, "done");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "go");
        assert_eq!(messages[1].tool_calls[0].name, "echo");
        assert_eq!(messages[2].role, Role::Tool);
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(messages[2].content.contains("hi"));
        assert_eq!(messages[3].content, "done");
    }

    #[tokio::test]
    async fn run_turn_surfaces_empty_response_after_exhaustion() {
        let server = MockServer::start().await;
        let attempts = u64::try_from(EMPTY_RESPONSE_MAX_ATTEMPTS).expect("fits");
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [{ "function": { "name": "echo" } }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_completion()))
            .up_to_n_times(attempts)
            .expect(attempts)
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let err = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "hi",
            TurnOptions::default(),
        )
        .await
        .expect_err("empty");
        assert!(
            matches!(err, AgentError::EmptyResponse(n) if n == EMPTY_RESPONSE_MAX_ATTEMPTS),
            "{err:?}"
        );
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn run_turn_does_not_retry_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {
                    "message": "invalid key",
                    "type": "auth_error",
                    "param": null,
                    "code": null
                }
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let err = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "hi",
            TurnOptions::default(),
        )
        .await
        .expect_err("api error");
        assert!(matches!(err, AgentError::Llm(LlmError::Api(_))), "{err:?}");
    }

    #[tokio::test]
    async fn run_turn_stops_retrying_when_api_error_follows_empty() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_completion()))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {
                    "message": "invalid key",
                    "type": "auth_error",
                    "param": null,
                    "code": null
                }
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server, "proxy-model");
        let mut messages = Vec::new();
        let err = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "hi",
            TurnOptions::default(),
        )
        .await
        .expect_err("api error after empty");
        assert!(matches!(err, AgentError::Llm(LlmError::Api(_))), "{err:?}");
    }

    #[tokio::test]
    async fn run_turn_rejects_zero_max_turns() {
        let config = LlmConfig::new("http://localhost:4000", "k", "m").expect("config");
        let client = LlmClient::new(&config);
        let mut messages = Vec::new();
        let err = run_turn_quiet(
            &client,
            &echo_registry(),
            &mut messages,
            "x",
            TurnOptions { max_turns: 0 },
        )
        .await
        .expect_err("zero");
        assert!(matches!(err, AgentError::Invalid(_)));
        assert!(messages.is_empty());
    }
}
