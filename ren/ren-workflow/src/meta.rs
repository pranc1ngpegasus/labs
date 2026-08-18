use std::ops::Range;

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
// `args_schema` holds a `serde_json::Value`, which is not `Eq` (JSON numbers
// may be floats), so only `PartialEq` can be derived here.
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

pub fn extract(script: &str) -> Result<WorkflowMeta, WorkflowError> {
    let statement_end = validate_first_statement(script)?;
    let statement = script.get(..statement_end).ok_or_else(|| {
        WorkflowError::InvalidMeta("metadata statement has an invalid byte boundary".into())
    })?;
    let engine = RhaiEngine::new();
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
    validate_metadata_name(&metadata.name)?;
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

pub fn validate_name(name: &str) -> Result<(), WorkflowError> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(WorkflowError::InvalidWorkflowName(name.to_owned()))
    }
}

fn validate_metadata_name(name: &str) -> Result<(), WorkflowError> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(WorkflowError::InvalidMeta(format!(
            "name `{name}` must contain only lowercase ASCII letters, digits, and hyphens"
        )))
    }
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

pub fn name_value_span(script: &str) -> Result<Range<usize>, WorkflowError> {
    let _ = validate_first_statement(script)?;
    let mut cursor = Cursor::new(script);
    let _ = consume_meta_prefix(&mut cursor)?;
    let mut stack = vec![b'}'];
    let mut expecting_top_level_key = true;

    while let Some(expected) = stack.last().copied() {
        cursor.skip_trivia()?;
        let byte = cursor
            .peek()
            .ok_or_else(|| WorkflowError::InvalidMeta("unterminated metadata map".into()))?;

        if stack.len() == 1 && expecting_top_level_key && byte != b'}' {
            let key_start = cursor.position;
            let key = if byte == b'"' {
                cursor.consume_string()?;
                let value_start = key_start + 1;
                let value_end = cursor.position.checked_sub(1).ok_or_else(|| {
                    WorkflowError::InvalidMeta("invalid metadata key range".into())
                })?;
                script.get(value_start..value_end).ok_or_else(|| {
                    WorkflowError::InvalidMeta("invalid metadata key boundary".into())
                })?
            } else if is_identifier_start(byte) {
                cursor.consume_identifier();
                script.get(key_start..cursor.position).ok_or_else(|| {
                    WorkflowError::InvalidMeta("invalid metadata key boundary".into())
                })?
            } else {
                return Err(WorkflowError::InvalidMeta(format!(
                    "expected metadata map key near byte {}",
                    cursor.position
                )));
            };
            cursor.skip_trivia()?;
            cursor.consume_byte(b':')?;
            cursor.skip_trivia()?;
            if key == "name" {
                if cursor.peek() != Some(b'"') {
                    return Err(WorkflowError::InvalidMeta(
                        "metadata `name` must be a string literal".into(),
                    ));
                }
                let value_start = cursor.position;
                cursor.consume_string()?;
                return Ok(value_start..cursor.position);
            }
            expecting_top_level_key = false;
            continue;
        }

        match byte {
            b'"' => cursor.consume_string()?,
            b'#' => {
                cursor.consume_bytes(b"#{")?;
                stack.push(b'}');
            },
            b'[' => {
                cursor.position += 1;
                stack.push(b']');
            },
            b'}' | b']' => {
                if byte != expected {
                    return Err(WorkflowError::InvalidMeta(
                        "mismatched delimiter in metadata map".into(),
                    ));
                }
                if stack.len() == 1 {
                    break;
                }
                cursor.position += 1;
                stack.pop();
            },
            b',' => {
                cursor.position += 1;
                if stack.len() == 1 {
                    expecting_top_level_key = true;
                }
            },
            b':' => cursor.position += 1,
            b'-' | b'0'..=b'9' => cursor.consume_number()?,
            byte if is_identifier_start(byte) => cursor.consume_identifier(),
            _ => {
                return Err(WorkflowError::InvalidMeta(format!(
                    "unexpected metadata token near byte {}",
                    cursor.position
                )));
            },
        }
    }

    Err(WorkflowError::InvalidMeta(
        "metadata map does not contain a `name` key".into(),
    ))
}

fn validate_first_statement(script: &str) -> Result<usize, WorkflowError> {
    let mut cursor = Cursor::new(script);
    let expression_start = consume_meta_prefix(&mut cursor)?;
    let mut stack = vec![b'}'];

    while let Some(expected) = stack.last().copied() {
        cursor.skip_trivia()?;
        let byte = cursor
            .peek()
            .ok_or_else(|| WorkflowError::InvalidMeta("unterminated metadata map".into()))?;
        match byte {
            b'"' => cursor.consume_string()?,
            b'#' => {
                cursor.consume_bytes(b"#{")?;
                stack.push(b'}');
            },
            b'[' => {
                cursor.position += 1;
                stack.push(b']');
            },
            b'}' | b']' => {
                if byte != expected {
                    return Err(WorkflowError::InvalidMeta(
                        "mismatched delimiter in metadata map".into(),
                    ));
                }
                cursor.position += 1;
                stack.pop();
            },
            b',' | b':' => cursor.position += 1,
            b'-' | b'0'..=b'9' => cursor.consume_number()?,
            byte if is_identifier_start(byte) => cursor.consume_literal_or_key()?,
            _ => {
                return Err(WorkflowError::InvalidMeta(format!(
                    "metadata must be a pure literal map; unexpected token near byte {}",
                    cursor.position
                )));
            },
        }
    }

    let expression_end = cursor.position;
    cursor.skip_trivia()?;
    cursor.consume_byte(b';')?;
    if expression_end <= expression_start + 2 {
        return Err(WorkflowError::InvalidMeta(
            "metadata map cannot be empty".into(),
        ));
    }
    Ok(cursor.position)
}

fn consume_meta_prefix(cursor: &mut Cursor<'_>) -> Result<usize, WorkflowError> {
    cursor.skip_trivia()?;
    cursor.consume_word("let")?;
    cursor.require_trivia()?;
    cursor.skip_trivia()?;
    cursor.consume_word("meta")?;
    cursor.skip_trivia()?;
    cursor.consume_byte(b'=')?;
    cursor.skip_trivia()?;
    let expression_start = cursor.position;
    cursor.consume_bytes(b"#{")?;
    Ok(expression_start)
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            bytes: source.as_bytes(),
            position: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_trivia(&mut self) -> Result<(), WorkflowError> {
        loop {
            while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
                self.position += 1;
            }
            if self.remaining_starts_with(b"//") {
                self.position += 2;
                while self.peek().is_some_and(|byte| byte != b'\n') {
                    self.position += 1;
                }
            } else if self.remaining_starts_with(b"/*") {
                self.skip_block_comment()?;
            } else {
                return Ok(());
            }
        }
    }

    /// Skips a `/* ... */` block comment, honouring Rhai's nested comments.
    fn skip_block_comment(&mut self) -> Result<(), WorkflowError> {
        self.position += 2;
        let mut depth = 1_usize;
        while depth > 0 {
            if self.remaining_starts_with(b"/*") {
                self.position += 2;
                depth += 1;
            } else if self.remaining_starts_with(b"*/") {
                self.position += 2;
                depth -= 1;
            } else if self.peek().is_some() {
                self.position += 1;
            } else {
                return Err(WorkflowError::InvalidMeta(
                    "unterminated comment before or inside metadata".into(),
                ));
            }
        }
        Ok(())
    }

    fn require_trivia(&self) -> Result<(), WorkflowError> {
        match self.peek() {
            Some(byte) if byte.is_ascii_whitespace() => Ok(()),
            _ if self.remaining_starts_with(b"//") || self.remaining_starts_with(b"/*") => Ok(()),
            _ => Err(WorkflowError::InvalidMeta(
                "expected whitespace after `let`".into(),
            )),
        }
    }

    fn consume_word(
        &mut self,
        expected: &str,
    ) -> Result<(), WorkflowError> {
        self.consume_bytes(expected.as_bytes())?;
        if self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(WorkflowError::InvalidMeta(format!(
                "expected `{expected}` in first statement"
            )));
        }
        Ok(())
    }

    fn consume_byte(
        &mut self,
        expected: u8,
    ) -> Result<(), WorkflowError> {
        if self.peek() != Some(expected) {
            return Err(WorkflowError::InvalidMeta(format!(
                "expected `{}` near byte {}",
                char::from(expected),
                self.position
            )));
        }
        self.position += 1;
        Ok(())
    }

    fn consume_bytes(
        &mut self,
        expected: &[u8],
    ) -> Result<(), WorkflowError> {
        if !self.remaining_starts_with(expected) {
            return Err(WorkflowError::InvalidMeta(format!(
                "expected `{}` near byte {}",
                String::from_utf8_lossy(expected),
                self.position
            )));
        }
        self.position += expected.len();
        Ok(())
    }

    fn remaining_starts_with(
        &self,
        expected: &[u8],
    ) -> bool {
        self.bytes
            .get(self.position..)
            .is_some_and(|remaining| remaining.starts_with(expected))
    }

    fn consume_string(&mut self) -> Result<(), WorkflowError> {
        self.position += 1;
        while let Some(byte) = self.peek() {
            self.position += 1;
            match byte {
                b'\\' => {
                    if self.peek().is_none() {
                        return Err(WorkflowError::InvalidMeta(
                            "unterminated escape in metadata string".into(),
                        ));
                    }
                    self.position += 1;
                },
                b'"' => return Ok(()),
                b'\n' | b'\r' => {
                    return Err(WorkflowError::InvalidMeta(
                        "newline in metadata string".into(),
                    ));
                },
                _ => {},
            }
        }
        Err(WorkflowError::InvalidMeta(
            "unterminated metadata string".into(),
        ))
    }

    fn consume_number(&mut self) -> Result<(), WorkflowError> {
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        let integer_start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        if self.position == integer_start {
            return Err(WorkflowError::InvalidMeta(
                "expected digits after `-` in metadata".into(),
            ));
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            let fraction_start = self.position;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
            if self.position == fraction_start {
                return Err(WorkflowError::InvalidMeta(
                    "expected digits after decimal point in metadata".into(),
                ));
            }
        }
        if self.peek().is_some_and(|byte| byte == b'e' || byte == b'E') {
            self.position += 1;
            if self.peek().is_some_and(|byte| byte == b'+' || byte == b'-') {
                self.position += 1;
            }
            let exponent_start = self.position;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
            if self.position == exponent_start {
                return Err(WorkflowError::InvalidMeta(
                    "expected exponent digits in metadata".into(),
                ));
            }
        }
        Ok(())
    }

    fn consume_identifier(&mut self) {
        self.position += 1;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            self.position += 1;
        }
    }

    fn consume_literal_or_key(&mut self) -> Result<(), WorkflowError> {
        let start = self.position;
        self.consume_identifier();
        let word = std::str::from_utf8(
            self.bytes
                .get(start..self.position)
                .ok_or_else(|| WorkflowError::InvalidMeta("invalid identifier range".into()))?,
        )
        .map_err(|error| WorkflowError::InvalidMeta(error.to_string()))?;
        if matches!(word, "true" | "false") {
            return Ok(());
        }
        let saved = self.position;
        self.skip_trivia()?;
        if self.peek() == Some(b':') {
            return Ok(());
        }
        self.position = saved;
        Err(WorkflowError::InvalidMeta(format!(
            "metadata identifier `{word}` is not a literal map key"
        )))
    }
}
