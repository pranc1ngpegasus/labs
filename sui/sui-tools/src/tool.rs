//! Thin tool-call registry for deep-research style agents.
//!
//! # Threat model
//!
//! Builtin tools are **not a sandbox**. [`BashTool`] runs unsandboxed shell
//! commands with the host credentials (see [`crate::bash`]). [`EditTool`]
//! (see [`crate::edit`]) writes to whatever path the caller passes.
//! [`CodeSearchTool`] only reads what was already indexed. Callers must gate
//! tool exposure and isolate the process when untrusted agents can invoke
//! these tools.
//!
//! After a bash session exits it is **not** auto-respawned; callers must create
//! a new [`BashTool`] / session. Auto-respawn is deferred as out of scope for
//! this foundation.
//!
//! # Bash concurrency
//!
//! [`BashTool`] locks the session only around write / drain / wait / kill — not
//! during the optional pre-drain sleep on `action=write`. Concurrent
//! `drain:true` writes may therefore interleave. Prefer `drain:false` +
//! `action=poll` / `action=drain` when overlapping calls are expected.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{BashSession, LexicalSearch, ProcessState, ToolsError, bash::validate_single_line};

/// Hard upper bound for `code_search` `limit`.
pub const MAX_SEARCH_LIMIT: usize = 100;
/// Default wait after a bash `write` before draining when `timeout_ms` is omitted.
const DEFAULT_WRITE_DRAIN_MS: u64 = 50;
/// Cap on `timeout_ms` so a single tool call cannot hold the session lock indefinitely.
const MAX_TIMEOUT_MS: u64 = 300_000;

/// Boxed future returned by [`Tool::call`].
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ToolsError>> + Send + 'a>>;

/// A callable agent tool.
pub trait Tool: Send + Sync {
    /// Stable tool name used in [`ToolRegistry::call`].
    fn name(&self) -> &str;

    /// Human-readable description for agent prompting.
    fn description(&self) -> &str;

    /// JSON-Schema-ish object describing accepted arguments.
    fn parameters_schema(&self) -> Value;

    /// Invokes the tool with JSON arguments.
    fn call(
        &self,
        args: Value,
    ) -> ToolFuture<'_>;
}

/// Registry of named tools.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a tool, replacing any previous tool with the same name.
    pub fn register(
        &mut self,
        tool: impl Tool + 'static,
    ) {
        let tool: Arc<dyn Tool> = Arc::new(tool);
        self.tools.insert(tool.name().to_owned(), tool);
    }

    /// Returns registered tool names in sorted order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    /// MCP-style descriptors for all registered tools.
    #[must_use]
    pub fn descriptors(&self) -> Vec<Value> {
        let mut tools: Vec<&Arc<dyn Tool>> = self.tools.values().collect();
        tools.sort_by(|left, right| left.name().cmp(right.name()));
        tools
            .into_iter()
            .map(|tool| {
                json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "inputSchema": tool.parameters_schema(),
                })
            })
            .collect()
    }

    /// Dispatches a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsError::UnknownTool`] when `name` is not registered, or
    /// any error produced by the tool itself.
    pub async fn call(
        &self,
        name: &str,
        args: Value,
    ) -> Result<Value, ToolsError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolsError::UnknownTool(name.to_owned()))?;
        tool.call(args).await
    }
}

/// Lexical code search tool (`code_search`).
///
/// Backend-agnostic: any [`LexicalSearch`] (in-memory BM25 or Tantivy).
pub struct CodeSearchTool {
    index: Arc<dyn LexicalSearch>,
}

impl CodeSearchTool {
    /// Wraps an existing lexical index backend.
    #[must_use]
    pub fn new(index: impl LexicalSearch + 'static) -> Self {
        Self {
            index: Arc::new(index),
        }
    }
}

impl Tool for CodeSearchTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "code_search"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "Lexical (BM25-ish) search over the indexed local codebase. Prefer this over blind filesystem exploration."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "limit": {
                    "type": "integer",
                    "description": format!("Max hits to return (default 10, max {MAX_SEARCH_LIMIT}; 0 returns no hits)"),
                    "minimum": 0,
                    "maximum": MAX_SEARCH_LIMIT
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn call(
        &self,
        args: Value,
    ) -> ToolFuture<'_> {
        Box::pin(async move {
            let args: CodeSearchArgs = serde_json::from_value(args)
                .map_err(|error| ToolsError::InvalidArgs(error.to_string()))?;
            let limit = args.limit.unwrap_or(10).min(MAX_SEARCH_LIMIT);
            let hits = self.index.search(&args.query, limit);
            Ok(json!({ "hits": hits }))
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeSearchArgs {
    query: String,
    limit: Option<usize>,
}

/// Persistent bash session tool (`bash`).
///
/// # Threat model
///
/// Unsandboxed shell — see module docs and [`crate::bash`].
///
/// # Arguments
///
/// - `action` (string, optional): `run` (default) | `write` | `drain` | `poll` | `wait` | `kill`
/// - `command` (string): required for `run` and `write`; single line, no NUL/CR/LF
/// - `timeout_ms` (integer, optional): for `run`, wall-clock budget (default 30000);
///   for `write`, wait before drain; for `wait`, max wait
/// - `drain` (bool, optional): for `write` only; if false, skip post-write drain (default true)
///
/// `run` starts a **fresh** bash process via [`crate::run_line`] (what the model
/// should use). Session actions (`write`/`drain`/`poll`/`wait`/`kill`) share
/// one persistent shell and must be named explicitly.
///
/// Concurrent `drain:true` writes may interleave because the pre-drain sleep
/// does not hold the session mutex. Prefer `drain:false` + poll for concurrency.
pub struct BashTool {
    session: Mutex<BashSession>,
    cwd: Option<std::path::PathBuf>,
}

impl BashTool {
    /// Takes ownership of an already-spawned session.
    #[must_use]
    pub fn new(session: BashSession) -> Self {
        Self {
            session: Mutex::new(session),
            cwd: None,
        }
    }

    /// Spawns a new bash session and wraps it.
    ///
    /// # Errors
    ///
    /// Propagates [`BashSession::spawn`] errors.
    pub fn spawn(cwd: Option<&std::path::Path>) -> Result<Self, ToolsError> {
        Ok(Self {
            session: Mutex::new(BashSession::spawn(cwd)?),
            cwd: cwd.map(std::path::Path::to_path_buf),
        })
    }

    async fn wait_action(
        &self,
        timeout_ms: Option<u64>,
    ) -> Result<Value, ToolsError> {
        let wait_ms = timeout_ms.unwrap_or(5_000).min(MAX_TIMEOUT_MS);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            {
                let mut session = self.session.lock().await;
                if let ProcessState::Exited { code } = session.poll()? {
                    let drained = session.drain().await?;
                    drop(session);
                    return Ok(json!({
                        "code": code,
                        "timed_out": false,
                        "killed": false,
                        "stdout": String::from_utf8_lossy(&drained.stdout),
                        "stderr": String::from_utf8_lossy(&drained.stderr),
                        "truncated": drained.truncated,
                    }));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let mut session = self.session.lock().await;
                session.kill().await?;
                let drained = session.drain().await?;
                drop(session);
                return Ok(json!({
                    "code": drained.state.exit_code(),
                    "timed_out": true,
                    "killed": true,
                    "stdout": String::from_utf8_lossy(&drained.stdout),
                    "stderr": String::from_utf8_lossy(&drained.stderr),
                    "truncated": drained.truncated,
                }));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Tool for BashTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "bash"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "Run a shell command. Default action is `run` (fresh process, waits for exit). \
         Pass `command` (single line) and optional `timeout_ms` (default 30000). \
         Prefer this over write/drain/poll. Session actions: write, drain, poll, wait, kill."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "write", "drain", "poll", "wait", "kill"],
                    "description": "Session action (default run)"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command for action=run or action=write (single line)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Milliseconds to wait (run budget, write drain delay, or wait timeout)",
                    "minimum": 0
                },
                "drain": {
                    "type": "boolean",
                    "description": "For write: whether to drain after waiting (default true)"
                }
            },
            "additionalProperties": false
        })
    }

    fn call(
        &self,
        args: Value,
    ) -> ToolFuture<'_> {
        // Lock only for session I/O — not during write pre-drain sleep or wait
        // poll ticks — so timeout_ms cannot block kill/poll for up to 60s.
        // Concurrent drain:true writes may interleave; prefer drain:false + poll.
        Box::pin(async move {
            let args: BashArgs = serde_json::from_value(args)
                .map_err(|error| ToolsError::InvalidArgs(error.to_string()))?;
            let action = args.action.as_deref().unwrap_or("run");

            match action {
                "run" => {
                    let command = args.command.as_deref().ok_or_else(|| {
                        ToolsError::InvalidArgs("command is required for action=run".into())
                    })?;
                    validate_single_line(command)?;
                    let wait_ms = args.timeout_ms.unwrap_or(30_000).min(MAX_TIMEOUT_MS);
                    let output = crate::run_line(
                        command,
                        self.cwd.as_deref(),
                        Duration::from_millis(wait_ms),
                    )
                    .await?;
                    Ok(json!({
                        "stdout": output.stdout,
                        "stderr": output.stderr,
                        "code": output.code,
                        "timed_out": output.timed_out,
                        "truncated": output.truncated,
                    }))
                },
                "write" => {
                    let command = args.command.as_deref().ok_or_else(|| {
                        ToolsError::InvalidArgs("command is required for action=write".into())
                    })?;
                    validate_single_line(command)?;
                    let should_drain = args.drain.unwrap_or(true);
                    {
                        let mut session = self.session.lock().await;
                        session.write_line(command).await?;
                        if !should_drain {
                            return Ok(json!({ "written": true }));
                        }
                        drop(session);
                    }
                    let wait_ms = args
                        .timeout_ms
                        .unwrap_or(DEFAULT_WRITE_DRAIN_MS)
                        .min(MAX_TIMEOUT_MS);
                    if wait_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                    }
                    let mut session = self.session.lock().await;
                    let output = session.drain().await?;
                    drop(session);
                    Ok(session_output_json(&output))
                },
                "drain" => {
                    let mut session = self.session.lock().await;
                    let output = session.drain().await?;
                    drop(session);
                    Ok(session_output_json(&output))
                },
                "poll" => {
                    let mut session = self.session.lock().await;
                    let output = session.read().await?;
                    drop(session);
                    Ok(json!({
                        "stdout": String::from_utf8_lossy(&output.stdout),
                        "stderr": String::from_utf8_lossy(&output.stderr),
                        "state": process_state_json(output.state),
                        "truncated": output.truncated,
                    }))
                },
                "wait" => self.wait_action(args.timeout_ms).await,
                "kill" => {
                    let mut session = self.session.lock().await;
                    session.kill().await?;
                    let drained = session.drain().await?;
                    drop(session);
                    Ok(json!({
                        "killed": true,
                        "stdout": String::from_utf8_lossy(&drained.stdout),
                        "stderr": String::from_utf8_lossy(&drained.stderr),
                        "state": process_state_json(drained.state),
                        "truncated": drained.truncated,
                    }))
                },
                other => Err(ToolsError::InvalidArgs(format!(
                    "unknown bash action `{other}`"
                ))),
            }
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArgs {
    action: Option<String>,
    command: Option<String>,
    timeout_ms: Option<u64>,
    drain: Option<bool>,
}

fn session_output_json(output: &crate::SessionOutput) -> Value {
    json!({
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
        "state": process_state_json(output.state),
        "truncated": output.truncated,
    })
}

fn process_state_json(state: ProcessState) -> Value {
    match state {
        ProcessState::Running => json!({ "running": true }),
        ProcessState::Exited { code } => json!({ "running": false, "code": code }),
    }
}

/// Builds a registry with `code_search` only (no bash spawn).
#[must_use]
pub fn code_search_registry(index: impl LexicalSearch + 'static) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(CodeSearchTool::new(index));
    registry
}

/// Builds a registry with `code_search`, `edit`, and `bash` when spawn works.
///
/// Unlike [`builtin_registry`], bash spawn failure does not fail the whole
/// registry — `code_search` and `edit` still register.
#[must_use]
pub fn coding_registry(
    index: impl LexicalSearch + 'static,
    bash_cwd: Option<&std::path::Path>,
) -> ToolRegistry {
    let mut registry = code_search_registry(index);
    registry.register(crate::edit::EditTool::new());
    if let Ok(bash) = BashTool::spawn(bash_cwd) {
        registry.register(bash);
    }
    registry
}

/// Builds a registry with `code_search`, `bash`, and `edit` builtins.
///
/// # Errors
///
/// Returns an error if the bash session cannot be spawned.
/// Prefer [`coding_registry`] when bash is optional, or [`code_search_registry`]
/// when bash is unavailable or unwanted.
pub fn builtin_registry(
    index: impl LexicalSearch + 'static,
    bash_cwd: Option<&std::path::Path>,
) -> Result<ToolRegistry, ToolsError> {
    let mut registry = code_search_registry(index);
    registry.register(BashTool::spawn(bash_cwd)?);
    registry.register(crate::edit::EditTool::new());
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bm25Index;

    async fn poll_until_stdout(
        registry: &ToolRegistry,
        needle: &str,
    ) -> Result<String, ToolsError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut stdout = String::new();
        loop {
            let result = registry.call("bash", json!({ "action": "drain" })).await?;
            if let Some(chunk) = result["stdout"].as_str() {
                stdout.push_str(chunk);
            }
            if stdout.contains(needle) {
                return Ok(stdout);
            }
            if let Some(state) = result.get("state")
                && state.get("running") == Some(&json!(false))
            {
                return Ok(stdout);
            }
            if tokio::time::Instant::now() > deadline {
                return Ok(stdout);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn dispatches_code_search() -> Result<(), ToolsError> {
        let mut index = Bm25Index::default();
        index.add_document("a", "auth.rs", "authenticate password");
        index.add_document("b", "ui.rs", "render widget");
        let tool = CodeSearchTool::new(index);

        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let result = registry
            .call("code_search", json!({ "query": "password", "limit": 5 }))
            .await?;
        let hits = result
            .get("hits")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolsError::Search("missing hits".into()))?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["path"], "auth.rs");
        assert_eq!(hits[0]["doc_id"], "a");
        assert!(hits[0]["score"].as_f64().is_some_and(f64::is_finite));
        assert!(hits[0]["snippet"].as_str().is_some_and(|s| !s.is_empty()));
        Ok(())
    }

    #[tokio::test]
    async fn dispatches_bash_echo() -> Result<(), ToolsError> {
        let tool = BashTool::spawn(None)?;
        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let _ = registry
            .call(
                "bash",
                json!({
                    "action": "write",
                    "command": "echo tool-dispatch-ok",
                    "drain": false
                }),
            )
            .await?;
        let stdout = poll_until_stdout(&registry, "tool-dispatch-ok").await?;
        assert!(
            stdout.contains("tool-dispatch-ok"),
            "stdout was: {stdout:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn bash_drain_false_and_actions() -> Result<(), ToolsError> {
        let registry = builtin_registry(Bm25Index::default(), None)?;
        assert_eq!(
            registry.names(),
            vec![
                "bash".to_owned(),
                "code_search".to_owned(),
                "edit".to_owned()
            ]
        );
        let descriptors = registry.descriptors();
        assert_eq!(descriptors.len(), 3);
        assert_eq!(descriptors[0]["name"], "bash");
        assert_eq!(descriptors[1]["name"], "code_search");
        assert_eq!(descriptors[2]["name"], "edit");

        let written = registry
            .call(
                "bash",
                json!({ "action": "write", "command": "echo action-ok", "drain": false }),
            )
            .await?;
        assert_eq!(written["written"], true);

        let stdout = poll_until_stdout(&registry, "action-ok").await?;
        assert!(stdout.contains("action-ok"));

        let polled = registry.call("bash", json!({ "action": "poll" })).await?;
        assert!(polled["state"]["running"].as_bool().unwrap_or(false));

        let _ = registry
            .call(
                "bash",
                json!({ "action": "write", "command": "exit", "drain": false }),
            )
            .await?;
        let waited = registry
            .call("bash", json!({ "action": "wait", "timeout_ms": 3000 }))
            .await?;
        assert_eq!(waited["code"], 0);
        assert_eq!(waited["timed_out"], false);
        Ok(())
    }

    #[tokio::test]
    async fn bash_drain_true_write_returns_output() -> Result<(), ToolsError> {
        let tool = BashTool::spawn(None)?;
        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let result = registry
            .call(
                "bash",
                json!({
                    "action": "write",
                    "command": "echo drain-true-ok",
                    "drain": true,
                    "timeout_ms": 200
                }),
            )
            .await?;
        let stdout = result["stdout"].as_str().unwrap_or("");
        // May need a follow-up drain if timing is tight.
        if stdout.contains("drain-true-ok") {
            return Ok(());
        }
        let stdout = poll_until_stdout(&registry, "drain-true-ok").await?;
        assert!(stdout.contains("drain-true-ok"), "stdout={stdout:?}");
        Ok(())
    }

    #[tokio::test]
    async fn bash_action_kill() -> Result<(), ToolsError> {
        let tool = BashTool::spawn(None)?;
        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let _ = registry
            .call(
                "bash",
                json!({ "action": "write", "command": "sleep 30", "drain": false }),
            )
            .await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let killed = registry.call("bash", json!({ "action": "kill" })).await?;
        assert_eq!(killed["killed"], true);
        assert_eq!(killed["state"]["running"], false);
        Ok(())
    }

    #[tokio::test]
    async fn bash_wait_timeout_returns_partial_output() -> Result<(), ToolsError> {
        let tool = BashTool::spawn(None)?;
        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let _ = registry
            .call(
                "bash",
                json!({
                    "action": "write",
                    "command": "echo before-hang; sleep 30",
                    "drain": false
                }),
            )
            .await?;
        // Give echo a moment to land in the buffer before wait kills.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let waited = registry
            .call("bash", json!({ "action": "wait", "timeout_ms": 100 }))
            .await?;
        assert_eq!(waited["timed_out"], true);
        assert_eq!(waited["killed"], true);
        let stdout = waited["stdout"].as_str().unwrap_or("");
        assert!(
            stdout.contains("before-hang"),
            "expected partial stdout, got: {stdout:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn bash_run_echo() -> Result<(), ToolsError> {
        let tool = BashTool::spawn(None)?;
        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let result = registry
            .call("bash", json!({ "command": "echo run-ok" }))
            .await?;
        assert_eq!(result["timed_out"], false);
        assert_eq!(result["code"], 0);
        let stdout = result["stdout"].as_str().unwrap_or("");
        assert!(stdout.contains("run-ok"), "stdout={stdout:?}");
        Ok(())
    }

    #[tokio::test]
    async fn bash_rejects_newline_command() {
        let tool = BashTool::spawn(None).expect("spawn");
        let mut registry = ToolRegistry::new();
        registry.register(tool);
        let err = registry
            .call("bash", json!({ "command": "echo a\necho b" }))
            .await
            .expect_err("newline");
        assert!(matches!(err, ToolsError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn bash_rejects_cr_command() {
        let tool = BashTool::spawn(None).expect("spawn");
        let mut registry = ToolRegistry::new();
        registry.register(tool);
        let err = registry
            .call("bash", json!({ "command": "echo a\recho b" }))
            .await
            .expect_err("cr");
        assert!(matches!(err, ToolsError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn bash_unknown_action_and_missing_command() {
        let tool = BashTool::spawn(None).expect("spawn");
        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let missing = registry.call("bash", json!({ "action": "write" })).await;
        assert!(matches!(missing, Err(ToolsError::InvalidArgs(_))));

        let unknown = registry.call("bash", json!({ "action": "nope" })).await;
        assert!(matches!(unknown, Err(ToolsError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn code_search_invalid_args() {
        let tool = CodeSearchTool::new(Bm25Index::default());
        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let missing = registry.call("code_search", json!({})).await;
        assert!(matches!(missing, Err(ToolsError::InvalidArgs(_))));

        let wrong_type = registry.call("code_search", json!({ "query": 1 })).await;
        assert!(matches!(wrong_type, Err(ToolsError::InvalidArgs(_))));

        let zero_limit = registry
            .call("code_search", json!({ "query": "x", "limit": 0 }))
            .await
            .expect("limit=0 should succeed");
        assert_eq!(zero_limit["hits"].as_array().expect("hits").len(), 0);

        let unknown_field = registry
            .call("code_search", json!({ "query": "x", "extra": true }))
            .await;
        assert!(matches!(unknown_field, Err(ToolsError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn code_search_clamps_limit() -> Result<(), ToolsError> {
        let mut index = Bm25Index::default();
        for i in 0..5 {
            index.add_document(format!("d{i}"), format!("f{i}.rs"), "shared_term unique");
        }
        let registry = code_search_registry(index);
        let result = registry
            .call(
                "code_search",
                json!({ "query": "shared_term", "limit": 10_000 }),
            )
            .await?;
        let hits = result["hits"].as_array().expect("hits");
        assert!(hits.len() <= MAX_SEARCH_LIMIT);
        assert_eq!(hits.len(), 5);
        Ok(())
    }

    #[tokio::test]
    async fn code_search_accepts_tantivy_backend() -> Result<(), ToolsError> {
        use crate::TantivyIndex;
        use crate::corpus::{TempDir, temp_dir};
        use std::fs;

        let src = TempDir(temp_dir("tool-tv-src"));
        let idx = TempDir(temp_dir("tool-tv-idx"));
        fs::create_dir_all(src.0.join("src")).map_err(|source| ToolsError::io(&src.0, source))?;
        fs::write(src.0.join("src/auth.rs"), "fn authenticate_password() {}")
            .map_err(|source| ToolsError::io(src.0.join("src/auth.rs"), source))?;

        let index = TantivyIndex::index_tree(&idx.0, &src.0, &["rs"])?;
        let registry = code_search_registry(index);
        let result = registry
            .call(
                "code_search",
                json!({ "query": "authenticate", "limit": 3 }),
            )
            .await?;
        let hits = result["hits"].as_array().expect("hits");
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0]["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("auth.rs"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let registry = ToolRegistry::new();
        let error = registry.call("nope", json!({})).await;
        assert!(matches!(error, Err(ToolsError::UnknownTool(_))));
    }
}
