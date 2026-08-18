use std::{io, path::PathBuf};

/// Errors returned by the memory subsystem.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory home is unavailable: set HOME or REN_MEMORY_HOME")]
    HomeUnavailable,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("YAML error: {0}")]
    Yaml(#[from] yaml_serde::Error),
    #[error("invalid memory configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid note at {path}: {message}")]
    InvalidNote { path: PathBuf, message: String },
    #[error("vault `{0}` is not registered; run `ren memory init --user` first")]
    UnknownVault(String),
    #[error("no memory vault matches the current directory; run `ren memory init --user` first")]
    VaultNotFound,
    #[error("more than one memory vault is registered; select one with --vault")]
    AmbiguousVault,
    #[error("note `{0}` was not found")]
    NoteNotFound(String),
    #[error("memory writer is busy")]
    WriterBusy,
    #[error("input exceeds the {limit}-byte limit")]
    InputTooLarge { limit: usize },
    #[error("unsafe input rejected: {0}")]
    UnsafeInput(String),
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("promotion workflow failed: {0}")]
    Workflow(String),
}

impl MemoryError {
    pub(crate) fn io(
        path: impl Into<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::HomeUnavailable | Self::InvalidConfig(_) => "invalid_config",
            Self::Io { .. } => "io_error",
            Self::Sqlite(_) => "index_error",
            Self::Json(_) | Self::Yaml(_) | Self::InvalidNote { .. } => "invalid_input",
            Self::UnknownVault(_) | Self::VaultNotFound | Self::AmbiguousVault => "vault_error",
            Self::NoteNotFound(_) => "not_found",
            Self::WriterBusy => "writer_busy",
            Self::InputTooLarge { .. } => "input_too_large",
            Self::UnsafeInput(_) => "unsafe_input",
            Self::Validation(_) => "validation_error",
            Self::Workflow(_) => "workflow_error",
        }
    }
}

pub type Result<T> = std::result::Result<T, MemoryError>;
