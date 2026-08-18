use std::pin::Pin;

use futures::{Stream, StreamExt};
use serde_json::{Value, json};

use crate::config::ApiMode;
use crate::error::{api_error, api_message, api_status};
use crate::{ChatMessage, LlmConfig, LlmError, Role, ToolCall, ToolSpec};

/// Successful non-streaming chat response.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatResponse {
    /// Assistant text from the first response (empty when the model only calls tools).
    pub content: String,
    /// Model id returned by the API.
    pub model: String,
    /// Function calls the client must execute before the next sample.
    pub tool_calls: Vec<ToolCall>,
    responses_output: Option<Vec<Value>>,
}

impl ChatResponse {
    /// Builds a text-only response from assistant text and a model id.
    #[must_use]
    pub fn new(
        content: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            content: content.into(),
            model: model.into(),
            tool_calls: Vec::new(),
            responses_output: None,
        }
    }

    /// Attaches tool calls to this response.
    #[must_use]
    pub fn with_tool_calls(
        mut self,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        self.tool_calls = tool_calls;
        self
    }

    /// Converts this response into the assistant history message expected by
    /// the next tool-loop sample.
    ///
    /// Responses-mode replies retain their complete raw output items here,
    /// including reasoning and provider metadata that [`ChatMessage`] cannot
    /// represent as ordinary chat fields. Chat Completions replies use the
    /// normal assistant/tool-call representation.
    #[must_use]
    pub fn assistant_message(&self) -> ChatMessage {
        self.responses_output.as_ref().map_or_else(
            || ChatMessage::assistant_tools(self.content.clone(), self.tool_calls.clone()),
            |output| {
                if output.is_empty() {
                    ChatMessage::assistant_tools(self.content.clone(), self.tool_calls.clone())
                } else {
                    ChatMessage::assistant_with_responses_output(
                        self.content.clone(),
                        self.tool_calls.clone(),
                        output.clone(),
                    )
                }
            },
        )
    }
}

/// One streamed text delta.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatChunk {
    /// Incremental assistant text, which may be empty on control chunks.
    pub delta: String,
    responses_output: Option<Vec<Value>>,
}

impl ChatChunk {
    /// Builds a chunk from an incremental text delta.
    #[must_use]
    pub fn new(delta: impl Into<String>) -> Self {
        Self {
            delta: delta.into(),
            responses_output: None,
        }
    }

    /// Returns the complete Responses output history carried by the final
    /// control chunk, when the server supplied it on `response.completed`.
    #[must_use]
    pub fn assistant_message(
        &self,
        content: impl Into<String>,
    ) -> Option<ChatMessage> {
        self.responses_output
            .clone()
            .map(|output| ChatMessage::assistant_with_responses_output(content, Vec::new(), output))
    }

    const fn with_responses_output(output: Vec<Value>) -> Self {
        Self {
            delta: String::new(),
            responses_output: Some(output),
        }
    }
}

/// Owned stream of chat chunks from [`LlmClient::chat_stream`].
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatChunk, LlmError>> + Send>>;

/// Direct OpenAI-compatible HTTP client.
///
/// The client supports both Chat Completions and Responses wire formats. The
/// configured [`ApiMode`] selects the endpoint for all calls; Chat Completions
/// remains the default for compatibility with existing callers.
///
/// This crate does not set an HTTP timeout. Wrap awaits with your runtime's
/// timeout helper when a deadline is required.
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    default_model: String,
    api_mode: ApiMode,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("base_url", &self.base_url)
            .field("default_model", &self.default_model)
            .field("api_mode", &self.api_mode)
            .finish_non_exhaustive()
    }
}

impl LlmClient {
    /// Builds a client from [`LlmConfig`].
    #[must_use]
    pub fn new(config: &LlmConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: config.base_url().to_owned(),
            api_key: config.api_key().to_owned(),
            default_model: config.model().to_owned(),
            api_mode: config.api_mode(),
        }
    }

    /// Builds a client from `SUI_LLM_*` environment variables.
    ///
    /// # Errors
    ///
    /// Propagates [`LlmConfig::from_env`] failures.
    pub fn from_env() -> Result<Self, LlmError> {
        Ok(Self::new(&LlmConfig::from_env()?))
    }

    /// Builds a client from the repository's `config.toml` convention, falling
    /// back to `SUI_LLM_*` environment variables when no `[llm]` section is
    /// configured.
    ///
    /// # Errors
    ///
    /// Propagates configuration-file, environment, and validation failures.
    pub fn from_config_or_env() -> Result<Self, LlmError> {
        Ok(Self::new(&LlmConfig::from_config_or_env()?))
    }

    /// Default model used when callers omit an override.
    #[must_use]
    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// API mode used by this client.
    #[must_use]
    pub const fn api_mode(&self) -> ApiMode {
        self.api_mode
    }

    /// Non-streaming chat using the configured default model.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidArgument`] for empty messages,
    /// transport/API errors, [`LlmError::EmptyResponse`], or
    /// [`LlmError::Refused`].
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
    ) -> Result<ChatResponse, LlmError> {
        self.complete(&self.default_model, messages, &[]).await
    }

    /// Non-streaming chat with an explicit model name.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidArgument`] for an empty model or messages,
    /// transport/API errors, [`LlmError::EmptyResponse`], or
    /// [`LlmError::Refused`].
    pub async fn chat_with_model(
        &self,
        model: &str,
        messages: &[ChatMessage],
    ) -> Result<ChatResponse, LlmError> {
        self.complete(model, messages, &[]).await
    }

    /// Non-streaming chat that advertises `tools` to the model.
    ///
    /// When the model returns [`ChatResponse::tool_calls`], the caller must
    /// execute them locally, append [`ChatMessage::assistant_tools`] plus
    /// [`ChatMessage::tool`] results, and sample again.
    ///
    /// # Errors
    ///
    /// Same as [`Self::chat`]. Empty assistant text is allowed when tool calls
    /// are present.
    pub async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ChatResponse, LlmError> {
        self.complete(&self.default_model, messages, tools).await
    }

    /// Non-streaming chat with an explicit model and advertised tools.
    ///
    /// # Errors
    ///
    /// Same as [`Self::chat_with_tools`], plus invalid empty model names.
    pub async fn chat_with_model_and_tools(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ChatResponse, LlmError> {
        self.complete(model, messages, tools).await
    }

    async fn complete(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ChatResponse, LlmError> {
        let model = require_model_argument(model)?;
        require_non_empty_messages(messages)?;
        let body = request_body(self.api_mode, model, messages, tools, false);
        let value = self.post_json(&body).await?;
        match self.api_mode {
            ApiMode::ChatCompletions => map_chat_response(&value, model),
            ApiMode::Responses => map_responses_response(&value, model),
        }
    }

    /// Streaming chat using the configured default model.
    ///
    /// Drive the returned stream with [`futures::StreamExt`]. Control chunks
    /// may have an empty `delta`; transport and parse failures are yielded as
    /// stream items.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidArgument`] for empty messages or an API error
    /// while opening the stream.
    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage],
    ) -> Result<ChatStream, LlmError> {
        self.chat_stream_with_model(&self.default_model, messages)
            .await
    }

    /// Streaming chat with an explicit model name.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::InvalidArgument`] for an empty model or messages,
    /// or an API error while opening the stream. Per-event errors are yielded
    /// on the returned stream.
    pub async fn chat_stream_with_model(
        &self,
        model: &str,
        messages: &[ChatMessage],
    ) -> Result<ChatStream, LlmError> {
        let model = require_model_argument(model)?;
        require_non_empty_messages(messages)?;
        let body = request_body(self.api_mode, model, messages, &[], true);
        let response = self.post_stream(&body).await?;
        Ok(Box::pin(decode_sse(response, self.api_mode)))
    }

    async fn post_json(
        &self,
        body: &Value,
    ) -> Result<Value, LlmError> {
        let response = self.request(body).send().await?;
        if !response.status().is_success() {
            return Err(api_status(response.status()));
        }
        self.read_bounded_json(response).await
    }

    /// Reads and JSON-parses a non-streaming response body with a hard size
    /// ceiling so a misbehaving server cannot exhaust memory.
    async fn read_bounded_json(
        &self,
        response: reqwest::Response,
    ) -> Result<Value, LlmError> {
        let mut bytes = Vec::with_capacity(4096);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
                return Err(LlmError::InvalidResponse(
                    "API response exceeded its size limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(LlmError::from)
    }

    async fn post_stream(
        &self,
        body: &Value,
    ) -> Result<reqwest::Response, LlmError> {
        let response = self
            .request(body)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(api_status(response.status()));
        }
        if !is_event_stream(response.headers()) {
            return Err(LlmError::InvalidResponse(
                "stream response must have content type text/event-stream",
            ));
        }
        Ok(response)
    }

    fn request(
        &self,
        body: &Value,
    ) -> reqwest::RequestBuilder {
        let endpoint = match self.api_mode {
            ApiMode::ChatCompletions => "chat/completions",
            ApiMode::Responses => "responses",
        };
        let url = format!("{}/{endpoint}", self.base_url.trim_end_matches('/'));
        self.http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key),
            )
            .json(&body)
    }
}

fn require_model_argument(model: &str) -> Result<&str, LlmError> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err(LlmError::InvalidArgument(
            "model must be a non-empty string".into(),
        ));
    }
    Ok(trimmed)
}

fn require_non_empty_messages(messages: &[ChatMessage]) -> Result<(), LlmError> {
    if messages.is_empty() {
        return Err(LlmError::InvalidArgument(
            "messages must not be empty".into(),
        ));
    }
    Ok(())
}

fn request_body(
    mode: ApiMode,
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    stream: bool,
) -> Value {
    match mode {
        ApiMode::ChatCompletions => {
            let mut body = json!({
                "model": model,
                "messages": messages.iter().map(chat_message_to_chat).collect::<Vec<_>>(),
            });
            if stream {
                body["stream"] = Value::Bool(true);
            }
            if !tools.is_empty() {
                body["tools"] = Value::Array(tools.iter().map(tool_to_chat).collect());
            }
            body
        },
        ApiMode::Responses => {
            let mut body = json!({
                "model": model,
                "input": messages.iter().flat_map(chat_message_to_response).collect::<Vec<_>>(),
                "store": false,
            });
            if stream {
                body["stream"] = Value::Bool(true);
            }
            if !tools.is_empty() {
                body["tools"] = Value::Array(tools.iter().map(tool_to_response).collect());
            }
            body
        },
    }
}

fn chat_message_to_chat(message: &ChatMessage) -> Value {
    match message.role {
        Role::System => json!({ "role": "system", "content": message.content }),
        Role::User => json!({ "role": "user", "content": message.content }),
        Role::Assistant => {
            let mut value = json!({ "role": "assistant" });
            if !message.content.is_empty() {
                value["content"] = Value::String(message.content.clone());
            }
            if !message.tool_calls.is_empty() {
                value["tool_calls"] =
                    Value::Array(message.tool_calls.iter().map(tool_call_to_chat).collect());
            }
            value
        },
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.as_deref().unwrap_or_default(),
            "content": message.content,
        }),
    }
}

fn chat_message_to_response(message: &ChatMessage) -> Vec<Value> {
    let mut items = Vec::with_capacity(1 + message.tool_calls.len());
    match message.role {
        Role::System => {
            items.push(json!({ "role": "system", "content": message.content }));
        },
        Role::User => {
            items.push(json!({ "role": "user", "content": message.content }));
        },
        Role::Assistant => {
            if let Some(output) = &message.responses_output
                && !output.is_empty()
            {
                return output.clone();
            }
            if !message.content.is_empty() {
                items.push(json!({ "role": "assistant", "content": message.content }));
            }
            items.extend(message.tool_calls.iter().map(tool_call_to_response));
        },
        Role::Tool => items.push(json!({
            "type": "function_call_output",
            "call_id": message.tool_call_id.as_deref().unwrap_or_default(),
            "output": message.content,
        })),
    }
    items
}

fn tool_to_chat(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

fn tool_to_response(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

fn tool_call_to_chat(call: &ToolCall) -> Value {
    json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": call.arguments,
        },
    })
}

fn tool_call_to_response(call: &ToolCall) -> Value {
    json!({
        "type": "function_call",
        "call_id": call.id,
        "name": call.name,
        "arguments": call.arguments,
    })
}

fn map_chat_response(
    value: &Value,
    default_model: &str,
) -> Result<ChatResponse, LlmError> {
    reject_api_error(value)?;
    let model = response_model(value, default_model);
    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return Err(LlmError::EmptyResponse);
    };
    let message = choice.get("message").unwrap_or(&Value::Null);
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str)
        && !refusal.is_empty()
    {
        return Err(LlmError::Refused(refusal.to_owned()));
    }
    let content = text_value(message.get("content")).unwrap_or_default();
    let tool_calls = match message.get("tool_calls") {
        Some(calls) => calls
            .as_array()
            .ok_or(LlmError::InvalidResponse(
                "Chat `tool_calls` must be an array",
            ))?
            .iter()
            .map(map_tool_call)
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    if content.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::EmptyResponse);
    }
    Ok(ChatResponse {
        content,
        model,
        tool_calls,
        responses_output: None,
    })
}

fn map_responses_response(
    value: &Value,
    default_model: &str,
) -> Result<ChatResponse, LlmError> {
    reject_api_error(value)?;
    validate_responses_status(value)?;
    let model = response_model(value, default_model);
    let mut content = value
        .get("output_text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let has_output_text = !content.is_empty();
    let mut tool_calls = Vec::new();
    let mut refusal = None;
    let raw_output = match value.get("output") {
        Some(output) => Some(output.as_array().ok_or(LlmError::InvalidResponse(
            "Responses `output` must be an array",
        ))?),
        None => None,
    };
    if let Some(output) = raw_output {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    tool_calls.push(map_response_tool_call(item)?);
                },
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            match part.get("type").and_then(Value::as_str) {
                                Some("output_text") if !has_output_text => {
                                    content.push_str(
                                        part.get("text")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default(),
                                    );
                                },
                                Some("refusal") => {
                                    refusal = part
                                        .get("refusal")
                                        .or_else(|| part.get("text"))
                                        .and_then(Value::as_str)
                                        .map(str::to_owned);
                                },
                                _ => {},
                            }
                        }
                    }
                },
                _ => {},
            }
        }
    }
    if let Some(refusal) = refusal.filter(|value| !value.is_empty()) {
        return Err(LlmError::Refused(refusal));
    }
    if content.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::EmptyResponse);
    }
    Ok(ChatResponse {
        content,
        model,
        tool_calls,
        responses_output: raw_output.map(ToOwned::to_owned),
    })
}

fn validate_responses_status(value: &Value) -> Result<(), LlmError> {
    if let Some(incomplete) = value.get("incomplete") {
        match incomplete {
            Value::Bool(true) => return Err(LlmError::IncompleteResponse),
            Value::Bool(false) | Value::Null => {},
            _ => {
                return Err(LlmError::InvalidResponse(
                    "Responses `incomplete` must be a boolean",
                ));
            },
        }
    }
    match value.get("status").and_then(Value::as_str) {
        None | Some("completed") => Ok(()),
        Some("incomplete" | "in_progress" | "queued") => Err(LlmError::IncompleteResponse),
        Some("failed") => Err(api_message("Responses API returned a failed response")),
        Some(_) => Err(LlmError::InvalidResponse(
            "Responses response has an unknown status",
        )),
    }
}

fn response_model(
    value: &Value,
    default_model: &str,
) -> String {
    value
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or(default_model)
        .to_owned()
}

fn reject_api_error(value: &Value) -> Result<(), LlmError> {
    if value.get("error").is_some_and(|error| !error.is_null()) {
        return Err(api_message("API returned an error response"));
    }
    Ok(())
}

fn map_tool_call(value: &Value) -> Result<ToolCall, LlmError> {
    let function = value
        .get("function")
        .and_then(Value::as_object)
        .ok_or(LlmError::InvalidResponse("Chat tool call has no function"))?;
    Ok(ToolCall::new(
        required_string(value, "id", "Chat tool call is missing id")?,
        required_string_value(function, "name", "Chat tool call is missing function name")?,
        required_string_value(
            function,
            "arguments",
            "Chat tool call is missing function arguments",
        )?,
    ))
}

fn map_response_tool_call(value: &Value) -> Result<ToolCall, LlmError> {
    Ok(ToolCall::new(
        required_string(
            value,
            "call_id",
            "Responses function call is missing call_id",
        )?,
        required_string(value, "name", "Responses function call is missing name")?,
        required_string(
            value,
            "arguments",
            "Responses function call is missing arguments",
        )?,
    ))
}

fn required_string<'a>(
    value: &'a Value,
    key: &str,
    message: &'static str,
) -> Result<&'a str, LlmError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(LlmError::InvalidResponse(message))
}

fn required_string_value<'a>(
    value: &'a serde_json::Map<String, Value>,
    key: &str,
    message: &'static str,
) -> Result<&'a str, LlmError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(LlmError::InvalidResponse(message))
}

fn text_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect(),
        ),
        _ => None,
    }
}

const MAX_JSON_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SSE_BUFFER_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

/// Incremental Server-Sent Events parser.
///
/// Follows the [`EventSource`] field contract plus the extra `data:` convention
/// used by `OpenAI`: consecutive `data:` lines are joined with `\n`, comments
/// and unknown fields are ignored, and an event is dispatched on an empty line
/// or at EOF. LF, CRLF, and CR-only line endings are all treated as the line
/// terminator. A lone CR held at the end of the buffer is awaited so a CRLF
/// split across chunk boundaries is not misread; the same CR then terminates
/// the line once EOF (or the following byte) confirms it — so a pure `\r\r`
/// blank line mid-stream is occasionally deferred to EOF.
#[derive(Debug)]
struct SseDecoder {
    buffer: Vec<u8>,
    data: Vec<String>,
    data_len: usize,
}

impl SseDecoder {
    fn push(
        &mut self,
        bytes: &[u8],
    ) -> Result<(), LlmError> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_SSE_BUFFER_BYTES {
            return Err(LlmError::InvalidResponse(
                "SSE input exceeded its size limit",
            ));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn next_event(
        &mut self,
        eof: bool,
    ) -> Result<Option<String>, LlmError> {
        loop {
            let line = if let Some((index, terminator_len)) =
                self.buffer.iter().enumerate().find_map(|(index, byte)| {
                    if *byte == b'\r' {
                        if self.buffer.get(index + 1).is_none() && !eof {
                            return None;
                        }
                        Some((
                            index,
                            usize::from(self.buffer.get(index + 1) == Some(&b'\n')) + 1,
                        ))
                    } else if *byte == b'\n' {
                        Some((index, 1))
                    } else {
                        None
                    }
                }) {
                let line = self.buffer.drain(..index).collect::<Vec<_>>();
                let _ = self.buffer.drain(..terminator_len);
                line
            } else if eof && !self.buffer.is_empty() {
                std::mem::take(&mut self.buffer)
            } else if eof && !self.data.is_empty() {
                return Ok(Some(self.take_data()));
            } else {
                return Ok(None);
            };
            let line = std::str::from_utf8(&line).map_err(api_error)?;
            if line.is_empty() {
                if !self.data.is_empty() {
                    return Ok(Some(self.take_data()));
                }
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.strip_prefix(' ').unwrap_or(data);
            if self
                .data_len
                .saturating_add(data.len())
                .saturating_add(usize::from(!self.data.is_empty()))
                > MAX_SSE_EVENT_BYTES
            {
                return Err(LlmError::InvalidResponse(
                    "SSE event exceeded its size limit",
                ));
            }
            self.data_len = self
                .data_len
                .saturating_add(data.len())
                .saturating_add(usize::from(!self.data.is_empty()));
            self.data.push(data.to_owned());
        }
    }

    fn take_data(&mut self) -> String {
        self.data_len = 0;
        self.data.drain(..).collect::<Vec<_>>().join("\n")
    }
}

#[derive(Debug)]
enum SseEvent {
    Chunk(ChatChunk),
    Done(Option<Vec<Value>>),
}

fn decode_sse(
    response: reqwest::Response,
    mode: ApiMode,
) -> impl Stream<Item = Result<ChatChunk, LlmError>> + Send {
    futures::stream::unfold(
        (
            response.bytes_stream(),
            SseDecoder {
                buffer: Vec::new(),
                data: Vec::new(),
                data_len: 0,
            },
            false,
            false,
        ),
        move |(mut bytes, mut decoder, mut done, mut saw_event)| async move {
            if done {
                return None;
            }
            loop {
                match decoder.next_event(false) {
                    Ok(Some(event)) => match map_sse_event(&event, mode) {
                        Ok(SseEvent::Chunk(chunk)) => {
                            saw_event = true;
                            return Some((Ok(chunk), (bytes, decoder, done, saw_event)));
                        },
                        Ok(SseEvent::Done(output)) => {
                            done = true;
                            return output.map(|output| {
                                (
                                    Ok(ChatChunk::with_responses_output(output)),
                                    (bytes, decoder, done, true),
                                )
                            });
                        },
                        Err(error) => {
                            done = true;
                            return Some((Err(error), (bytes, decoder, done, saw_event)));
                        },
                    },
                    Ok(None) => {},
                    Err(error) => {
                        done = true;
                        return Some((Err(error), (bytes, decoder, done, saw_event)));
                    },
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => {
                        if let Err(error) = decoder.push(chunk.as_ref()) {
                            done = true;
                            return Some((Err(error), (bytes, decoder, done, saw_event)));
                        }
                    },
                    Some(Err(error)) => {
                        done = true;
                        return Some((Err(error.into()), (bytes, decoder, done, saw_event)));
                    },
                    None => match decoder.next_event(true) {
                        Ok(Some(event)) => match map_sse_event(&event, mode) {
                            Ok(SseEvent::Chunk(chunk)) => {
                                done = true;
                                return Some((Ok(chunk), (bytes, decoder, done, true)));
                            },
                            Ok(SseEvent::Done(output)) => {
                                done = true;
                                return output.map(|output| {
                                    (
                                        Ok(ChatChunk::with_responses_output(output)),
                                        (bytes, decoder, done, true),
                                    )
                                });
                            },
                            Err(error) => {
                                done = true;
                                return Some((Err(error), (bytes, decoder, done, saw_event)));
                            },
                        },
                        Ok(None) if saw_event => return None,
                        Ok(None) => {
                            done = true;
                            return Some((
                                Err(LlmError::InvalidResponse("empty SSE stream")),
                                (bytes, decoder, done, saw_event),
                            ));
                        },
                        Err(error) => {
                            done = true;
                            return Some((Err(error), (bytes, decoder, done, saw_event)));
                        },
                    },
                }
            }
        },
    )
}

fn map_sse_event(
    event: &str,
    mode: ApiMode,
) -> Result<SseEvent, LlmError> {
    if event.trim() == "[DONE]" {
        return Ok(SseEvent::Done(None));
    }
    let value: Value = serde_json::from_str(event)?;
    if value.get("error").is_some() {
        return Err(api_message("API returned an error event"));
    }
    match mode {
        ApiMode::ChatCompletions => {
            let choice = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first());
            let Some(choice) = choice else {
                if value.get("choices").and_then(Value::as_array).is_some() {
                    return Ok(SseEvent::Chunk(ChatChunk::new(String::new())));
                }
                return Err(LlmError::InvalidResponse(
                    "Chat SSE event is missing choices",
                ));
            };
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(refusal) = delta.get("refusal").and_then(Value::as_str)
                && !refusal.is_empty()
            {
                return Err(LlmError::Refused(refusal.to_owned()));
            }
            Ok(SseEvent::Chunk(ChatChunk::new(
                text_value(delta.get("content")).unwrap_or_default(),
            )))
        },
        ApiMode::Responses => match value.get("type").and_then(Value::as_str) {
            Some("response.completed") => {
                let output = value
                    .get("response")
                    .map(|response| {
                        response
                            .get("output")
                            .ok_or(LlmError::InvalidResponse(
                                "Responses completed event is missing output",
                            ))?
                            .as_array()
                            .cloned()
                            .ok_or(LlmError::InvalidResponse(
                                "Responses completed output must be an array",
                            ))
                    })
                    .transpose()?;
                Ok(SseEvent::Done(output))
            },
            Some("response.incomplete") => Err(LlmError::IncompleteResponse),
            Some("response.failed" | "error") => Err(api_message("API returned an error event")),
            Some("response.output_text.delta") => Ok(SseEvent::Chunk(ChatChunk::new(
                value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ))),
            Some("response.refusal.delta") => {
                let refusal = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if refusal.is_empty() {
                    Ok(SseEvent::Chunk(ChatChunk::new(String::new())))
                } else {
                    Err(LlmError::Refused(refusal.to_owned()))
                }
            },
            Some(
                "response.created"
                | "response.in_progress"
                | "response.output_item.added"
                | "response.output_item.done"
                | "response.content_part.added"
                | "response.output_text.done"
                | "response.function_call_arguments.delta"
                | "response.function_call_arguments.done",
            ) => Ok(SseEvent::Chunk(ChatChunk::new(String::new()))),
            Some(_) => Err(LlmError::InvalidResponse("unknown Responses SSE event")),
            None => Err(LlmError::InvalidResponse(
                "Responses SSE event is missing type",
            )),
        },
    }
}

fn is_event_stream(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path},
    };

    use super::*;
    use crate::{ApiMode, LlmConfig};

    fn client_for(
        server: &MockServer,
        mode: ApiMode,
    ) -> LlmClient {
        let config =
            LlmConfig::new_with_mode(server.uri(), "test-key", "model", mode).expect("config");
        LlmClient::new(&config)
    }

    #[tokio::test]
    async fn chat_serializes_messages_and_maps_response() {
        let server = MockServer::start().await;
        let expected = json!({
            "model": "model",
            "messages": [
                { "role": "system", "content": "system" },
                { "role": "user", "content": "hello" },
                { "role": "assistant", "content": "prior" },
                { "role": "tool", "tool_call_id": "call_1", "content": "result" }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "returned-model",
                "choices": [{
                    "message": { "content": "answer" }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server, ApiMode::ChatCompletions);
        let response = client
            .chat(&[
                ChatMessage::system("system"),
                ChatMessage::user("hello"),
                ChatMessage::assistant("prior"),
                ChatMessage::tool("call_1", "result"),
            ])
            .await
            .expect("chat");
        assert_eq!(response.content, "answer");
        assert_eq!(response.model, "returned-model");
    }

    #[tokio::test]
    async fn chat_with_tools_serializes_chat_tools_and_maps_calls() {
        let server = MockServer::start().await;
        let tool = ToolSpec::new("echo", "Echo", json!({ "type": "object" }));
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_json(json!({
                "model": "model",
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "echo",
                        "description": "Echo",
                        "parameters": { "type": "object" }
                    }
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "function": { "name": "echo", "arguments": "{\"x\":1}" }
                        }]
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = client_for(&server, ApiMode::ChatCompletions)
            .chat_with_tools(&[ChatMessage::user("hi")], &[tool])
            .await
            .expect("chat");
        assert_eq!(
            response.tool_calls,
            vec![ToolCall::new("call_1", "echo", "{\"x\":1}")]
        );
    }

    #[tokio::test]
    async fn chat_stream_decodes_chunked_sse() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("accept", "text/event-stream"))
            .and(body_json(json!({
                "model": "model",
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": true
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let mut stream = client_for(&server, ApiMode::ChatCompletions)
            .chat_stream(&[ChatMessage::user("hi")])
            .await
            .expect("stream");
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk.expect("chunk").delta);
        }
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn chat_stream_yields_midstream_parse_error() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: {not-json\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let mut stream = client_for(&server, ApiMode::ChatCompletions)
            .chat_stream(&[ChatMessage::user("hi")])
            .await
            .expect("stream");
        assert_eq!(
            stream.next().await.expect("first").expect("chunk").delta,
            "ok"
        );
        let error = stream
            .next()
            .await
            .expect("error")
            .expect_err("parse error");
        assert!(matches!(error, LlmError::Api(_)));
        assert_eq!(error.to_string(), "LLM API error");
    }

    #[tokio::test]
    async fn responses_serializes_input_and_maps_text_and_tool_calls() {
        let server = MockServer::start().await;
        let tool = ToolSpec::new("echo", "Echo", json!({ "type": "object" }));
        let messages = [
            ChatMessage::system("rules"),
            ChatMessage::user("hi"),
            ChatMessage::assistant_tools("", vec![ToolCall::new("call_1", "echo", "{\"x\":1}")]),
            ChatMessage::tool("call_1", "done"),
        ];
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_json(json!({
                "model": "model",
                "input": [
                    { "role": "system", "content": "rules" },
                    { "role": "user", "content": "hi" },
                    { "type": "function_call", "call_id": "call_1", "name": "echo", "arguments": "{\"x\":1}" },
                    { "type": "function_call_output", "call_id": "call_1", "output": "done" }
                ],
                "store": false,
                "tools": [{
                    "type": "function",
                    "name": "echo",
                    "description": "Echo",
                    "parameters": { "type": "object" }
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "gpt-responses",
                "output_text": "answer",
                "output": [{
                    "type": "function_call",
                    "call_id": "call_2",
                    "name": "echo",
                    "arguments": "{\"x\":2}"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = client_for(&server, ApiMode::Responses)
            .chat_with_tools(&messages, &[tool])
            .await
            .expect("responses");
        assert_eq!(response.content, "answer");
        assert_eq!(response.model, "gpt-responses");
        assert_eq!(response.tool_calls[0].id, "call_2");
    }

    #[tokio::test]
    async fn responses_maps_raw_output_message_without_sdk_convenience_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "raw answer" }]
                }]
            })))
            .mount(&server)
            .await;

        let response = client_for(&server, ApiMode::Responses)
            .chat(&[ChatMessage::user("hi")])
            .await
            .expect("responses");
        assert_eq!(response.content, "raw answer");
    }

    #[tokio::test]
    async fn responses_maps_refusal_from_raw_output_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "refusal", "refusal": "not allowed" }]
                }]
            })))
            .mount(&server)
            .await;

        let error = client_for(&server, ApiMode::Responses)
            .chat(&[ChatMessage::user("hi")])
            .await
            .expect_err("refusal");
        assert!(matches!(error, LlmError::Refused(message) if message == "not allowed"));
    }

    #[tokio::test]
    async fn responses_stream_maps_output_text_events() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"type\":\"response.created\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
            "data: {\"type\":\"response.completed\"}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_json(json!({
                "model": "model",
                "input": [{ "role": "user", "content": "hi" }],
                "store": false,
                "stream": true
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let mut stream = client_for(&server, ApiMode::Responses)
            .chat_stream(&[ChatMessage::user("hi")])
            .await
            .expect("stream");
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&chunk.expect("chunk").delta);
        }
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn api_errors_are_opaque_and_status_is_preserved_as_source() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("secret response body"))
            .mount(&server)
            .await;
        let error = client_for(&server, ApiMode::ChatCompletions)
            .chat(&[ChatMessage::user("hi")])
            .await
            .expect_err("api error");
        assert!(matches!(error, LlmError::Api(_)));
        assert_eq!(error.to_string(), "LLM API error");
        assert!(!format!("{error:?}").contains("secret response body"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[tokio::test]
    async fn chat_validates_model_and_messages_before_http() {
        let config = LlmConfig::new("http://localhost:4000", "key", "model").expect("config");
        let client = LlmClient::new(&config);
        assert!(matches!(
            client
                .chat_with_model(" ", &[ChatMessage::user("hi")])
                .await,
            Err(LlmError::InvalidArgument(_))
        ));
        assert!(matches!(
            client.chat(&[]).await,
            Err(LlmError::InvalidArgument(_))
        ));
    }

    #[test]
    fn debug_hides_api_key() {
        let config =
            LlmConfig::new("http://localhost:4000", "super-secret-key", "model").expect("config");
        let client = LlmClient::new(&config);
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("super-secret-key"));
        assert!(!rendered.contains("api_key"));
        assert!(rendered.contains("default_model: \"model\""));
    }

    #[test]
    fn sse_decoder_handles_fragmented_lines() {
        let mut decoder = SseDecoder {
            buffer: Vec::new(),
            data: Vec::new(),
            data_len: 0,
        };
        decoder.push(b"data: {\"a\": \"va").expect("push");
        assert_eq!(decoder.next_event(false).expect("decode"), None);
        decoder.push(b"lue\"}\n\n").expect("push");
        assert_eq!(
            decoder.next_event(false).expect("decode"),
            Some("{\"a\": \"value\"}".into())
        );
    }

    #[test]
    fn sse_decoder_joins_multiline_data_on_blank_line() {
        let mut decoder = SseDecoder {
            buffer: Vec::new(),
            data: Vec::new(),
            data_len: 0,
        };
        decoder
            .push(b"data: first\ndata: second\n\n")
            .expect("push");
        assert_eq!(
            decoder.next_event(false).expect("decode"),
            Some("first\nsecond".into())
        );
    }

    #[test]
    fn sse_decoder_flushes_multiline_data_on_eof_without_terminator() {
        let mut decoder = SseDecoder {
            buffer: Vec::new(),
            data: Vec::new(),
            data_len: 0,
        };
        decoder.push(b"data: one\ndata: two").expect("push");
        assert_eq!(
            decoder.next_event(true).expect("decode"),
            Some("one\ntwo".into())
        );
    }

    #[test]
    fn sse_decoder_handles_crlf_line_endings() {
        let mut decoder = SseDecoder {
            buffer: Vec::new(),
            data: Vec::new(),
            data_len: 0,
        };
        decoder
            .push(b"data: {\"a\":1}\r\n\r\ndata: {\"b\":2}\r\n\r\n")
            .expect("push");
        assert_eq!(
            decoder.next_event(false).expect("decode"),
            Some("{\"a\":1}".into())
        );
        assert_eq!(
            decoder.next_event(false).expect("decode"),
            Some("{\"b\":2}".into())
        );
    }

    #[test]
    fn sse_decoder_handles_cr_only_line_endings() {
        let mut decoder = SseDecoder {
            buffer: Vec::new(),
            data: Vec::new(),
            data_len: 0,
        };
        // CR-only line endings join consecutive data lines into one event. A
        // pure-CR blank line is ambiguous mid-stream with a split CRLF boundary,
        // so the final event is flushed eagerly at EOF.
        decoder.push(b"data: foo\rdata: bar\r\r").expect("push");
        assert_eq!(decoder.next_event(false).expect("decode"), None);
        assert_eq!(
            decoder.next_event(true).expect("decode"),
            Some("foo\nbar".into())
        );
    }

    #[test]
    fn sse_decoder_ignores_comments_and_metadata_fields() {
        let mut decoder = SseDecoder {
            buffer: Vec::new(),
            data: Vec::new(),
            data_len: 0,
        };
        decoder
            .push(b": keep-alive\nevent: message\nretry: 5000\ndata: {\"a\":1}\n\n")
            .expect("push");
        assert_eq!(
            decoder.next_event(false).expect("decode"),
            Some("{\"a\":1}".into())
        );
    }

    #[test]
    fn sse_decoder_enforces_event_and_input_size_limits() {
        let mut decoder = SseDecoder {
            buffer: Vec::new(),
            data: Vec::new(),
            data_len: 0,
        };
        decoder.push(b"data: ").expect("push");
        decoder
            .push(&vec![b'a'; MAX_SSE_EVENT_BYTES + 1])
            .expect("buffer push within global limit");
        decoder.push(b"\n\n").expect("push");
        let error = decoder.next_event(false).expect_err("event too large");
        assert!(matches!(error, LlmError::InvalidResponse(_)));
    }

    #[test]
    fn malformed_sse_event_is_an_opaque_api_error() {
        let error =
            map_sse_event("{not-json", ApiMode::ChatCompletions).expect_err("malformed event");
        assert!(matches!(error, LlmError::Api(_)));
        assert_eq!(error.to_string(), "LLM API error");
        assert!(!format!("{error:?}").contains("not-json"));
    }

    #[test]
    fn successful_error_envelope_is_an_opaque_api_error() {
        let error = map_chat_response(&json!({ "error": { "message": "secret" } }), "model")
            .expect_err("error envelope");
        assert!(matches!(error, LlmError::Api(_)));
        assert_eq!(error.to_string(), "LLM API error");
        assert!(!format!("{error:?}").contains("secret"));
    }

    #[test]
    fn api_error_exposes_http_status() {
        let error = crate::error::api_status(reqwest::StatusCode::UNAUTHORIZED);
        assert!(matches!(&error, LlmError::Api(_)));
        assert_eq!(error.api_status(), Some(reqwest::StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn chat_empty_choices_and_empty_content_are_empty_response() {
        assert!(matches!(
            map_chat_response(&json!({ "model": "m", "choices": [] }), "m"),
            Err(LlmError::EmptyResponse)
        ));
        assert!(matches!(
            map_chat_response(
                &json!({ "model": "m", "choices": [{ "message": { "content": null } }] }),
                "m"
            ),
            Err(LlmError::EmptyResponse)
        ));
        assert!(matches!(
            map_chat_response(
                &json!({ "model": "m", "choices": [{ "message": { "content": "" } }] }),
                "m"
            ),
            Err(LlmError::EmptyResponse)
        ));
    }

    #[test]
    fn chat_refusal_is_surfaced() {
        let error = map_chat_response(
            &json!({
                "model": "m",
                "choices": [{ "message": { "refusal": "cannot do that" } }]
            }),
            "m",
        )
        .expect_err("refusal");
        assert!(matches!(error, LlmError::Refused(refusal) if refusal == "cannot do that"));
    }

    #[test]
    fn chat_maps_content_part_array() {
        let response = map_chat_response(
            &json!({
                "model": "m",
                "choices": [{
                    "message": {
                        "content": [
                            { "type": "text", "text": "part one" },
                            { "type": "text", "text": " and two" }
                        ]
                    }
                }]
            }),
            "m",
        )
        .expect("response");
        assert_eq!(response.content, "part one and two");
    }

    #[tokio::test]
    async fn chat_model_override_is_sent_in_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_json(json!({
                "model": "override-model",
                "messages": [{ "role": "user", "content": "hi" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "override-model",
                "choices": [{ "message": { "content": "ok" } }]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server, ApiMode::ChatCompletions);
        assert_eq!(
            client
                .chat_with_model("override-model", &[ChatMessage::user("hi")])
                .await
                .expect("chat")
                .model,
            "override-model"
        );
    }

    #[tokio::test]
    async fn assistant_tool_result_history_is_serialized() {
        let server = MockServer::start().await;
        let expected = json!({
            "model": "model",
            "messages": [
                { "role": "user", "content": "hi" },
                {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "echo", "arguments": "{\"x\":1}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "result" }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "choices": [{ "message": { "content": "done" } }]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server, ApiMode::ChatCompletions);
        let response = client
            .chat(&[
                ChatMessage::user("hi"),
                ChatMessage::assistant_tools(
                    "",
                    vec![ToolCall::new("call_1", "echo", "{\"x\":1}")],
                ),
                ChatMessage::tool("call_1", "result"),
            ])
            .await
            .expect("chat");
        assert_eq!(response.content, "done");
    }

    #[tokio::test]
    async fn empty_api_key_still_sends_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "choices": [{ "message": { "content": "ok" } }]
            })))
            .mount(&server)
            .await;

        let config = LlmConfig::new_with_mode(server.uri(), "", "model", ApiMode::ChatCompletions)
            .expect("config");
        let client = LlmClient::new(&config);
        client.chat(&[ChatMessage::user("hi")]).await.expect("chat");

        let received = server.received_requests().await.expect("received");
        assert!(!received.is_empty(), "expected at least one request");
        let got = received[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        assert!(
            got.is_some_and(|value| value == "Bearer" || value == "Bearer "),
            "empty key should still send an Authorization header, got {got:?}"
        );
    }

    #[tokio::test]
    async fn chat_stream_rejects_non_2xx_on_open() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let Err(error) = client_for(&server, ApiMode::ChatCompletions)
            .chat_stream(&[ChatMessage::user("hi")])
            .await
        else {
            panic!("stream open must fail on 401");
        };
        assert!(matches!(error, LlmError::Api(_)));
        assert_eq!(error.api_status(), Some(reqwest::StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn chat_stream_surfaces_refusal_event() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"refusal\":\"no\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let mut stream = client_for(&server, ApiMode::ChatCompletions)
            .chat_stream(&[ChatMessage::user("hi")])
            .await
            .expect("stream");
        let error = stream.next().await.expect("event").expect_err("refusal");
        assert!(matches!(error, LlmError::Refused(refusal) if refusal == "no"));
    }

    #[tokio::test]
    async fn chat_stream_empty_body_is_invalid_stream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(b"", "text/event-stream"))
            .mount(&server)
            .await;

        let mut stream = client_for(&server, ApiMode::ChatCompletions)
            .chat_stream(&[ChatMessage::user("hi")])
            .await
            .expect("stream");
        let error = stream.next().await.expect("event").expect_err("empty");
        assert!(matches!(error, LlmError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn responses_replays_tool_loop_history() {
        let server = MockServer::start().await;
        let tool = ToolSpec::new("echo", "Echo", json!({ "type": "object" }));
        let first_output = json!({
            "type": "function_call",
            "call_id": "call_2",
            "name": "echo",
            "arguments": "{\"x\":2}",
            "reasoning_summary": [{ "summary": ["think"] }]
        });
        let second_output = json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "final" }]
        });

        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_json(json!({
                "model": "model",
                "input": [{ "role": "user", "content": "hi" }],
                "store": false,
                "tools": [{ "type": "function", "name": "echo", "description": "Echo", "parameters": { "type": "object" } }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "output": [first_output.clone()],
                "store": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_json(json!({
                "model": "model",
                "input": [
                    { "role": "user", "content": "hi" },
                    first_output.clone(),
                    { "type": "function_call_output", "call_id": "call_2", "output": "done" }
                ],
                "store": false,
                "tools": [{ "type": "function", "name": "echo", "description": "Echo", "parameters": { "type": "object" } }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "output": [second_output.clone()],
                "store": false
            })))
            .mount(&server)
            .await;

        let client = client_for(&server, ApiMode::Responses);
        let response = client
            .chat_with_tools(&[ChatMessage::user("hi")], core::slice::from_ref(&tool))
            .await
            .expect("first turn");
        assert_eq!(response.tool_calls[0].name, "echo");
        assert_eq!(response.tool_calls[0].arguments, "{\"x\":2}");

        let history = [
            ChatMessage::user("hi"),
            response.assistant_message(),
            ChatMessage::tool("call_2", "done"),
        ];
        let final_response = client
            .chat_with_tools(&history, &[tool])
            .await
            .expect("second turn");
        assert_eq!(final_response.content, "final");
    }

    #[tokio::test]
    async fn responses_incomplete_status_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "model",
                "status": "incomplete",
                "output": []
            })))
            .mount(&server)
            .await;

        let error = client_for(&server, ApiMode::Responses)
            .chat(&[ChatMessage::user("hi")])
            .await
            .expect_err("incomplete");
        assert!(matches!(error, LlmError::IncompleteResponse));
    }

    #[tokio::test]
    async fn responses_error_envelope_is_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": { "message": "secret-error-body", "type": "server_error" }
            })))
            .mount(&server)
            .await;

        let error = client_for(&server, ApiMode::Responses)
            .chat(&[ChatMessage::user("hi")])
            .await
            .expect_err("error envelope");
        assert!(matches!(error, LlmError::Api(_)));
        assert!(!format!("{error:?}").contains("secret-error-body"));
    }

    #[test]
    fn responses_missing_output_and_model_fallback() {
        assert!(matches!(
            map_responses_response(&json!({ "model": "m", "output": [] }), "m"),
            Err(LlmError::EmptyResponse)
        ));

        let response = map_responses_response(
            &json!({
                "output": [{ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "x" }] }]
            }),
            "fallback",
        )
        .expect("response");
        assert_eq!(response.model, "fallback");
    }

    #[test]
    fn responses_malformed_function_call_is_invalid_response() {
        assert!(matches!(
            map_responses_response(
                &json!({ "model": "m", "output": [{ "type": "function_call", "name": "echo", "arguments": "{}" }] }),
                "m"
            ),
            Err(LlmError::InvalidResponse(_))
        ));
        assert!(matches!(
            map_responses_response(
                &json!({ "model": "m", "output": [{ "type": "function_call", "call_id": "c", "arguments": "{}" }] }),
                "m"
            ),
            Err(LlmError::InvalidResponse(_))
        ));
    }

    #[test]
    fn responses_maps_complete_function_call() {
        let response = map_responses_response(
            &json!({
                "model": "m",
                "output": [{
                    "type": "function_call",
                    "call_id": "call_9",
                    "name": "search",
                    "arguments": "{\"q\":\"x\"}"
                }]
            }),
            "m",
        )
        .expect("response");
        assert_eq!(
            response.tool_calls,
            vec![ToolCall::new("call_9", "search", "{\"q\":\"x\"}")]
        );
    }
}
