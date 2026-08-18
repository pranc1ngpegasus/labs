---
title: 44 — Error Recovery Paths
status: draft
depends: [15-pipeline-core, 22-shutdown-sequence]
spec_refs: [10-recording-pipeline, 01-architecture]
---

# 44 — Error Recovery

Implement recovery strategies for common pipeline failure modes.

## Location

`koe-core/src/pipeline/error_recovery.rs`

## Error Scenarios & Recovery

| Failure | Detection | Recovery |
|---------|-----------|----------|
| Ring buffer overflow | Write returns false → drop counter increments | Consumer skips ahead; resume on next available chunk |
| Encoder error (disk full) | `CodecError` on encode | Pause pipeline; emit `StorageFull`; wait for space → resume |
| Speech analyzer error | `onError` callback from SFSpeechAnalyzer | Log error; transcription stops; audio recording continues (non-fatal) |
| Native capture stream broken (app quit) | SCK `applicationUnavailable` / tap stops | Drain pipeline; finalize with partial results |
| AEC filter divergence | ERLE drops below threshold | Reset filter coefficients to zero; log event; transient echo until reconvergence |
| Consumer lag | `RecvError::Lagged(n)` | Log warning with count; skip ahead |
| Permission revoked mid-session | TCC notification | Stop capture; notify user; keep partial results |

## Error Handling Philosophy

Per [01-architecture]:
- **Native callbacks must not panic** — errors are logged and capture is gracefully torn down
- **Rust core uses `thiserror`** — typed errors at module boundaries
- **CLI** — non-zero exit code + stderr message
- **GUI** — in-app notifications (not modal dialogs) to avoid disrupting recording state

## Implementation Patterns

### Non-Fatal Errors (Continue Recording)
```rust
async fn handle_non_fatal(error: &PipelineError, state: &mut PipelineState) {
    match error {
        PipelineError::TranscriptionError(_) => {
            log::warn!("Transcription error; audio recording continues");
            state.transcription_active = false;
            // Emit notification for GUI/CLI
        }
        PipelineError::AecDivergence => {
            log::warn!("AEC divergence detected; resetting filter");
            state.aec.as_mut().unwrap().reset();
        }
        PipelineError::RingBufferOverflow { count } => {
            log::warn!("Ring buffer overflow: {} frames dropped", count);
            state.metrics.dropped_frames += count;
        }
        _ => {}
    }
}
```

### Fatal Errors (Stop Recording)
```rust
async fn handle_fatal(error: &PipelineError, pipeline: &mut RecordingPipeline) {
    log::error!("Fatal pipeline error: {}", error);
    // Attempt graceful shutdown with partial results
    pipeline.emergency_stop().await;
}
```

## Timeout Recovery

For transient failures (e.g., app temporarily stops producing audio), implement a timeout:
```
No audio samples for 5 seconds → warn
No audio samples for 30 seconds → auto-pause (keep files open, await resume)
No audio samples for 5 minutes → auto-stop and finalize
```

## Verification

- Fill disk during recording → StorageFull error → pause → free space → resume
- Kill target app during capture → pipeline drains and finalizes
- Corrupt audio callback → log error, attempt reconnection
- Verify partial results are saved on fatal errors
