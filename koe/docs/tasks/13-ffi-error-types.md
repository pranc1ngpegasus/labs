---
title: 13 — FFI Error Types
status: draft
depends: [12-ffi-core-exports]
spec_refs: [07-native-bridge, 01-architecture]
---

# 13 — FFI Error Type Definitions

Define error types that cross the FFI boundary with uniffi.

## Location

`koe-ffi/src/error.rs`

## Error Types

```rust
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CaptureError {
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("No audio source found for {bundle_id}")]
    NoAudioSource { bundle_id: String },
    #[error("Capture stream error: {msg}")]
    StreamError { msg: String },
    #[error("Internal error: {msg}")]
    Internal { msg: String },
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TranscriptionError {
    #[error("Unsupported locale: {locale}")]
    UnsupportedLocale { locale: String },
    #[error("Analyzer not available on this OS version")]
    NotAvailable,
    #[error("Transcription internal error: {msg}")]
    Internal { msg: String },
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RecordingError {
    #[error("{0}")]
    Capture(#[from] CaptureError),
    #[error("{0}")]
    Transcription(#[from] TranscriptionError),
    #[error("Insufficient disk space: need {needed}, have {available}")]
    InsufficientDiskSpace { needed: u64, available: u64 },
    #[error("Output already exists: {path}")]
    OutputExists { path: String },
    #[error("Config validation error: {msg}")]
    ConfigError { msg: String },
    #[error("Internal error: {msg}")]
    Internal { msg: String },
}

#[derive(Debug, uniffi::Record)]  // plain data, no Error derive needed
pub struct RecordingSummary {
    pub duration_sec: f64,
    pub bytes_written: u64,
    pub transcript_segment_count: u64,
    pub dropped_audio_frames: u64,
    pub format: OutputFormat,
}
```

## uniffi::Error Trait

uniffi maps `Result<T, E>` where `E: uniffi::Error` to Swift's `throws`.
Swift receives typed errors with pattern matching:

```swift
do {
    let handle = try KoeFfi.startRecording(...)
} catch let error as CaptureError {
    switch error {
    case .permissionDenied(let msg): ...
    case .noAudioSource(let bundleId): ...
    // etc.
    }
}
```

## Conversion Layer

Ensure `koe-core` errors map cleanly into these FFI error types.
Add `From` implementations where appropriate.

## Verification

- Verify error propagation from native layer through FFI to Rust
- Verify Swift catch blocks receive correct error types
- Test each error variant is distinguishable on Swift side
