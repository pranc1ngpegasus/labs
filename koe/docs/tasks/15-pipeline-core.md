---
title: 15 — Pipeline Core (RecordingPipeline)
status: draft
depends: [12-ffi-core-exports, 13-ffi-error-types, 14-ffi-callback-interfaces]
spec_refs: [10-recording-pipeline, 01-architecture]
---

# 15 — RecordingPipeline Struct & Lifecycle

Implement the central `RecordingPipeline` in `koe-core`.

## Location

`koe-core/src/pipeline.rs`

## Data Types

```rust
pub struct RecordingPipeline {
    config: PipelineConfig,
    state: PipelineState,
    aec: Option<AcousticEchoCanceller>,
    encoder: Box<dyn AudioEncoder>,
    speech: SpeechAnalyzerHandle,
    transcript_fmt: Box<dyn TranscriptFormatter>,
    file_writer: FileWriter,
    drop_counter: Arc<AtomicU64>,
    metrics: PipelineMetrics,
}

pub enum PipelineState {
    Idle,
    Recording { start_time: Instant, bytes_written: u64, segments: Vec<TranscriptionSegment> },
    Paused { elapsed_before_pause: Duration },
    Stopped,
}

pub struct PipelineConfig {
    pub source: AudioSourceConfig,
    pub output_path: PathBuf,
    pub transcript_output_path: Option<PathBuf>,
    pub locale: String,
    pub audio_format: OutputFormat,
    pub transcript_format: TranscriptFormat,
    pub enable_aec: bool,
    pub comfort_noise: bool,
    pub monitor: bool,
}
```

## Lifecycle Methods

### `start(config: PipelineConfig) -> Result<Self, PipelineError>`
1. Validate config (check permissions, disk space)
2. Open output files (audio + transcript)
3. Initialize encoder based on format
4. Initialize speech analyzer with locale
5. Create ring buffer (FFI call)
6. Initialize AEC if `source` includes `Both`
7. Start native capture (FFI call)
8. Spawn consumer task loop
9. Return `RecordingPipeline` in `Recording` state

### `stop(&mut self) -> Result<RecordingSummary, PipelineError>`
1. Set shutdown flag (AtomicBool)
2. Stop native capture (FFI)
3. Drain ring buffer
4. Finalize speech analyzer (flush partial segments)
5. Finalize encoder (write trailer/close OGG stream)
6. Flush and close files
7. Return `RecordingSummary`

### `pause(&mut self)`
1. Signal native capture to pause (keep tap alive, stop producing)
2. Transition to `Paused` state
3. Do NOT finalize speech analyzer

### `resume(&mut self)`
1. Signal native capture to resume
2. Transition back to `Recording` state
3. Speech analyzer resumes transparently

## Verification

- Integration test: start → feed some audio → stop → verify summary
- Test pause/resume cycle
- Test error: start with denied permission → appropriate error
- Test error: start with full disk → `InsufficientDiskSpace`
