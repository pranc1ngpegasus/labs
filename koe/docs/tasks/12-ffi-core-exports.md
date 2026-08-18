---
title: 12 — FFI Core Exports
status: draft
depends: [11-uniffi-setup, 03-permission-checker, 07-core-audio-process-tap, 08-screen-capture-kit-capture, 09-microphone-capture, 10-speech-analyzer-bridge]
spec_refs: [07-native-bridge]
---

# 12 — FFI Core Interface Exports

Define the Rust→Swift FFI surface using uniffi proc-macros.

## Location

`koe-ffi/src/lib.rs`

## Types to Export

```rust
#[uniffi::export]
pub enum AudioSourceConfig {
    AppAudio { bundle_id: String },
    PidAudio { pid: i32 },
    Microphone,
    Both { bundle_id: String },
}

#[uniffi::export]
pub enum Permission { Microphone, ScreenRecording, Accessibility }

#[uniffi::export]
pub enum PermissionStatus { Authorized, Denied, Restricted, NotDetermined }

#[uniffi::export]
pub struct TranscriptionSegment {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub is_final: bool,
    pub confidence: f32,
}

#[uniffi::export]
pub struct AppInfo {
    pub pid: i32,
    pub name: String,
    pub bundle_id: Option<String>,
    pub has_audio: bool,
}

#[uniffi::export]
pub enum OutputFormat { Ogg { quality: f32 }, Wav { bits_per_sample: u16 }, Flac { compression_level: u8 } }

#[uniffi::export]
pub enum TranscriptFormat { Txt, Srt, Vtt, Json }
```

## Functions to Export

```rust
#[uniffi::export] pub fn check_permission(permission: Permission) -> PermissionStatus;
#[uniffi::export] pub fn request_permission(permission: Permission) -> PermissionStatus;
#[uniffi::export] pub fn enumerate_apps() -> Vec<AppInfo>;
#[uniffi::export] pub fn start_capture(source: AudioSourceConfig, callback: Arc<dyn AudioCallback>) -> Result<CaptureHandle, CaptureError>;
#[uniffi::export] pub fn stop_capture(handle: CaptureHandle);
#[uniffi::export] pub fn start_transcription(locale: String, callback: Arc<dyn TranscriptionCallback>) -> Result<TranscriptionHandle, TranscriptionError>;
#[uniffi::export] pub fn feed_transcription_audio(handle: TranscriptionHandle, pcm: Vec<f32>);
#[uniffi::export] pub fn finalize_transcription(handle: TranscriptionHandle);
#[uniffi::export] pub fn start_recording(config...) -> Result<RecordingHandle, RecordingError>;
#[uniffi::export] pub fn stop_recording(handle: RecordingHandle) -> Result<RecordingSummary, RecordingError>;
#[uniffi::export] pub fn pause_recording(handle: RecordingHandle);
#[uniffi::export] pub fn resume_recording(handle: RecordingHandle);
```

## Handle Types

Use opaque handle types (`CaptureHandle`, `TranscriptionHandle`, `RecordingHandle`)
that wrap internal state. uniffi supports `!Sync + !Send` handles via `#[uniffi::export]`
on an `impl` block.

## Memory Ownership

- Swift allocates audio buffers, passes ownership to Rust
- Rust receives `Vec<f32>`, processes, drops
- Handles owned by Swift side; Rust cleans up on `stop_*` or drop

## Verification

- Generate Swift bindings, check they compile
- Call `check_permission(.microphone)` from Swift test
- Call `enumerate_apps()` from Swift test
