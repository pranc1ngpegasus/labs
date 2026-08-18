---
title: 14 — FFI Callback Interfaces
status: draft
depends: [12-ffi-core-exports]
spec_refs: [07-native-bridge]
---

# 14 — FFI Callback Interfaces

Define callback traits that bridge the native→Rust and Rust→native directions.

## Location

`koe-ffi/src/callbacks.rs`

## AudioCallback (Native → Rust)

```rust
#[uniffi::export(callback_interface)]
pub trait AudioCallback: Send + Sync {
    /// Called on the native capture thread (or SCK serial queue).
    /// `pcm` is Float32, 48kHz, interleaved stereo.
    /// `timestamp_ms` is a monotonic clock value for AEC alignment.
    fn on_audio(&self, pcm: Vec<f32>, timestamp_ms: u64);
}
```

## TranscriptionCallback (Native → Rust)

```rust
#[uniffi::export(callback_interface)]
pub trait TranscriptionCallback: Send + Sync {
    fn on_segment(&self, segment: TranscriptionSegment);
    fn on_error(&self, error: String);
}
```

## ProgressCallback (Rust → CLI/GUI)

```rust
#[uniffi::export(callback_interface)]
pub trait ProgressCallback: Send + Sync {
    fn on_status(&self, status: RecordingStatus);
    fn on_segment(&self, segment: TranscriptionSegment);
    fn on_error(&self, error: String);
}

#[uniffi::export]
pub struct RecordingStatus {
    pub elapsed_ms: u64,
    pub bytes_written: u64,
    pub level_left: f32,
    pub level_right: f32,
    pub state: RecordingState,
}

#[uniffi::export]
pub enum RecordingState { Recording, Paused, Stopping, Stopped }
```

## Implementation Notes

- uniffi callback interfaces create Swift classes conforming to a generated protocol
- Rust holds `Arc<dyn AudioCallback>`, Swift implements the protocol
- Swift owner must retain callback objects for the lifetime of the capture session

## Performance Constraint

`on_audio` is called from real-time or near-real-time threads. Rust-side handler must:
- Not block
- Minimize allocations
- Ideally just push to a `tokio::sync::mpsc` channel

## Verification

- Implement a mock AudioCallback in Swift, pass to `start_capture`
- Verify `on_audio` fires with correct PCM data
- Verify timestamps are monotonic
- Test that callback objects are properly retained/released
