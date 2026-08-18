use rhai::{Dynamic, Engine as RhaiEngine, Scope};
use serde::{Deserialize, Serialize};

use crate::{WorkflowError, value::dynamic_to_json};

/// One declared workflow phase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaPhase {
    /// Phase title.
    pub title: String,
    /// Phase detail shown to users.
    pub detail: String,
}

/// Pure-literal metadata declared by the first workflow statement.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkflowMeta {
    /// Stable lowercase workflow name.
    pub name: String,
    /// Human-readable workflow description.
    pub description: String,
    /// Guidance describing when callers should select this workflow.
    #[serde(default)]
    pub when_to_use: Option<String>,
    /// JSON Schema describing the workflow's `args` value.
    #[serde(default)]
    pub args_schema: Option<serde_json::Value>,
    /// Declared user-facing phases.
    #[serde(default)]
    pub phases: Vec<MetaPhase>,
}

/// Extracts and validates pure-literal `let meta = #{...};` metadata.
///
/// # Errors
///
/// Returns [`WorkflowError::InvalidMeta`] when the first statement is not valid metadata.
pub fn extract(script: &str) -> Result<WorkflowMeta, WorkflowError> {
    let statement_end = first_statement_end(script)?;
    let statement = script.get(..statement_end).ok_or_else(|| {
        WorkflowError::InvalidMeta("metadata statement has an invalid byte boundary".into())
    })?;
    let engine = {
        let mut engine = RhaiEngine::new();
        engine.set_max_operations(10_000);
        engine.set_max_call_levels(32);
        engine.set_max_string_size(65_536);
        engine.set_max_array_size(1_024);
        engine.set_max_map_size(1_024);
        engine
    };
    let ast = engine
        .compile(statement)
        .map_err(|error| WorkflowError::InvalidMeta(error.to_string()))?;
    let mut scope = Scope::new();
    let _: Dynamic = engine
        .eval_ast_with_scope(&mut scope, &ast)
        .map_err(|error| WorkflowError::InvalidMeta(error.to_string()))?;
    let dynamic = scope.get_value::<Dynamic>("meta").ok_or_else(|| {
        WorkflowError::InvalidMeta("first statement did not define `meta`".into())
    })?;
    let value = dynamic_to_json(&dynamic)?;
    let metadata: WorkflowMeta = serde_json::from_value(value)
        .map_err(|error| WorkflowError::InvalidMeta(error.to_string()))?;
    if !is_valid_name(&metadata.name) {
        return Err(WorkflowError::InvalidMeta(format!(
            "name `{}` must contain only lowercase ASCII letters, digits, and hyphens",
            metadata.name
        )));
    }
    if metadata
        .args_schema
        .as_ref()
        .is_some_and(|schema| !schema.is_object())
    {
        return Err(WorkflowError::InvalidMeta(
            "`args_schema` must be a map".into(),
        ));
    }
    Ok(metadata)
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn first_statement_end(script: &str) -> Result<usize, WorkflowError> {
    let bytes = script.as_bytes();
    let mut index = 0;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut string_quote = b'"';
    let mut escaped = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == string_quote {
                in_string = false;
            }
            index += 1;
            continue;
        }

        match byte {
            b'"' | b'\'' => {
                in_string = true;
                string_quote = byte;
                index += 1;
            },
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            },
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = index.saturating_add(2);
            },
            b'#' if bytes.get(index + 1) == Some(&b'{') => {
                depth += 1;
                index += 2;
            },
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            },
            b'}' | b']' => {
                depth -= 1;
                if depth < 0 {
                    return Err(WorkflowError::InvalidMeta(
                        "mismatched delimiter in metadata statement".into(),
                    ));
                }
                index += 1;
            },
            b';' if depth == 0 => {
                let end = index + 1;
                let prefix = script.get(..end).ok_or_else(|| {
                    WorkflowError::InvalidMeta("invalid metadata boundary".into())
                })?;
                if !prefix.trim_start().starts_with("let meta") {
                    return Err(WorkflowError::InvalidMeta(
                        "first statement must be `let meta = #{...};`".into(),
                    ));
                }
                return Ok(end);
            },
            _ => index += 1,
        }
    }

    Err(WorkflowError::InvalidMeta(
        "unterminated metadata statement".into(),
    ))
}
