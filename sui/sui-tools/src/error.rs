use std::{io, path::PathBuf};

use thiserror::Error;

/// Errors produced by the tool-calling foundation.
#[derive(Debug, Error)]
pub enum ToolsError {
    /// A tool name was not registered.
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    /// Tool arguments failed to deserialize or failed validation.
    #[error("invalid tool arguments: {0}")]
    InvalidArgs(String),
    /// A bash session operation failed.
    #[error("bash session error: {0}")]
    Bash(String),
    /// BM25 indexing or search failed.
    #[error("search error: {0}")]
    Search(String),
    /// Reading or writing files failed.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// Path involved in the I/O operation.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A unified diff could not be validated or applied to the target file.
    #[error("edit error: {0}")]
    Edit(String),
    /// JSON parsing or serialization failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl ToolsError {
    /// Creates an I/O error tied to a specific path.
    #[must_use]
    pub fn io(
        path: impl Into<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_unknown_tool_and_io() {
        let unknown = ToolsError::UnknownTool("nope".into());
        assert_eq!(unknown.to_string(), "unknown tool `nope`");

        let io_err = ToolsError::io("/tmp/x", io::Error::new(io::ErrorKind::NotFound, "missing"));
        let display = io_err.to_string();
        assert!(display.contains("/tmp/x"), "{display}");
        assert!(
            display.contains("missing") || display.contains("I/O"),
            "{display}"
        );
    }
}
