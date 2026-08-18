//! Pipeline error types.

use koe_ffi::{CaptureError, MonitorError, RecordingError, TranscriptionError};
use thiserror::Error;

/// Errors raised by [`super::RecordingPipeline`].
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("{0}")]
    Recording(#[from] RecordingError),
    #[error("{0}")]
    Capture(#[from] CaptureError),
    #[error("{0}")]
    Transcription(#[from] TranscriptionError),
    #[error("{0}")]
    Monitor(#[from] MonitorError),
    #[error("invalid pipeline state: {0}")]
    InvalidState(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("codec error: {0}")]
    Codec(#[from] crate::codec::CodecError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
